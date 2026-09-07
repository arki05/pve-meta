//! Top-level `App`: login check, `?vmid=`/`?dc=` routing, the guest list, the
//! version-poll long-poll loop, and the not-logged-in notice.

use std::collections::HashMap;
use std::rc::Rc;

use anyhow::Error;
use serde_json::Value;
use yew::prelude::*;

use pwt::prelude::*;
use pwt::widget::{Column, DesktopApp, Row, ThemeModeSelector};

use proxmox_yew_comp::{authentication_from_cookie, ExistingProduct};

use crate::api::{self, InventoryEntry, Operator};
use crate::editor::Editor;
use crate::guests::GuestList;
use crate::model::DocId;

/// Parse `?vmid=`/`?dc=` from the current URL. `dc=1` wins if both are present.
/// Returns `(selected document, embedded mode)`.
fn parse_route() -> (Option<DocId>, bool) {
    let mut vmid: Option<u32> = None;
    let mut dc = false;

    if let Some(win) = web_sys::window() {
        if let Ok(search) = win.location().search() {
            for (k, v) in url::form_urlencoded::parse(search.trim_start_matches('?').as_bytes()) {
                match k.as_ref() {
                    "vmid" => vmid = v.parse().ok(),
                    "dc" => dc = v == "1" || v.eq_ignore_ascii_case("true"),
                    _ => {}
                }
            }
        }
    }

    if dc {
        (Some(DocId::Datacenter), true)
    } else if let Some(id) = vmid {
        (Some(DocId::Guest(id)), true)
    } else {
        (None, false)
    }
}

pub enum Msg {
    GuestsLoaded(Result<Vec<InventoryEntry>, Error>),
    RegistryLoaded(Result<Vec<Operator>, Error>),
    SchemasLoaded(Result<HashMap<String, Value>, Error>),
    SelectDoc(DocId),
    VersionToken(String),
}

pub struct App {
    user: Option<String>,
    embedded: bool,
    selected: Option<DocId>,
    guests: Option<Result<Vec<InventoryEntry>, String>>,
    registry: Rc<Vec<Operator>>,
    schemas: Rc<HashMap<String, Value>>,
    version_token: Option<String>,
}

impl App {
    fn current_label(&self) -> String {
        match self.selected {
            None => String::new(),
            Some(DocId::Datacenter) => "Datacenter".to_string(),
            Some(DocId::Guest(vmid)) => {
                let entry = self
                    .guests
                    .as_ref()
                    .and_then(|r| r.as_ref().ok())
                    .and_then(|list| list.iter().find(|e| e.vmid == vmid));
                match entry {
                    Some(e) => format!(
                        "{vmid} {} ({}, {})",
                        e.name.clone().unwrap_or_default(),
                        e.guest_type,
                        e.node
                    ),
                    None => vmid.to_string(),
                }
            }
        }
    }
}

impl Component for App {
    type Message = Msg;
    type Properties = ();

    fn create(ctx: &Context<Self>) -> Self {
        let login = authentication_from_cookie(&ExistingProduct::PVE);
        let user = login.as_ref().map(|a| a.userid.clone());
        if let Some(info) = login {
            proxmox_yew_comp::http_set_auth(info);
        }

        let (selected, embedded) = parse_route();

        ctx.link()
            .send_future(async { Msg::GuestsLoaded(api::inventory().await) });
        ctx.link()
            .send_future(async { Msg::RegistryLoaded(api::registry().await) });
        if let Some(DocId::Guest(vmid)) = selected {
            ctx.link()
                .send_future(async move { Msg::SchemasLoaded(api::schemas(vmid).await) });
        }

        // Long-poll `GET /meta/version` for the app's lifetime. Runs regardless of
        // login state (harmless if unauthenticated: the request just fails and we
        // back off) so nothing extra needs wiring once a login happens.
        let poll_link = ctx.link().clone();
        wasm_bindgen_futures::spawn_local(async move {
            let mut since: Option<String> = None;
            loop {
                match api::version(Some(25), since.as_deref()).await {
                    Ok(info) => {
                        if since.as_ref() != Some(&info.token) {
                            since = Some(info.token.clone());
                            poll_link.send_message(Msg::VersionToken(info.token));
                        }
                    }
                    Err(e) => {
                        log::warn!("pve-meta-ui: version poll failed: {e}");
                        gloo_timers::future::TimeoutFuture::new(3_000).await;
                    }
                }
            }
        });

        Self {
            user,
            embedded,
            selected,
            guests: None,
            registry: Rc::new(Vec::new()),
            schemas: Rc::new(HashMap::new()),
            version_token: None,
        }
    }

