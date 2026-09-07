//! Recursive rendering of one JSON object subtree: nested sections, leaf rows (with
//! type-appropriate widgets and the comment-key "note"), and the two array cases the
//! spec calls out (scalar arrays as a comma-separated field, arrays of objects as
//! read-only JSON with an "edit in Source" hint).

use std::rc::Rc;

use serde_json::{json, Map, Value};
use yew::prelude::*;

use pwt::prelude::*;
use pwt::widget::form::{Checkbox, Combobox, Field, Number};
use pwt::widget::{ActionIcon, Column, Row};

use crate::model::is_comment_key;
use crate::schema::{FieldKind, FieldSpec};

use super::add_key::AddKeyDialog;
use super::{object_keys, FormEvent};

/// Render every (non-comment) key of the object `value` as a row or nested section, in
/// schema order first (for keys the schema knows about), then remaining keys in their
/// original order.
pub fn render_object_entries(
    path: &[String],
    value: &Value,
    schema_fields: Option<&[FieldSpec]>,
    on_event: &Callback<FormEvent>,
) -> Vec<Html> {
    let obj = match value.as_object() {
        Some(o) => o,
        None if value.is_null() => return Vec::new(),
        None => {
            return vec![html! {
                <p class="pwt-color-error">
                    {"This value is not an object — edit it in Source view."}
                </p>
            }];
        }
    };

    let mut ordered: Vec<&String> = Vec::new();
    if let Some(fields) = schema_fields {
        for f in fields {
            if obj.contains_key(&f.key) {
                ordered.push(&f.key);
            }
        }
    }
    for k in obj.keys() {
        if is_comment_key(k) {
            continue;
        }
        if !ordered.contains(&k) {
            ordered.push(k);
        }
    }

    ordered
        .into_iter()
        .map(|key| {
            let field_spec = schema_fields.and_then(|fs| fs.iter().find(|f| &f.key == key));
            render_entry(path, obj, key, field_spec, on_event)
        })
        .collect()
}

fn render_entry(
    path: &[String],
    obj: &Map<String, Value>,
    key: &str,
    field_spec: Option<&FieldSpec>,
    on_event: &Callback<FormEvent>,
) -> Html {
    let value = obj.get(key).cloned().unwrap_or(Value::Null);
    let mut child_path = path.to_vec();
    child_path.push(key.to_string());

    let label = field_spec
        .map(|f| f.label().to_string())
        .unwrap_or_else(|| key.to_string());
    let required = field_spec.map(|f| f.required).unwrap_or(false);
    let description = field_spec.and_then(|f| f.description.clone());

    let schema_nested = field_spec.and_then(|f| match &f.kind {
        FieldKind::Object(nested) => Some(nested.clone()),
        _ => None,
    });

    if value.is_object() || schema_nested.is_some() {
        let section_key = child_path.join(".");
        return html! {
            <ObjectSection
                key={section_key}
                path={child_path}
                value={value}
                schema_fields={schema_nested.map(Rc::new)}
                label={label}
                required={required}
                on_event={on_event.clone()}
            />
        };
    }

    let comment_key = format!("{key}__");
    let comment = obj.get(&comment_key).and_then(Value::as_str).map(String::from);

    if let Value::Array(items) = &value {
        let widget = if !items.is_empty() && items.iter().all(Value::is_object) {
            render_readonly_array(&value)
        } else {
            render_scalar_array_widget(&child_path, items, on_event)
        };
        return html! {
            <ValueRow
                path={child_path}
                label={label}
                required={required}
                description={description}
                comment={comment}
                widget={widget}
                on_event={on_event.clone()}
            />
        };
    }

    let widget = render_scalar_widget(&child_path, &value, field_spec, on_event);
    html! {
        <ValueRow
            path={child_path}
            label={label}
            required={required}
            description={description}
            comment={comment}
            widget={widget}
            on_event={on_event.clone()}
        />
    }
}

fn render_scalar_widget(
    path: &[String],
    value: &Value,
    field_spec: Option<&FieldSpec>,
    on_event: &Callback<FormEvent>,
) -> Html {
    let kind = field_spec.map(|f| &f.kind);

    if matches!(kind, Some(FieldKind::Boolean)) || matches!(value, Value::Bool(_)) {
        return render_bool_widget(path, value, on_event);
    }

    if let Some(FieldKind::Enum(options)) = kind {
        return render_enum_widget(path, value, options, on_event);
    }

    let (minimum, maximum) = match kind {
        Some(FieldKind::Integer { minimum, maximum }) | Some(FieldKind::Number { minimum, maximum }) => {
            (*minimum, *maximum)
        }
        _ => (None, None),
    };
    if matches!(kind, Some(FieldKind::Integer { .. }) | Some(FieldKind::Number { .. })) || value.is_number() {
        return render_number_widget(path, value, minimum, maximum, on_event);
    }

    render_string_widget(path, value, on_event)
}

