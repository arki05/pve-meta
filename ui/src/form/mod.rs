//! The Form view: one collapsible [`pwt::widget::Panel`] per top-level namespace,
//! recursively rendering the JSON subtree inside. Edits never touch the server; they
//! bubble up as [`FormEvent`]s that the caller (`crate::editor::Editor`) applies to its
//! local working copy and turns into a merge patch (`crate::patch::make_patch`).

mod add_key;
mod section;

pub use add_key::AddKeyDialog;
pub use section::render_object_entries;

use std::collections::HashMap;
use std::rc::Rc;

use serde_json::Value;
use yew::prelude::*;

use pwt::prelude::*;
use pwt::widget::{ActionIcon, Column, Panel};

use crate::api::Operator;
use crate::model::is_comment_key;
use crate::schema::{parse_object_schema, FieldSpec};

/// A user edit to the working copy, bubbled up from anywhere in the recursive form.
#[derive(Debug, Clone, PartialEq)]
pub enum FormEvent {
    /// Set a scalar/array leaf at `path` to `value`.
    SetValue { path: Vec<String>, value: Value },
    /// Set the comment text for the key at `path` (stored under `<lastkey>__`).
    SetComment { path: Vec<String>, text: String },
    /// Delete the key at `path` entirely.
    DeleteKey { path: Vec<String> },
    /// Add a new key `key` under object `path` with an initial `value`.
    AddKey {
        path: Vec<String>,
        key: String,
        value: Value,
    },
    /// Remove a whole top-level namespace.
    RemoveNamespace { namespace: String },
    /// Add a new, empty top-level namespace.
    AddNamespace { namespace: String },
}

#[derive(Properties, PartialEq, Clone)]
pub struct FormRootProps {
    /// The current working copy (comments included).
    pub working: Rc<Value>,
    /// `GET /meta/schemas/{vmid}` result, keyed by namespace. Empty for the datacenter
    /// document (no schema endpoint for it).
    pub schemas: Rc<HashMap<String, Value>>,
    /// `GET /meta/registry` result, used to label each namespace's owner.
    pub registry: Rc<Vec<Operator>>,
    pub on_event: Callback<FormEvent>,
}

/// Renders every top-level namespace as a collapsible panel, plus an "add namespace"
/// control.
#[function_component(FormRoot)]
pub fn form_root(props: &FormRootProps) -> Html {
    let obj = props.working.as_object();
    let namespaces: Vec<&String> = obj
        .map(|m| m.keys().filter(|k| !is_comment_key(k)).collect())
        .unwrap_or_default();

    let mut col = Column::new().class("pve-meta-form-root").gap(2).padding(2);

    if namespaces.is_empty() {
        col.add_child(html! {
            <p class="pwt-color-neutral-alt">
                {"This document has no namespaces yet. Use \"add namespace\" below to create one."}
            </p>
        });
    }

    for namespace in namespaces {
        let value = obj.and_then(|m| m.get(namespace)).cloned().unwrap_or(Value::Null);
        let schema_fields = props
            .schemas
            .get(namespace)
            .map(|s| Rc::new(parse_object_schema(s)));
        let owner_label = crate::api::find_owner(&props.registry, namespace)
            .map(|(op, scope)| format!("{} · {}", op.name, scope))
            .unwrap_or_else(|| "unclaimed".to_string());

        col.add_child(html! {
            <NamespacePanel
                key={namespace.clone()}
                namespace={namespace.clone()}
                value={value}
                schema_fields={schema_fields}
                owner_label={owner_label}
                on_event={props.on_event.clone()}
            />
        });
    }

    col.add_child(html! {
        <AddNamespaceButton on_event={props.on_event.clone()} />
    });

    col.into()
}

#[derive(Properties, PartialEq, Clone)]
struct NamespacePanelProps {
    namespace: String,
    value: Value,
    schema_fields: Option<Rc<Vec<FieldSpec>>>,
    owner_label: String,
    on_event: Callback<FormEvent>,
}

