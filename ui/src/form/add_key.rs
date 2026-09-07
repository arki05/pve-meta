//! The "+ add key" dialog: asks for a key name and a type (or picks one of the
//! schema's still-missing properties, which also fills in its default value).

use std::rc::Rc;

use serde_json::{json, Value};
use yew::prelude::*;

use pwt::prelude::*;
use pwt::widget::form::{Combobox, Field};
use pwt::widget::{Button, Dialog, Row};

use crate::schema::{FieldKind, FieldSpec};

use super::FormEvent;

#[derive(Clone, Copy, PartialEq, Eq)]
enum NewKind {
    String,
    Number,
    Boolean,
    Object,
    Array,
}

impl NewKind {
    const ALL: [NewKind; 5] = [
        NewKind::String,
        NewKind::Number,
        NewKind::Boolean,
        NewKind::Object,
        NewKind::Array,
    ];

    fn label(self) -> &'static str {
        match self {
            NewKind::String => "string",
            NewKind::Number => "number",
            NewKind::Boolean => "boolean",
            NewKind::Object => "object",
            NewKind::Array => "array",
        }
    }

    fn from_label(s: &str) -> Self {
        NewKind::ALL
            .into_iter()
            .find(|k| k.label() == s)
            .unwrap_or(NewKind::String)
    }

    fn from_field_kind(kind: &FieldKind) -> Self {
        match kind {
            FieldKind::String | FieldKind::Enum(_) => NewKind::String,
            FieldKind::Number { .. } => NewKind::Number,
            FieldKind::Integer { .. } => NewKind::Number,
            FieldKind::Boolean => NewKind::Boolean,
            FieldKind::Object(_) => NewKind::Object,
            FieldKind::Array { .. } => NewKind::Array,
            FieldKind::Unknown => NewKind::String,
        }
    }

    fn default_value(self) -> Value {
        match self {
            NewKind::String => json!(""),
            NewKind::Number => json!(0),
            NewKind::Boolean => json!(false),
            NewKind::Object => json!({}),
            NewKind::Array => json!([]),
        }
    }
}

#[derive(Properties, PartialEq, Clone)]
pub struct AddKeyDialogProps {
    pub path: Vec<String>,
    #[prop_or_default]
    pub existing_keys: Vec<String>,
    #[prop_or_default]
    pub schema_fields: Option<Rc<Vec<FieldSpec>>>,
    pub on_event: Callback<FormEvent>,
}

#[function_component(AddKeyDialog)]
pub fn add_key_dialog(props: &AddKeyDialogProps) -> Html {
    let show = use_state(|| false);
    let key_name = use_state(String::new);
    let kind = use_state(|| NewKind::String);

    if !*show {
        let show = show.clone();
        return Button::new("+ add key")
            .onclick(move |_| show.set(true))
            .into();
    }

    let missing: Vec<&FieldSpec> = props
        .schema_fields
        .as_deref()
        .map(|fields| {
            fields
                .iter()
                .filter(|f| !props.existing_keys.contains(&f.key))
                .collect()
        })
        .unwrap_or_default();

    let reset_and_close = {
        let show = show.clone();
        let key_name = key_name.clone();
        let kind = kind.clone();
        Callback::from(move |_: ()| {
            show.set(false);
            key_name.set(String::new());
            kind.set(NewKind::String);
        })
    };

    let mut dialog = Dialog::new("Add key").on_close({
        let reset_and_close = reset_and_close.clone();
        move |_| reset_and_close.emit(())
    });

    if !missing.is_empty() {
        let items: Rc<Vec<AttrValue>> = Rc::new(
            missing
                .iter()
                .map(|f| AttrValue::from(f.key.clone()))
                .collect(),
        );
        let missing_owned: Vec<FieldSpec> = missing.iter().map(|f| (*f).clone()).collect();
        let key_name_setter = key_name.clone();
        let kind_setter = kind.clone();
        dialog.add_child(
            Row::new()
                .gap(2)
                .class("pwt-align-items-center")
                .with_child(html! {<span class="pwt-color-neutral-alt">{"from schema:"}</span>})
                .with_child(Combobox::new().items(items).on_change(move |s: String| {
                    if let Some(f) = missing_owned.iter().find(|f| f.key == s) {
                        key_name_setter.set(f.key.clone());
                        kind_setter.set(NewKind::from_field_kind(&f.kind));
                    }
                })),
        );
    }

    let key_input = {
        let key_name_setter = key_name.clone();
        Field::new()
            .placeholder("key name")
            .value((*key_name).clone())
            .on_change(move |s: String| key_name_setter.set(s))
    };

    let kind_items: Rc<Vec<AttrValue>> = Rc::new(
        NewKind::ALL
            .iter()
            .map(|k| AttrValue::from(k.label()))
            .collect(),
    );
    let kind_select = {
        let kind_setter = kind.clone();
        Combobox::new()
            .items(kind_items)
            .value((*kind).label())
            .on_change(move |s: String| kind_setter.set(NewKind::from_label(&s)))
    };

    dialog.add_child(
        Row::new()
            .gap(2)
            .padding(2)
            .with_child(key_input)
            .with_child(kind_select),
    );

    let can_add = !key_name.trim().is_empty() && !props.existing_keys.contains(&*key_name);
    if !can_add && !key_name.is_empty() {
        dialog.add_child(html! {
            <p class="pwt-color-error">{"a key with that name already exists"}</p>
        });
    }

    let add = {
        let on_event = props.on_event.clone();
        let path = props.path.clone();
        let key_name = key_name.clone();
        let kind = *kind;
        let reset_and_close = reset_and_close.clone();
        Callback::from(move |_: MouseEvent| {
            let key = key_name.trim().to_string();
            if key.is_empty() {
                return;
            }
            on_event.emit(FormEvent::AddKey {
                path: path.clone(),
                key,
                value: kind.default_value(),
            });
            reset_and_close.emit(());
        })
    };
    let cancel = {
        let reset_and_close = reset_and_close.clone();
        Callback::from(move |_: MouseEvent| reset_and_close.emit(()))
    };

    dialog.add_child(
        Row::new()
            .gap(2)
            .padding(2)
            .with_flex_spacer()
            .with_child(Button::new("Cancel").onclick(cancel))
            .with_child(Button::new("Add").disabled(!can_add).onclick(add)),
    );

    dialog.into()
}
