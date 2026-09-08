//! The metadata editor page.
//!
//! One `LoadableComponent` for one document (`docs/DESIGN.md` §6), structured exactly
//! like `proxmox_yew_comp::NotesView`: `load()` fetches a text document and its digest,
//! `toolbar()` returns the standard three-class `Toolbar`, `main_view()` fills the
//! remaining height, and `dialog_view()` returns the modals. Everything else — the
//! outer column, the load-error strip, dialog stacking, off-screen refresh suspension —
//! comes from `LoadableComponentMaster`.
//!
//! The one deviation from the pwt stack is the editor itself: Monaco, mounted into a
//! plain `Container` from `rendered()` through the glue in `js/pve-meta-monaco.js`
//! (`crate::monaco`).
//!
//! Two disciplines run through this file, both from `docs/REVIEW-2026-09-07.md`:
//!
//! * **Every async result carries the identity it was requested for** (`crate::request`).
//!   `LoadableComponentMaster` respawns a load per `Msg::Load` and cancels nothing, so an
//!   answer for the view the user just left must be dropped, not applied — otherwise the
//!   page shows (and Apply then writes) one view's text under another view's path (F4).
//! * **Unapplied edits are never discarded without asking.** Reload and a view switch go
//!   through the same confirmation the Discard button uses (F5).

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
use pwt::widget::{
    Button, Column, ConfirmDialog, Container, Dialog, Fa, Row, Toolbar, error_message,
};
use pwt_macros::builder;

use proxmox_yew_comp::{
    ConfirmButton, LoadableComponent, LoadableComponentContext, LoadableComponentMaster,
    LoadableComponentScopeExt, LoadableComponentState,
};

use crate::api::{self, WriteResult};
use crate::model::{Access, DocId, ViewOutcome, top_level_keys_from_yaml, view_outcome};
use crate::monaco;
use crate::request::{Channel, RequestId, RequestTracker};

/// `GET /meta/version` is polled this often. Its token covers the whole store, so a
/// change only triggers a digest check of this one document.
const VERSION_POLL_MS: u32 = 5_000;

/// The "View as" entry standing for the whole document (the empty prefix). A sentinel is
/// needed because a `Combobox` cannot hold an empty value — the same trick PDM's
/// `ViewSelector` uses for its `__dashboard__` entry.
const WHOLE_DOCUMENT: &str = "__document__";

/// Everything one load produces, plus the identity it was loaded for.
pub struct Loaded {
    /// Which document, which view, which generation this answers.
    id: RequestId,
    /// Only when the grants for this document have not been fetched yet.
    access: Option<Access>,
    /// Set instead of `access` when the grants could not be fetched at all.
    access_error: Option<String>,
    /// Top-level keys of the whole document, for the "View as" selector; present when
    /// this load's view was empty (a whole-document read, `docs/REVIEW-2026-09-08-
    /// pass2.md` P10 — a view switch otherwise never re-fetches it) or when a selected
    /// view's own answer was ambiguous and `load()` fetched the whole document to
    /// settle it (`docs/REVIEW-2026-09-08-pass3.md` R3).
    keys: Option<Vec<String>>,
    digest: String,
    text: String,
}

/// Modal states of the page.
#[derive(PartialEq)]
pub enum ViewState {
    /// The Apply confirmation: a diff of the loaded text against the edited one.
    ConfirmApply,
    /// "Discard all unapplied changes?" before leaving the edited view.
    ConfirmSwitchView,
}

