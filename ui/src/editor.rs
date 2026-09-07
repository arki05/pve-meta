//! Per-document editor: loads a [`Document`], hosts the Form/Source view toggle, the
//! local working copy + pending-patch bar, apply/discard, the 409-conflict dialog, and
//! reacts to the version-poll banner from `crate::app`.

use std::collections::HashMap;
use std::rc::Rc;

use anyhow::Error;
use serde_json::{json, Value};
use yew::html::Scope;
use yew::prelude::*;

use pwt::prelude::*;
use pwt::widget::{ActionIcon, Button, Column, Dialog, Row};

use crate::api::{self, Operator};
use crate::form::{FormEvent, FormRoot};
use crate::model::{DocId, Document, Touched};
use crate::patch;
use crate::source::{ConvertConfirmDialog, DiffDialog, SourceEvent, SourceView, VerifyOutcome};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ViewMode {
    Form,
    Source,
}

#[derive(Properties, PartialEq, Clone)]
pub struct EditorProps {
    pub id: DocId,
    pub schemas: Rc<HashMap<String, Value>>,
    pub registry: Rc<Vec<Operator>>,
    /// The latest `GET /meta/version` token known by `crate::app`'s long-poll loop.
    /// Changing this (while unchanged `id`) triggers a digest check.
    pub version_token: Option<String>,
}

struct DiffState {
    old: String,
    new: String,
    touched: Vec<Touched>,
}

pub enum Msg {
    Loaded(Result<Document, Error>),
    FormEdit(FormEvent),
    Discard,
    ApplyClicked,
    ApplyResult(Result<Document, Error>),
    Reload,
    ReloadResult(Result<Document, Error>),
    DismissError,
    SwitchView(ViewMode),
    ConfirmSwitchDiscard,
    ConfirmSwitchCancel,
    ConflictCancel,
    SourceEdit(SourceEvent),
    ConvertConfirmed,
    ConvertCancelled,
    ConvertResult(Result<Document, Error>),
    VerifyResult(Result<Document, Error>),
    SourceApplyDryRunResult(Result<Document, Error>),
    DiffConfirmed,
    DiffCancelled,
    SourceApplyResult(Result<Document, Error>),
    ServerCheckResult(Result<Document, Error>),
}

pub struct Editor {
    id: DocId,
    loaded: Option<Document>,
    error: Option<String>,
    working: Value,
    view: ViewMode,
    pending_view: Option<ViewMode>,
    show_switch_confirm: bool,
    applying: bool,
    conflict: bool,
    server_changed_banner: bool,
    pending_server_doc: Option<Document>,
    source_text: String,
    source_format: String,
    source_busy: bool,
    verify_outcome: Option<VerifyOutcome>,
    show_convert_confirm: bool,
    diff_dialog: Option<DiffState>,
}

impl Editor {
    fn is_dirty(&self) -> bool {
        match &self.loaded {
            None => false,
            Some(doc) => {
                !patch::patch_is_empty(&patch::make_patch(&doc.data, &self.working))
                    || self.source_text != doc.raw.clone().unwrap_or_default()
            }
        }
    }

    /// Replace `loaded` with `fresh`, re-applying whatever local form patch was pending
    /// on top of it (a no-op if there was none — including right after our own
    /// successful write, where re-applying the just-written patch is idempotent).
    /// Raw-text (Source view) edits are left untouched: they aren't expressible as a
    /// merge patch, so the user re-applies/re-verifies them against the new digest.
    fn apply_reload(&mut self, fresh: Document) {
        let old_data = self
            .loaded
            .as_ref()
            .map(|d| d.data.clone())
            .unwrap_or(Value::Null);
        let pending_patch = patch::make_patch(&old_data, &self.working);
        self.working = patch::apply_patch(&fresh.data, &pending_patch);
        self.loaded = Some(fresh);
        self.conflict = false;
        self.server_changed_banner = false;
        self.pending_server_doc = None;
        self.error = None;
    }

