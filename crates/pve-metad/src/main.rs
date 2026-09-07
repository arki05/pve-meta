//! `pve-metad`: the HTTPS API daemon.
//!
//! Serves `pve-meta-api`'s router tree under `/api2/json/meta/...`, plus the editor UI's static
//! files, over TLS using the node's real PVE certificate. Must run as root: it reads
//! `/etc/pve/authkey.pub[.old]`, `/etc/pve/pve-www.key` and `/etc/pve/local/pve-ssl.{key,pem}`,
//! and writes under `/etc/pve/meta`.

use std::future::Future;
use std::io::IsTerminal as _;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use anyhow::{Context as _, Error};
use futures::StreamExt as _;
use http::request::Parts;
use http::{HeaderMap, Method};
use hyper::Response;
use hyper_util::server::graceful::GracefulShutdown;
use openssl::ssl::SslAcceptor;
use tokio::net::TcpListener;

use proxmox_http::Body;
use proxmox_log::LevelFilter;
use proxmox_rest_server::connection::{AcceptBuilder, TlsAcceptorBuilder};
use proxmox_rest_server::{ApiConfig, AuthError, RestEnvironment, RestServer};
use proxmox_router::{RpcEnvironmentType, UserInformation};

/// v1 has no per-path authorization: any authenticated caller (or a valid API token) gets full
/// access. This is the seam where claims-based enforcement slots in later.
struct AllowAll;

impl UserInformation for AllowAll {
    fn is_superuser(&self, _userid: &str) -> bool {
        true
    }
    fn is_group_member(&self, _userid: &str, _group: &str) -> bool {
        false
    }
    fn lookup_privs(&self, _userid: &str, _path: &[&str]) -> u64 {
        u64::MAX
    }
}

fn env_path(var: &str, default: &str) -> PathBuf {
    std::env::var_os(var).map(PathBuf::from).unwrap_or_else(|| PathBuf::from(default))
}

fn listen_addr() -> Result<SocketAddr, Error> {
    let raw = std::env::var("PVE_META_LISTEN").unwrap_or_else(|_| "[::]:8007".to_string());
    raw.parse().with_context(|| format!("invalid PVE_META_LISTEN address '{raw}'"))
}

#[allow(clippy::type_complexity)]
fn check_auth<'a>(
    headers: &'a HeaderMap,
    method: &'a Method,
) -> Pin<Box<dyn Future<Output = Result<(String, Box<dyn UserInformation + Send + Sync>), AuthError>> + Send + 'a>> {
    let method = method.clone();
    Box::pin(async move {
        let auth = pve_meta_api::auth::global().expect("auth not initialized");
        match pve_meta_api::auth::identify(auth, headers, &method) {
            Ok(identity) => Ok((identity.auth_id, Box::new(AllowAll) as _)),
            Err(pve_meta_api::auth::AuthFailure::Missing) => Err(AuthError::NoData),
            Err(e @ pve_meta_api::auth::AuthFailure::Invalid(_)) => {
                Err(AuthError::Generic(anyhow::anyhow!("{e}")))
            }
        }
    })
}

fn ui_dir() -> PathBuf {
    env_path("PVE_META_UI_DIR", "/usr/share/pve-meta/ui")
}

