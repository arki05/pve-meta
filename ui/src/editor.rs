//! The metadata page: one tree of one document (`docs/DESIGN.md` §8).
//!
//! One `LoadableComponent` (`docs/design/PDM-DESIGN-LANGUAGE.md` §2), structured exactly
//! like `proxmox_yew_comp::PermissionPanel` — the stack's own `DataTable`-over-`TreeStore`
//! page: `load()` fetches, `main_view()` is the table, `toolbar()` is the standard
//! three-class `Toolbar`, `dialog_view()` returns the modals. Everything else — the outer
//! column, the load-error strip, dialog stacking, off-screen refresh suspension — comes
//! from `LoadableComponentMaster`.
//!
//! **Rows are edited in an `EditWindow`, not in the cell.** That is what the Proxmox stack
//! does for every key/value grid it has: `proxmox_yew_comp::ObjectGrid` (the widget behind
//! the node and datacenter option pages) selects a row and opens an `EditWindow` on the
//! Edit button, a double click or Space (`COMP/src/object_grid.rs:314-341,447-470`).
//! There is no editable-cell support in pwt's `DataTable` at all — no cell editor, no
//! commit/rollback, no per-cell validation state — so an inline field would be a bespoke
//! widget living outside the design system, in the one place (a grid) where the system
//! already has an answer. The dialog also has room for the row's *description*, which is a
//! second key on the wire (the sibling comment key) and has nowhere to go in a cell.
//!
//! Monaco keeps exactly two jobs (§8): "Edit as text" for one subtree, and the diff that
//! confirms applying it.
//!
//! Two disciplines run through this file, both from `docs/REVIEW-2026-09-07.md`:
//!
//! * **Every async result carries the identity it was requested for** (`crate::request`).
//!   `LoadableComponentMaster` respawns a load per `Msg::Load` and cancels nothing, so an
//!   answer for a document the page has left must be dropped, not applied (F4).
//! * **A write is never made from stale state.** Every write sends the digest the tree was
//!   built from; a 409 reloads and says so (§8).

use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;

use anyhow::{Error, anyhow};
use serde_json::{Value, json};
use wasm_bindgen::prelude::Closure;
use web_sys::Element;
use yew::html::IntoPropValue;
use yew::virtual_dom::{Key, VComp, VNode};

use pwt::css::{AlignItems, ColorScheme, FlexFit, FontStyle, JustifyContent};
use pwt::prelude::*;
use pwt::props::ExtractPrimaryKey;
use pwt::state::{Selection, SlabTree, SlabTreeNodeMut, ThemeObserver, TreeStore};
use pwt::widget::data_table::{
    DataTable, DataTableCellRenderArgs, DataTableColumn, DataTableHeader, DataTableMouseEvent,
};
use pwt::widget::form::{Checkbox, Combobox, Field, FormContext, Number, TextArea};
use pwt::widget::{
    Button, Column, Container, Dialog, Fa, InputPanel, Row, SegmentedButton, Toolbar, error_message,
};
use pwt_macros::builder;

use proxmox_yew_comp::{
    ConfirmButton, EditWindow, LoadableComponent, LoadableComponentContext,
    LoadableComponentMaster, LoadableComponentScopeExt, LoadableComponentState,
};

use crate::api::{self, WriteResult};
use crate::edit::{self, Write};
use crate::grammar::Operator;
use crate::model::{Access, DocId, GuestInfo};
use crate::monaco;
use crate::request::{Channel, RequestId, RequestTracker};
use crate::tree::{self, BuildContext, Node, Row as TreeRow, ValueKind};

/// `GET /meta/version` is polled this often. Its token covers the whole store, so a change
/// reloads this document — cheap, and it is the only signal that a registration, a tag or
/// a grant moved, none of which touch this document's own digest.
const VERSION_POLL_MS: u32 = 5_000;

/// Which text format the "Edit as text" dialog is showing. Presentation only (§8): the
/// apply always sends `text`, and JSON is a subset of YAML.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextFormat {
    Yaml,
    Json,
}

impl TextFormat {
    fn language(self) -> &'static str {
        match self {
            TextFormat::Yaml => "yaml",
            TextFormat::Json => "json",
        }
    }
}

/// The "Edit as text" dialog's buffers.
///
/// Both renderings of the same subtree are kept, so the format toggle never has to throw
/// away what was typed (there is no YAML parser in the page — the server renders YAML, the
/// page renders JSON — so a toggle cannot convert one buffer into the other).
struct TextState {
    /// The subtree being edited; empty is the whole document.
    path: String,
    format: TextFormat,
    yaml: Option<String>,
    yaml_draft: Option<String>,
    json: String,
    json_draft: Option<String>,
    /// Bumped whenever the text Monaco should show changes without the user typing it.
    generation: u64,
}

impl TextState {
    /// The pristine text of the shown format, as loaded.
    fn loaded(&self) -> &str {
        match self.format {
            TextFormat::Yaml => self.yaml.as_deref().unwrap_or(""),
            TextFormat::Json => &self.json,
        }
    }

    /// The text on screen.
    fn current(&self) -> &str {
        match self.format {
            TextFormat::Yaml => self
                .yaml_draft
                .as_deref()
                .unwrap_or_else(|| self.yaml.as_deref().unwrap_or("")),
            TextFormat::Json => self.json_draft.as_deref().unwrap_or(&self.json),
        }
    }

    fn dirty(&self) -> bool {
        match self.format {
            TextFormat::Yaml => self.yaml_draft.is_some(),
            TextFormat::Json => self.json_draft.is_some(),
        }
    }

    fn ready(&self) -> bool {
        match self.format {
            TextFormat::Yaml => self.yaml.is_some(),
            TextFormat::Json => true,
        }
    }
}

/// Everything one load produces, plus the identity it was loaded for.
pub struct Loaded {
    id: RequestId,
    access: Result<Access, String>,
    operators: Result<Vec<Operator>, String>,
    guest: Option<GuestInfo>,
    digest: String,
    data: Value,
    parse_error: Option<String>,
    /// The store's version token as of this load, so a change *between* the load and the
    /// first poll tick is a change the poll notices rather than swallows as its baseline.
    version_token: Option<String>,
}

