//! The metadata editor page.
//!
//! One `LoadableComponent` for one document (`docs/DESIGN.md` §6), structured exactly
//! like `proxmox_yew_comp::NotesView`: `load()` fetches a text document and its digest,
//! `toolbar()` returns the standard three-class `Toolbar`, `main_view()` fills the
//! remaining height, and `dialog_view()` returns the one modal. Everything else — the
//! outer column, the load-error strip, dialog stacking, off-screen refresh suspension —
//! comes from `LoadableComponentMaster`.
//!
//! The one deviation from the pwt stack is the editor itself: Monaco, mounted into a
//! plain `Container` from `rendered()` through the glue in `js/pve-meta-monaco.js`
//! (`crate::monaco`).

use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;

use anyhow::Error;
use wasm_bindgen::prelude::Closure;
use web_sys::Element;
use yew::html::IntoPropValue;
use yew::virtual_dom::{VComp, VNode};

use pwt::css::{AlignItems, ColorScheme, FlexFit, FontStyle, JustifyContent};
use pwt::prelude::*;
use pwt::state::ThemeObserver;
use pwt::widget::form::Combobox;
use pwt::widget::{Button, Column, Container, Dialog, Fa, Row, Toolbar, error_message};
use pwt_macros::builder;

use proxmox_yew_comp::{
    ConfirmButton, LoadableComponent, LoadableComponentContext, LoadableComponentMaster,
    LoadableComponentScopeExt, LoadableComponentState,
};

use crate::api::{self, GuestEntry, WriteResult};
use crate::model::{Access, DocId};
use crate::monaco;

/// `GET /meta/version` is polled this often. Its token covers the whole store, so a
/// change only triggers a digest check of this one document.
const VERSION_POLL_MS: u32 = 5_000;

/// The "View as" entry standing for the whole document (the empty prefix). A sentinel is
/// needed because a `Combobox` cannot hold an empty value — the same trick PDM's
/// `ViewSelector` uses for its `__dashboard__` entry.
const WHOLE_DOCUMENT: &str = "__document__";

/// Everything one load produces.
pub struct Loaded {
    /// Only on the first load — the grants do not change under us.
    access: Option<Access>,
    /// Only on the first load — the guest's name and node, for the identity line.
    guest: Option<GuestEntry>,
    /// Top-level keys of the whole document, for the "View as" selector.
    keys: Vec<String>,
    digest: String,
    text: String,
}

/// Modal states of the page.
#[derive(PartialEq)]
pub enum ViewState {
    ConfirmApply,
}

pub enum Msg {
    Loaded(Box<Loaded>),
    SelectView(String),
    EditorInput(String),
    ShowDiff,
    CloseDialog,
    Apply,
    Applied(Result<WriteResult, Error>),
    Discard,
    VersionToken(String),
    ServerDigest(String),
    ThemeChanged(bool),
}

/// The metadata editor for one document.
#[derive(Clone, PartialEq, Properties)]
#[builder]
pub struct MetaEditor {
    /// Document to edit.
    pub doc: DocId,

    /// Guest type (`lxc` or `qemu`), as passed by the tab loader.
    #[builder(IntoPropValue, into_prop_value)]
    #[prop_or_default]
    pub guest_type: Option<AttrValue>,

    /// Node the guest lives on, as passed by the tab loader.
    #[builder(IntoPropValue, into_prop_value)]
    #[prop_or_default]
    pub node: Option<AttrValue>,
}

impl MetaEditor {
    /// Create a new instance.
    pub fn new(doc: DocId) -> Self {
        yew::props!(Self { doc })
    }
}