    fn apply_form_event(&mut self, ev: FormEvent) {
        match ev {
            FormEvent::SetValue { path, value } => patch::set_path(&mut self.working, &path, value),
            FormEvent::SetComment { mut path, text } => {
                if let Some(last) = path.pop() {
                    let mut comment_path = path;
                    comment_path.push(format!("{last}__"));
                    if text.is_empty() {
                        patch::delete_path(&mut self.working, &comment_path);
                    } else {
                        patch::set_path(&mut self.working, &comment_path, Value::String(text));
                    }
                }
            }
            FormEvent::DeleteKey { path } => patch::delete_path(&mut self.working, &path),
            FormEvent::AddKey { mut path, key, value } => {
                path.push(key);
                patch::set_path(&mut self.working, &path, value);
            }
            FormEvent::RemoveNamespace { namespace } => {
                patch::delete_path(&mut self.working, std::slice::from_ref(&namespace))
            }
            FormEvent::AddNamespace { namespace } => {
                patch::set_path(&mut self.working, std::slice::from_ref(&namespace), json!({}))
            }
        }
    }
}

impl Component for Editor {
    type Message = Msg;
    type Properties = EditorProps;

    fn create(ctx: &Context<Self>) -> Self {
        let id = ctx.props().id;
        ctx.link()
            .send_future(async move { Msg::Loaded(api::get(id, true, true).await) });
        Self {
            id,
            loaded: None,
            error: None,
            working: Value::Null,
            view: ViewMode::Form,
            pending_view: None,
            show_switch_confirm: false,
            applying: false,
            conflict: false,
            server_changed_banner: false,
            pending_server_doc: None,
            source_text: String::new(),
            source_format: String::new(),
            source_busy: false,
            verify_outcome: None,
            show_convert_confirm: false,
            diff_dialog: None,
        }
    }

    fn changed(&mut self, ctx: &Context<Self>, old_props: &Self::Properties) -> bool {
        if ctx.props().version_token.is_some() && ctx.props().version_token != old_props.version_token {
            let id = self.id;
            ctx.link()
                .send_future(async move { Msg::ServerCheckResult(api::get(id, true, true).await) });
        }
        true
    }

