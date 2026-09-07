//! Left column: "Datacenter" + every guest from `GET /meta/inventory`.

use std::rc::Rc;

use yew::prelude::*;

use pwt::prelude::*;
use pwt::widget::{Column, Row};

use crate::api::InventoryEntry;
use crate::model::DocId;

#[derive(Properties, PartialEq, Clone)]
pub struct GuestListProps {
    pub entries: Rc<Vec<InventoryEntry>>,
    pub selected: Option<DocId>,
    pub on_select: Callback<DocId>,
}

#[function_component(GuestList)]
pub fn guest_list(props: &GuestListProps) -> Html {
    let mut col = Column::new().class("pve-meta-guest-list");

    col.add_child(guest_row(
        html! {<span class="pwt-font-weight-bold">{"Datacenter"}</span>},
        None,
        props.selected == Some(DocId::Datacenter),
        {
            let on_select = props.on_select.clone();
            Callback::from(move |_| on_select.emit(DocId::Datacenter))
        },
    ));

    for entry in props.entries.iter() {
        let doc_id = DocId::Guest(entry.vmid);
        let is_selected = props.selected == Some(doc_id);
        let onclick = {
            let on_select = props.on_select.clone();
            Callback::from(move |_| on_select.emit(doc_id))
        };
        let name = entry.name.clone().unwrap_or_default();
        let label = html! {
            <>
                <span class="pwt-font-weight-bold">{entry.vmid.to_string()}</span>
                {" "}{name}
                <span class="pve-meta-guest-meta">
                    {format!(" ({}, {})", entry.guest_type, entry.node)}
                </span>
            </>
        };
        col.add_child(guest_row(
            label,
            Some(entry.has_meta),
            is_selected,
            onclick,
        ));
    }

    col.into()
}

fn guest_row(label: Html, has_meta: Option<bool>, selected: bool, onclick: Callback<MouseEvent>) -> Html {
    let marker = html! {
        <span class={classes!("pve-meta-guest-marker", has_meta.unwrap_or(false).then_some("pve-meta-guest-marker-active"))}>
            {"\u{25CF}"}
        </span>
    };
    Row::new()
        .class("pve-meta-guest-row")
        .class(selected.then_some("pve-meta-guest-row-selected"))
        .padding(1)
        .gap(2)
        .onclick(onclick)
        .with_child(marker)
        .with_child(html! {<span class="pve-meta-guest-label">{label}</span>})
        .into()
}