#[doc(hidden)]
pub struct PveMetaEditor {
    state: LoadableComponentState<ViewState>,
    /// The caller's effective grants.
    access: Access,
    /// Set once the grants have been fetched.
    access_loaded: bool,
    /// Guest name and node, for the identity line.
    guest: Option<GuestEntry>,
    /// The selected view: a key-path prefix, empty for the whole document.
    view: String,
    /// The prefixes the "View as" selector offers.
    views: Rc<Vec<AttrValue>>,
    /// Text and digest as loaded.
    loaded: String,
    digest: String,
    /// Live editor buffer; `None` while unmodified.
    draft: Option<String>,
    /// Bumped by every load, so `rendered()` knows when to push new text into Monaco.
    generation: u64,
    mounted_generation: Option<u64>,
    /// The last message of a failed write, shown verbatim under the editor.
    write_error: Option<String>,
    /// A poll found this document's digest changed while there were unapplied edits,
    /// or a write was refused with a 409.
    stale: bool,
    version_token: Option<String>,
    editor_ref: NodeRef,
    editor_id: Option<String>,
    diff_ref: NodeRef,
    diff_id: Option<String>,
    dialog_open: bool,
    /// Kept alive for as long as the editor exists.
    on_change: Option<Closure<dyn Fn(String)>>,
    /// Kept alive so the `pwt-theme-changed` listeners stay registered.
    _theme_observer: ThemeObserver,
    dark_mode: bool,
}

pwt::impl_deref_mut_property!(PveMetaEditor, state, LoadableComponentState<ViewState>);

impl PveMetaEditor {
    /// True if the caller may edit the selected view.
    fn writable(&self, ctx: &LoadableComponentContext<Self>) -> bool {
        self.access.may_write(ctx.props().doc, &self.view)
    }

    /// The text the editor currently shows.
    fn current_text(&self) -> &str {
        self.draft.as_deref().unwrap_or(&self.loaded)
    }

    /// The document identity line, e.g. `200 traefik` + `(lxc, node1)`.
    fn header(&self, ctx: &LoadableComponentContext<Self>) -> Row {
        let props = ctx.props();

        let (icon, title) = match props.doc {
            DocId::Datacenter => ("building", tr!("Datacenter")),
            DocId::Guest(vmid) => {
                let icon = match props.guest_type.as_deref() {
                    Some("lxc") => "cube",
                    _ => "desktop",
                };
                let name = self.guest.as_ref().and_then(|g| g.name.clone());
                match name {
                    Some(name) if !name.is_empty() => (icon, format!("{vmid} {name}")),
                    _ => (icon, vmid.to_string()),
                }
            }
        };

        let mut details: Vec<String> = Vec::new();
        if let Some(guest_type) = props.guest_type.as_deref() {
            details.push(guest_type.to_string());
        }
        if let Some(node) = props.node.as_deref() {
            details.push(node.to_string());
        }

        Row::new()
            .class(AlignItems::Baseline)
            .class("pwt-border-bottom")
            .padding(2)
            .gap(2)
            .with_child(Fa::new(icon))
            .with_child(
                Container::from_tag("span")
                    .class(FontStyle::TitleMedium)
                    .with_child(title),
            )
            .with_optional_child((!details.is_empty()).then(|| {
                Container::from_tag("span")
                    .class("pwt-color-on-neutral-alt")
                    .with_child(format!("({})", details.join(", ")))
            }))
    }

    /// The non-modal "changed on the server" notice.
    fn stale_banner(&self, ctx: &LoadableComponentContext<Self>) -> Row {
        let link = ctx.link().clone();
        Row::new()
            .padding(2)
            .gap(2)
            .class(AlignItems::Center)
            .class(ColorScheme::WarningContainer)
            .class("pwt-default-colors")
            .class("pwt-border-bottom")
            .with_child(Fa::new("exclamation-triangle"))
            .with_child(tr!(
                "This document was changed on the server since it was loaded."
            ))
            .with_flex_spacer()
            .with_child(Button::new(tr!("Reload")).on_activate(move |_| link.send_reload()))
    }

    /// The Apply confirmation: a Monaco diff of the loaded text against the edited one.
    fn diff_dialog(&self, ctx: &LoadableComponentContext<Self>) -> Html {
        let link = ctx.link();
        Dialog::new(tr!("Apply") + ": " + &tr!("Changes"))
            .width(900)
            .height(600)
            .resizable(true)
            .on_close(link.callback(|_| Msg::CloseDialog))
            .with_child(
                Container::new()
                    .class(FlexFit)
                    .class("pve-meta-monaco-host")
                    .into_html_with_ref(self.diff_ref.clone()),
            )
            .with_child(
                Row::new()
                    .padding(2)
                    .gap(2)
                    .class(JustifyContent::FlexEnd)
                    .class("pwt-border-top")
                    .with_child(
                        Button::new(tr!("Cancel")).on_activate(link.callback(|_| Msg::CloseDialog)),
                    )
                    .with_child(
                        Button::new(tr!("Apply"))
                            .class(ColorScheme::Primary)
                            .on_activate(link.callback(|_| Msg::Apply)),
                    ),
            )
            .into()
    }