/// Modal states of the page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewState {
    /// Edit (or set) the selected row.
    EditRow,
    /// Add a key under the selected map row.
    AddRow,
    /// The Monaco text editor for one subtree.
    EditText,
    /// The diff that confirms applying it.
    ConfirmText,
}

pub enum Msg {
    Loaded(Box<Loaded>),
    Select(Option<Key>),
    /// Reload the document. Leaves the standing notice alone.
    Reload,
    /// The Reload button: catch up *and* drop whatever the page was complaining about.
    UserReload,
    VersionToken(String),
    /// Open the row dialog for `path` (the Value column's "set" action, and a double
    /// click).
    EditRow(String),
    OpenAdd,
    OpenEdit,
    RemoveRow,
    /// A write issued outside a dialog (the Remove button) answered.
    WriteFinished(RequestId, Result<WriteResult, Error>),
    /// A dialog's submit succeeded.
    WriteOk,
    /// A dialog's submit failed: the server's message, and whether it was a 409.
    WriteFailed(String, bool),
    OpenText,
    TextLoaded(RequestId, Result<String, String>),
    TextFormat(TextFormat),
    TextInput(String),
    ShowTextDiff,
    ApplyText,
    CancelDiff,
    CloseDialog,
    ThemeChanged(bool),
}

/// The metadata tree for one document.
#[derive(Clone, PartialEq, Properties)]
#[builder]
pub struct MetaEditor {
    /// Document to show.
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

impl ExtractPrimaryKey for Node {
    fn extract_key(&self) -> Key {
        Key::from(self.path.as_str())
    }
}

#[doc(hidden)]
pub struct PveMetaTree {
    state: LoadableComponentState<ViewState>,
    /// What the page is asking for, and what an answer must match to be applied.
    requests: RequestTracker,
    store: TreeStore<Node>,
    columns: Rc<Vec<DataTableHeader<Node>>>,
    selection: Selection,
    /// The selected row's path.
    selected: Option<String>,

    /// The caller's effective grants for this document.
    access: Access,
    /// Why the grants are unknown, if the endpoint refused to answer.
    access_error: Option<String>,
    /// Every registration; empty when `/meta/operators` is unavailable.
    operators: Vec<Operator>,
    /// Why the registrations are unknown. Not an error strip: the tree is complete
    /// without them, only the declared rows and the Owner column are missing.
    operators_error: Option<String>,
    guest: GuestInfo,

    data: Value,
    digest: String,
    rows: Vec<TreeRow>,
    /// The document's own note (its bare `__` key).
    description: Option<String>,
    /// Set when the stored file does not parse: the tree is empty and only a root replace
    /// through "Edit as text" can repair it (`docs/DESIGN.md` §4).
    parse_error: Option<String>,

    /// Mirrors the master's view state, which it does not expose.
    dialog: Option<ViewState>,
    /// A version-poll tick that arrived while a dialog was open.
    pending_refresh: bool,
    /// The last message of a failed write, shown verbatim under the tree.
    write_error: Option<String>,
    /// The standing "this document moved on the server" notice: set by a 409 and by a
    /// poll tick that arrives while a dialog holds the page still. It deliberately
    /// survives the reload it triggers — the reload is what makes the tree correct again,
    /// and clearing the notice with it would leave the user with a page that silently
    /// changed under them. A user-initiated Reload, or the next write that lands, clears
    /// it.
    notice: Option<String>,
    version_token: Option<String>,
    /// True until the first load has expanded the tree once.
    first_load: bool,

    text: Option<TextState>,
    text_ref: NodeRef,
    text_editor: Option<String>,
    /// The generation the mounted text editor is showing.
    text_generation: u64,
    diff_ref: NodeRef,
    diff_id: Option<String>,
    /// Kept alive for as long as the text editor exists.
    on_change: Option<Closure<dyn Fn(String)>>,
    /// Kept alive so the `pwt-theme-changed` listeners stay registered.
    _theme_observer: ThemeObserver,
    dark_mode: bool,
}

pwt::impl_deref_mut_property!(PveMetaTree, state, LoadableComponentState<ViewState>);

impl PveMetaTree {
    /// The selected row, if it still exists.
    fn selected_node(&self) -> Option<&Node> {
        tree::find(&self.rows, self.selected.as_deref()?)
    }

    /// True if the caller may create a key under the row `add_dialog()` would target:
    /// the selected map, else the selected leaf's parent, else the document root.
    ///
    /// Mirrors `text_dialog()`'s gate rather than asking "is there *any* writable scope
    /// anywhere" — that older check ignored the selection entirely, so a principal
    /// scoped to one prefix kept an enabled Add button while an unwritable row (or
    /// nothing) was selected, and the dialog opened targeting it only to fail at
    /// submit (`docs/REVIEW-2026-09-08-rev5.md` S7). The server remains the arbiter
    /// either way; this only decides whether the button is offered.
    fn may_add(&self) -> bool {
        let parent = match self.selected_node() {
            Some(node) if node.kind.is_map() => node.path.clone(),
            Some(node) => parent_path(&node.path),
            None => String::new(),
        };
        self.access.may_write(&parent) || (parent.is_empty() && self.access.write)
    }

    /// The subtree "Edit as text" acts on: the selected map, else the whole document.
    fn text_target(&self) -> String {
        match self.selected_node() {
            Some(node) if node.kind.is_map() && node.is_set() => node.path.clone(),
            _ => String::new(),
        }
    }

    /// Rebuild the row model and the store from the loaded document.
    fn rebuild(&mut self) {
        let rows = tree::build(
            &self.data,
            &BuildContext {
                access: &self.access,
                operators: &self.operators,
                tags: &self.guest.tags,
                scoped: self.requests.doc().scoped(),
            },
        );

        let expand = self.first_load;
        let mut slab: SlabTree<Node> = SlabTree::new();
        {
            let mut root = slab.set_root(Node::root());
            root.set_expanded(true);
            append_rows(&mut root, &rows, expand);
        }
        // `update_root_tree` re-applies whatever the user had expanded, so a reload — and
        // the version poll fires one every 5 s — never collapses the tree underneath them.
        self.store.write().update_root_tree(slab);

        self.description = tree::document_description(&self.data);
        self.rows = rows;
        self.first_load = false;

        // A row that no longer exists cannot stay selected: the toolbar's Edit/Remove act
        // on the selection, and acting on a path that is gone is exactly the write this
        // page must never make.
        let selected = self
            .selected
            .as_deref()
            .filter(|path| tree::find(&self.rows, path).is_some())
            .map(str::to_string);
        if selected != self.selected {
            match &selected {
                Some(path) => self.selection.select(Key::from(path.as_str())),
                None => self.selection.clear(),
            }
            self.selected = selected;
        }
    }