fn get_index(
    _env: RestEnvironment,
    parts: Parts,
) -> Pin<Box<dyn Future<Output = Response<Body>> + Send>> {
    Box::pin(async move {
        let dir = ui_dir();
        let html = match std::fs::read_to_string(dir.join("index.html")) {
            Ok(html) => html,
            Err(e) => {
                return Response::builder()
                    .status(404)
                    .body(Body::from(format!("index.html not found under {}: {e}", dir.display())))
                    .unwrap();
            }
        };

        let auth = pve_meta_api::auth::global();
        let ticket = pve_meta_api::auth::extract_ticket_cookie(&parts.headers);
        let (username, csrf_token) = match (auth, ticket) {
            (Some(auth), Some(ticket)) => match auth.verify_ticket(&ticket) {
                Ok(userid) => {
                    let userid = userid.to_string();
                    let csrf = auth.assemble_csrf(&userid).unwrap_or_default();
                    (userid, csrf)
                }
                Err(_) => (String::new(), String::new()),
            },
            _ => (String::new(), String::new()),
        };

        let script = format!(
            "<script>Proxmox = {{ Setup: {{ auth_cookie_name: 'PVEAuthCookie' }}, \
             UserName: \"{}\", CSRFPreventionToken: \"{}\" }};</script>",
            username.replace('\\', "\\\\").replace('"', "\\\""),
            csrf_token.replace('\\', "\\\\").replace('"', "\\\"")
        );
        let html = match html.find("<head>") {
            Some(pos) => {
                let split = pos + "<head>".len();
                format!("{}{}{}", &html[..split], script, &html[split..])
            }
            None => format!("{script}{html}"),
        };

        Response::builder()
            .status(200)
            .header("Content-Type", "text/html; charset=utf-8")
            .header("Cache-Control", "no-cache")
            .body(Body::from(html))
            .unwrap()
    })
}

fn make_tls_acceptor() -> Result<SslAcceptor, Error> {
    let key = env_path("PVE_META_TLS_KEY", "/etc/pve/local/pve-ssl.key");
    let cert = env_path("PVE_META_TLS_CERT", "/etc/pve/local/pve-ssl.pem");
    TlsAcceptorBuilder::new()
        .certificate_paths_pem(key, cert)
        .build()
        .context("building TLS acceptor")
}

async fn run() -> Result<(), Error> {
    let is_tty = std::io::stderr().is_terminal();
    let mut logger = proxmox_log::Logger::from_env("PVE_META_LOG", LevelFilter::INFO);
    logger = if is_tty { logger.stderr() } else { logger.journald() };
    logger.init().context("initializing logger")?;

    pve_meta_api::store::init_default();
    pve_meta_api::auth::init_default();

    let dir = ui_dir();
    let config = ApiConfig::new(dir.clone(), RpcEnvironmentType::PUBLIC)
        .default_api2_handler(&pve_meta_api::api::ROUTER)
        .auth_handler_func(check_auth)
        .index_handler_func(get_index)
        .alias("ui", dir);
    let rest_server = RestServer::new(config);

    let acceptor = Arc::new(Mutex::new(make_tls_acceptor()?));
    let connections = AcceptBuilder::new().debug(false);
    let addr = listen_addr()?;

    proxmox_daemon::catch_shutdown_signal(std::future::pending())?;
    proxmox_daemon::catch_reload_signal(std::future::pending())?;

    proxmox_daemon::server::create_daemon::<_, _, TcpListener>(
        addr,
        move |listener: TcpListener| {
            let mut secure = connections.accept_tls(listener, acceptor);
            Ok(async move {
                proxmox_systemd::notify::SystemdNotify::Ready
                    .notify()
                    .context("notifying systemd")?;
                tracing::info!(%addr, "pve-metad listening");
                let graceful = GracefulShutdown::new();
                loop {
                    tokio::select! {
                        conn = secure.next() => {
                            let Some(conn) = conn else { break };
                            match conn {
                                Ok(conn) => match rest_server.api_service(&conn) {
                                    Ok(svc) => {
                                        let watcher = graceful.watcher();
                                        tokio::spawn(async move {
                                            let _ = svc.serve(conn, Some(watcher)).await;
                                        });
                                    }
                                    Err(e) => tracing::warn!("failed to build api service: {e:#}"),
                                },
                                Err(e) => tracing::warn!("failed to accept connection: {e:#}"),
                            }
                        }
                        _ = proxmox_daemon::shutdown_future() => break,
                    }
                }
                graceful.shutdown().await;
                Ok::<(), Error>(())
            })
        },
        Some(pidfile()),
    )
    .await
}

fn pidfile() -> &'static str {
    "/run/pve-metad.pid"
}

fn main() -> Result<(), Error> {
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(run())
}