    fn dispose_diff(&mut self) {
        if let Some(id) = self.diff_id.take() {
            monaco::dispose(&id);
        }
    }
}

impl Drop for PveMetaEditor {
    fn drop(&mut self) {
        // Monaco leaks a ResizeObserver and a model otherwise.
        if let Some(id) = self.editor_id.take() {
            monaco::dispose(&id);
        }
        self.dispose_diff();
    }
}

impl LoadableComponent for PveMetaEditor {
    type Properties = MetaEditor;
    type Message = Msg;
    type ViewState = ViewState;

    fn create(ctx: &LoadableComponentContext<Self>) -> Self {
        let theme_observer =
            ThemeObserver::new(ctx.link().callback(|(_, dark)| Msg::ThemeChanged(dark)));
        let dark_mode = theme_observer.dark_mode();

        // The store's content hash, polled on an interval — the native module answers
        // `GET /meta/version` immediately, there is no long poll.
        let link = ctx.link().clone();
        ctx.link().spawn(async move {
            loop {
                gloo_timers::future::TimeoutFuture::new(VERSION_POLL_MS).await;
                match api::version().await {
                    Ok(info) => link.send_message(Msg::VersionToken(info.token)),
                    Err(err) => log::warn!("pve-meta-ui: version poll failed: {err}"),
                }
            }
        });

        Self {
            state: LoadableComponentState::new(),
            access: Access::default(),
            access_loaded: false,
            guest: None,
            view: String::new(),
            views: Rc::new(vec![AttrValue::from(WHOLE_DOCUMENT)]),
            loaded: String::new(),
            digest: String::new(),
            draft: None,
            generation: 0,
            mounted_generation: None,
            write_error: None,
            stale: false,
            version_token: None,
            editor_ref: NodeRef::default(),
            editor_id: None,
            diff_ref: NodeRef::default(),
            diff_id: None,
            dialog_open: false,
            on_change: None,
            _theme_observer: theme_observer,
            dark_mode,
        }
    }

    fn load(
        &self,
        ctx: &LoadableComponentContext<Self>,
    ) -> Pin<Box<dyn Future<Output = Result<(), Error>>>> {
        let doc = ctx.props().doc;
        let view = self.view.clone();
        let need_access = !self.access_loaded;
        let need_guest = matches!(doc, DocId::Guest(_)) && self.guest.is_none();
        let link = ctx.link().clone();

        Box::pin(async move {
            let access = match need_access {
                true => Some(api::access().await?),
                false => None,
            };

            // The identity line wants the guest's name; a failure here is not worth
            // failing the page over.
            let guest = match need_guest {
                true => api::guests()
                    .await
                    .map_err(|err| log::warn!("pve-meta-ui: failed to list guests: {err}"))
                    .ok()
                    .and_then(|list| {
                        list.into_iter()
                            .find(|entry| DocId::Guest(entry.vmid) == doc)
                    }),
                false => None,
            };

            // The whole document, for the top-level keys the "View as" selector offers.
            let keys = crate::model::top_level_keys(&api::get_data(doc).await?.data);
            let document = api::get_text(doc, &view).await?;

            link.send_message(Msg::Loaded(Box::new(Loaded {
                access,
                guest,
                keys,
                digest: document.digest,
                text: document.text,
            })));

            Ok(())
        })
    }