fn render_bool_widget(path: &[String], value: &Value, on_event: &Callback<FormEvent>) -> Html {
    let checked = value.as_bool().unwrap_or(false);
    let path = path.to_vec();
    let on_event = on_event.clone();
    Checkbox::new()
        .checked(checked)
        .on_change(move |b: bool| {
            on_event.emit(FormEvent::SetValue {
                path: path.clone(),
                value: json!(b),
            })
        })
        .into()
}

fn render_enum_widget(
    path: &[String],
    value: &Value,
    options: &[String],
    on_event: &Callback<FormEvent>,
) -> Html {
    let current = value.as_str().unwrap_or_default().to_string();
    let items: Rc<Vec<AttrValue>> = Rc::new(options.iter().cloned().map(AttrValue::from).collect());
    let path = path.to_vec();
    let on_event = on_event.clone();
    Combobox::new()
        .items(items)
        .value(current)
        .on_change(move |s: String| {
            on_event.emit(FormEvent::SetValue {
                path: path.clone(),
                value: json!(s),
            })
        })
        .into()
}

/// Format a number the way it should round-trip: no trailing `.0` for whole numbers.
fn format_number(n: f64) -> String {
    if n.fract() == 0.0 && n.abs() < 1e15 {
        format!("{}", n as i64)
    } else {
        format!("{n}")
    }
}

fn render_number_widget(
    path: &[String],
    value: &Value,
    minimum: Option<f64>,
    maximum: Option<f64>,
    on_event: &Callback<FormEvent>,
) -> Html {
    let n = value.as_f64().unwrap_or(0.0);
    let path = path.to_vec();
    let on_event = on_event.clone();
    let mut field = Number::<f64>::new().value(format_number(n));
    if let Some(min) = minimum {
        field = field.min(min);
    }
    if let Some(max) = maximum {
        field = field.max(max);
    }
    field
        .on_change(move |res: Option<Result<f64, String>>| {
            if let Some(Ok(n)) = res {
                // Simplification vs. the letter of the spec: we decide integer-vs-float
                // from the parsed value's fractional part (no fractional part -> store
                // as an integer), not from whether the typed text contained a `.`/`e` —
                // Number<f64>'s on_change only hands back the parsed float, not the raw
                // text. A deliberately-typed "8080.0" round-trips as the integer 8080.
                let value = if n.fract() == 0.0 && n.abs() < 1e15 {
                    json!(n as i64)
                } else {
                    json!(n)
                };
                on_event.emit(FormEvent::SetValue {
                    path: path.clone(),
                    value,
                });
            }
        })
        .into()
}

fn render_string_widget(path: &[String], value: &Value, on_event: &Callback<FormEvent>) -> Html {
    let text = match value {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    };
    let path = path.to_vec();
    let on_event = on_event.clone();
    Field::new()
        .value(text)
        .on_change(move |s: String| {
            on_event.emit(FormEvent::SetValue {
                path: path.clone(),
                value: json!(s),
            })
        })
        .into()
}

fn value_to_plain_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

fn parse_comma_list(s: &str, prefer_numeric: bool) -> Value {
    let parts: Vec<&str> = s.split(',').map(str::trim).filter(|p| !p.is_empty()).collect();
    if prefer_numeric {
        let mut nums = Vec::with_capacity(parts.len());
        let mut all_ok = true;
        for p in &parts {
            match p.parse::<f64>() {
                Ok(n) if n.fract() == 0.0 => nums.push(json!(n as i64)),
                Ok(n) => nums.push(json!(n)),
                Err(_) => {
                    all_ok = false;
                    break;
                }
            }
        }
        if all_ok {
            return Value::Array(nums);
        }
    }
    Value::Array(parts.into_iter().map(|p| json!(p)).collect())
}

fn render_scalar_array_widget(path: &[String], items: &[Value], on_event: &Callback<FormEvent>) -> Html {
    let prefer_numeric = !items.is_empty() && items.iter().all(Value::is_number);
    let text = items.iter().map(value_to_plain_string).collect::<Vec<_>>().join(", ");
    let path = path.to_vec();
    let on_event = on_event.clone();
    Field::new()
        .value(text)
        .placeholder("comma-separated values")
        .on_change(move |s: String| {
            on_event.emit(FormEvent::SetValue {
                path: path.clone(),
                value: parse_comma_list(&s, prefer_numeric),
            })
        })
        .into()
}