    fn open_dialog(&mut self, ctx: &LoadableComponentContext<Self>, state: ViewState) {
        self.dialog = Some(state);
        ctx.link().change_view(Some(state));
    }

    /// Close whichever modal is open, dropping the Monaco instances with it.
    fn close_dialog(&mut self, ctx: &LoadableComponentContext<Self>) {
        self.dialog = None;
        self.text = None;
        self.dispose_text();
        self.dispose_diff();
        ctx.link().change_view(None);
        if self.pending_refresh {
            // The store moved while the dialog held the page still.
            self.pending_refresh = false;
            ctx.link().send_message(Msg::Reload);
        }
    }

    fn dispose_text(&mut self) {
        if let Some(id) = self.text_editor.take() {
            monaco::dispose(&id);
        }
        self.on_change = None;
    }

    fn dispose_diff(&mut self) {
        if let Some(id) = self.diff_id.take() {
            monaco::dispose(&id);
        }
    }

    /// The document identity line: `200 test-ct-200 (lxc, node1)`, plus the document's own
    /// note when it has one.
    fn header(&self, ctx: &LoadableComponentContext<Self>) -> Column {
        let props = ctx.props();

        let (icon, title) = match props.doc {
            DocId::Datacenter => ("building", tr!("Datacenter")),
            DocId::Guest(vmid) => {
                let icon = match self
                    .guest
                    .guest_type
                    .as_deref()
                    .or(props.guest_type.as_deref())
                {
                    Some("lxc") => "cube",
                    _ => "desktop",
                };
                let title = match &self.guest.name {
                    Some(name) => format!("{vmid} {name}"),
                    None => vmid.to_string(),
                };
                (icon, title)
            }
        };

        let mut details: Vec<String> = Vec::new();
        if let Some(guest_type) = self
            .guest
            .guest_type
            .as_deref()
            .or(props.guest_type.as_deref())
        {
            details.push(guest_type.to_string());
        }
        if let Some(node) = self.guest.node.as_deref().or(props.node.as_deref()) {
            details.push(node.to_string());
        }
        if !self.guest.tags.is_empty() {
            details.push(self.guest.tags.join(", "));
        }

        Column::new()
            .class("pwt-border-bottom")
            .padding(2)
            .gap(1)
            .with_child(
                Row::new()
                    .class(AlignItems::Baseline)
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
                    })),
            )
            .with_optional_child(self.description.as_deref().map(|note| {
                Container::from_tag("span")
                    .class("pwt-color-on-neutral-alt")
                    .with_child(note.to_string())
            }))
    }

    /// The non-modal "changed on the server" notice.
    fn stale_banner(&self, ctx: &LoadableComponentContext<Self>, message: &str) -> Row {
        Row::new()
            .padding(2)
            .gap(2)
            .class(AlignItems::Center)
            .class(ColorScheme::WarningContainer)
            .class("pwt-default-colors")
            .class("pwt-border-bottom")
            .with_child(Fa::new("exclamation-triangle"))
            .with_child(message.to_string())
            .with_flex_spacer()
            .with_child(
                Button::refresh(self.loading())
                    .on_activate(ctx.link().callback(|_| Msg::UserReload)),
            )
    }

    /// The unparsable-file notice: the tree cannot be built, only a root replace repairs it.
    fn parse_error_banner(&self, ctx: &LoadableComponentContext<Self>, message: &str) -> Column {
        Column::new()
            .padding(2)
            .gap(1)
            .class(ColorScheme::ErrorContainer)
            .class("pwt-default-colors")
            .class("pwt-border-bottom")
            .with_child(
                Row::new()
                    .gap(2)
                    .class(AlignItems::Center)
                    .with_child(Fa::new("exclamation-circle"))
                    .with_child(tr!(
                        "This document is not valid YAML and cannot be shown as a tree."
                    ))
                    .with_flex_spacer()
                    .with_child(
                        Button::new(tr!("Edit as text"))
                            .disabled(!self.access.write)
                            .on_activate(ctx.link().callback(|_| Msg::OpenText)),
                    ),
            )
            .with_child(Container::from_tag("span").with_child(message.to_string()))
    }

    /// The row editor. One `EditWindow` per row, the `ObjectGrid` pattern.
    fn row_dialog(&self, ctx: &LoadableComponentContext<Self>) -> Option<Html> {
        let node = self.selected_node()?.clone();
        let doc = ctx.props().doc;
        let digest = self.digest.clone();
        let link = ctx.link().clone();

        let title = match node.is_set() {
            true => tr!("Edit") + ": " + &node.path,
            false => tr!("Set") + ": " + &node.path,
        };

        let had_description = node.note.is_some();

        Some(
            EditWindow::new(title)
                .width(560)
                .edit(node.is_set())
                .inline_error(true)
                .on_done(link.callback(|_| Msg::CloseDialog))
                .renderer({
                    let node = node.clone();
                    move |_form_ctx: &FormContext| row_form(&node)
                })
                .on_submit({
                    let node = node.clone();
                    move |form_ctx: FormContext| {
                        let link = link.clone();
                        let digest = digest.clone();
                        let built = row_writes(&node, &form_ctx, had_description);
                        async move {
                            let writes = built.map_err(|err| anyhow!(err))?;
                            submit(doc, writes, digest, link).await
                        }
                    }
                })
                .into(),
        )
    }

    /// The Add dialog: a key, a type and a value, under the selected map row.
    fn add_dialog(&self, ctx: &LoadableComponentContext<Self>) -> Html {
        let doc = ctx.props().doc;
        let digest = self.digest.clone();
        let link = ctx.link().clone();
        let parent = match self.selected_node() {
            Some(node) if node.kind.is_map() => node.path.clone(),
            Some(node) => parent_path(&node.path),
            None => String::new(),
        };

        let title = match parent.is_empty() {
            true => tr!("Add") + ": " + &tr!("Key"),
            false => tr!("Add") + ": " + &parent,
        };

        EditWindow::new(title)
            .width(560)
            .inline_error(true)
            .on_done(link.callback(|_| Msg::CloseDialog))
            .renderer(move |_form_ctx: &FormContext| add_form())
            .on_submit({
                let parent = parent.clone();
                move |form_ctx: FormContext| {
                    let link = link.clone();
                    let digest = digest.clone();
                    let built = add_writes(&parent, &form_ctx);
                    async move {
                        let writes = built.map_err(|err| anyhow!(err))?;
                        submit(doc, writes, digest, link).await
                    }
                }
            })
            .into()
    }

    /// The "Edit as text" dialog: Monaco over one subtree, YAML or JSON.
    fn text_dialog(&self, ctx: &LoadableComponentContext<Self>) -> Html {
        let link = ctx.link();
        let text = self.text.as_ref();
        let format = text.map(|t| t.format).unwrap_or(TextFormat::Yaml);
        let path = text.map(|t| t.path.clone()).unwrap_or_default();
        let writable = self.access.may_write(&path) || (path.is_empty() && self.access.write);

        let title = match path.is_empty() {
            true => tr!("Edit as text") + ": " + &tr!("Whole document"),
            false => tr!("Edit as text") + ": " + &path,
        };

        let toggle = |label: String, value: TextFormat| {
            let active = format == value;
            Button::new(label)
                .pressed(active)
                .class(active.then_some(ColorScheme::Primary))
                .on_activate(link.callback(move |_| Msg::TextFormat(value)))
        };

        Dialog::new(title)
            .width(900)
            .height(600)
            .resizable(true)
            .on_close(link.callback(|_| Msg::CloseDialog))
            .with_child(
                Row::new()
                    .padding(2)
                    .gap(2)
                    .class(AlignItems::Center)
                    .class("pwt-border-bottom")
                    .with_child(
                        SegmentedButton::new()
                            .aria_label(tr!("Format"))
                            .with_button(toggle(tr!("YAML"), TextFormat::Yaml))
                            .with_button(toggle(tr!("JSON"), TextFormat::Json)),
                    )
                    .with_flex_spacer()
                    .with_optional_child((!writable).then(|| {
                        Container::from_tag("span")
                            .class("pwt-color-on-neutral-alt")
                            .with_child(tr!("Read-only"))
                    })),
            )
            .with_child(
                Container::new()
                    .class(FlexFit)
                    .class("pve-meta-monaco-host")
                    .into_html_with_ref(self.text_ref.clone()),
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
                            .disabled(!writable || !text.is_some_and(TextState::dirty))
                            .on_activate(link.callback(|_| Msg::ShowTextDiff)),
                    ),
            )
            .into()
    }

    /// The diff that confirms a text apply.
    fn diff_dialog(&self, ctx: &LoadableComponentContext<Self>) -> Html {
        let link = ctx.link();
        Dialog::new(tr!("Apply") + ": " + &tr!("Changes"))
            .width(900)
            .height(600)
            .resizable(true)
            .on_close(link.callback(|_| Msg::CancelDiff))
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
                        Button::new(tr!("Back")).on_activate(link.callback(|_| Msg::CancelDiff)),
                    )
                    .with_child(
                        Button::new(tr!("Apply"))
                            .class(ColorScheme::Primary)
                            .on_activate(link.callback(|_| Msg::ApplyText)),
                    ),
            )
            .into()
    }
}