    fn update(&mut self, ctx: &Context<Self>, msg: Self::Message) -> bool {
        let id = self.id;
        match msg {
            Msg::Loaded(Ok(doc)) => {
                self.source_text = doc.raw.clone().unwrap_or_default();
                self.source_format = doc.format.clone();
                self.working = doc.data.clone();
                self.loaded = Some(doc);
                self.error = None;
            }
            Msg::Loaded(Err(e)) if api::is_not_found(&e) => {
                // No metadata file yet for this guest/datacenter — start from an
                // empty document; the first Apply creates it (docs/API.md).
                let doc = Document::empty(&self.id);
                self.source_text = doc.raw.clone().unwrap_or_default();
                self.source_format = doc.format.clone();
                self.working = doc.data.clone();
                self.loaded = Some(doc);
                self.error = None;
            }
            Msg::Loaded(Err(e)) => self.error = Some(api::error_text(&e)),
            Msg::FormEdit(ev) => self.apply_form_event(ev),
            Msg::Discard => {
                if let Some(doc) = &self.loaded {
                    self.working = doc.data.clone();
                }
            }
            Msg::ApplyClicked => {
                if let Some(doc) = &self.loaded {
                    let patch = patch::make_patch(&doc.data, &self.working);
                    if !patch::patch_is_empty(&patch) {
                        self.applying = true;
                        let digest = doc.digest.clone();
                        ctx.link().send_future(async move {
                            Msg::ApplyResult(api::patch(id, patch, api::digest_opt(&digest), false).await)
                        });
                    }
                }
            }
            Msg::ApplyResult(Ok(_)) => {
                self.applying = false;
                ctx.link()
                    .send_future(async move { Msg::ReloadResult(api::get(id, true, true).await) });
            }
            Msg::ApplyResult(Err(e)) => {
                self.applying = false;
                if api::is_conflict(&e) {
                    self.conflict = true;
                } else {
                    self.error = Some(api::error_text(&e));
                }
            }
            Msg::Reload => {
                if let Some(doc) = self.pending_server_doc.take() {
                    self.apply_reload(doc);
                } else {
                    ctx.link()
                        .send_future(async move { Msg::ReloadResult(api::get(id, true, true).await) });
                }
            }
            Msg::ReloadResult(Ok(doc)) => self.apply_reload(doc),
            Msg::ReloadResult(Err(e)) => self.error = Some(api::error_text(&e)),
            Msg::DismissError => self.error = None,
            Msg::SwitchView(target) => {
                if target != self.view {
                    if self.is_dirty() {
                        self.pending_view = Some(target);
                        self.show_switch_confirm = true;
                    } else {
                        self.view = target;
                    }
                }
            }
            Msg::ConfirmSwitchDiscard => {
                match self.view {
                    ViewMode::Form => {
                        if let Some(doc) = &self.loaded {
                            self.working = doc.data.clone();
                        }
                    }
                    ViewMode::Source => {
                        if let Some(doc) = &self.loaded {
                            self.source_text = doc.raw.clone().unwrap_or_default();
                        }
                    }
                }
                if let Some(target) = self.pending_view.take() {
                    self.view = target;
                }
                self.show_switch_confirm = false;
            }
            Msg::ConfirmSwitchCancel => {
                self.pending_view = None;
                self.show_switch_confirm = false;
            }
            Msg::ConflictCancel => self.conflict = false,
            Msg::SourceEdit(SourceEvent::TextChanged(text)) => {
                self.source_text = text;
                self.verify_outcome = None;
            }
            Msg::SourceEdit(SourceEvent::FormatChanged(format)) => self.source_format = format,
            Msg::SourceEdit(SourceEvent::ConvertClicked) => self.show_convert_confirm = true,
            Msg::SourceEdit(SourceEvent::VerifyClicked) => {
                if let Some(doc) = &self.loaded {
                    self.source_busy = true;
                    let digest = doc.digest.clone();
                    let content = self.source_text.clone();
                    ctx.link().send_future(async move {
                        Msg::VerifyResult(api::put_raw(id, content, None, api::digest_opt(&digest), true).await)
                    });
                }
            }
            Msg::VerifyResult(result) => {
                self.source_busy = false;
                match result {
                    Ok(doc) => self.verify_outcome = Some(VerifyOutcome::Touched(doc.touched)),
                    Err(e) if api::is_conflict(&e) => self.conflict = true,
                    Err(e) => self.verify_outcome = Some(VerifyOutcome::Error(api::error_text(&e))),
                }
            }
            Msg::SourceEdit(SourceEvent::ApplyClicked) => {
                if let Some(doc) = &self.loaded {
                    self.source_busy = true;
                    let digest = doc.digest.clone();
                    let content = self.source_text.clone();
                    ctx.link().send_future(async move {
                        Msg::SourceApplyDryRunResult(
                            api::put_raw(id, content, None, api::digest_opt(&digest), true).await,
                        )
                    });
                }
            }
            Msg::SourceApplyDryRunResult(result) => {
                self.source_busy = false;
                match result {
                    Ok(doc) => {
                        let old = self.loaded.as_ref().and_then(|d| d.raw.clone()).unwrap_or_default();
                        self.diff_dialog = Some(DiffState {
                            old,
                            new: self.source_text.clone(),
                            touched: doc.touched,
                        });
                    }
                    Err(e) if api::is_conflict(&e) => self.conflict = true,
                    Err(e) => self.verify_outcome = Some(VerifyOutcome::Error(api::error_text(&e))),
                }
            }
            Msg::DiffCancelled => self.diff_dialog = None,
            Msg::DiffConfirmed => {
                if let Some(doc) = &self.loaded {
                    self.source_busy = true;
                    let digest = doc.digest.clone();
                    let content = self.source_text.clone();
                    ctx.link().send_future(async move {
                        Msg::SourceApplyResult(api::put_raw(id, content, None, api::digest_opt(&digest), false).await)
                    });
                }
            }
            Msg::SourceApplyResult(result) => {
                self.source_busy = false;
                self.diff_dialog = None;
                match result {
                    Ok(_) => {
                        ctx.link()
                            .send_future(async move { Msg::ReloadResult(api::get(id, true, true).await) });
                    }
                    Err(e) if api::is_conflict(&e) => self.conflict = true,
                    Err(e) => self.error = Some(api::error_text(&e)),
                }
            }
            Msg::ConvertCancelled => self.show_convert_confirm = false,
            Msg::ConvertConfirmed => {
                self.show_convert_confirm = false;
                if let Some(doc) = &self.loaded {
                    self.source_busy = true;
                    let digest = doc.digest.clone();
                    let target = self.source_format.clone();
                    ctx.link().send_future(async move {
                        Msg::ConvertResult(api::convert(id, &target, api::digest_opt(&digest)).await)
                    });
                }
            }
            Msg::ConvertResult(result) => {
                self.source_busy = false;
                match result {
                    Ok(_) => {
                        ctx.link()
                            .send_future(async move { Msg::ReloadResult(api::get(id, true, true).await) });
                    }
                    Err(e) if api::is_conflict(&e) => self.conflict = true,
                    Err(e) => self.error = Some(api::error_text(&e)),
                }
            }
            Msg::ServerCheckResult(Ok(fresh)) => {
                let changed = self
                    .loaded
                    .as_ref()
                    .map(|d| d.digest != fresh.digest)
                    .unwrap_or(false);
                if changed {
                    if self.is_dirty() {
                        self.server_changed_banner = true;
                        self.pending_server_doc = Some(fresh);
                    } else {
                        self.apply_reload(fresh);
                    }
                }
            }
            Msg::ServerCheckResult(Err(e)) => {
                log::warn!("pve-meta-ui: version-poll digest check failed: {e}");
            }
        }
        true
    }