fn render_readonly_array(value: &Value) -> Html {
    let text = serde_json::to_string_pretty(value).unwrap_or_default();
    Column::new()
        .class("pve-meta-readonly-array")
        .gap(1)
        .with_child(html! {<pre class="pve-meta-source">{text}</pre>})
        .with_child(html! {<span class="pwt-color-neutral-alt">{"array of objects — edit in Source view"}</span>})
        .into()
}

#[derive(Properties, PartialEq, Clone)]
struct ValueRowProps {
    path: Vec<String>,
    label: String,
    #[prop_or_default]
    required: bool,
    #[prop_or_default]
    description: Option<String>,
    #[prop_or_default]
    comment: Option<String>,
    widget: Html,
    on_event: Callback<FormEvent>,
}

/// One leaf/array row: label, widget, comment ("note") text or editor, delete button.
#[function_component(ValueRow)]
fn value_row(props: &ValueRowProps) -> Html {
    let editing_comment = use_state(|| false);

    let note_toggle = {
        let editing_comment = editing_comment.clone();
        Callback::from(move |_: web_sys::Event| editing_comment.set(!*editing_comment))
    };

    let delete = {
        let path = props.path.clone();
        let on_event = props.on_event.clone();
        Callback::from(move |_: web_sys::Event| {
            on_event.emit(FormEvent::DeleteKey { path: path.clone() })
        })
    };

    let note_area = if *editing_comment || props.comment.is_some() {
        if *editing_comment {
            let text = props.comment.clone().unwrap_or_default();
            let path = props.path.clone();
            let on_event = props.on_event.clone();
            Field::new()
                .class("pve-meta-comment-field")
                .placeholder("note")
                .value(text)
                .on_change(move |s: String| {
                    on_event.emit(FormEvent::SetComment {
                        path: path.clone(),
                        text: s,
                    })
                })
                .into()
        } else {
            html! {
                <span class="pve-meta-comment-text pwt-color-neutral-alt">
                    {props.comment.clone().unwrap_or_default()}
                </span>
            }
        }
    } else if let Some(description) = &props.description {
        html! {<span class="pve-meta-comment-text pwt-color-neutral-alt">{description.clone()}</span>}
    } else {
        html! {}
    };

    Row::new()
        .class("pve-meta-value-row")
        .class("pwt-align-items-center")
        .gap(2)
        .padding(1)
        .with_child(html! {
            <span class="pve-meta-row-label">
                {props.label.clone()}
                if props.required {
                    <span class="pve-meta-required" title="required">{" *"}</span>
                }
            </span>
        })
        .with_child(html! {<span class="pve-meta-row-widget">{props.widget.clone()}</span>})
        .with_child(note_area)
        .with_child(
            ActionIcon::new("fa fa-comment-o")
                .aria_label("edit note")
                .on_activate(note_toggle),
        )
        .with_child(
            ActionIcon::new("fa fa-trash")
                .aria_label("delete")
                .on_activate(delete),
        )
        .into()
}

#[derive(Properties, PartialEq, Clone)]
pub(crate) struct ObjectSectionProps {
    pub path: Vec<String>,
    pub value: Value,
    #[prop_or_default]
    pub schema_fields: Option<Rc<Vec<FieldSpec>>>,
    pub label: String,
    #[prop_or_default]
    pub required: bool,
    pub on_event: Callback<FormEvent>,
}

/// A nested object, indented under its parent, with its own collapse state and
/// "+ add key" control.
#[function_component(ObjectSection)]
pub(crate) fn object_section(props: &ObjectSectionProps) -> Html {
    let on_event = props.on_event.clone();
    let open = use_state(|| true);

    let toggle = {
        let open = open.clone();
        Callback::from(move |_: MouseEvent| open.set(!*open))
    };

    let header = html! {
        <span class="pve-meta-section-title" onclick={toggle}>
            <i class={classes!("fa", if *open { "fa-caret-down" } else { "fa-caret-right" })} />
            {" "}
            {props.label.clone()}
            if props.required {
                <span class="pve-meta-required" title="required">{" *"}</span>
            }
        </span>
    };

    let mut col = Column::new().class("pve-meta-section").gap(1);
    col.add_child(header);

    if *open {
        let schema_fields_ref = props.schema_fields.as_deref().map(|v| v.as_slice());
        let mut body = Column::new()
            .class("pve-meta-section-body")
            .padding(1)
            .gap(1)
            .children(render_object_entries(
                &props.path,
                &props.value,
                schema_fields_ref,
                &on_event,
            ));
        body.add_child(html! {
            <AddKeyDialog
                path={props.path.clone()}
                existing_keys={object_keys(&props.value)}
                schema_fields={props.schema_fields.clone()}
                on_event={on_event.clone()}
            />
        });
        col.add_child(body);
    }

    col.into()
}