    fn update(&mut self, ctx: &LoadableComponentContext<Self>, msg: Self::Message) -> bool {
        match msg {
            Msg::Loaded(loaded) => {
                let Loaded {
                    access,
                    guest,
                    keys,
                    digest,
                    text,
                } = *loaded;

                if let Some(access) = access {
                    self.access = access;
                    self.access_loaded = true;
                }
                if guest.is_some() {
                    self.guest = guest;
                }

                let options = self.access.view_options(&keys);
                // Fall back to the whole document when the selected prefix is gone.
                if !options.contains(&self.view) {
                    self.view = String::new();
                }
                self.views = Rc::new(
                    options
                        .into_iter()
                        .map(|option| match option.is_empty() {
                            true => AttrValue::from(WHOLE_DOCUMENT),
                            false => AttrValue::from(option),
                        })
                        .collect(),
                );

                self.loaded = text;
                self.digest = digest;
                self.draft = None;
                self.stale = false;
                self.write_error = None;
                self.generation += 1;
            }
            Msg::SelectView(view) => {
                let view = match view.as_str() {
                    WHOLE_DOCUMENT => String::new(),
                    other => other.to_string(),
                };
                if view == self.view {
                    return false;
                }
                self.view = view;
                self.draft = None;
                self.write_error = None;
                ctx.link().send_reload();
            }
            Msg::EditorInput(text) => {
                self.draft = (text != self.loaded).then_some(text);
            }
            Msg::ShowDiff => {
                self.dialog_open = true;
                ctx.link().change_view(Some(ViewState::ConfirmApply));
            }
            Msg::CloseDialog => {
                self.dialog_open = false;
                self.dispose_diff();
                ctx.link().change_view(None);
            }
            Msg::Apply => {
                self.dialog_open = false;
                self.dispose_diff();
                ctx.link().change_view(None);

                let doc = ctx.props().doc;
                let view = self.view.clone();
                let text = self.current_text().to_string();
                let digest = self.digest.clone();
                let link = ctx.link().clone();
                ctx.link().spawn(async move {
                    let result = api::put_text(doc, &view, text, &digest).await;
                    link.send_message(Msg::Applied(result));
                });
            }
            Msg::Applied(Ok(_)) => {
                self.write_error = None;
                ctx.link().send_reload();
            }
            Msg::Applied(Err(err)) => {
                // A 409 is not an error the user can act on by reading it — it means the
                // document moved underneath, so show the notice that offers a reload.
                self.stale = api::is_conflict(&err);
                // Everything else is the server's own message, shown verbatim.
                self.write_error = Some(err.to_string());
            }
            Msg::Discard => {
                self.draft = None;
                self.write_error = None;
                // Force `rendered()` to put the loaded text back into the editor.
                self.generation += 1;
            }
            Msg::VersionToken(token) => {
                if self.version_token.as_deref() == Some(token.as_str()) {
                    return false;
                }
                self.version_token = Some(token);
                // The store token covers every document, so it only says "something,
                // somewhere, moved". Ask this one whether it was this document.
                let doc = ctx.props().doc;
                let view = self.view.clone();
                let link = ctx.link().clone();
                ctx.link().spawn(async move {
                    match api::get_text(doc, &view).await {
                        Ok(document) => link.send_message(Msg::ServerDigest(document.digest)),
                        Err(err) => log::warn!("pve-meta-ui: digest check failed: {err}"),
                    }
                });
                return false;
            }
            Msg::ServerDigest(digest) => {
                if digest == self.digest {
                    return false;
                }
                // Never throw away unapplied edits: say so and offer the reload instead.
                if self.draft.is_some() {
                    self.stale = true;
                } else {
                    ctx.link().send_reload();
                    return false;
                }
            }
            Msg::ThemeChanged(dark) => {
                self.dark_mode = dark;
                monaco::set_theme(dark);
                return false;
            }
        }
        true
    }