    fn view(&self, ctx: &Context<Self>) -> Html {
        let link = ctx.link();

        if self.loaded.is_none() {
            return match &self.error {
                Some(err) => error_panel(err),
                None => html! {<div class="pve-meta-loading">{"Loading…"}</div>},
            };
        }
        let doc = self.loaded.as_ref().expect("checked above");

        let mut toolbar = Row::new()
            .class("pve-meta-toolbar pwt-align-items-center")
            .gap(2)
            .padding(2)
            .with_child(
                Button::new("Form")
                    .pressed(self.view == ViewMode::Form)
                    .onclick(link.callback(|_: MouseEvent| Msg::SwitchView(ViewMode::Form))),
            )
            .with_child(
                Button::new("Source")
                    .pressed(self.view == ViewMode::Source)
                    .onclick(link.callback(|_: MouseEvent| Msg::SwitchView(ViewMode::Source))),
            )
            .with_flex_spacer();

        if self.server_changed_banner {
            toolbar.add_child(html! {<span class="pwt-color-warning">{"\u{25CF} changed on server"}</span>});
            toolbar.add_child(
                ActionIcon::new("fa fa-refresh")
                    .aria_label("reload")
                    .on_activate(link.callback(|_: web_sys::Event| Msg::Reload)),
            );
        }

        let mut col = Column::new().class("pve-meta-editor").with_child(toolbar);

        if let Some(err) = &self.error {
            col.add_child(error_banner(err, link.callback(|_: MouseEvent| Msg::DismissError)));
        }

        match self.view {
            ViewMode::Form => {
                let working = Rc::new(self.working.clone());
                let schemas = ctx.props().schemas.clone();
                let registry = ctx.props().registry.clone();
                let on_event = link.callback(Msg::FormEdit);
                col.add_child(html! {
                    <FormRoot working={working} schemas={schemas} registry={registry} on_event={on_event} />
                });

                let patch = patch::make_patch(&doc.data, &self.working);
                if !patch::patch_is_empty(&patch) {
                    col.add_child(pending_bar(&patch, self.applying, link));
                }
            }
            ViewMode::Source => {
                let on_event = link.callback(Msg::SourceEdit);
                col.add_child(html! {
                    <SourceView
                        text={self.source_text.clone()}
                        format={self.source_format.clone()}
                        busy={self.source_busy}
                        verify_outcome={self.verify_outcome.clone()}
                        on_event={on_event}
                    />
                });
            }
        }

        if self.show_switch_confirm {
            col.add_child(switch_confirm_dialog(link));
        }
        if self.conflict {
            col.add_child(conflict_dialog(link));
        }
        if self.show_convert_confirm {
            let on_confirm = link.callback(|_| Msg::ConvertConfirmed);
            let on_cancel = link.callback(|_| Msg::ConvertCancelled);
            col.add_child(html! {
                <ConvertConfirmDialog target_format={self.source_format.clone()} on_confirm={on_confirm} on_cancel={on_cancel} />
            });
        }
        if let Some(diff) = &self.diff_dialog {
            let on_confirm = link.callback(|_| Msg::DiffConfirmed);
            let on_cancel = link.callback(|_| Msg::DiffCancelled);
            col.add_child(html! {
                <DiffDialog old={diff.old.clone()} new={diff.new.clone()} touched={diff.touched.clone()} on_confirm={on_confirm} on_cancel={on_cancel} />
            });
        }

        col.into()
    }
}