pub enum Msg {
    Loaded(Box<Loaded>),
    SelectView(String),
    ConfirmSwitchView,
    EditorInput(String),
    ShowDiff,
    CloseDialog,
    Apply,
    Applied(RequestId, Result<WriteResult, Error>),
    Discard,
    Reload,
    VersionToken(String),
    ServerDigest(RequestId, api::DocText),
    /// `api::access(doc)` answered a re-fetch issued outside `load()`: `Msg::Reload`, or
    /// a version-poll tick whose token changed (`docs/REVIEW-2026-09-08-pass4.md` Q2).
    AccessRefreshed(RequestId, Result<Access, String>),
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

/// A live Monaco instance, and the identity of the buffer it is showing.
struct MountedEditor {
    id: String,
    doc: DocId,
    view: String,
    /// The load/discard generation whose text is in the buffer.
    generation: u64,
}

#[doc(hidden)]
pub struct PveMetaEditor {
    state: LoadableComponentState<ViewState>,
    /// What the page is asking for, and what an answer must match to be applied.
    requests: RequestTracker,
    /// The caller's effective grants for this document.
    access: Access,
    /// The document the grants were fetched for; `None` while they are unknown.
    access_for: Option<DocId>,
    /// Why the grants are unknown, if the endpoint refused to answer.
    access_error: Option<String>,
    /// Top-level keys of the whole document, as parsed from its YAML text.
    keys: Vec<String>,
    /// The prefixes the "View as" selector offers.
    views: Rc<Vec<AttrValue>>,
    /// Bumped when a view selection is cancelled, to make the `Combobox` re-read the
    /// value prop it is not otherwise controlled by (`PWT/src/widget/form/selector.rs`
    /// only force-feeds the value when the prop itself changed).
    view_revision: u64,
    /// The view a pending confirmation would switch to.
    pending_view: Option<String>,
    /// Text and digest as loaded.
    loaded: String,
    digest: String,
    /// Live editor buffer; `None` while unmodified.
    draft: Option<String>,
    /// Bumped by every load, so `rendered()` knows when to push new text into Monaco.
    generation: u64,
    /// The last message of a failed write, shown verbatim under the editor.
    write_error: Option<String>,
    /// A poll found this document's digest changed while there were unapplied edits,
    /// or a write was refused with a 409.
    stale: bool,
    version_token: Option<String>,
    editor_ref: NodeRef,
    editor: Option<MountedEditor>,
    diff_ref: NodeRef,
    diff_id: Option<String>,
    /// The identity the open diff was built for.
    diff_request: Option<RequestId>,
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
    fn writable(&self, _ctx: &LoadableComponentContext<Self>) -> bool {
        self.access.may_write(&self.requests.view())
    }

    /// The text the editor currently shows.
    fn current_text(&self) -> &str {
        self.draft.as_deref().unwrap_or(&self.loaded)
    }

    /// True while the editor holds changes that are not on the server.
    fn dirty(&self) -> bool {
        self.draft.is_some()
    }

    /// Recompute the "View as" selector's items from the current grants and keys.
    ///
    /// Called wherever either input changes: a load's answer (`Msg::Loaded`) and the
    /// version poll's whole-document digest check (`Msg::ServerDigest`) both refresh
    /// `self.keys`, and the dropdown should never lag one load behind what those two
    /// already know (`docs/REVIEW-2026-09-08-pass3.md` R3). `Msg::AccessRefreshed`
    /// refreshes `self.access` the same way, on Reload and on a token-changed poll tick
    /// (Q2).
    fn refresh_views(&mut self) {
        self.views = Rc::new(
            self.access
                .view_options(&self.keys)
                .iter()
                .map(|option| match option.is_empty() {
                    true => AttrValue::from(WHOLE_DOCUMENT),
                    false => AttrValue::from(option.clone()),
                })
                .collect(),
        );
    }

    /// Re-fetch this document's grants outside `load()`.
    ///
    /// `load()` only fetches `api::access` once per document (`need_access`, editor.rs
    /// `load()`), because the same grants cover every view of it. But they live in the
    /// *datacenter* document's `scopes` map, so nothing about a guest document's own
    /// digest or content ever signals that an admin changed them — a load, however
    /// often it reruns, never notices a revoked scope. Called from `Msg::Reload` (an
    /// explicit request to catch up) and from the version-poll tick whenever the store
    /// token changed (the closest thing to a signal the poll has,
    /// `docs/REVIEW-2026-09-08-pass4.md` Q2 — R3's own fix text asked for this and it was
    /// never shipped). The answer feeds `refresh_views()`/`view_outcome()` exactly like
    /// fresh `keys` already do.
    fn refresh_access(&self, ctx: &LoadableComponentContext<Self>) {
        let id = self.requests.issue(Channel::Access);
        let doc = id.doc;
        let link = ctx.link().clone();
        ctx.link().spawn(async move {
            let result = api::access(doc).await.map_err(|e| e.to_string());
            link.send_message(Msg::AccessRefreshed(id, result));
        });
    }