impl Drop for PveMetaTree {
    fn drop(&mut self) {
        // Monaco leaks a ResizeObserver and a model otherwise.
        self.dispose_text();
        self.dispose_diff();
    }
}

impl LoadableComponent for PveMetaTree {
    type Properties = MetaEditor;
    type Message = Msg;
    type ViewState = ViewState;

    fn create(ctx: &LoadableComponentContext<Self>) -> Self {
        let theme_observer =
            ThemeObserver::new(ctx.link().callback(|(_, dark)| Msg::ThemeChanged(dark)));
        let dark_mode = theme_observer.dark_mode();

        let store: TreeStore<Node> = TreeStore::new().view_root(false);
        let columns = Rc::new(columns(&store, ctx.link().clone()));

        let selection = Selection::new().on_select({
            let link = ctx.link().clone();
            move |selection: Selection| link.send_message(Msg::Select(selection.selected_key()))
        });

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
            store,
            columns,
            selection,
            selected: None,
            access: Access::default(),
            access_error: None,
            operators: Vec::new(),
            operators_error: None,
            guest: GuestInfo::default(),
            data: json!({}),
            digest: String::new(),
            rows: Vec::new(),
            description: None,
            parse_error: None,
            dialog: None,
            pending_refresh: false,
            write_error: None,
            notice: None,
            version_token: None,
            first_load: true,
            text: None,
            text_ref: NodeRef::default(),
            text_editor: None,
            text_generation: 0,
            diff_ref: NodeRef::default(),
            diff_id: None,
            on_change: None,
            _theme_observer: theme_observer,
            dark_mode,
        }
    }

    fn changed(&mut self, ctx: &LoadableComponentContext<Self>, _old: &Self::Properties) -> bool {
        // A different document is a different page: nothing loaded for the old one may
        // survive into it.
        if self.requests.set_doc(ctx.props().doc) {
            self.access = Access::default();
            self.access_error = None;
            self.guest = GuestInfo::default();
            self.data = json!({});
            self.digest.clear();
            self.rows.clear();
            self.selected = None;
            self.selection.clear();
            self.notice = None;
            self.write_error = None;
            self.parse_error = None;
            self.first_load = true;
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
        let link = ctx.link().clone();

        Box::pin(async move {
            // Grants, registrations and the guest's tags are all re-read on every load:
            // none of them lives inside this document, so nothing about its digest ever
            // signals that one moved (`docs/REVIEW-2026-09-08-pass4.md` Q2), and the
            // version poll reloads on any store change anyway.
            let access = api::access(doc).await.map_err(|err| err.to_string());
            let operators = api::operators().await.map_err(|err| {
                if api::is_unimplemented(&err) {
                    tr!("this node's API does not serve operator registrations")
                } else {
                    err.to_string()
                }
            });
            let guest = match doc {
                DocId::Guest(vmid) => match api::guest_info(vmid).await {
                    Ok(info) => info,
                    Err(err) => {
                        log::warn!("pve-meta-ui: could not read the guest list: {err}");
                        None
                    }
                },
                DocId::Datacenter => None,
            };

            let version = api::version().await.ok().map(|info| info.token);
            let document = api::get_data(doc, "").await;

            // Nothing past this point may touch the page unless it still wants this
            // answer: the document may have changed while this was in flight, and
            // `LoadableComponentMaster` cancels nothing (F4).
            if !tracker.accepts(&id) {
                log::debug!("pve-meta-ui: dropping a stale load of {}", doc.label());
                return Ok(());
            }

            let (digest, data, parse_error) = match document {
                Ok(document) => (document.digest, document.data, None),
                Err(err) => {
                    // An unparsable file 422s as JSON but still answers as text, with the
                    // parse error (`docs/DESIGN.md` §4) — show that rather than an empty
                    // page, so it can be repaired through "Edit as text".
                    match api::get_text(doc, "").await {
                        Ok(text) if text.parse_error.is_some() => {
                            (text.digest, json!({}), text.parse_error)
                        }
                        _ => return Err(err),
                    }
                }
            };

            link.send_message(Msg::Loaded(Box::new(Loaded {
                id,
                access,
                operators,
                guest,
                digest,
                data,
                parse_error,
                version_token: version,
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
                    operators,
                    guest,
                    digest,
                    data,
                    parse_error,
                    version_token,
                } = *loaded;

                // The future checked this before sending; check it again here, where the
                // state it would overwrite actually lives.
                if !self.requests.accepts(&id) {
                    return false;
                }

                match access {
                    Ok(access) => {
                        self.access = access;
                        self.access_error = None;
                    }
                    // A page that cannot ask for its grants stays usable and read-only
                    // rather than failing outright.
                    Err(err) => {
                        self.access = Access::default();
                        self.access_error = Some(err);
                    }
                }
                match operators {
                    Ok(operators) => {
                        self.operators = operators;
                        self.operators_error = None;
                    }
                    Err(err) => {
                        self.operators = Vec::new();
                        self.operators_error = Some(err);
                    }
                }
                self.guest = guest.unwrap_or_default();
                if version_token.is_some() {
                    self.version_token = version_token;
                }
                self.digest = digest;
                self.data = data;
                self.parse_error = parse_error;
                self.rebuild();
            }
            Msg::Select(key) => {
                let selected = key.map(|key| key.to_string());
                if selected == self.selected {
                    return false;
                }
                self.selected = selected;
            }
            Msg::Reload => {
                // Invalidate before reloading: a write issued earlier must not have its
                // late answer applied over the document this reload is about to fetch
                // (`docs/REVIEW-2026-09-08-pass2.md` P8).
                self.requests.invalidate();
                ctx.link().send_reload();
            }
            Msg::UserReload => {
                self.notice = None;
                self.write_error = None;
                ctx.link().send_message(Msg::Reload);
            }
            Msg::VersionToken(token) => {
                if self.version_token.as_deref() == Some(token.as_str()) {
                    return false;
                }
                // No "first tick is only a baseline" case: `load()` reads the token
                // alongside the document, so a change between the two is a change this
                // tick has to act on, not one it may adopt as its starting point.
                self.version_token = Some(token);
                if self.dialog.is_some() {
                    // Never reload the tree out from under an open editor (§8: the poll
                    // does not refresh while a cell is being edited). Say the document
                    // moved and catch up when the dialog closes; a write from that dialog
                    // still carries the digest it was opened with, so it 409s rather than
                    // silently overwriting whatever moved.
                    self.pending_refresh = true;
                    self.notice = Some(tr!(
                        "This document was changed on the server since it was loaded."
                    ));
                    return true;
                }
                // Nothing is being edited, so catching up *is* the answer: no notice.
                self.notice = None;
                ctx.link().send_message(Msg::Reload);
                return false;
            }
            Msg::EditRow(path) => {
                self.selection.select(Key::from(path.as_str()));
                self.selected = Some(path);
                self.open_dialog(ctx, ViewState::EditRow);
            }
            Msg::OpenAdd => self.open_dialog(ctx, ViewState::AddRow),
            Msg::OpenEdit => {
                if self.selected_node().is_none() {
                    return false;
                }
                self.open_dialog(ctx, ViewState::EditRow);
            }
            Msg::RemoveRow => {
                let Some(node) = self.selected_node() else {
                    return false;
                };
                let path = node.path.clone();
                let comment = node.comment_path().to_string();
                let has_note = node.note.is_some();
                let mut writes = vec![Write::Delete { view: path }];
                if has_note && !comment.starts_with(&format!("{}.", node.path)) {
                    // The row's own note is a sibling key; a map's own `__` goes away
                    // with the subtree that holds it.
                    writes.push(Write::Delete { view: comment });
                }

                let id = self.requests.issue(Channel::Apply);
                let doc = id.doc;
                let digest = self.digest.clone();
                let link = ctx.link().clone();
                ctx.link().spawn(async move {
                    let result = api::apply(doc, writes, &digest).await;
                    link.send_message(Msg::WriteFinished(id, result));
                });
            }
            Msg::WriteFinished(id, result) => {
                if !self.requests.accepts(&id) {
                    log::warn!("pve-meta-ui: dropping a write result; the page moved on");
                    return false;
                }
                match result {
                    Ok(_) => {
                        self.write_error = None;
                        self.notice = None;
                        ctx.link().send_message(Msg::Reload);
                    }
                    Err(err) => {
                        self.write_error = Some(err.to_string());
                        if api::is_conflict(&err) {
                            self.notice = Some(conflict_notice());
                            ctx.link().send_message(Msg::Reload);
                        }
                    }
                }
            }
            Msg::WriteOk => {
                self.write_error = None;
                self.notice = None;
                ctx.link().send_message(Msg::Reload);
                return false;
            }
            Msg::WriteFailed(message, conflict) => {
                self.write_error = Some(message);
                if conflict {
                    // §8: 409 → reload and say so. The dialog keeps what was typed and
                    // shows the server's message inline, so nothing typed is lost.
                    self.notice = Some(conflict_notice());
                    ctx.link().send_message(Msg::Reload);
                }
            }
            Msg::OpenText => {
                let path = self.text_target();
                let subtree = subtree_at(&self.data, &path);
                self.text = Some(TextState {
                    path: path.clone(),
                    format: TextFormat::Yaml,
                    yaml: None,
                    yaml_draft: None,
                    json: serde_json::to_string_pretty(&subtree).unwrap_or_default(),
                    json_draft: None,
                    generation: 0,
                });
                self.open_dialog(ctx, ViewState::EditText);

                let id = self.requests.issue(Channel::Text);
                let doc = id.doc;
                let link = ctx.link().clone();
                ctx.link().spawn(async move {
                    let result = api::get_text(doc, &path)
                        .await
                        .map(|document| document.text)
                        .map_err(|err| err.to_string());
                    link.send_message(Msg::TextLoaded(id, result));
                });
            }
            Msg::TextLoaded(id, result) => {
                if !self.requests.accepts(&id) {
                    return false;
                }
                let Some(text) = self.text.as_mut() else {
                    return false;
                };
                match result {
                    Ok(yaml) => {
                        text.yaml = Some(yaml);
                        text.generation += 1;
                    }
                    Err(err) => {
                        // Fall back to the JSON rendering, which needs no round trip.
                        text.format = TextFormat::Json;
                        text.generation += 1;
                        self.write_error = Some(err);
                    }
                }
            }
            Msg::TextFormat(format) => {
                let Some(text) = self.text.as_mut() else {
                    return false;
                };
                if text.format == format {
                    return false;
                }
                text.format = format;
                text.generation += 1;
            }
            Msg::TextInput(input) => {
                let Some(text) = self.text.as_mut() else {
                    return false;
                };
                match text.format {
                    TextFormat::Yaml => {
                        text.yaml_draft =
                            (Some(input.as_str()) != text.yaml.as_deref()).then_some(input)
                    }
                    TextFormat::Json => text.json_draft = (input != text.json).then_some(input),
                }
                // Only the Apply button's enabled state depends on this.
            }
            Msg::ShowTextDiff => {
                self.dispose_text();
                self.open_dialog(ctx, ViewState::ConfirmText);
            }
            Msg::CancelDiff => {
                self.dispose_diff();
                self.open_dialog(ctx, ViewState::EditText);
                if let Some(text) = self.text.as_mut() {
                    // The editor is remounted from scratch; make it show the draft.
                    text.generation += 1;
                }
            }
            Msg::ApplyText => {
                let Some(text) = self.text.as_ref() else {
                    return false;
                };
                let writes = vec![Write::PutText {
                    view: text.path.clone(),
                    text: text.current().to_string(),
                }];
                let id = self.requests.issue(Channel::Apply);
                let doc = id.doc;
                let digest = self.digest.clone();
                let link = ctx.link().clone();
                self.close_dialog(ctx);
                ctx.link().spawn(async move {
                    let result = api::apply(doc, writes, &digest).await;
                    link.send_message(Msg::WriteFinished(id, result));
                });
            }
            Msg::CloseDialog => self.close_dialog(ctx),
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
        let node = self.selected_node();
        let writable = node.is_some_and(|node| node.writable);
        let removable = writable && node.is_some_and(Node::is_set);

        Some(
            Toolbar::new()
                .class("pwt-w-100")
                .class("pwt-overflow-hidden")
                .class("pwt-border-bottom")
                .with_child(
                    Button::new(tr!("Add"))
                        .disabled(!self.may_add())
                        .on_activate(link.callback(|_| Msg::OpenAdd)),
                )
                .with_spacer()
                .with_child(
                    Button::new(tr!("Edit"))
                        .disabled(!writable)
                        .on_activate(link.callback(|_| Msg::OpenEdit)),
                )
                .with_child(
                    ConfirmButton::new(tr!("Remove"))
                        .dangerous(true)
                        .disabled(!removable)
                        .confirm_message(match node {
                            Some(node) => {
                                tr!(
                                    "Are you sure you want to remove entry {0}",
                                    node.path.clone()
                                )
                            }
                            None => tr!("Are you sure you want to remove this entry?"),
                        })
                        .on_activate(link.callback(|_| Msg::RemoveRow)),
                )
                .with_spacer()
                .with_child(
                    Button::new(tr!("Edit as text")).on_activate(link.callback(|_| Msg::OpenText)),
                )
                .with_flex_spacer()
                .with_optional_child((!self.access.write && self.access.scopes.is_empty()).then(
                    || {
                        Container::from_tag("span")
                            .class("pwt-color-on-neutral-alt")
                            .with_child(tr!("Read-only"))
                    },
                ))
                .with_child(
                    Button::refresh(self.loading()).on_activate(link.callback(|_| Msg::UserReload)),
                )
                .into(),
        )
    }

    fn main_view(&self, ctx: &LoadableComponentContext<Self>) -> Html {
        let link = ctx.link().clone();

        Column::new()
            .class(FlexFit)
            .with_child(self.header(ctx).key("header"))
            .with_optional_child(
                self.notice
                    .as_deref()
                    .map(|message| self.stale_banner(ctx, message).key("stale-banner")),
            )
            .with_optional_child(
                self.parse_error
                    .as_deref()
                    .map(|err| self.parse_error_banner(ctx, err).key("parse-error")),
            )
            .with_child(
                DataTable::new(Rc::clone(&self.columns), self.store.clone())
                    .key("tree")
                    .class(FlexFit)
                    .selection(self.selection.clone())
                    .striped(false)
                    .hover(true)
                    // Rows carry a description line, so their height varies; virtual
                    // scrolling assumes a uniform one. Documents are small.
                    .virtual_scroll(false)
                    .on_row_dblclick(move |event: &mut DataTableMouseEvent| {
                        link.send_message(Msg::EditRow(event.record_key.to_string()));
                    }),
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
            .with_optional_child(self.operators_error.as_deref().map(|err| {
                Row::new()
                    .key("operators-error")
                    .padding(2)
                    .gap(2)
                    .class(AlignItems::Center)
                    .class("pwt-border-top")
                    .class("pwt-color-on-neutral-alt")
                    .with_child(Fa::new("info-circle"))
                    .with_child(tr!(
                        "Declared keys and owners are unavailable: {0}",
                        err.to_string()
                    ))
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
            ViewState::EditRow => self.row_dialog(ctx),
            ViewState::AddRow => Some(self.add_dialog(ctx)),
            ViewState::EditText => Some(self.text_dialog(ctx)),
            ViewState::ConfirmText => Some(self.diff_dialog(ctx)),
        }
    }

    fn rendered(&mut self, ctx: &LoadableComponentContext<Self>, _first_render: bool) {
        // The text editor belongs to the dialog it was mounted in; nothing else may keep
        // it alive (Monaco leaks a ResizeObserver and a model).
        if self.dialog != Some(ViewState::EditText) {
            self.dispose_text();
        }
        if self.dialog != Some(ViewState::ConfirmText) {
            self.dispose_diff();
        }

        if self.dialog == Some(ViewState::EditText) {
            let (value, language, generation, read_only) = match self.text.as_ref() {
                Some(text) if text.ready() => (
                    text.current().to_string(),
                    text.format.language(),
                    text.generation,
                    !(self.access.may_write(&text.path)
                        || (text.path.is_empty() && self.access.write)),
                ),
                _ => (String::new(), "yaml", 0, true),
            };

            match &self.text_editor {
                None => {
                    if let Some(el) = self.text_ref.cast::<Element>() {
                        let id = monaco::mount(
                            &el,
                            &monaco::MountOptions {
                                value: &value,
                                language,
                                read_only,
                                theme: if self.dark_mode { "dark" } else { "light" },
                            },
                        );
                        let link = ctx.link().clone();
                        self.on_change = Some(monaco::on_change(&id, move |text| {
                            link.send_message(Msg::TextInput(text))
                        }));
                        self.text_editor = Some(id);
                        self.text_generation = generation;
                    }
                }
                Some(id) => {
                    // Never write over what the user is typing: only a load or a format
                    // switch bumps the generation.
                    if self.text_generation != generation {
                        monaco::set_language(id, language);
                        monaco::set_value(id, &value);
                        self.text_generation = generation;
                    }
                    monaco::set_read_only(id, read_only);
                }
            }
        }

        if self.dialog == Some(ViewState::ConfirmText) && self.diff_id.is_none() {
            if let (Some(text), Some(el)) = (self.text.as_ref(), self.diff_ref.cast::<Element>()) {
                self.diff_id = Some(monaco::mount_diff(
                    &el,
                    text.loaded(),
                    text.current(),
                    text.format.language(),
                ));
            }
        }
    }
}

/// Append `rows` under `parent`, expanding every map node when `expand` is set.
fn append_rows(parent: &mut SlabTreeNodeMut<'_, Node>, rows: &[TreeRow], expand: bool) {
    for row in rows {
        let mut child = parent.append(row.node.clone());
        if row.children.is_empty() {
            // A map with no children yet must still read as a map, not as a leaf.
            child.set_leaf_node(!row.node.kind.is_map());
        } else {
            child.set_expanded(expand);
            append_rows(&mut child, &row.children, expand);
        }
    }
}

/// The three columns of `docs/DESIGN.md` §8: key, value, owner.
fn columns(
    store: &TreeStore<Node>,
    link: yew::html::Scope<LoadableComponentMaster<PveMetaTree>>,
) -> Vec<DataTableHeader<Node>> {
    vec![
        DataTableColumn::new(tr!("Key"))
            .width("340px")
            .tree_column(store.clone())
            .render_cell(|args: &mut DataTableCellRenderArgs<Node>| {
                let node = args.record();
                let icon = match (&node.kind, node.is_set()) {
                    (ValueKind::Map, _) => "folder-o",
                    (ValueKind::Array, _) => "list",
                    _ => "file-text-o",
                };
                let mut key = Row::new()
                    .class(AlignItems::Baseline)
                    .gap(1)
                    .with_child(Fa::new(icon).fixed_width())
                    .with_child(Container::from_tag("span").with_child(node.key.clone()));
                if !node.is_set() {
                    key.add_class("pwt-opacity-50");
                }
                Column::new()
                    .with_child(key)
                    .with_optional_child(node.description.as_deref().map(|note| {
                        Container::from_tag("span")
                            .class("pwt-font-label-small")
                            .class("pwt-color-on-neutral-alt")
                            .with_child(note.to_string())
                    }))
                    .into()
            })
            .into(),
        DataTableColumn::new(tr!("Value"))
            .flex(1)
            .render_cell(move |args: &mut DataTableCellRenderArgs<Node>| {
                let node = args.record();
                if node.is_set() {
                    return Container::from_tag("span")
                        .class("pwt-font-monospace")
                        .with_child(node.display_value())
                        .into();
                }
                // A declared-but-unset row: its default, greyed, plus the "set" action.
                let default = node.default_text().unwrap_or_default();
                let path = node.path.clone();
                let link = link.clone();
                Row::new()
                    .class(AlignItems::Baseline)
                    .gap(2)
                    .with_child(
                        Container::from_tag("span")
                            .class("pwt-font-monospace")
                            .class("pwt-opacity-50")
                            .with_child(match default.is_empty() {
                                true => tr!("not set"),
                                false => default,
                            }),
                    )
                    .with_optional_child(node.writable.then(|| {
                        Container::from_tag("a")
                            .class("pwt-pointer")
                            .attribute("role", "button")
                            .attribute("tabindex", "0")
                            .onclick(move |event: MouseEvent| {
                                event.stop_propagation();
                                link.send_message(Msg::EditRow(path.clone()));
                            })
                            .with_child(tr!("Set"))
                    }))
                    .into()
            })
            .into(),
        DataTableColumn::new(tr!("Owner"))
            .width("220px")
            .render_cell(
                |args: &mut DataTableCellRenderArgs<Node>| match &args.record().owner {
                    Some(owner) => Container::from_tag("span")
                        .attribute("title", owner.detail())
                        .with_child(owner.label())
                        .into(),
                    None => html! {},
                },
            )
            .into(),
    ]
}

/// The form of the row dialog: one field of the row's type, plus its description.
fn row_form(node: &Node) -> Html {
    let mut panel = InputPanel::new().padding(4).width("auto");

    let text = edit::field_text(node.value.as_ref().or(node.default.as_ref()));
    let label = tr!("Value");

    match &node.kind {
        ValueKind::Map => {
            panel.add_field(
                label,
                Field::new()
                    .name("value")
                    .disabled(true)
                    .placeholder(tr!("a map — edit its keys, or use \"Edit as text\"")),
            );
        }
        ValueKind::Boolean => {
            let checked = node
                .value
                .as_ref()
                .or(node.default.as_ref())
                .and_then(Value::as_bool)
                .unwrap_or(false);
            panel.add_field(label, Checkbox::new().name("value").default(checked));
        }
        ValueKind::Integer => {
            panel.add_field(
                label,
                Number::<i64>::new()
                    .name("value")
                    .default(text.parse::<i64>().ok()),
            );
        }
        ValueKind::Number => {
            panel.add_field(
                label,
                Number::<f64>::new()
                    .name("value")
                    .default(text.parse::<f64>().ok()),
            );
        }
        ValueKind::Enum(values) => {
            let items: Rc<Vec<AttrValue>> =
                Rc::new(values.iter().map(|v| AttrValue::from(v.clone())).collect());
            panel.add_field(
                label,
                Combobox::new()
                    .name("value")
                    .required(true)
                    .items(items)
                    .default(text),
            );
        }
        ValueKind::Array => {
            panel.add_large_field(
                false,
                false,
                label,
                TextArea::new()
                    .name("value")
                    .default(text)
                    .attribute("rows", "4"),
            );
        }
        ValueKind::Text => {
            panel.add_field(
                label,
                Field::new().name("value").default(text).autofocus(true),
            );
        }
    }

    // The row's note is the sibling comment key (`docs/DESIGN.md` §2) — a second key on
    // the wire, which is why it belongs in a dialog rather than in a cell.
    panel.add_field(
        tr!("Description"),
        Field::new()
            .name("description")
            .default(node.note.clone().unwrap_or_default())
            .placeholder(tr!("stored as {0}", node.comment_path())),
    );

    panel.into()
}

/// The writes the row dialog's submit performs.
fn row_writes(
    node: &Node,
    form_ctx: &FormContext,
    had_description: bool,
) -> Result<Vec<Write>, String> {
    let data = form_ctx.get_submit_data();
    let description = data
        .get("description")
        .map(|value| edit::field_text(Some(value)))
        .unwrap_or_default();

    let value = match (&node.kind, node.is_set()) {
        // A map row's value is its children; setting one only creates the empty map.
        (ValueKind::Map, true) => None,
        (ValueKind::Map, false) => Some(json!({})),
        (kind, _) => {
            let text = edit::field_text(data.get("value"));
            Some(edit::parse_value(kind, &text)?)
        }
    };

    let writes = edit::row_writes(
        &node.path,
        node.comment_path(),
        value,
        Some(&description),
        had_description,
    );
    match writes.is_empty() {
        true => Err(tr!("nothing changed")),
        false => Ok(writes),
    }
}

/// The form of the Add dialog.
fn add_form() -> Html {
    let types: Rc<Vec<AttrValue>> = Rc::new(
        ["string", "integer", "number", "boolean", "array", "object"]
            .iter()
            .map(|t| AttrValue::from(*t))
            .collect(),
    );

    InputPanel::new()
        .padding(4)
        .width("auto")
        .with_field(
            tr!("Key"),
            Field::new().name("key").required(true).autofocus(true),
        )
        .with_field(
            tr!("Type"),
            Combobox::new()
                .name("type")
                .required(true)
                .items(types)
                .default("string"),
        )
        .with_field(tr!("Value"), Field::new().name("value"))
        .with_field(tr!("Description"), Field::new().name("description"))
        .into()
}

/// The writes the Add dialog's submit performs.
fn add_writes(parent: &str, form_ctx: &FormContext) -> Result<Vec<Write>, String> {
    let data = form_ctx.get_submit_data();
    let key = edit::field_text(data.get("key"));
    edit::validate_key(&key)?;

    let kind = match edit::field_text(data.get("type")).as_str() {
        "integer" => ValueKind::Integer,
        "number" => ValueKind::Number,
        "boolean" => ValueKind::Boolean,
        "array" => ValueKind::Array,
        "object" => ValueKind::Map,
        _ => ValueKind::Text,
    };

    let text = edit::field_text(data.get("value"));
    let value = match (&kind, text.trim().is_empty()) {
        (ValueKind::Map, true) => json!({}),
        (ValueKind::Array, true) => json!([]),
        _ => edit::parse_value(&kind, &text)?,
    };

    let description = edit::field_text(data.get("description"));
    let path = tree::join(parent, &key);
    let comment = format!("{path}__");
    Ok(edit::row_writes(
        &path,
        &comment,
        Some(value),
        Some(&description),
        false,
    ))
}

/// Run a dialog's writes, reporting the outcome to the page.
async fn submit(
    doc: DocId,
    writes: Vec<Write>,
    digest: String,
    link: yew::html::Scope<LoadableComponentMaster<PveMetaTree>>,
) -> Result<(), Error> {
    match api::apply(doc, writes, &digest).await {
        Ok(_) => {
            link.send_message(Msg::WriteOk);
            Ok(())
        }
        Err(err) => {
            // The page reloads and shows the notice; the dialog keeps what was typed and
            // shows the server's own message inline.
            link.send_message(Msg::WriteFailed(err.to_string(), api::is_conflict(&err)));
            Err(err)
        }
    }
}

/// The standing notice a 409 leaves behind.
fn conflict_notice() -> String {
    tr!(
        "This document changed on the server, so your change was not applied. The tree \
         below has been reloaded."
    )
}

/// The subtree at a dotted path, or `{}` if it is not there.
fn subtree_at(data: &Value, path: &str) -> Value {
    if path.is_empty() {
        return data.clone();
    }
    data.pointer(&pointer_of(path))
        .cloned()
        .unwrap_or(json!({}))
}

/// A dotted key path as a JSON pointer (`traefik.spec` → `/traefik/spec`).
fn pointer_of(path: &str) -> String {
    path.split('.')
        .map(|segment| format!("/{}", segment.replace('~', "~0").replace('/', "~1")))
        .collect()
}

/// `traefik.spec.host` → `traefik.spec`.
fn parent_path(path: &str) -> String {
    match path.rfind('.') {
        Some(dot) => path[..dot].to_string(),
        None => String::new(),
    }
}

impl From<MetaEditor> for VNode {
    fn from(val: MetaEditor) -> Self {
        let comp = VComp::new::<LoadableComponentMaster<PveMetaTree>>(Rc::new(val), None);
        VNode::from(comp)
    }
}