    fn update(&mut self, ctx: &Context<Self>, msg: Self::Message) -> bool {
        match msg {
            Msg::GuestsLoaded(result) => {
                self.guests = Some(result.map_err(|e| api::error_text(&e)));
            }
            Msg::RegistryLoaded(Ok(registry)) => self.registry = Rc::new(registry),
            Msg::RegistryLoaded(Err(e)) => {
                log::error!("pve-meta-ui: failed to load registry: {e}")
            }
            Msg::SchemasLoaded(Ok(schemas)) => self.schemas = Rc::new(schemas),
            Msg::SchemasLoaded(Err(e)) => log::error!("pve-meta-ui: failed to load schemas: {e}"),
            Msg::SelectDoc(id) => {
                if self.embedded {
                    // The route is fixed by the query string in embedded mode.
                    return false;
                }
                self.selected = Some(id);
                self.schemas = Rc::new(HashMap::new());
                if let DocId::Guest(vmid) = id {
                    ctx.link()
                        .send_future(async move { Msg::SchemasLoaded(api::schemas(vmid).await) });
                }
            }
            Msg::VersionToken(token) => {
                self.version_token = Some(token);
                // "Also refresh the guest list when the token changes."
                ctx.link()
                    .send_future(async { Msg::GuestsLoaded(api::inventory().await) });
            }
        }
        true
    }

    fn view(&self, ctx: &Context<Self>) -> Html {
        if self.user.is_none() {
            return DesktopApp::new(not_logged_in_notice()).into();
        }

        let mut header = Row::new()
            .class("pve-meta-header pwt-bg-color-primary pwt-color-on-primary pwt-align-items-center")
            .padding(2)
            .gap(2)
            .with_child(html! {<span class="pwt-font-title-medium">{"pve-meta"}</span>})
            .with_child(html! {<span class="pve-meta-current-label">{self.current_label()}</span>})
            .with_flex_spacer();

        if !self.embedded {
            header.add_child(html! {<span>{self.user.clone().unwrap_or_default()}</span>});
        }
        header.add_child(ThemeModeSelector::new());

        let mut content = Row::new().class("pve-meta-content pwt-flex-fill");

        if !self.embedded {
            let entries = Rc::new(
                self.guests
                    .as_ref()
                    .and_then(|r| r.as_ref().ok())
                    .cloned()
                    .unwrap_or_default(),
            );
            let on_select = ctx.link().callback(Msg::SelectDoc);
            content.add_child(html! {
                <GuestList entries={entries} selected={self.selected} on_select={on_select} />
            });
        }

        let main_area = match self.selected {
            Some(id) => {
                let key = id.label();
                let schemas = self.schemas.clone();
                let registry = self.registry.clone();
                let version_token = self.version_token.clone();
                html! {
                    <Editor key={key} id={id} schemas={schemas} registry={registry} version_token={version_token} />
                }
            }
            None => html! {
                <div class="pve-meta-placeholder pwt-color-neutral-alt">
                    {"Select a guest or the datacenter document."}
                </div>
            },
        };
        content.add_child(main_area);

        let mut root = Column::new().class("pve-meta-app pwt-viewport").with_child(header);
        if let Some(Err(err)) = &self.guests {
            root.add_child(html! {<div class="pwt-color-error">{err.clone()}</div>});
        }
        root.add_child(content);

        DesktopApp::new(root).into()
    }
}

fn not_logged_in_notice() -> Html {
    let host = web_sys::window()
        .and_then(|w| w.location().hostname().ok())
        .unwrap_or_default();
    let href = format!("https://{host}:8006/");
    Column::new()
        .class("pve-meta-not-logged-in")
        .padding(4)
        .gap(2)
        .with_child(html! {<h2>{"Not logged in to Proxmox VE"}</h2>})
        .with_child(html! {
            <p>{"Log in to the PVE web UI on this host first, then reload."}</p>
        })
        .with_child(html! {<a href={href.clone()} target="_blank">{href}</a>})
        .into()
}
