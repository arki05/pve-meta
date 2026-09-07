//! Source view: raw text editor, format converter, verify (dry-run), and apply (with a
//! unified-diff confirmation).

use similar::{ChangeTag, TextDiff};
use yew::prelude::*;

use pwt::prelude::*;
use pwt::widget::form::{Combobox, TextArea};
use pwt::widget::{Button, Column, Dialog, Row};

use crate::model::Touched;

/// Events bubbled up from the (otherwise dumb/controlled) source view.
#[derive(Debug, Clone, PartialEq)]
pub enum SourceEvent {
    TextChanged(String),
    FormatChanged(String),
    ConvertClicked,
    VerifyClicked,
    ApplyClicked,
}

/// Result of the last verify (dry-run), shown under the toolbar.
#[derive(Debug, Clone, PartialEq)]
pub enum VerifyOutcome {
    Touched(Vec<Touched>),
    Error(String),
}

#[derive(Properties, PartialEq, Clone)]
pub struct SourceViewProps {
    pub text: String,
    pub format: String,
    #[prop_or_default]
    pub busy: bool,
    #[prop_or_default]
    pub verify_outcome: Option<VerifyOutcome>,
    pub on_event: Callback<SourceEvent>,
}

const FORMATS: [&str; 3] = ["yaml", "toml", "json"];

#[function_component(SourceView)]
pub fn source_view(props: &SourceViewProps) -> Html {
    let format_items: std::rc::Rc<Vec<AttrValue>> =
        std::rc::Rc::new(FORMATS.iter().map(|f| AttrValue::from(*f)).collect());

    let on_event = props.on_event.clone();
    let on_text_change = {
        let on_event = on_event.clone();
        Callback::from(move |s: String| on_event.emit(SourceEvent::TextChanged(s)))
    };
    let on_format_change = {
        let on_event = on_event.clone();
        Callback::from(move |s: String| on_event.emit(SourceEvent::FormatChanged(s)))
    };
    let on_convert = {
        let on_event = on_event.clone();
        Callback::from(move |_: MouseEvent| on_event.emit(SourceEvent::ConvertClicked))
    };
    let on_verify = {
        let on_event = on_event.clone();
        Callback::from(move |_: MouseEvent| on_event.emit(SourceEvent::VerifyClicked))
    };
    let on_apply = {
        let on_event = on_event.clone();
        Callback::from(move |_: MouseEvent| on_event.emit(SourceEvent::ApplyClicked))
    };

    let outcome = match &props.verify_outcome {
        Some(VerifyOutcome::Touched(touched)) if touched.is_empty() => {
            html! {<p class="pwt-color-success">{"Verified: no changes."}</p>}
        }
        Some(VerifyOutcome::Touched(touched)) => {
            html! {
                <ul class="pve-meta-touched-list">
                    { for touched.iter().map(|t| html!{<li>{format!("{} ({})", t.path, t.op)}</li>}) }
                </ul>
            }
        }
        Some(VerifyOutcome::Error(err)) => html! {<p class="pwt-color-error">{err.clone()}</p>},
        None => html! {},
    };

    Column::new()
        .class("pve-meta-source-view")
        .gap(2)
        .padding(2)
        .with_child(
            Row::new()
                .gap(2)
                .class("pwt-align-items-center")
                .with_child(html! {<span>{"format:"}</span>})
                .with_child(
                    Combobox::new()
                        .items(format_items)
                        .value(props.format.clone())
                        .on_change(on_format_change),
                )
                .with_child(Button::new("Convert").disabled(props.busy).onclick(on_convert))
                .with_flex_spacer()
                .with_child(Button::new("Verify").disabled(props.busy).onclick(on_verify))
                .with_child(Button::new("Apply").disabled(props.busy).onclick(on_apply)),
        )
        .with_child(outcome)
        .with_child(
            TextArea::new()
                .class("pve-meta-source")
                .value(props.text.clone())
                .on_change(on_text_change),
        )
        .into()
}

/// A confirmation dialog for converting to a new format (comments in the file are
/// lost; comment keys survive).
#[derive(Properties, PartialEq, Clone)]
pub struct ConvertConfirmDialogProps {
    pub target_format: String,
    pub on_confirm: Callback<()>,
    pub on_cancel: Callback<()>,
}

#[function_component(ConvertConfirmDialog)]
pub fn convert_confirm_dialog(props: &ConvertConfirmDialogProps) -> Html {
    let on_confirm = props.on_confirm.clone();
    let on_cancel = props.on_cancel.clone();
    Dialog::new("Convert format")
        .on_close({
            let on_cancel = on_cancel.clone();
            move |_| on_cancel.emit(())
        })
        .with_child(html! {
            <p class="pve-meta-dialog-message">
                {format!(
                    "Convert this document to {}? Comments in the file are lost; comment keys (key__) survive.",
                    props.target_format
                )}
            </p>
        })
        .with_child(
            Row::new()
                .gap(2)
                .padding(2)
                .with_flex_spacer()
                .with_child(Button::new("Cancel").onclick(move |_| on_cancel.emit(())))
                .with_child(Button::new("Convert").onclick(move |_| on_confirm.emit(()))),
        )
        .into()
}

/// The unified-diff confirmation dialog shown before a real (non-dry-run) raw apply.
#[derive(Properties, PartialEq, Clone)]
pub struct DiffDialogProps {
    pub old: String,
    pub new: String,
    pub touched: Vec<Touched>,
    pub on_confirm: Callback<()>,
    pub on_cancel: Callback<()>,
}

#[function_component(DiffDialog)]
pub fn diff_dialog(props: &DiffDialogProps) -> Html {
    let on_confirm = props.on_confirm.clone();
    let on_cancel = props.on_cancel.clone();
    Dialog::new("Apply — review changes")
        .resizable(true)
        .on_close({
            let on_cancel = on_cancel.clone();
            move |_| on_cancel.emit(())
        })
        .with_child(colored_diff(&props.old, &props.new))
        .with_child(html! {
            <ul class="pve-meta-touched-list">
                { for props.touched.iter().map(|t| html!{<li>{format!("{} ({})", t.path, t.op)}</li>}) }
            </ul>
        })
        .with_child(
            Row::new()
                .gap(2)
                .padding(2)
                .with_flex_spacer()
                .with_child(Button::new("Cancel").onclick(move |_| on_cancel.emit(())))
                .with_child(Button::new("Apply").onclick(move |_| on_confirm.emit(()))),
        )
        .into()
}

/// Render a unified line diff, `+`/`-` lines colored via `pwt-color-success`/
/// `pwt-color-error`.
fn colored_diff(old: &str, new: &str) -> Html {
    let diff = TextDiff::from_lines(old, new);
    let lines: Vec<Html> = diff
        .iter_all_changes()
        .map(|change| {
            let (prefix, class) = match change.tag() {
                ChangeTag::Delete => ("-", "pwt-color-error"),
                ChangeTag::Insert => ("+", "pwt-color-success"),
                ChangeTag::Equal => (" ", ""),
            };
            let text = format!("{prefix} {}", change.value().trim_end_matches('\n'));
            html! {<div class={class}>{text}</div>}
        })
        .collect();

    html! {<pre class="pve-meta-source pve-meta-diff">{ for lines }</pre>}
}