#[function_component(NamespacePanel)]
fn namespace_panel(props: &NamespacePanelProps) -> Html {
    let open = use_state(|| true);

    let toggle = {
        let open = open.clone();
        Callback::from(move |_: MouseEvent| open.set(!*open))
    };

    let remove = {
        let on_event = props.on_event.clone();
        let namespace = props.namespace.clone();
        Callback::from(move |_: web_sys::Event| {
            on_event.emit(FormEvent::RemoveNamespace {
                namespace: namespace.clone(),
            });
        })
    };

    let title = html! {
        <span class="pve-meta-panel-title" onclick={toggle}>
            <i class={classes!("fa", if *open { "fa-caret-down" } else { "fa-caret-right" })} />
            {" "}
            {props.namespace.clone()}
        </span>
    };

    let mut panel = Panel::new()
        .class("pve-meta-namespace-panel")
        .title(title)
        .with_tool(html! {<span class="pwt-color-neutral-alt pve-meta-owner-label">{props.owner_label.clone()}</span>})
        .with_tool(
            ActionIcon::new("fa fa-trash")
                .aria_label("remove namespace")
                .on_activate(remove),
        );

    if *open {
        let path = vec![props.namespace.clone()];
        let schema_fields_ref = props.schema_fields.as_deref().map(|v| v.as_slice());
        panel.add_child(
            Column::new()
                .class("pve-meta-namespace-body")
                .padding(2)
                .gap(1)
                .children(render_object_entries(
                    &path,
                    &props.value,
                    schema_fields_ref,
                    &props.on_event,
                ))
                .with_child(html! {
                    <AddKeyDialog
                        path={path.clone()}
                        existing_keys={object_keys(&props.value)}
                        schema_fields={props.schema_fields.clone()}
                        on_event={props.on_event.clone()}
                    />
                }),
        );
    }

    panel.into()
}

/// Keys already present directly under `value` (used to keep "+ add key" from offering
/// duplicates).
pub(crate) fn object_keys(value: &Value) -> Vec<String> {
    value
        .as_object()
        .map(|m| m.keys().filter(|k| !is_comment_key(k)).cloned().collect())
        .unwrap_or_default()
}

#[derive(Properties, PartialEq, Clone)]
struct AddNamespaceButtonProps {
    on_event: Callback<FormEvent>,
}

#[function_component(AddNamespaceButton)]
fn add_namespace_button(props: &AddNamespaceButtonProps) -> Html {
    let show = use_state(|| false);
    let name = use_state(String::new);

    if !*show {
        let show = show.clone();
        return pwt::widget::Button::new("+ add namespace")
            .onclick(move |_| show.set(true))
            .into();
    }

    let oninput = {
        let name = name.clone();
        Callback::from(move |value: String| name.set(value))
    };

    let can_add = !name.trim().is_empty();
    let add = {
        let on_event = props.on_event.clone();
        let name = name.clone();
        let show = show.clone();
        Callback::from(move |_| {
            let namespace = name.trim().to_string();
            if namespace.is_empty() {
                return;
            }
            on_event.emit(FormEvent::AddNamespace { namespace });
            name.set(String::new());
            show.set(false);
        })
    };
    let cancel = {
        let show = show.clone();
        let name = name.clone();
        Callback::from(move |_| {
            show.set(false);
            name.set(String::new());
        })
    };

    pwt::widget::Row::new()
        .gap(2)
        .class("pwt-align-items-center")
        .with_child(
            pwt::widget::form::Field::new()
                .placeholder("namespace name")
                .value((*name).clone())
                .on_change(oninput),
        )
        .with_child(
            pwt::widget::Button::new("Add")
                .disabled(!can_add)
                .onclick(add),
        )
        .with_child(pwt::widget::Button::new("Cancel").onclick(cancel))
        .into()
}