fn pending_bar(patch: &Value, applying: bool, link: &Scope<Editor>) -> Html {
    let items = patch::describe_patch(patch);
    let summary = items
        .iter()
        .map(|(p, op)| format!("{p} ({op})"))
        .collect::<Vec<_>>()
        .join(", ");
    Row::new()
        .class("pve-meta-pending-bar pwt-align-items-center")
        .gap(2)
        .padding(2)
        .with_child(html! {<span class="pve-meta-pending-label">{format!("Pending: {summary}")}</span>})
        .with_flex_spacer()
        .with_child(
            Button::new("Discard")
                .disabled(applying)
                .onclick(link.callback(|_: MouseEvent| Msg::Discard)),
        )
        .with_child(
            Button::new("Apply")
                .disabled(applying)
                .onclick(link.callback(|_: MouseEvent| Msg::ApplyClicked)),
        )
        .into()
}

fn conflict_dialog(link: &Scope<Editor>) -> Html {
    Dialog::new("Document changed")
        .on_close(link.callback(|_| Msg::ConflictCancel))
        .with_child(html! {
            <p class="pve-meta-dialog-message">
                {"The document changed on the server. Reload and re-apply your changes?"}
            </p>
        })
        .with_child(
            Row::new()
                .gap(2)
                .padding(2)
                .with_flex_spacer()
                .with_child(Button::new("Cancel").onclick(link.callback(|_: MouseEvent| Msg::ConflictCancel)))
                .with_child(Button::new("Reload").onclick(link.callback(|_: MouseEvent| Msg::Reload))),
        )
        .into()
}

fn switch_confirm_dialog(link: &Scope<Editor>) -> Html {
    Dialog::new("Unapplied changes")
        .on_close(link.callback(|_| Msg::ConfirmSwitchCancel))
        .with_child(html! {
            <p class="pve-meta-dialog-message">
                {"You have unapplied changes in this view. Discard them to switch, or Cancel and use Apply first."}
            </p>
        })
        .with_child(
            Row::new()
                .gap(2)
                .padding(2)
                .with_flex_spacer()
                .with_child(Button::new("Cancel").onclick(link.callback(|_: MouseEvent| Msg::ConfirmSwitchCancel)))
                .with_child(
                    Button::new("Discard")
                        .onclick(link.callback(|_: MouseEvent| Msg::ConfirmSwitchDiscard)),
                ),
        )
        .into()
}

fn error_banner(text: &str, on_dismiss: Callback<MouseEvent>) -> Html {
    Row::new()
        .class("pve-meta-error-banner pwt-color-error pwt-align-items-center")
        .gap(2)
        .padding(2)
        .with_child(html! {<span>{text.to_string()}</span>})
        .with_flex_spacer()
        .with_child(Button::new("Dismiss").onclick(on_dismiss))
        .into()
}

fn error_panel(text: &str) -> Html {
    Column::new()
        .class("pve-meta-error-panel")
        .padding(4)
        .with_child(html! {<p class="pwt-color-error">{text.to_string()}</p>})
        .into()
}