    fn toolbar(&self, ctx: &LoadableComponentContext<Self>) -> Option<Html> {
        let link = ctx.link();
        let dirty = self.draft.is_some();
        let writable = self.writable(ctx);

        let selected = match self.view.is_empty() {
            true => AttrValue::from(WHOLE_DOCUMENT),
            false => AttrValue::from(self.view.clone()),
        };

        let view_as = Combobox::new()
            .aria_label(tr!("View as"))
            // Not clearable: an empty value has no meaning here, and `required` is what
            // suppresses the field's clear trigger (`PWT/src/widget/form/selector.rs`).
            .required(true)
            .items(self.views.clone())
            .value(selected)
            .render_value(|value: &AttrValue| match value.as_str() {
                WHOLE_DOCUMENT => html! {{ tr!("Whole document") }},
                other => html! {{ other }},
            })
            .on_change(link.callback(Msg::SelectView));

        Some(
            Toolbar::new()
                .class("pwt-w-100")
                .class("pwt-overflow-hidden")
                .class("pwt-border-bottom")
                .with_child(Container::from_tag("span").with_child(tr!("View as") + ":"))
                .with_child(view_as)
                .with_spacer()
                .with_child(
                    Button::new(tr!("Apply"))
                        .disabled(!dirty || !writable)
                        .on_activate(link.callback(|_| Msg::ShowDiff)),
                )
                .with_child(
                    ConfirmButton::new(tr!("Discard"))
                        .dangerous(true)
                        .disabled(!dirty)
                        .confirm_message(tr!("Discard all unapplied changes?"))
                        .on_activate(link.callback(|_| Msg::Discard)),
                )
                .with_flex_spacer()
                .with_optional_child((!writable).then(|| {
                    Container::from_tag("span")
                        .class("pwt-color-on-neutral-alt")
                        .with_child(tr!("Read-only"))
                }))
                .with_child(Button::refresh(self.loading()).on_activate({
                    let link = link.clone();
                    move |_| link.send_reload()
                }))
                .into(),
        )
    }

    fn main_view(&self, ctx: &LoadableComponentContext<Self>) -> Html {
        Column::new()
            .class(FlexFit)
            .with_child(self.header(ctx).key("header"))
            .with_optional_child(
                self.stale
                    .then(|| self.stale_banner(ctx).key("stale-banner")),
            )
            .with_child(
                Container::new()
                    .key("editor")
                    .class(FlexFit)
                    .class("pve-meta-monaco-host")
                    .into_html_with_ref(self.editor_ref.clone()),
            )
            .with_optional_child(self.write_error.as_deref().map(|err| {
                error_message(err)
                    .key("write-error")
                    .class("pwt-border-top")
            }))
            .into()
    }

    fn dialog_view(
        &self,
        ctx: &LoadableComponentContext<Self>,
        view_state: &Self::ViewState,
    ) -> Option<Html> {
        match view_state {
            ViewState::ConfirmApply => Some(self.diff_dialog(ctx)),
        }
    }

    fn rendered(&mut self, ctx: &LoadableComponentContext<Self>, _first_render: bool) {
        let read_only = !self.writable(ctx);

        match &self.editor_id {
            None => {
                if let Some(el) = self.editor_ref.cast::<Element>() {
                    let id = monaco::mount(
                        &el,
                        &monaco::MountOptions {
                            value: &self.loaded,
                            language: "yaml",
                            read_only,
                            theme: if self.dark_mode { "dark" } else { "light" },
                        },
                    );
                    let link = ctx.link().clone();
                    self.on_change = Some(monaco::on_change(&id, move |text| {
                        link.send_message(Msg::EditorInput(text))
                    }));
                    self.mounted_generation = Some(self.generation);
                    self.editor_id = Some(id);
                }
            }
            Some(id) => {
                // Never write over the buffer the user is typing in: only a load, a
                // discard or a view switch bumps the generation.
                if self.mounted_generation != Some(self.generation) {
                    monaco::set_value(id, &self.loaded);
                    self.mounted_generation = Some(self.generation);
                }
                monaco::set_read_only(id, read_only);
            }
        }

        if self.dialog_open && self.diff_id.is_none() {
            if let Some(el) = self.diff_ref.cast::<Element>() {
                self.diff_id = Some(monaco::mount_diff(&el, &self.loaded, self.current_text()));
            }
        }
    }
}

impl From<MetaEditor> for VNode {
    fn from(val: MetaEditor) -> Self {
        let comp = VComp::new::<LoadableComponentMaster<PveMetaEditor>>(Rc::new(val), None);
        VNode::from(comp)
    }
}
