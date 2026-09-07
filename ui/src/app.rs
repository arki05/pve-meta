//! Application root: session bootstrap, `?vmid=`/`?dc=1` routing, and the page frame.
//!
//! The frame is the one PDM uses for a content page (`docs/design/PDM-DESIGN-LANGUAGE.md`
//! §1.3): a `pwt-viewport` column holding a single `pwt-content-spacer`, whose only child
//! is the editor — which makes it full-bleed and border-less, exactly like PDM's Notes
//! page. `DesktopApp` brings the `ThemeLoader` that injects the stylesheet and toggles
//! `pwt-dark-mode`; never write a theme `<link>` by hand.

use std::collections::HashMap;

use yew::prelude::*;

use pwt::css::{AlignItems, FlexFit, FontStyle, JustifyContent};
use pwt::prelude::*;
use pwt::widget::{Column, Container, DesktopApp, Fa};

use proxmox_login::Authentication;
use proxmox_yew_comp::{ExistingProduct, authentication_from_cookie};

use crate::editor::MetaEditor;
use crate::model::DocId;

/// The current page's query parameters.
pub fn location_query() -> HashMap<String, String> {
    let mut params = HashMap::new();
    if let Some(window) = web_sys::window() {
        if let Ok(search) = window.location().search() {
            for (key, value) in
                url::form_urlencoded::parse(search.trim_start_matches('?').as_bytes())
            {
                params.insert(key.into_owned(), value.into_owned());
            }
        }
    }
    params
}

/// The document addressed by `?vmid=<id>` or `?dc=1`, if any.
fn parse_route(query: &HashMap<String, String>) -> Option<DocId> {
    match query.get("dc").map(String::as_str) {
        Some("1") | Some("true") => return Some(DocId::Datacenter),
        _ => {}
    }
    query.get("vmid")?.parse().ok().map(DocId::Guest)
}

/// Session-resolution state: see `crate::auth` for how a usable ticket and CSRF token are
/// found now that nothing server-side templates one into this page.
enum AuthState {
    Resolving,
    LoggedIn,
    NotLoggedIn,
}

pub enum Msg {
    AuthResolved(Option<Authentication>),
}

pub struct App {
    auth: AuthState,
    doc: Option<DocId>,
    guest_type: Option<AttrValue>,
    node: Option<AttrValue>,
}

impl Component for App {
    type Message = Msg;
    type Properties = ();

    fn create(ctx: &Context<Self>) -> Self {
        // Fast path: a cookie ticket plus an already known CSRF token. Otherwise resolve
        // one asynchronously (parent-frame token when embedded, ticket renewal else).
        let auth = match authentication_from_cookie(&ExistingProduct::PVE) {
            Some(info) => {
                proxmox_yew_comp::http_set_auth(info);
                AuthState::LoggedIn
            }
            None => {
                ctx.link()
                    .send_future(async { Msg::AuthResolved(crate::auth::resolve_auth().await) });
                AuthState::Resolving
            }
        };

        let query = location_query();

        Self {
            auth,
            doc: parse_route(&query),
            guest_type: query.get("type").map(|t| AttrValue::from(t.clone())),
            node: query.get("node").map(|n| AttrValue::from(n.clone())),
        }
    }

    fn update(&mut self, _ctx: &Context<Self>, msg: Self::Message) -> bool {
        match msg {
            Msg::AuthResolved(Some(info)) => {
                proxmox_yew_comp::http_set_auth(info);
                self.auth = AuthState::LoggedIn;
            }
            Msg::AuthResolved(None) => self.auth = AuthState::NotLoggedIn,
        }
        true
    }

    fn view(&self, _ctx: &Context<Self>) -> Html {
        let content: Html = match (&self.auth, self.doc) {
            (AuthState::Resolving, _) => Container::from_tag("i")
                .class("pwt-loading-icon")
                .class(FlexFit)
                .into(),
            (AuthState::NotLoggedIn, _) => empty_state(
                "sign-out",
                tr!("Not logged in"),
                tr!("Log in to the Proxmox VE web interface on this host, then reload."),
            ),
            (AuthState::LoggedIn, None) => empty_state(
                "tags",
                tr!("No document selected"),
                tr!("Open this page from the Metadata tab of a guest or of the datacenter."),
            ),
            (AuthState::LoggedIn, Some(doc)) => MetaEditor::new(doc)
                .guest_type(self.guest_type.clone())
                .node(self.node.clone())
                .into(),
        };

        let body = Column::new().class("pwt-viewport").with_child(
            Container::new()
                .class("pwt-content-spacer")
                .class(FlexFit)
                .with_child(content),
        );

        DesktopApp::new(body).into()
    }
}

/// A centered, large-icon empty state with a title and an explanatory hint — the same
/// shape as PDM's `renderer::empty_state`.
fn empty_state(icon: &str, title: String, hint: String) -> Html {
    Column::new()
        .class(FlexFit)
        .class(JustifyContent::Center)
        .class(AlignItems::Center)
        .gap(2)
        .padding(4)
        .with_child(Fa::new(icon).large_3x())
        .with_child(
            Container::from_tag("span")
                .class(FontStyle::TitleMedium)
                .with_child(title),
        )
        .with_child(Container::from_tag("span").with_child(hint))
        .into()
}