    /// Show `view` from now on: strand everything in flight and load the new one.
    ///
    /// The buffer keeps showing the outgoing view's text until `Msg::Loaded` overwrites
    /// it — `dirty()` is false throughout (the draft was just dropped), so nothing reads
    /// it as belonging to the new view. Blanking it here just to have it refill a moment
    /// later made every switch flash empty for the round trip
    /// (`docs/REVIEW-2026-09-08-pass2.md` low findings, `editor.rs:205`).
    fn switch_view(&mut self, ctx: &LoadableComponentContext<Self>, view: String) {
        if !self.requests.set_view(view) {
            return;
        }
        self.draft = None;
        self.stale = false;
        self.write_error = None;
        self.generation += 1;
        self.close_dialog(ctx);
        ctx.link().send_reload();
    }

    /// Close whichever modal is open, dropping the diff editor with it.
    fn close_dialog(&mut self, ctx: &LoadableComponentContext<Self>) {
        if self.pending_view.take().is_some() {
            // The selector already shows the view the user picked; make it re-read the
            // one that is actually loaded.
            self.view_revision += 1;
        }
        self.dialog_open = false;
        self.dispose_diff();
        ctx.link().change_view(None);
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
                // The guest's name would need either `GET /meta/guests` (an O(N) scan of
                // every guest in the vmlist just to find this one's name, `docs/REVIEW-
                // 2026-09-08-pass2.md` P10) or a name/node field the single-document GET
                // does not carry (`docs/DESIGN.md` §3); the vmid on its own is enough to
                // identify the document, and `props.node` already covers node.
                (icon, vmid.to_string())
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

    /// The Reload button. With unapplied edits it asks first — a reload throws them away
    /// just as the Discard button next to it does, so it uses the same confirmation.
    ///
    /// Both branches dispatch `Msg::Reload` rather than the framework's own
    /// `LoadableComponentScope::send_reload()` directly: that helper only sends the
    /// master's `Msg::Load`, bypassing `PveMetaEditor::update`'s `Msg::Reload` arm
    /// entirely — which is where `self.requests.invalidate()` (P8) and the grants
    /// re-fetch (`refresh_access`, Q2) live. A clean Reload (no draft to confirm) is by
    /// far the common case, so routing it around `Msg::Reload` would have left both of
    /// those unexercised for everyday use.
    fn reload_button(&self, ctx: &LoadableComponentContext<Self>) -> Html {
        let link = ctx.link().clone();
        let loading = self.loading();

        if !self.dirty() {
            return Button::refresh(loading)
                .on_activate(link.callback(|_| Msg::Reload))
                .into();
        }

        let icon_class = match loading {
            true => "fa fa-fw fa-refresh fa-spin",
            false => "fa fa-fw fa-refresh",
        };

        ConfirmButton::new_icon(icon_class)
            .aria_label(tr!("Refresh"))
            .dangerous(true)
            .disabled(loading)
            .confirm_message(tr!("Discard all unapplied changes?"))
            .on_activate(link.callback(|_| Msg::Reload))
            .into()
    }

    /// The non-modal "changed on the server" notice.
    fn stale_banner(&self, ctx: &LoadableComponentContext<Self>) -> Column {
        Column::new()
            .padding(2)
            .gap(1)
            .class(ColorScheme::WarningContainer)
            .class("pwt-default-colors")
            .class("pwt-border-bottom")
            .with_child(
                Row::new()
                    .gap(2)
                    .class(AlignItems::Center)
                    .with_child(Fa::new("exclamation-triangle"))
                    .with_child(tr!(
                        "This document was changed on the server since it was loaded."
                    ))
                    .with_flex_spacer()
                    .with_child(self.reload_button(ctx)),
            )
            .with_optional_child(self.dirty().then(|| {
                Container::from_tag("span").with_child(tr!(
                    "Your unapplied changes are kept until you reload or discard them."
                ))
            }))
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

    /// The same confirmation the Discard button raises, before a view switch drops the
    /// edits made in the view being left.
    fn switch_view_dialog(&self, ctx: &LoadableComponentContext<Self>) -> Html {
        let link = ctx.link();
        ConfirmDialog::new(tr!("Confirm"), tr!("Discard all unapplied changes?"))
            .dangerous(true)
            .on_confirm(link.callback(|_| Msg::ConfirmSwitchView))
            .on_close(link.callback(|_| Msg::CloseDialog))
            .into()
    }

    fn dispose_diff(&mut self) {
        self.diff_request = None;
        if let Some(id) = self.diff_id.take() {
            monaco::dispose(&id);
        }
    }

    /// Drop the mounted editor, its model and its change listener.
    fn dispose_editor(&mut self) {
        if let Some(editor) = self.editor.take() {
            monaco::dispose(&editor.id);
        }
        self.on_change = None;
    }
}

impl Drop for PveMetaEditor {
    fn drop(&mut self) {
        // Monaco leaks a ResizeObserver and a model otherwise.
        self.dispose_editor();
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
        // `GET /meta/version` immediately, there is no long poll. The future runs in the
        // component's `AsyncPool`, which aborts it when the component goes away.
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
            requests: RequestTracker::new(ctx.props().doc),
            access: Access::default(),
            access_for: None,
            access_error: None,
            keys: Vec::new(),
            views: Rc::new(vec![AttrValue::from(WHOLE_DOCUMENT)]),
            view_revision: 0,
            pending_view: None,
            loaded: String::new(),
            digest: String::new(),
            draft: None,
            generation: 0,
            write_error: None,
            stale: false,
            version_token: None,
            editor_ref: NodeRef::default(),
            editor: None,
            diff_ref: NodeRef::default(),
            diff_id: None,
            diff_request: None,
            dialog_open: false,
            on_change: None,
            _theme_observer: theme_observer,
            dark_mode,
        }
    }

    fn changed(&mut self, ctx: &LoadableComponentContext<Self>, _old: &Self::Properties) -> bool {
        // A different document is a different page: nothing loaded for the old one — its
        // text, its digest, its grants, its keys — may survive into it. `app.rs` mounts
        // one `MetaEditor` per navigation and never changes its `doc` prop in place, so
        // this never fires with unapplied edits in the buffer; guard that assumption
        // instead of silently discarding them the way pre-F5 code did
        // (`docs/REVIEW-2026-09-08-pass2.md` low findings, `editor.rs:451`).
        if self.requests.set_doc(ctx.props().doc) {
            debug_assert!(
                !self.dirty(),
                "pve-meta-ui: doc prop changed with unapplied edits in the buffer"
            );
            self.access = Access::default();
            self.access_for = None;
            self.access_error = None;
            self.keys.clear();
            self.draft = None;
            self.loaded.clear();
            self.digest.clear();
            self.stale = false;
            self.write_error = None;
            self.generation += 1;
            self.close_dialog(ctx);
            ctx.link().send_reload();
        }
        true
    }

    fn load(
        &self,
        ctx: &LoadableComponentContext<Self>,
    ) -> Pin<Box<dyn Future<Output = Result<(), Error>>>> {
        let tracker = self.requests.clone();
        let id = tracker.issue(Channel::Load);
        let doc = id.doc;
        let view = id.view.clone();
        let need_access = self.access_for != Some(doc);
        let link = ctx.link().clone();

        Box::pin(async move {
            let (access, access_error) = match need_access {
                true => match api::access(doc).await {
                    Ok(access) => (Some(access), None),
                    // A page that cannot ask for its grants stays usable and read-only
                    // rather than failing outright: `/meta/access` parses the whole
                    // `scopes` map, so one malformed entry can 400 it for everyone
                    // (`docs/REVIEW-2026-09-07.md` F7) — and this document may be exactly
                    // the one that has to be edited to fix that.
                    Err(err) => (None, Some(err.to_string())),
                },
                false => (None, None),
            };

            // One GET, whatever the view: `docs/REVIEW-2026-09-08-pass2.md` P10 removed
            // both the `GET /meta/guests` scan this load used to make solely to find the
            // guest's name (an O(N) round trip over every guest in the vmlist for one
            // field the header no longer shows) and the second `view=""` read this load
            // used to make solely for the "View as" selector's top-level keys. A
            // selected view's own answer only carries the keys *inside* that view
            // (`docs/DESIGN.md` §3), so those keys come only from a whole-document read
            // — the first load of a document is always one (`RequestTracker::new` starts
            // at the empty view), and `self.keys` then survives every later view switch.
            let document = api::get_text(doc, &view).await;

            // Nothing past this point may touch the page unless it still wants this
            // answer: the user may have switched view or document while this was in
            // flight, and `LoadableComponentMaster` cancels nothing (F4).
            if !tracker.accepts(&id) {
                log::debug!(
                    "pve-meta-ui: dropping a stale load of {}, view '{}'",
                    doc.label(),
                    view,
                );
                return Ok(());
            }

            let document = document?;

            // A selected (non-empty) view whose own answer carries no top-level keys is
            // ambiguous: `view::extract` (`crates/pve-meta-core/src/api.rs`) hands back
            // the same empty object whether the key still exists and simply has no
            // content, or has been deleted since the view was last listed
            // (`docs/REVIEW-2026-09-08-pass3.md` R3 — the invariant P10's fold-in-one-GET
            // change broke). Settle it with one extra whole-document read, paid only in
            // this rare case — an ordinary load, whatever it answers, never pays it — so
            // the "is the selected view still an option" check below sees the document's
            // current top-level keys rather than whatever `self.keys` last held.
            let keys = if view.is_empty() {
                Some(document_keys(&document))
            } else if document_keys(&document).is_empty() {
                match api::get_text(doc, "").await {
                    Ok(whole) => {
                        if !tracker.accepts(&id) {
                            log::debug!(
                                "pve-meta-ui: dropping a stale load of {}, view '{}'",
                                doc.label(),
                                view,
                            );
                            return Ok(());
                        }
                        Some(document_keys(&whole))
                    }
                    Err(err) => {
                        log::warn!(
                            "pve-meta-ui: failed to validate an empty answer for {} view \
                             '{view}': {err}",
                            doc.label(),
                        );
                        None
                    }
                }
            } else {
                None
            };

            link.send_message(Msg::Loaded(Box::new(Loaded {
                id,
                access,
                access_error,
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
                    id,
                    access,
                    access_error,
                    keys,
                    digest,
                    text,
                } = *loaded;

                // The future checked this before sending; check it again here, where the
                // state it would overwrite actually lives.
                if !self.requests.accepts(&id) {
                    return false;
                }

                if let Some(access) = access {
                    self.access = access;
                    self.access_for = Some(id.doc);
                    self.access_error = None;
                }
                if let Some(err) = access_error {
                    // No grants until the endpoint answers: the page stays read-only and
                    // says why. `access_for` stays unset, so a Reload retries.
                    self.access = Access::default();
                    self.access_for = None;
                    self.access_error = Some(err);
                }
                if let Some(keys) = keys {
                    self.keys = keys;
                }
                self.refresh_views();

                // The selected prefix is gone (deleted, or no longer granted). `keys`
                // above is fresh whenever it mattered — `load()` fetches a whole-document
                // read to settle exactly this whenever the answered view itself carried
                // no top-level keys (`docs/REVIEW-2026-09-08-pass3.md` R3) — so this
                // reads the document's *current* state, not whatever `self.keys` last
                // held before P10 folded the key list into the single document GET.
                match view_outcome(&self.access, &self.keys, &id.view, self.dirty()) {
                    ViewOutcome::Keep => {}
                    ViewOutcome::FallBack => {
                        // Fall back to the whole document — and load *it*: the text in
                        // hand is the answer for a view that no longer exists, not for
                        // the one now selected.
                        self.switch_view(ctx, String::new());
                        return true;
                    }
                    ViewOutcome::ConfirmFallBack => {
                        // There is a draft against the vanished view — the same question
                        // a manual switch would ask (F5), not a silent discard.
                        self.pending_view = Some(String::new());
                        ctx.link().change_view(Some(ViewState::ConfirmSwitchView));
                        return true;
                    }
                }

                self.loaded = text;
                self.digest = digest;
                self.draft = None;
                self.stale = false;
                self.write_error = None;
                self.generation += 1;
                // Whatever the diff dialog was showing was a picture of the text that
                // just moved.
                self.close_dialog(ctx);
            }
            Msg::SelectView(view) => {
                let view = match view.as_str() {
                    WHOLE_DOCUMENT => String::new(),
                    other => other.to_string(),
                };
                if view == self.requests.view() {
                    return false;
                }
                // Leaving an edited view throws the edits away, so ask first — the same
                // question, in the same dialog, as the Discard button (F5).
                if self.dirty() {
                    self.pending_view = Some(view);
                    ctx.link().change_view(Some(ViewState::ConfirmSwitchView));
                    return true;
                }
                self.switch_view(ctx, view);
            }
            Msg::ConfirmSwitchView => {
                let Some(view) = self.pending_view.take() else {
                    return false;
                };
                ctx.link().change_view(None);
                self.switch_view(ctx, view);
            }
            Msg::EditorInput(text) => {
                self.draft = (text != self.loaded).then_some(text);
            }
            Msg::ShowDiff => {
                self.dialog_open = true;
                self.diff_request = Some(self.requests.issue(Channel::Diff));
                ctx.link().change_view(Some(ViewState::ConfirmApply));
            }
            Msg::CloseDialog => {
                self.close_dialog(ctx);
            }
            Msg::Apply => {
                self.close_dialog(ctx);

                let id = self.requests.issue(Channel::Apply);
                let doc = id.doc;
                let view = id.view.clone();
                let text = self.current_text().to_string();
                let digest = self.digest.clone();
                let link = ctx.link().clone();
                ctx.link().spawn(async move {
                    let result = api::put_text(doc, &view, text, &digest).await;
                    link.send_message(Msg::Applied(id, result));
                });
            }
            Msg::Applied(id, result) => {
                // The write went out for one view of one document. If the page has moved
                // on, its outcome says nothing about what is on screen now — the version
                // poll will pick the change up.
                if !self.requests.accepts(&id) {
                    log::warn!(
                        "pve-meta-ui: dropping the result of a write to {}, view '{}': \
                         the page moved on",
                        id.doc.label(),
                        id.view,
                    );
                    return false;
                }
                match result {
                    Ok(_) => {
                        self.write_error = None;
                        ctx.link().send_reload();
                    }
                    Err(err) => {
                        // A 409 is not an error the user can act on by reading it — it
                        // means the document moved underneath, so show the notice that
                        // offers a reload.
                        self.stale = api::is_conflict(&err);
                        // Everything else is the server's own message, shown verbatim.
                        self.write_error = Some(err.to_string());
                    }
                }
            }
            Msg::Discard => {
                self.draft = None;
                self.write_error = None;
                // Force `rendered()` to put the loaded text back into the editor.
                self.generation += 1;
            }
            Msg::Reload => {
                // Confirmed by the caller (the toolbar's and the banner's Reload both ask
                // when there is something to lose). Invalidate before reloading: an
                // Apply/Diff/Digest request issued earlier must not have its late answer
                // applied over the document this reload is about to fetch
                // (`docs/REVIEW-2026-09-08-pass2.md` P8 — this channel was the one
                // `RequestTracker::invalidate()` existed for but never got called).
                self.requests.invalidate();
                self.draft = None;
                self.write_error = None;
                self.generation += 1;
                // The datacenter document's scopes are invisible to this document's own
                // digest, so an explicit reload is also the only other place (besides a
                // token-changed poll tick) that ever catches up on a revoked or widened
                // scope (Q2).
                self.refresh_access(ctx);
                ctx.link().send_reload();
            }
            Msg::VersionToken(token) => {
                if self.version_token.as_deref() == Some(token.as_str()) {
                    return false;
                }
                self.version_token = Some(token);
                // The datacenter document's scopes live outside this one, so a change to
                // them never moves this document's own digest — the store token changing
                // at all is the only signal available, so re-fetch grants on every tick
                // that reaches here rather than only when this document also moved (Q2).
                self.refresh_access(ctx);
                // The store token covers every document, so it only says "something,
                // somewhere, moved". Ask this one whether it was this document — always
                // for the *whole* document, never just the selected view: `get_document`
                // (`crates/pve-meta-core/src/api.rs`) reads the whole document and takes
                // its digest before filtering, so it is the same one request either way,
                // and it doubles as a refresh of the "View as" key list on every poll
                // tick, instead of only on the next whole-document load
                // (`docs/REVIEW-2026-09-08-pass3.md` R3).
                let id = self.requests.issue(Channel::Digest);
                let doc = id.doc;
                let link = ctx.link().clone();
                ctx.link().spawn(async move {
                    match api::get_text(doc, "").await {
                        Ok(document) => link.send_message(Msg::ServerDigest(id, document)),
                        Err(err) => log::warn!("pve-meta-ui: digest check failed: {err}"),
                    }
                });
                return false;
            }
            Msg::ServerDigest(id, document) => {
                // A digest read for a view the page has left says nothing about the one
                // it now shows.
                if !self.requests.accepts(&id) {
                    return false;
                }
                // Always the whole document (see above): fresh top-level keys,
                // regardless of whether the digest moved — keep `self.keys` (and the
                // "View as" items built from it) from going stale between whole-document
                // loads (R3).
                self.keys = document_keys(&document);
                self.refresh_views();

                // React to the selected view disappearing the moment fresh keys say so,
                // rather than waiting for a follow-up load to rediscover it: cheaper (no
                // detour through an ambiguous, now-stale answer for a view that is
                // already known to be gone) and exactly the same decision `Msg::Loaded`
                // makes for the same reason.
                match view_outcome(&self.access, &self.keys, &id.view, self.dirty()) {
                    ViewOutcome::Keep => {}
                    ViewOutcome::FallBack => {
                        self.switch_view(ctx, String::new());
                        return true;
                    }
                    ViewOutcome::ConfirmFallBack => {
                        self.pending_view = Some(String::new());
                        ctx.link().change_view(Some(ViewState::ConfirmSwitchView));
                        return true;
                    }
                }

                if document.digest == self.digest {
                    return true;
                }
                // Never throw away unapplied edits: say so and offer the reload instead.
                if self.dirty() {
                    self.stale = true;
                } else {
                    ctx.link().send_reload();
                    return false;
                }
            }
            Msg::AccessRefreshed(id, result) => {
                // An answer for a document/view the page has since left says nothing
                // about the one it now shows — same discipline as every other channel.
                if !self.requests.accepts(&id) {
                    return false;
                }
                match result {
                    Ok(access) => {
                        self.access = access;
                        self.access_for = Some(id.doc);
                        self.access_error = None;
                    }
                    Err(err) => {
                        // No grants until the endpoint answers again: the page stays
                        // read-only and says why, exactly like a failed `load()` fetch.
                        self.access = Access::default();
                        self.access_for = None;
                        self.access_error = Some(err);
                    }
                }
                self.refresh_views();

                // The freshly re-fetched grants may no longer cover the selected view
                // (a scope was narrowed or revoked) — the same fallback `Msg::Loaded`
                // and `Msg::ServerDigest` apply when fresh `keys` say a view is gone
                // (`docs/REVIEW-2026-09-08-pass4.md` Q2).
                match view_outcome(&self.access, &self.keys, &id.view, self.dirty()) {
                    ViewOutcome::Keep => {}
                    ViewOutcome::FallBack => {
                        self.switch_view(ctx, String::new());
                        return true;
                    }
                    ViewOutcome::ConfirmFallBack => {
                        self.pending_view = Some(String::new());
                        ctx.link().change_view(Some(ViewState::ConfirmSwitchView));
                        return true;
                    }
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
        let dirty = self.dirty();
        let writable = self.writable(ctx);
        let view = self.requests.view();

        let selected = match view.is_empty() {
            true => AttrValue::from(WHOLE_DOCUMENT),
            false => AttrValue::from(view.clone()),
        };

        let view_as = Combobox::new()
            // A cancelled switch maps the selection back to the value the field already
            // had, which a `Selector` does not re-assert on its own; a changed key makes
            // it start over from the `value` prop.
            .key(format!("view-as:{}:{}", self.view_revision, view))
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
                .with_child(self.reload_button(ctx))
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
            .with_optional_child(self.access_error.as_deref().map(|err| {
                error_message(&tr!(
                    "Could not determine your access to this document; it is shown \
                     read-only. {0}",
                    err
                ))
                .key("access-error")
                .class("pwt-border-top")
            }))
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
            ViewState::ConfirmSwitchView => Some(self.switch_view_dialog(ctx)),
        }
    }

    fn rendered(&mut self, ctx: &LoadableComponentContext<Self>, _first_render: bool) {
        let doc = ctx.props().doc;
        let view = self.requests.view();
        let read_only = !self.writable(ctx);
        let generation = self.generation;

        // An editor belongs to the buffer it was mounted for: its model, its undo stack
        // and its change listener are all that view's. Never carry one across.
        let stale_editor = self
            .editor
            .as_ref()
            .is_some_and(|editor| editor.doc != doc || editor.view != view);
        if stale_editor {
            self.dispose_editor();
        }

        match &mut self.editor {
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
                    self.editor = Some(MountedEditor {
                        id,
                        doc,
                        view,
                        generation,
                    });
                }
            }
            Some(editor) => {
                // Never write over the buffer the user is typing in: only a load, a
                // discard or a view switch bumps the generation.
                if editor.generation != generation {
                    monaco::set_value(&editor.id, &self.loaded);
                    editor.generation = generation;
                }
                monaco::set_read_only(&editor.id, read_only);
            }
        }

        if self.dialog_open && self.diff_id.is_none() {
            // The diff pictures one document, one view, one loaded text. If any of those
            // moved since the dialog opened, it is on its way out — do not fill it.
            let current = self
                .diff_request
                .as_ref()
                .is_some_and(|id| self.requests.accepts(id));
            if current {
                if let Some(el) = self.diff_ref.cast::<Element>() {
                    self.diff_id = Some(monaco::mount_diff(&el, &self.loaded, self.current_text()));
                }
            }
        }
    }
}

/// The top-level keys of a whole-document read, in document order.
///
/// The server sends them as an ordered `keys` array; the YAML text is the fallback for a
/// server that does not (the same text, parsed — never `format=json`, whose object arrives
/// reordered, `docs/DESIGN.md` §8).
fn document_keys(document: &api::DocText) -> Vec<String> {
    match document.keys.is_empty() {
        true => top_level_keys_from_yaml(&document.text),
        false => document.keys.clone(),
    }
}

impl From<MetaEditor> for VNode {
    fn from(val: MetaEditor) -> Self {
        let comp = VComp::new::<LoadableComponentMaster<PveMetaEditor>>(Rc::new(val), None);
        VNode::from(comp)
    }
}
