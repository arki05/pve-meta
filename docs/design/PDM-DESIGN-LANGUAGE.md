# PDM Design Language

How Proxmox composes pages in its Rust/Yew UIs (Proxmox Datacenter Manager), and how the
pve-meta editor page must be rebuilt to match.

**Method.** Source-only study, no running instance. Citations are `path:line` against these
checkouts, pinned at the revisions already in `ui/Cargo.toml`'s `[patch.crates-io]`:

| Short name | Path |
|---|---|
| `PDM` | `<scratch>/upstream/proxmox-datacenter-manager/ui` |
| `COMP` | `.../scratchpad/upstream/proxmox-yew-comp` (rev `2384eea`) |
| `PWT` | `.../scratchpad/upstream/proxmox-yew-widget-toolkit` (rev `3ce6f16`) |
| `SCSS` | `/Users/arki/Documents/proxmox/pve-meta/ui/pwt-assets/scss` (vendored, byte-identical to upstream `pwt-assets/scss`) |

Companion document: `docs/design/PROXMOX-CONVENTIONS.md` (how Proxmox *writes* code — Rust
style, Perl API modules, ExtJS/JS, Makefile/debian).

---

## 0. The one-paragraph summary

A PDM page is **never** hand-laid-out. It is a `Column` (or a `Container.pwt-content-spacer`)
whose children are pwt widgets that bring their own chrome. The *only* CSS classes PDM writes
are pwt utility classes (`pwt-content-spacer`, `pwt-border-bottom`, `pwt-overflow-hidden`,
`pwt-w-100`, `pwt-font-title-medium`) and typed `pwt::css::*` enums (`FlexFit`,
`AlignItems::Center`, `ColorScheme::Primary`). There is no `pdm-page`, no `pdm-toolbar`, no
`pdm-header`. The app-local stylesheet `PDM/css/pdm.scss` is 184 lines and defines exactly
three things: a KVGrid wrap override, guest-type status-badge icons, and tag pills — all built
on `var(--pwt-*)` tokens.

**The previous pve-meta attempt looked wrong because it invented its own layer**
(`pve-meta-header`, `pve-meta-toolbar`, `pve-meta-editor`, `pve-meta-pending-bar`, a
primary-colored header bar) instead of using `LoadableComponent` + `Toolbar` +
`pwt-content-spacer`. See `ui/src/app.rs:220-266` and `ui/src/editor.rs:413-438` for the
current state.

---

## 1. Page anatomy

### 1.1 The app shell

`PDM/src/main.rs:295-352` — the whole application is one `Column.pwt-viewport` holding a
nav bar and a content area, wrapped in `DesktopApp`:

```rust
let mut body: Html = Column::new()
    .class("pwt-viewport")
    .with_child(
        TopNavBar::new(self.running_tasks.clone())
            .username(username.clone())
            .on_logout(ctx.link().callback(|_| Msg::Logout)),
    )
    .with_child({ /* MainMenu or login Dialog */ })
    .with_optional_child(subscription_alert)
    .into();
...
DesktopApp::new(html! { /* ContextProviders */ })
    .catalog_url_builder(RenderFn::new(|lang| format!("locale/catalog-{lang}.mo")))
    .into()
```

`pwt-viewport` (`SCSS/_utilities.scss:184-238`) is `position:absolute; width:100dvw;
height:100dvh; display:flex; overflow:hidden` — it is the *only* place a fixed viewport is
established. Everything below it is flex.

`DesktopApp` (`PWT/src/widget/desktop_app.rs:94-95`) internally wraps the body in a
`ThemeLoader`, which is what injects the theme `<link>` and toggles `pwt-dark-mode` on
`<html>`. **You get theme handling for free by using `DesktopApp`; never write a `<link>`
for a theme yourself.**

### 1.2 The nav bar

`PDM/src/top_nav_bar.rs:258-287` — note it is a plain `Row` with *neutral-alt* background and
a bottom border, **not** a primary-colored bar:

```rust
Row::new()
    .attribute("role", "banner")
    .attribute("aria-label", "Datacenter Manager")
    .class("pwt-bg-color-neutral-alt")
    .class("pwt-justify-content-space-between pwt-align-items-center")
    .class("pwt-border-bottom")
    .padding(2)
    .with_child(html! { <a href="https://www.proxmox.com" target="_blank">
                          <img {src} height="30" alt="Proxmox logo"/></a> })
    .with_child(Container::from_tag("span")
        .class("pwt-font-title-medium").padding_x(4).with_child(text))
    .with_flex_spacer()
    .with_child(SearchBox::new())
    .with_flex_spacer()
    .with_child(button_group)
    .with_optional_child(dialog)
    .into()
```

> **Do not** paint the header `pwt-bg-color-primary` / `pwt-color-on-primary` the way
> `ui/src/app.rs:220-222` currently does. Proxmox never does that. Header = `neutral-alt` +
> `pwt-border-bottom`. Title text = `pwt-font-title-medium`.

### 1.3 The content page wrapper — `pwt-content-spacer`

This is the single most important class and the one the previous attempt missed. Definition,
`SCSS/_content_spacer.scss:25-49`:

```scss
.pwt-content-spacer {
    @include color-scheme-vars("surface");
    color: var(--pwt-color);
    background-color: var(--pwt-color-background);

    padding: var(--pwt-spacer-2);
    gap: var(--pwt-spacer-2);

    display: flex;
    flex-direction: column;

    &:has(> *:only-child) { padding: 0px; }

    & > * {
        @include color-scheme-vars("neutral");
        color: var(--pwt-color);
        background-color: var(--pwt-color-background);
    }

    & > *:not(:only-child) { border: 1px solid var(--pwt-color-border); }
}
```

Read that carefully — it encodes the entire PDM "look":

* The page ground is **surface** (slightly tinted); each child card is **neutral** (plain).
* With **one** child, padding collapses to 0 and the child gets **no border** — a single
  full-bleed table/editor fills the tab edge to edge.
* With **more than one** child, you get 2-spacer padding and gap, and every child grows a
  1px `--pwt-color-border` outline. That is where PDM's "stack of cards" appearance comes from.
  **You never write that border or that background yourself.**

Canonical uses: `PDM/src/main_menu.rs:245-249` (Notes page), `PDM/src/main_menu.rs:399-404`
(remotes page), `PDM/src/certificates.rs:24`, `PDM/src/configuration/mod.rs:151-174`.

Two siblings exist for when you need the pieces separately
(`SCSS/_content_spacer.scss:51-59`): `pwt-content-spacer-padding` (just the padding) and
`pwt-content-spacer-colors` (just the surface colors). PDM uses the latter on the dashboard
status strip, `PDM/src/dashboard/view.rs:570-575`.

### 1.4 Canonical content-page shapes

**(a) Stacked panels (config page)** — `PDM/src/configuration/mod.rs:151-174`, verbatim:

```rust
#[function_component(NetworkTimePanel)]
pub fn create_network_time_panel() -> Html {
    Container::new()
        .class("pwt-content-spacer")
        .class(pwt::css::FlexFit)
        .with_child(Panel::new().title(tr!("Time")).with_child(html! { <TimePanel/> }))
        .with_child(Panel::new().title(tr!("DNS")).with_child(html! { <DnsPanel/> }))
        .with_child(
            Panel::new()
                .min_height(200)
                .class(pwt::css::FlexFit)
                .title(tr!("Network Interfaces"))
                .with_child(NetworkView::new()),
        )
        .into()
}
```

**(b) Single full-bleed child** — `PDM/src/main_menu.rs:245-249`:

```rust
Container::new()
    .class("pwt-content-spacer")
    .class(pwt::css::FlexFit)
    .with_child(notes)
    .into()
```

**(c) Tabbed detail page** — `PDM/src/pve/remote/mod.rs:41-85`, verbatim:

```rust
let title: Html = Row::new()
    .gap(2)
    .class(AlignItems::Baseline)
    .with_child(Fa::new("building"))
    .with_child(tr! {"Remote '{0}'", props.remote})
    .into();

TabPanel::new()
    .router(true)
    .class(pwt::css::FlexFit)
    .title(title)
    .class(ColorScheme::Neutral)
    .with_item_builder(
        TabBarItem::new().key("tasks_view").label(tr!("Tasks")).icon_class("fa fa-list"),
        { let remote = props.remote.clone();
          move |_| RemoteTaskList::new().remote(remote.clone()).into() },
    )
    .with_item_builder(
        TabBarItem::new().key("notes_view").label(tr!("Notes")).icon_class("fa fa-sticky-note-o"),
        { let remote = props.remote.clone();
          move |_| NotesView::edit_property(format!("/pve/remotes/{remote}/options"), "description")
                       .on_submit(None).into() },
    )
    .into()
```

Note the title is an **`Html` `Row` with an `Fa` icon and a `tr!` string**, not a plain string.
`TabPanel` also takes `.tool(...)` for a right-aligned header action
(`PDM/src/pve/lxc/mod.rs:75-96` hangs an "Open Web UI" `Button` + `Tooltip` there).

**(d) Section separators inside a scrolling page** — `PDM/src/renderer.rs:103-111`:

```rust
/// Returns a simple row with a title and an icon that can be used to separate sections.
pub(crate) fn render_title_row(title: String, icon: &str) -> Row {
    Row::new()
        .class(pwt::css::AlignItems::Baseline)
        .class(pwt::css::FontStyle::TitleMedium)
        .gap(2)
        .with_child(Fa::new(icon))
        .with_child(title)
}
```

used as `Column::new().padding(4).gap(2).with_child(render_title_row(tr!("Resources"), "cube"))
.with_child(html!{<hr/>})` — `PDM/src/pve/lxc/mod.rs:122-130`.

**(e) Empty state** — `PDM/src/renderer.rs:114-128`, a centered `Fa::large_3x()` + title +
hint. Used at `PDM/src/dashboard/view.rs:533-537`.

---

## 2. `LoadableComponent` — the load/toolbar/content/error/dialog contract

**This is the skeleton to copy for the pve-meta editor page.** It is not optional decoration:
it is where Proxmox's "reload button spins, errors appear as a strip under the content,
dialogs stack on top, auto-refresh pauses when off-screen" behavior lives.

Trait, `COMP/src/loadable_component.rs:132-200`:

```rust
pub trait LoadableComponent:
    Sized + DerefMut<Target = LoadableComponentState<Self::ViewState>> + 'static
{
    type Properties: Properties;
    type Message: 'static;
    type ViewState: 'static + PartialEq;

    fn create(ctx: &LoadableComponentContext<Self>) -> Self;
    fn load(&self, ctx: &LoadableComponentContext<Self>)
        -> Pin<Box<dyn Future<Output = Result<(), Error>>>>;
    fn update(&mut self, ctx: &LoadableComponentContext<Self>, msg: Self::Message) -> bool { true }
    fn changed(&mut self, ctx: &LoadableComponentContext<Self>, _old: &Self::Properties) -> bool { true }
    fn toolbar(&self, ctx: &LoadableComponentContext<Self>) -> Option<Html> { None }
    fn main_view(&self, ctx: &LoadableComponentContext<Self>) -> Html;
    fn dialog_view(&self, ctx: &LoadableComponentContext<Self>, vs: &Self::ViewState) -> Option<Html> { None }
    fn rendered(&mut self, ctx: &LoadableComponentContext<Self>, first_render: bool) {}
}
```

The master component's `view()`, `COMP/src/loadable_component.rs:543-603` — **this is the page
skeleton, verbatim**:

```rust
fn view(&self, ctx: &Context<Self>) -> Html {
    let main_view = self.state.main_view(ctx);

    let dialog: Option<Html> = match &self.state.view_state {
        ViewState::Main => None,
        ViewState::Dialog(view_state) => self.state.dialog_view(ctx, view_state),
        ViewState::Error(title, msg, reload_on_close) => { /* AlertDialog */ }
        ViewState::TaskProgress(task_id) => { /* TaskProgress */ }
        ViewState::TaskLog(task_id, endtime) => { /* TaskViewer */ }
    };

    let toolbar = self.state.toolbar(ctx);

    let mut alert_msg = None;
    if dialog.is_none() {
        if let Some(msg) = &self.state.last_load_error {
            alert_msg = Some(pwt::widget::error_message(msg).class("pwt-border-top"));
        }
    }

    Column::new()
        .class("pwt-flex-fill pwt-overflow-auto")
        .with_optional_child(toolbar)
        .with_child(main_view)
        .with_optional_child(alert_msg)
        .with_optional_child(dialog)
        .into_html_with_ref(self.state.node_ref.clone())
}
```

So the fixed vertical order of every PDM content page is:

```
toolbar?  →  main_view  →  load-error strip?  →  dialog?
```

Scope helpers (`COMP/src/loadable_component.rs:232-348`): `link.send_reload()`,
`link.send_redraw()`, `link.change_view(Some(ViewState::X))`,
`link.change_view_callback(|_| Some(ViewState::X))`, `link.show_error(title, err, reload)`,
`link.show_task_progress(upid)`, `link.start_task(...)`.

`LoadableComponentState` (`COMP/src/loadable_component.rs:375-412`) models loading as a
**counter, not a bool**, and stores the error as a `String`, not an `anyhow::Error`:

```rust
pub struct LoadableComponentState<V: PartialEq> {
    loading: usize,
    last_load_error: Option<String>,
    // ...
}
impl<V: PartialEq> LoadableComponentState<V> {
    pub fn loading(&self) -> bool { self.loading > 0 }
    pub fn last_load_errors(&self) -> Option<&str> { self.last_load_error.as_deref() }
}
```

Registration glue at the bottom of the file (`COMP/src/user_panel.rs:452-457`,
`PDM/src/remotes/config.rs:425-429`) — needed because `LoadableComponentMaster` is not a
`#[widget]` (a `#[widget(...)]`-annotated struct gets its `Into<Html>` generated by the macro,
e.g. `COMP/src/status_row.rs` has no such impl):

```rust
impl From<RemoteConfigPanel> for VNode {
    fn from(val: RemoteConfigPanel) -> Self {
        let comp = VComp::new::<LoadableComponentMaster<PbsRemoteConfigPanel>>(Rc::new(val), None);
        VNode::from(comp)
    }
}
```

### 2.1 The closest existing analogue to the pve-meta editor: `NotesView`

`COMP/src/notes_view.rs` is a `LoadableComponent` that loads a text document with a digest,
shows a toolbar with one button, renders the document full-height, and edits it in an
`EditWindow` containing a full-height text control. That is *structurally identical* to what
pve-meta needs. Verbatim, `COMP/src/notes_view.rs:150-236`:

```rust
        let link = ctx.link().clone();
        Box::pin(async move {
            let resp = loader.apply().await?;
            let notes = resp.data;
            let digest = resp.attribs.get("digest").cloned();
            link.send_message(Msg::Load(NotesWithDigest { notes, digest }));
            Ok(())
        })
    }
    ...
    fn toolbar(&self, ctx: &LoadableComponentContext<Self>) -> Option<Html> {
        let props = ctx.props();
        props.on_submit.is_some().then_some(
            Toolbar::new()
                .class("pwt-w-100")
                .class("pwt-overflow-hidden")
                .class("pwt-border-bottom")
                .with_child(
                    Button::new(tr!("Edit"))
                        .on_activate(ctx.link().change_view_callback(|_| ViewState::EditNotes)),
                )
                .into(),
        )
    }

    fn main_view(&self, _ctx: &LoadableComponentContext<Self>) -> Html {
        Container::new()
            .padding(2)
            .class("pwt-flex-fit")
            .class("pwt-embedded-html")
            .with_child(Markdown::new().text(self.data.notes.clone()))
            .into()
    }

    fn dialog_view(&self, ctx: &LoadableComponentContext<Self>, view_state: &Self::ViewState)
        -> Option<Html>
    {
        match view_state {
            ViewState::EditNotes => {
                let dialog = EditWindow::new(tr!("Edit") + ": " + &tr!("Notes"))
                    .width(800)
                    .height(400)
                    .on_done(ctx.link().change_view_callback(|_| None))
                    .resizable(true)
                    .loader(self.edit_window_loader.clone())
                    .on_submit({ /* ... form_ctx.read().get_submit_data() ... */ })
                    .renderer(|_form_ctx| {
                        Column::new()
                            .class(pwt::css::FlexFit)
                            .with_child(
                                TextArea::new()
                                    .padding(2)
                                    .name("notes")
                                    .submit_empty(true)
                                    .class(pwt::css::FlexFit),
                            )
                            .into()
                    });
                Some(dialog.into())
            }
        }
    }
```

---

## 3. Toolbars and buttons

### 3.1 Toolbar

`PWT/src/widget/toolbar.rs:39-86`: `Toolbar::new()`, `.with_spacer()` (a small fixed gap,
marking a *group* boundary), `.with_flex_spacer()` (pushes the rest right),
`.scroll_mode(MiniScrollMode::Arrow)`.

Styling, `SCSS/_toolbar.scss:1-18`: `.pwt-toolbar { flex: none; }` with an inner
`.pwt-toolbar-content { display:flex; gap: var(--pwt-spacer-2); padding: var(--pwt-spacer-2);
align-items:center; }`. **The padding and gap are already there — do not add your own.**

The invariant classes PDM puts on every list toolbar (`COMP/src/user_panel.rs:246-249`,
`COMP/src/notes_view.rs:171-174`, `PDM/src/remotes/auto_installer/token_panel.rs:192-195`):

```rust
Toolbar::new()
    .class("pwt-w-100")
    .class("pwt-overflow-hidden")
    .class("pwt-border-bottom")
```

`PDM/src/guests.rs:431` uses the equivalent typed builder `.border_bottom(true)`.

### 3.2 Button ordering convention

Left group = create/mutate, in the order **Add · Edit · Remove**, `with_spacer()` between
logical groups, then `with_flex_spacer()`, then the refresh button on the far right. Verbatim,
`PDM/src/remotes/auto_installer/token_panel.rs:192-225`:

```rust
let toolbar = Toolbar::new()
    .class("pwt-w-100")
    .class(pwt::css::Overflow::Hidden)
    .class("pwt-border-bottom")
    .with_child(Button::new(tr!("Add"))
        .onclick(link.change_view_callback(|_| Some(ViewState::Create))))
    .with_spacer()
    .with_child(Button::new(tr!("Edit"))
        .disabled(self.selection.is_empty())
        .onclick(link.change_view_callback(|_| Some(ViewState::Edit))))
    .with_child(
        ConfirmButton::new(tr!("Remove"))
            .confirm_message(tr!("Are you sure you want to remove this entry?"))
            .disabled(self.selection.is_empty())
            .on_activate(link.callback(|_| Message::RemoveEntry)),
    )
    .with_spacer()
    .with_child(
        ConfirmButton::new(tr!("Regenerate Secret"))
            .confirm_message(tr!("Do you want to regenerate the secret of the selected token? \
                                 All existing ISOs with this token will lose access!"))
            .disabled(self.selection.is_empty())
            .dangerous(true)
            .on_activate(link.callback(|_| Message::RegenerateSecret)),
    )
    .with_flex_spacer()
    .with_child(Button::refresh(self.loading()).onclick({
        let link = ctx.link().clone();
        move |_| link.send_reload()
    }));
```

### 3.3 Button rules

* `Button::new(tr!("Text"))` — the default. **Plain text, no icon, no color class.** PDM's
  toolbar buttons are *unstyled*. Search the whole of `PDM/src` and you find exactly two
  `ColorScheme::Primary` buttons — both on the "no remotes yet" empty state
  (`PDM/src/dashboard/view.rs:541-546`), i.e. the single call to action on an otherwise empty
  page. **Never give an ordinary toolbar button a color scheme.**
* `Button::refresh(loading)` (`PWT/src/widget/button.rs:53-150`) — the standard reload button;
  it renders the spinner itself when `loading` is true. Always last, after
  `with_flex_spacer()`.
* Destructive: `ConfirmButton::new(tr!("Remove")).dangerous(true).confirm_message(...)`
  (`COMP/src/confirm_button.rs:16-105`). There is a shorthand for the common case,
  `ConfirmButton::remove_entry(name)` (`confirm_button.rs:72-80`), producing
  `tr!("Are you sure you want to remove entry {0}", name)`.
  For *very* destructive actions there is a type-to-confirm dialog,
  `SafeConfirmDialog::new(verify_id)` (`COMP/src/safe_confirm_dialog.rs:18-83`).
* `Button::new_icon("fa fa-...")` for icon-only, `ActionIcon::new("fa fa-...")`
  (`PWT/src/widget/action_icon.rs:14-44`) for a bare clickable glyph (used in status rows).
  Icon-only controls always get `.aria_label(...)` and a `Tooltip::new(...).tip(...)` wrapper —
  `PDM/src/dashboard/status_row.rs:159-171`.
* `MenuButton::new(tr!("Add")).show_arrow(true).menu(Menu::new().with_item(MenuItem::new(...)
  .icon_class("fa fa-building").on_select(...)))` when one button opens a choice —
  `PDM/src/remotes/config.rs:186-224`.
* `SegmentedButton` for a mutually-exclusive view toggle, with `.pressed(active)` +
  `.class(active.then_some(ColorScheme::Primary))` on the active segment —
  `COMP/src/markdown_editor.rs:287-329`. **This is the pattern for pve-meta's Form/Source
  toggle**, not two loose `Button`s.

### 3.4 Icon vocabulary (font-awesome 4 names, as `"fa fa-<name>"`)

Actually used in PDM: `tachometer` (dashboard), `server` (remote/node), `building` (PVE),
`floppy-o` (PBS), `desktop` (guests), `cube` (LXC), `list` (tasks), `sticky-note-o` (notes),
`file-text-o` (config), `key` (access), `certificate`, `cogs`/`gears`/`wrench` (config),
`refresh` (+ `fa-spin` while loading), `shield`, `cubes`, `sitemap`, `terminal`, `th-large`,
`plus-square-o`, `language`, `sign-out`, `user`, `book`, `external-link`, `pencil`, `columns`,
`eye`, `check`, `times`, `ban`, `clock-o`, `check-circle`, `exclamation-circle`,
`times-circle`, `exclamation-triangle`, `circle-o-notch`.
Sources: `PDM/src/main_menu.rs:171-409`, `PDM/src/configuration/subscription_registry.rs:57-62`,
`COMP/src/markdown_editor.rs:314-328`.

`Fa` builder — `PWT/src/widget/fa.rs:23-99`: `Fa::new("cube")`, `.spin()`, `.pulse()`,
`.large()`, `.large_3x()`; `.class(FontColor::Warning)` to tint.

---

## 4. Panels, cards and tables

### 4.1 `Panel`

`PWT/src/widget/panel.rs:15-121`. Props are exactly three: `title: Option<Html>`,
`tools: Vec<VNode>`, `header_class: Classes`. There is **no** `.icon()`, no `.toolbar()`, and
crucially **no `pwt-panel-body` class** — the body is just `ContainerBuilder::with_child(...)`.

Header assembly (`PWT/src/widget/panel.rs:75-121`):

```rust
pub(crate) fn create_panel_title(title: Option<Html>, tools: Vec<VNode>) -> Row {
    let mut header = Row::new()
        .attribute("role", "group").attribute("aria-label", "panel header")
        .class("pwt-align-items-center pwt-gap-1");
    if let Some(title) = title {
        header.add_child(html!{<div role="none" class="pwt-panel-header-text">{title}</div>});
    }
    if !tools.is_empty() { header.add_flex_spacer(); header.add_child(VList::with_children(tools, None)); }
    header
}
```

`.pwt-panel-header-text` is styled `headline-small` in the accent color
(`SCSS/_panel.scss:13-49`). To put an icon in a panel title, build the title as an `Html` `Row`
with an `Fa`, exactly like the tab titles in §1.4(c).

To hang a whole toolbar in a panel header: `Panel::new().title(...).with_tool(Toolbar::new()...)`.
`Dialog` and `TabPanel` use the identical `title`/`tools`/`with_tool` convention
(`PWT/src/widget/dialog.rs:26-113`, `PWT/src/widget/tab/tab_panel.rs:50-198`).

Dashboard widgets deliberately turn the border off because `pwt-content-spacer` already draws
one: `widget.border(false).class(css::FlexFit)` — `PDM/src/dashboard/view.rs:189-193`.

### 4.2 `DataTable`

`PWT/src/widget/data_table/data_table.rs:109-260`. Defaults already match Proxmox
(`striped: true`, `show_header: true`). Canonical `main_view`, `COMP/src/user_panel.rs:302-311`:

```rust
DataTable::new(columns(), self.store.clone())
    .class("pwt-flex-fill pwt-overflow-auto")
    .selection(self.selection.clone())
    .striped(true)
    .on_row_dblclick(move |_: &mut _| { link.change_view(Some(ViewState::Edit)); })
    .into()
```

Columns are built once into an `Rc<Vec<DataTableHeader<T>>>` (PDM caches them in a struct
field or a `thread_local!`), `PDM/src/remotes/config.rs:337-359`:

```rust
fn remote_list_columns() -> Rc<Vec<DataTableHeader<Remote>>> {
    Rc::new(vec![
        DataTableColumn::new(tr!("Remote ID"))
            .width("200px")
            .render(|item: &Remote| { html! { &item.id } })
            .sorter(|a: &Remote, b: &Remote| a.id.cmp(&b.id))
            .sort_order(true)
            .into(),
        ...
    ])
}
```

`Store::with_extract_key(|r| Key::from(r.id.as_str()))` + `Selection::new().on_select(...)`
drive it (`COMP/src/user_panel.rs:203-213`).

---

## 5. Dialogs

### 5.1 `EditWindow`

`COMP/src/edit_window.rs:37-179`. The important semantics:

* `EditWindow::new(tr!("Add") + ": " + &tr!("User"))` / `tr!("Edit") + ": " + &tr!("Remote")` —
  **that exact `"<Verb>: <Noun>"` title composition** is the convention
  (`COMP/src/user_panel.rs:404,415`, `PDM/src/remotes/edit_remote.rs:63`,
  `COMP/src/notes_view.rs:200`).
* Edit vs. create mode is **inferred**: `is_edit()` returns `self.edit.unwrap_or(self.loader.is_some())`
  (`edit_window.rs:176-178`), and the submit button label follows from it
  (`edit_window.rs:363-372`). Pass `.loader(...)` and you get an edit dialog.
* `submit_digest: bool` defaults to **true**: the loaded response's `attribs["digest"]` is
  folded into the form and a hidden `digest` field is added, so the PUT is
  optimistic-concurrency-checked server-side (`edit_window.rs:270-280`, `359-361`).
* Errors: a submit failure raises a stacked `AlertDialog` by default; `.inline_error(true)`
  instead shows a tinted strip inside the dialog above the toolbar, auto-cleared on the next
  edit (`edit_window.rs:389-419`, `286-289`).
* `.width(800).height(400).resizable(true)` for a large body; default `draggable(true)`,
  `auto_center(true)`.

Verbatim edit dialog, `PDM/src/remotes/edit_remote.rs:56-88`:

```rust
EditWindow::new(tr!("Edit") + ": " + &tr!("Remote"))
    .width(800)
    .min_height(400)
    .on_done(props.on_done.clone())
    .loader((load_remote, url))
    .renderer({
        let remote_id = props.remote_id.clone();
        move |form_ctx| edit_remote_input_panel(form_ctx, &remote_id)
    })
    .on_submit({
        let url = format!("/remotes/remote/{}", percent_encode_component(&props.remote_id));
        move |form_ctx: FormContext| {
            let url = url.clone();
            async move {
                let data = form_ctx.get_submit_data();
                let data = delete_empty_values(&data, &["web-url"], true);
                proxmox_yew_comp::http_put(&url, Some(data)).await
            }
        }
    })
    .into()
```

Its body is always an `InputPanel` (`PWT/src/widget/input_panel.rs:63-478`), never a
hand-built grid — `PDM/src/remotes/edit_remote.rs:91-132`:

```rust
InputPanel::new()
    .class(FlexFit)
    .padding(4)
    .width("auto")
    .with_field(tr!("Remote ID"), DisplayField::new().value(remote_id.to_string()).key("remote-id"))
    .with_field(tr!("User/Token"), Field::new().name("authid")
        .schema(&pdm_api_types::Authid::API_SCHEMA).required(true))
    .with_field(tr!("Password/Secret"), Field::new().name("token")
        .placeholder(tr!("Unchanged")).input_type(InputType::Password).required(false))
    .with_custom_child(
        Container::new().key("nodes-title").padding_top(4)
            .class("pwt-font-title-medium").with_child(tr!("Endpoints")),
    )
    .with_custom_child(NodeUrlList::new().name("nodes").key("nodes").padding_top(2))
    .into()
```

`InputPanel` methods: `.with_field(label, field)`, `.with_right_field(...)` (two-column),
`.with_large_field(...)` (spans), `.with_advanced_field(...)` (hidden behind the "Advanced"
checkbox, enabled by `EditWindow::advanced_checkbox(true)`), `.with_custom_child(...)` for a
section heading or a bespoke widget. Sub-headings inside a dialog are
`Container::new().padding_top(4).class("pwt-font-title-medium")`.

### 5.2 Plain `Dialog` and message boxes

`Dialog::new(title)` for anything that is not a form —
`PWT/src/widget/dialog.rs:26-113`. Layout inside is your business, but PDM uses
`Container::new().padding(4).class("pwt-gap-2 pwt-d-grid pwt-align-items-baseline")`
(`COMP/src/theme_dialog.rs:38-60`).

Modal error/notice: `AlertDialog::new(msg).title(title).on_close(...)`
(`PWT/src/widget/alert_dialog.rs:15-68`), which is a `MessageBox` with an
`exclamation-triangle` icon. `ConfirmDialog` (`PWT/src/widget/`, used from
`COMP/src/confirm_button.rs:129-140`) is the yes/no.

### 5.3 Wizard

For multi-step creation PDM uses `Wizard` instead of `EditWindow` —
`PDM/src/remotes/add_wizard.rs:87-146`: `Wizard::new(tr!("Add Remote")).width(800)
.tab_bar_style(TabBarStyle::MaterialPrimary).with_page(TabBarItem::new().key("connection")
.label(...), |p: &WizardPageRenderInfo| ...).on_submit(...)`. Not needed for pve-meta.

---

## 6. Status, errors, loading

### 6.1 Inline error — `error_message`

`PWT/src/widget/mod.rs:221-223` and `PWT/src/widget/message_box.rs:80-88`:

```rust
pub fn error_message(text: &str) -> Row { message_box::message(text, "fa-exclamation-triangle") }

pub(crate) fn message(text: impl Into<Html>, icon_class: impl Into<Classes>) -> Row {
    let icon_class = classes!("fa-lg", "fa", "fa-align-center", icon_class);
    Row::new().padding(2).class("pwt-align-items-center")
        .with_child(html! {<span class={"pwt-message-sign"} role="none"><i class={icon_class}/></span>})
        .with_child(html! {<p style={"overflow-wrap: anywhere;"}>{text.into()}</p>})
}
```

Usage is always the same shape — `PDM/src/dashboard/view.rs:618`,
`PDM/src/pve/remote_overview.rs:177`, `COMP/src/layout/mod.rs:7-21`:

```rust
view.add_optional_child(self.template.error.as_ref().map(|e| error_message(&e.to_string())));
```

`LoadableComponent` adds `.class("pwt-border-top")` when placing it under the content
(`COMP/src/loadable_component.rs:592`).

**There is no `Alert`, `Toast`, `Snackbar` or `Notification` widget in the PDM stack.**
Transient errors go through `link.show_error(title, err, reload_on_close)`, which opens an
`AlertDialog` (`COMP/src/loadable_component.rs:308-316`, used at
`PDM/src/remotes/config.rs:171`, `COMP/src/user_panel.rs:225`).

The escalation rule is worth copying: the **first** load failure is a modal
`AlertDialog`; **repeated** failures (auto-refresh) demote to the inline strip so the page
stays usable — `COMP/src/loadable_component.rs:482-489`, `588-594`.

### 6.2 Banner (persistent, non-modal notice)

The only banner idiom in PDM, `PDM/src/remotes/auto_installer/prepared_answer_form.rs:1099-1108`:

```rust
Container::new()
    .padding(4)
    .class(FlexFit)
    .class(ColorScheme::WarningContainer)
    .class("pwt-default-colors")
    .with_child(tr!("Please record the configuration token or ISO preparation command line \
                     - it will only be displayed once."))
```

`ColorScheme::WarningContainer` sets `--pwt-color-background`/`--pwt-color` to the warning
container pair; `pwt-default-colors` makes descendants inherit them. **This — not a hand-rolled
`pve-meta-pending-bar` — is how a "changed on server" banner should be built.**

### 6.3 Status icons

`PDM/src/configuration/subscription_registry.rs:57-62` — the canonical mapping:

```rust
S::Active    => Fa::new("check-circle").class(FontColor::Success),
S::New       => Fa::new("clock-o").class(FontColor::Primary),
S::NotFound  => Fa::new("exclamation-circle").class(FontColor::Error),
S::Invalid   => Fa::new("times-circle").class(FontColor::Warning),
S::Expired   => Fa::new("clock-o").class(FontColor::Warning),
S::Suspended => Fa::new("ban").class(FontColor::Error),
```

`COMP/src/status.rs:12-52` wraps this as a `Status::{Success,Warning,Error,Unknown}` enum with
`Into<Fa>`/`Into<Classes>`; `COMP/src/status_row.rs:8-21` is a titled row with optional icon and
trailing status.

### 6.4 Loading

* `Progress::new()` (`PWT/src/widget/progress.rs:16-42`) — indeterminate bar when given no
  value. Used as a whole-page placeholder: `return Progress::new().into();`
  (`PDM/src/dashboard/view.rs:525`), or `.with_optional_child(loading.then_some(Progress::new()))`.
* `Mask::new(content).visible(loading)` (`PWT/src/widget/mask.rs:10-35`) — overlays existing
  content with a "Loading..." mask instead of replacing it. Used on the login panel,
  `PDM/src/main.rs:325`.
* `Button::refresh(loading)` for the toolbar; `"fa fa-refresh fa-spin"` for a menu/nav entry
  (`PDM/src/main_menu.rs:397`).
* The tri-state helper for ad-hoc `Option<Result<T,E>>` data — `COMP/src/layout/mod.rs:7-21`:

```rust
pub fn render_loaded_data<T, E: Display, F: Fn(&T) -> Html>(
    data: &Option<Result<T, E>>, renderer: F,
) -> Html {
    match data {
        None => pwt::widget::Progress::new().class("pwt-delay-visibility").into(),
        Some(Err(err)) => pwt::widget::error_message(&err.to_string()).padding(2).into(),
        Some(Ok(data)) => renderer(data),
    }
}
```

### 6.5 "Changed on server" / stale-data handling

**PDM has no "the record changed underneath you, reload?" dialog.** Its entire concurrency
story is the ExtJS-inherited `digest`:

* load captures `resp.attribs["digest"]` (`COMP/src/notes_view.rs:154`);
* the write sends it back (`COMP/src/notes_view.rs:42-53`);
* the server rejects a stale digest, and the resulting error surfaces through the normal
  `AlertDialog`/`error_message` path.

`EditWindow` automates this via `submit_digest` (§5.1). `http_post_full`
(`COMP/src/http_helpers.rs:288-307`) exists specifically so a caller can read the new digest
after a mutation.

The nearest thing to a freshness indicator is `DashboardStatusRow`
(`PDM/src/dashboard/status_row.rs:149-220`): a `Row` with a refresh `ActionIcon` (spinning
while loading, `disabled` while loading, wrapped in a `Tooltip::tip(tr!("Refresh now"))`) and a
`tr!("Last refresh: {0}", date)` label, then `with_flex_spacer()` and trailing actions.

---

## 7. Tokens, classes, typography

### 7.1 Spacing

`SCSS/_theme_post.scss:2-11`:

```scss
:root {
    --pwt-spacer-base-width: #{$spacer};
    --pwt-spacer-0: 0;
    --pwt-spacer-1: calc(var(--pwt-spacer-base-width) * 1);
    --pwt-spacer-2: calc(var(--pwt-spacer-base-width) * 2);
    --pwt-spacer-3: calc(var(--pwt-spacer-base-width) * 3);
    --pwt-spacer-4: calc(var(--pwt-spacer-base-width) * 4);
    --pwt-icon-spacer: #{$icon_spacer};
}
```

`$spacer` is **3px in Crisp** (`SCSS/crisp/_theme_config_crisp.scss:9`) and **5px in Desktop**
(`SCSS/desktop/_theme_config_desktop.scss:9`). Density classes `pwt-density-high/medium/touch`
override `--pwt-spacer-base-width` (`SCSS/_theme_post.scss:13-23`).

**The scale is 0–4 and nothing else** (`SCSS/_utilities.scss:9-100`). `.padding(N)`/`.gap(N)`
with `N <= 4` emit `pwt-p-N`/`pwt-gap-N`; anything else silently becomes an inline style
(`PWT/src/props/pwt_space.rs:60-79`) — which is how you accidentally leave the design system.

Observed PDM usage: `padding(2)` for toolbars/nav/compact rows, `padding(4)` for dialog bodies
and section columns, `gap(1)` for icon+text, `gap(2)` for everything else.

### 7.2 Typography

Generated by `SCSS/mixins/_fonts.scss:24-38` — 5 roles × 3 sizes:
`pwt-font-{display|headline|title|label|body}-{large|medium|small}`, plus `pwt-font-monospace`
(`_fonts.scss:20-22`, family `monospace` per `_theme_common.scss:5`).

Rust mirror: `css::FontStyle::TitleMedium` etc. (`PWT/src/css.rs:771-813`).

Desktop sizes (`SCSS/_font_sizes.scss`): `title-medium` 16px/24px, `title-small` 14px/20px,
`body-medium` 14px/20px, `body-small` 12px/16px, `label-small` 11px/16px, `headline-small`
20px/24px. Crisp swaps in `_font_sizes_small.scss` (tighter).

What PDM actually uses: `pwt-font-title-medium` for section/nav-bar headings (4 sites),
`pwt-font-headline-medium` (3), `pwt-font-label-small` (1). **Nothing else.** Body text is
unclassed and inherits.

### 7.3 Colors

Material-Design-3 tonal palettes; role variables are defined only in
`SCSS/_theme_scheme_light.scss:2-80` and `SCSS/_theme_scheme_dark.scss`. The two variables you
actually read are set by the `color-scheme-vars` mixin
(`SCSS/mixins/_color_scheme.scss:3-6`):

```scss
@mixin color-scheme-vars($name, $important: false) {
    --pwt-color-background: var(--pwt-color-#{$name});
    --pwt-color:            var(--pwt-color-on-#{$name});
}
```

so **`--pwt-color-background` and `--pwt-color` are always "the current surface pair"**, and
`.pwt-scheme-<name>` (from `css::ColorScheme`) reassigns them for a subtree.

Other stable role tokens (`SCSS/_theme_scheme_light.scss:67-80`): `--pwt-color-border`,
`--pwt-high-contrast-border`, `--pwt-color-focus`, `--pwt-color-shadow`,
`--pwt-color-surface-tint`, `--pwt-backdrop-color`; plus the semantic pairs
`--pwt-color-{primary,secondary,tertiary,success,error,warning}` and their `-container` /
`on-` variants.

Corner radii (`SCSS/_theme_post.scss:26-41`): `--pwt-button-corner-shape`,
`--pwt-input-corner-shape`, `--pwt-dialog-corner-shape`, `--pwt-card-corner-shape`,
`--pwt-shape-corner-{none,extra-small,…,full}`.

Elevation (`SCSS/_theme_post.scss:44-58`): `--pwt-elevation-level-0..5`,
`--pwt-state-layer-opacity-{hover,focus,press,drag,…}`, `--pwt-disabled-opacity` (0.38),
`--pwt-dimmed-opacity` (0.5).

### 7.4 `pwt::css` enums (Rust ⇄ class)

`PWT/src/css.rs`. Frequency of use across `PDM/src` (the honest picture of what matters):

| Rank | Item | count |
|---|---|---|
| 1 | `FlexFit` | 243 |
| 2 | `AlignItems::Center` | 58 |
| 3 | `AlignItems::Baseline` | 46 |
| 4 | `JustifyContent::Center` | 24 |
| 5 | `FontColor::Warning` | 19 |
| 6 | `ColorScheme::Neutral` | 17 |
| 7 | `FontColor::Error` / `ColorScheme::Primary` | 15 |
| 8 | `Flex::Fill` | 14 |
| 9 | `JustifyContent::FlexEnd` | 13 |
| 10 | `Overflow::Auto` | 12 |

Raw string classes across `PDM/src`: `pwt-content-spacer` (24), `pwt-border-bottom` (17),
`pwt-overflow-hidden` (14), `pwt-loading-icon` (12), `pwt-w-100` (8),
`pwt-flex-direction-row` (6), `pwt-font-title-medium` (4), `pwt-default-colors` (4),
`pwt-content-spacer-colors` (4), `pwt-color-warning` (4), `pwt-font-headline-medium` (3).

`FlexFit` (`PWT/src/css.rs:650-655`) = `flex: 1 1 auto; overflow: auto` — the workhorse. Put it
on anything that should consume the remaining height of its flex parent.

### 7.5 What to avoid

* **No bespoke class prefix.** Delete `pve-meta-header`, `pve-meta-toolbar`, `pve-meta-editor`,
  `pve-meta-content`, `pve-meta-pending-bar`, `pve-meta-error-banner`, `pve-meta-dialog-message`,
  `pve-meta-placeholder`, `pve-meta-loading`. If a rule is truly needed (Monaco host sizing),
  put it in a small app SCSS built on `var(--pwt-*)`, the way `PDM/css/pdm.scss` does.
* **No raw px/rem for padding, gap or margin.** Use `.padding(N)`/`.gap(N)` with `N ∈ 0..=4`,
  or `var(--pwt-spacer-N)` in SCSS.
* **No hard-coded colors.** Use `ColorScheme::*` / `FontColor::*` / `var(--pwt-color-*)`.
* **No `pwt-bg-color-primary` header.** See §1.2.
* **No colored toolbar buttons.** See §3.3.
* Do not invent `pwt-panel-body` — it doesn't exist (§4.1).
* Do not re-implement toolbar padding/gap or content-spacer borders — they're in the CSS.

---

## 8. Theme handling

Three orthogonal settings, all in `localStorage`, all read/written by
`PWT/src/state/theme.rs:137-274`:

| Key | Values | Effect |
|---|---|---|
| `ThemeName` | `Desktop`, `Crisp`, `Material`, `Mobile`, `Solarized` | which `<name>-yew-style.css` is loaded |
| `ThemeMode` | `auto`, `light`, `dark` | `pwt-dark-mode` / `pwt-light-mode` class on `<html>` |
| `ThemeDensity` | — | `pwt-density-*` class on `<html>` |

Storing dispatches a custom `pwt-theme-changed` DOM event (`theme.rs:157-164`).
`ThemeObserver` (`theme.rs:289-330`) listens to that plus a live
`matchMedia("(prefers-color-scheme: dark)")` listener, and re-emits `(Theme, use_dark_mode)`.

`ThemeLoader` (`PWT/src/widget/theme_loader.rs`) does the DOM work: injects the stylesheet
`<link>` (`theme_loader.rs:219-232`), sets the mode class (`set_dark_mode_on_document_root`,
`:73-88`) and the density class (`:108-134`). Filename pattern
`format!("{}-yew-style.css", theme.name.to_lowercase())` (`theme_loader.rs:139`).
`DesktopApp::new(body)` wires it up automatically (`PWT/src/widget/desktop_app.rs:94-95`).

**Light/dark is a class on `<html>`, not `prefers-color-scheme` and not a `data-` attribute** —
`SCSS/_theme_scheme_light.scss:2` is `:root.pwt-light-mode, :root:not(.pwt-dark-mode)` and
`SCSS/_theme_scheme_dark.scss:2` is `:root.pwt-dark-mode`. Light is the fallback; dark is
opt-in. This matters for Monaco (§10).

`index.html` carries no theme link and no body class — only a FOUC-avoidance
`prefers-color-scheme` background rule. `PDM/index.html` and
`/Users/arki/Documents/proxmox/pve-meta/ui/index.html` already agree on this.

pve-meta already builds both `desktop-yew-style.css` and `crisp-yew-style.css`
(`ui/Trunk.toml` hooks) — good; keep both.

---

## 9. What PDM does *not* have (be honest about this)

* **No code editor.** No Monaco, no CodeMirror, no Ace anywhere in `PDM/src`, `COMP/src` or
  `PWT/src` (`grep -rn "monaco\|CodeMirror\|ace/" ` → 0 hits; the only "Monaco" string in the
  tree is the timezone `Europe/Monaco` in `COMP/src/time_zone_selector.rs:407`).
* **No syntax highlighting** of any kind.
* **No diff view.** `similar`/`diff` crates are absent.
* **No in-place third-party JS widget.** The one place PDM needs a JS library — xterm.js — it
  does **not** embed. `COMP/src/xtermjs.rs:148-152` is the entire component:

  ```rust
  fn view(&self, ctx: &Context<Self>) -> Html {
      let props = ctx.props();
      let url = xtermjs_url(&props.console_type, &props.node_name, props.vnc);
      html! {<iframe class="pwt-flex-fit" src={format!("/{url}")}/>}
  }
  ```

  i.e. an `<iframe>` to the legacy ExtJS-served console page. There are **no `.js` files and no
  `#[wasm_bindgen(module = ...)]` imports anywhere in `PDM/ui`**.
* **No toast/snackbar/notification** widget (§6.1).
* **No "reload, the server changed" dialog** — only digests (§6.5).
* The richest text editing in the whole stack is `COMP/src/markdown_editor.rs`: a plain
  `<textarea class="pwt-textarea">` driven as a `ManagedField`, with a debounced
  `pulldown-cmark` preview and a `SegmentedButton` Write/Split/Preview switcher.

So for pve-meta's Monaco requirement **there is no precedent to copy**. Everything in §10 is
new ground, and the design guidance is: keep the *frame* 100% PDM, keep the *editor* a
self-contained, clearly-bounded exception.

---

## 10. How the pve-meta editor page should be structured

### 10.1 Requirements → PDM pattern

| pve-meta need | PDM pattern | Cite |
|---|---|---|
| Guest/datacenter identity line | `TabPanel.title(Row + Fa + tr!("CT {0}", …))`, or `render_title_row()` at the top of the content column | `PDM/src/pve/lxc/mod.rs:64-74`, `PDM/src/renderer.rs:103-111` |
| Toolbar: View-as / Reload / Apply / Discard | `Toolbar` with the standard three classes; `SegmentedButton` for View-as; `Button::refresh(loading)` after `with_flex_spacer()` | `COMP/src/user_panel.rs:246-280`, `COMP/src/markdown_editor.rs:287-329` |
| Full-height editor area | `Container.class(FlexFit)` as the `main_view` of a `LoadableComponent` | `COMP/src/notes_view.rs:183-190` |
| Diff confirm dialog | `Dialog::new(tr!("Apply changes"))` with a `Row` of `Button`s, or `ConfirmButton.confirm_message(...)` if no diff body is needed | `PWT/src/widget/dialog.rs:26-113`, `COMP/src/confirm_button.rs:62-105` |
| "Changed on server" banner | `Container.padding(4).class(ColorScheme::WarningContainer).class("pwt-default-colors")` placed between toolbar and content | `PDM/src/remotes/auto_installer/prepared_answer_form.rs:1099-1108` |
| Load/save errors | `error_message(...)` strip under content; `link.show_error(...)` for action failures | `COMP/src/loadable_component.rs:588-601`, `:308-316` |
| Stale-write protection | digest round-trip, no dialog | `COMP/src/notes_view.rs:42-53,154` |
| Unsaved-changes guard on tab switch | `ConfirmDialog` via `ConfirmButton`, or a `ViewState` variant + `Dialog` | `COMP/src/confirm_button.rs:129-140` |

### 10.2 The skeleton to write

```rust
//! The pve-meta metadata editor page.

use std::pin::Pin;
use std::rc::Rc;

use anyhow::Error;
use yew::virtual_dom::{VComp, VNode};

use pwt::css::{ColorScheme, FlexFit};
use pwt::prelude::*;
use pwt::widget::{Button, Column, Container, Dialog, Fa, Row, SegmentedButton, Toolbar};
use pwt_macros::builder;

use proxmox_yew_comp::{
    ConfirmButton, LoadableComponent, LoadableComponentContext, LoadableComponentMaster,
    LoadableComponentState,
};

/// Which representation the editor shows.
#[derive(Copy, Clone, PartialEq, Eq, Default)]
pub enum ViewAs { #[default] Form, Yaml, Json }

/// Modal states of the page.
#[derive(PartialEq)]
pub enum ViewState { ConfirmApply, ConfirmDiscard }

pub enum Msg {
    SetViewAs(ViewAs),
    EditorInput(String),
    Apply,
    Discard,
    Applied(Result<(), Error>),
}

#[derive(Clone, PartialEq, Properties)]
#[builder]
pub struct MetaEditor {
    /// Document to edit.
    pub doc: DocId,
    /// Label shown in the header, e.g. `"CT 200 (traefik)"`.
    #[builder(IntoPropValue, into_prop_value)]
    #[prop_or_default]
    pub subtitle: Option<AttrValue>,
}

impl MetaEditor {
    pub fn new(doc: DocId) -> Self { yew::props!(Self { doc }) }
}

#[doc(hidden)]
pub struct PveMetaEditor {
    state: LoadableComponentState<ViewState>,
    view_as: ViewAs,
    /// Text as loaded, plus the digest it came with.
    loaded: Rc<Document>,
    /// Live editor buffer; `None` while unmodified.
    draft: Option<String>,
    /// Set when a version poll saw a newer digest than `loaded`.
    stale: bool,
    monaco_id: AttrValue,
}

pwt::impl_deref_mut_property!(PveMetaEditor, state, LoadableComponentState<ViewState>);

impl LoadableComponent for PveMetaEditor {
    type Properties = MetaEditor;
    type Message = Msg;
    type ViewState = ViewState;

    fn create(ctx: &LoadableComponentContext<Self>) -> Self { /* … */ }

    fn load(&self, ctx: &LoadableComponentContext<Self>)
        -> Pin<Box<dyn Future<Output = Result<(), Error>>>>
    { /* GET, keep resp.attribs["digest"] */ }

    fn toolbar(&self, ctx: &LoadableComponentContext<Self>) -> Option<Html> {
        let link = ctx.link();
        let dirty = self.draft.is_some();

        let view_as = SegmentedButton::new()
            .aria_label(tr!("View as"))
            .with_button(self.view_btn(link, "fa fa-list-alt", tr!("Form"), ViewAs::Form))
            .with_button(self.view_btn(link, "fa fa-file-code-o", tr!("YAML"), ViewAs::Yaml))
            .with_button(self.view_btn(link, "fa fa-file-text-o", tr!("JSON"), ViewAs::Json));

        Some(
            Toolbar::new()
                .class("pwt-w-100")
                .class("pwt-overflow-hidden")
                .class("pwt-border-bottom")
                .with_child(view_as)
                .with_spacer()
                .with_child(
                    Button::new(tr!("Apply"))
                        .disabled(!dirty)
                        .onclick(link.change_view_callback(|_| Some(ViewState::ConfirmApply))),
                )
                .with_child(
                    ConfirmButton::new(tr!("Discard"))
                        .dangerous(true)
                        .disabled(!dirty)
                        .confirm_message(tr!("Discard all unapplied changes?"))
                        .on_activate(link.callback(|_| Msg::Discard)),
                )
                .with_flex_spacer()
                .with_child(Button::refresh(self.loading()).onclick({
                    let link = link.clone();
                    move |_| link.send_reload()
                }))
                .into(),
        )
    }

    fn main_view(&self, ctx: &LoadableComponentContext<Self>) -> Html {
        let props = ctx.props();

        // Identity line — same shape as PDM's section separator.
        let header = Row::new()
            .class(pwt::css::AlignItems::Baseline)
            .class(pwt::css::FontStyle::TitleMedium)
            .padding(2)
            .gap(2)
            .with_child(Fa::new(props.doc.icon()))
            .with_child(props.doc.title())
            .with_flex_spacer()
            .with_optional_child(props.subtitle.clone().map(|s| {
                Container::from_tag("span").class(pwt::css::FontColor::NeutralAlt).with_child(s)
            }));

        // "changed on server" — non-modal, warning-container banner.
        let banner = self.stale.then(|| {
            Row::new()
                .padding(2)
                .gap(2)
                .class(pwt::css::AlignItems::Center)
                .class(ColorScheme::WarningContainer)
                .class("pwt-default-colors")
                .class("pwt-border-bottom")
                .with_child(Fa::new("exclamation-triangle"))
                .with_child(tr!("This document was changed on the server since it was loaded."))
                .with_flex_spacer()
                .with_child(Button::new(tr!("Reload")).onclick({
                    let link = ctx.link().clone();
                    move |_| link.send_reload()
                }))
        });

        Column::new()
            .class(FlexFit)
            .with_child(header)
            .with_optional_child(banner)
            .with_child(
                // Monaco host: a plain Container with a stable id and no chrome of its own.
                Container::new()
                    .id(self.monaco_id.clone())
                    .class(FlexFit)
                    .class("pve-meta-monaco-host"),
            )
            .into()
    }

    fn dialog_view(&self, ctx: &LoadableComponentContext<Self>, vs: &Self::ViewState)
        -> Option<Html>
    {
        match vs {
            ViewState::ConfirmApply => Some(self.create_diff_dialog(ctx)),
            ViewState::ConfirmDiscard => None, // handled by ConfirmButton
        }
    }

    fn rendered(&mut self, ctx: &LoadableComponentContext<Self>, first_render: bool) {
        if first_render { self.mount_monaco(ctx); } else { self.sync_monaco(ctx); }
    }
}

impl From<MetaEditor> for VNode {
    fn from(val: MetaEditor) -> Self {
        let comp = VComp::new::<LoadableComponentMaster<PveMetaEditor>>(Rc::new(val), None);
        VNode::from(comp)
    }
}
```

Note what this buys you for free from `LoadableComponentMaster::view()`
(`COMP/src/loadable_component.rs:596-602`): the outer
`Column.class("pwt-flex-fill pwt-overflow-auto")`, the toolbar slot, the load-error strip with
`pwt-border-top`, dialog stacking, and off-screen auto-refresh suspension.

### 10.3 Diff confirm dialog

Follow the plain-`Dialog` shape, with the diff body inside a scrolling, monospace container:

```rust
Dialog::new(tr!("Apply") + ": " + &tr!("Changes"))
    .width(900)
    .height(600)
    .resizable(true)
    .on_close(ctx.link().change_view_callback(|_| None))
    .with_child(
        Container::new()
            .class(FlexFit)
            .class("pwt-font-monospace")
            .class("pwt-overflow-auto")
            .padding(2)
            .with_child(diff_body),
    )
    .with_child(
        Row::new()
            .padding(2)
            .gap(2)
            .class(pwt::css::JustifyContent::FlexEnd)
            .class("pwt-border-top")
            .with_child(Button::new(tr!("Cancel"))
                .onclick(ctx.link().change_view_callback(|_| None)))
            .with_child(Button::new(tr!("Apply"))
                .class(ColorScheme::Primary)
                .onclick(ctx.link().callback(|_| Msg::Apply))),
    )
```

`JustifyContent::FlexEnd` for the button row is PDM's convention (13 sites). This is the one
legitimate place for `ColorScheme::Primary` — the confirming action of a modal.

### 10.4 Where the page lives

If the editor becomes one item among several (form view, source view, history), wrap it in a
`TabPanel` exactly as `PDM/src/pve/remote/mod.rs:41-85` does, with
`.router(true).class(FlexFit).title(<Row + Fa + name>)`. Otherwise a bare
`Container.class("pwt-content-spacer").class(FlexFit).with_child(MetaEditor::new(doc))` gives
the correct single-child, full-bleed, no-padding behaviour (§1.3).

---

## 11. Embedding Monaco in a pwt page

There is no Proxmox precedent (§9), so this section is design guidance, anchored to the pwt
mechanisms that do exist.

### 11.1 Mount point

`WidgetBuilder::id()` (`PWT/src/props/widget_builder.rs:44-73`) gives a `Container` a stable
DOM id, and `pwt::widget::get_unique_element_id()` (`PWT/src/widget/mod.rs:215-219`) yields
`PwtElementId<N>` if you want per-instance uniqueness:

```rust
static UNIQUE_ELEMENT_ID: AtomicUsize = AtomicUsize::new(0);
pub fn get_unique_element_id() -> String {
    let id = UNIQUE_ELEMENT_ID.fetch_add(1, Ordering::SeqCst);
    format!("PwtElementId{}", id)
}
```

Prefer a `NodeRef` where you can — that is what pwt itself uses for raw DOM
(`COMP/src/markdown_editor.rs:113,342,396`, `PWT/src/props/mod.rs:12-25`'s `IntoVTag`
`into_html_with_ref`). A fixed `id` is the pragmatic choice only because Monaco's JS glue lives
outside wasm and needs something to look up. If you go the `id` route, generate it once in
`create()` and store it in the component (never recompute per render).

### 11.2 Lifecycle

Yew has no `use_effect` in this stack — mounting happens in `Component::rendered`
(the pattern used by `PwtMask::rendered`, `PWT/src/widget/mask.rs:75-82`, and
`MarkdownEditorField::rendered`, `COMP/src/markdown_editor.rs:394-403`):

* `rendered(first_render = true)` → call the JS glue's `mount(id, initial_text, options)`.
* `rendered(first_render = false)` → `set_value` only when the model actually changed
  (compare against a stored generation/hash; never write the buffer the user is typing in).
* `destroy()` → call `dispose(id)`; Monaco leaks a ResizeObserver and a model otherwise.
* Value changes flow **into** the component via a `Callback<String>` handed to the glue at
  mount time and invoked from JS; model it as `Msg::EditorInput(String)`.

### 11.3 The JS glue

Nothing in the Proxmox stack has `.js` files, so this is a deliberate deviation — keep it
small and declare it as such in a comment. Two viable shapes:

1. `#[wasm_bindgen(module = "/js/monaco-glue.js")]` with `extern "C"` fns
   `mount/set_value/set_theme/dispose`. Requires the file to be shipped in `dist/` and
   referenced by trunk (`data-trunk rel="copy-file"` in `index.html`, alongside the existing
   font-awesome hook in `ui/Trunk.toml`).
2. A plain `<script>` in `index.html` defining `window.pveMetaMonaco = {...}`, called from Rust
   via `js_sys::Reflect`/`Function::call`. Fewer build moving parts; weaker typing.

Either way Monaco itself is a vendored asset (AMD or ESM bundle) under `dist/monaco/`,
installed by the `ui` Makefile target the same way `desktop-yew-style.css` is.

### 11.4 Making Monaco match the pwt theme

Monaco takes an explicit theme object; it does **not** inherit CSS. So read pwt's variables at
mount time and on every `pwt-theme-changed` event, and call
`monaco.editor.defineTheme(...)` + `setTheme(...)`.

The variables to read off the mount element (`getComputedStyle(el).getPropertyValue(name)`):

| Monaco color key | pwt variable | Notes |
|---|---|---|
| `editor.background` | `--pwt-color-background` | the current surface pair (§7.3) |
| `editor.foreground` | `--pwt-color` | ditto |
| `editorWidget.background` | `--pwt-color-surface` | |
| `editorWidget.border`, `editorGroup.border` | `--pwt-color-border` | |
| `editorLineNumber.foreground` | `--pwt-color-neutral-alt` | |
| `editorLineNumber.activeForeground` | `--pwt-color-on-neutral` | |
| `editorCursor.foreground` | `--pwt-color-primary` | |
| `editor.selectionBackground` | `--pwt-color-primary-container` | |
| `editorError.foreground` | `--pwt-color-error` | |
| `editorWarning.foreground` | `--pwt-color-warning` | |
| `editorInfo.foreground` | `--pwt-color-primary` | |
| `focusBorder` | `--pwt-color-focus` | |

Base must follow the mode: `document.documentElement.classList.contains('pwt-dark-mode')`
→ `"vs-dark"`, else `"vs"` (§8; remember `auto` mode adds no class at all in light, so test for
`pwt-dark-mode` and default to light).

Also mirror the typography so the editor doesn't read as a foreign widget:

```js
fontFamily: getComputedStyle(el).getPropertyValue('font-family'), // or 'monospace'
fontSize: parseFloat(getComputedStyle(el).fontSize),              // inherits pwt body size
lineHeight: parseFloat(getComputedStyle(el).lineHeight),
```

Because `--pwt-color-background`/`--pwt-color` are *scoped* (any `.pwt-scheme-*` ancestor
rebinds them), reading them off the **mount element itself** rather than `:root` gives the
right values automatically.

Re-run the whole theme sync from a `pwt-theme-changed` listener plus a
`matchMedia("(prefers-color-scheme: dark)")` listener — exactly the two sources
`ThemeObserver` watches (`PWT/src/state/theme.rs:289-330`). Easiest is to hold a `ThemeObserver`
in the component and call the glue's `set_theme()` from its callback, so there is one source of
truth.

### 11.5 Sizing

Monaco needs an explicitly sized box; it will not grow from content. `Container.class(FlexFit)`
inside the `Column.class(FlexFit)` chain gives it `flex: 1 1 auto; overflow: auto` all the way
up to `pwt-viewport`, which is enough — but add `automaticLayout: true` (or a `ResizeObserver`,
mirroring `PWT/src/dom/dom_size_observer.rs`) so it re-measures when the flex box changes.
The one app-level CSS rule worth writing:

```scss
.pve-meta-monaco-host { min-height: 0; }   /* let the flex child shrink below content height */
```

which is exactly the trick `COMP/src/markdown_editor.rs:378-379` uses
(`.style("flex", "1 1 auto").style("min-height", "0")`).

---

## 12. pwt-in-ExtJS vs. a native ExtJS panel — fidelity comparison

The editor page ships as a tab inside PVE's ExtJS SPA (same-origin iframe). Two options.

### 12.1 Option A — pwt/Yew page in an iframe tab (the current direction)

**What matches well**

* Crisp theme is explicitly designed to look like PVE: *"This theme is similar to what we
  currently use for our products. The `3px` spacing is very dense…"*
  (`SCSS/crisp/_theme_config_crisp.scss:1-9`). Its spacer (3px) and small font scale
  (`_font_sizes_small.scss`) are much closer to ExtJS-Crisp than Desktop's 5px.
* Font-awesome 4 icon names are shared between both stacks, so icon vocabulary is identical.
* Toolbar/table/dialog *idioms* (Add·Edit·Remove, refresh on the right, "Edit: Notes" dialog
  titles) are the same because pwt was written to reproduce the ExtJS product look.

**Real mismatch risks, honestly**

1. **Two separate color systems.** pwt's palettes are Material-Design-3 HCT-derived
   (`SCSS/mixins/_color_palette.scss:56-61`, `SCSS/docs/introduction.md:11-12`), even in the
   Crisp variant; PVE's ExtJS themes are hand-picked ExtJS "crisp"/"proxmox-dark" palettes.
   The greys, the blue, and especially the *border* color will not be pixel-identical. Adjacent
   ExtJS chrome and pwt content will read as "close but not the same".
2. **Fonts.** pwt's sans stack is Roboto Flex, shipped as webfonts under `dist/fonts/`
   (`SCSS/_theme_common.scss`, `ui/index.html`'s `copy-dir`); PVE's ExtJS uses Helvetica/Arial
   system stacks. Even at the same px size the color, x-height and metrics differ visibly. This
   is the single most noticeable giveaway. *Mitigation:* override
   `$pwt-font-sans-serif` in `ui/css/crisp-yew-style.scss` before `@import "theme_pre"` to match
   PVE's stack, and stop copying the font files.
3. **Two independent theme states, and PVE's is resolved server-side.** PVE has exactly two
   themes, `crisp` and `proxmox-dark`, plus `__default__` = follow the OS
   (`/usr/share/javascript/proxmox-widget-toolkit/proxmoxlib.js:123-146`). The choice lives in
   a cookie `PVEThemeCookie` written by `Proxmox.window.ThemeEditWindow`, which then just
   calls `window.location.reload()` (`proxmoxlib.js:20421-20469`) — because **pveproxy reads
   the cookie and decides at page-render time** which stylesheet the templated `index.html`
   links (`/usr/share/perl5/PVE/Service/pveproxy.pm:212-221`). Our iframe is a *static* file
   under `/pve2/js/pve-meta-ui/`, so it never goes through that template and gets **no** theme
   information at all. Meanwhile pwt keeps `ThemeMode`/`ThemeName` in `localStorage`
   (`PWT/src/state/theme.rs:175-196`). Nothing syncs them, so a user on "Proxmox Dark" gets a
   light iframe unless you explicitly bridge: read `PVEThemeCookie` at startup
   (`document.cookie`, same-origin so it is readable) and call
   `Theme::store_theme_name("Crisp")` + `Theme::store_theme_mode(...)`
   (`PWT/src/state/theme.rs:225-268`) before the first render — mapping
   `crisp → Light`, `proxmox-dark → Dark`, absent/`__default__` → `System`.
   **This must be done or the tab will regularly look broken.**
   *Naming trap:* PVE's dark ExtJS theme defines its own CSS custom properties **also called
   `--pwt-*`** (`theme-proxmox-dark.css:1`: `--pwt-panel-background: #262626;
   --pwt-text-color: #f2f2f2; --pwt-gauge-*`, read back via `getComputedStyle` in
   `proxmoxlib.js:10636-10653`). These are a completely different, unrelated namespace from
   pwt-the-widget-toolkit's `--pwt-color-*`. In a same-origin iframe the two documents do not
   share a cascade so nothing collides today, but never inline pwt's CSS into the PVE page,
   and never assume a `--pwt-*` variable means the same thing on both sides.
4. **Scrollbars and focus rings** differ (`color-scheme: light/dark` is set on `:root` by pwt,
   `SCSS/_theme_scheme_light.scss:3`), so the iframe's scrollbar may not match the surrounding
   page.
5. **Payload.** A wasm bundle plus Roboto Flex plus Monaco inside a tab of an already-large SPA.
6. **Iframe seams.** Dialogs opened by the pwt page are clipped to the iframe; they cannot
   center over the PVE window. PDM has the same limitation with its xterm iframe
   (`COMP/src/xtermjs.rs:151`) and lives with it.

### 12.2 Option B — native ExtJS panel with Monaco

**Pros:** perfect chrome fidelity (it *is* the same widget set, same theme cookie, same fonts,
same scrollbars); dialogs are real `Proxmox.window.Edit` windows that center over the SPA; no
wasm payload; toolbar/`Ext.Msg` behaviour is free. The tab would be a normal entry in
`PVE.lxc.Config`'s `items:` array
(`/usr/share/pve-manager/js/pvemanagerlib.js:39669-39724`), and an options-style page would be
a `Proxmox.grid.PendingObjectGrid` like `PVE.lxc.Options`
(`pvemanagerlib.js:42408-42500`) — see `docs/design/PROXMOX-CONVENTIONS.md` §7 for the exact
idioms.

**Cons:** none of the existing Rust model/patch/schema/diff code (`ui/src/model.rs`,
`patch.rs`, `schema.rs`, `source.rs` — ~1200 lines, natively unit-tested) is reusable in the
browser; it would have to be reimplemented in JS or exposed as an API round-trip. You also give
up the type-shared API layer.

### 12.3 Recommendation

**Stay with pwt (Option A), but commit to the fidelity work as part of the rebuild:**

1. Ship **Crisp** as the default `ThemeName`, not Desktop.
2. **Bridge the theme**: read PVE's theme cookie on startup and write
   `ThemeMode`/`ThemeName` before the first render (`Theme::store_theme_mode` /
   `store_theme_name`, `PWT/src/state/theme.rs:225-268`); re-read on `pwt-theme-changed` never —
   PVE is the master while embedded.
3. **Override the font stack** to PVE's, and drop the Roboto Flex copy in embedded builds.
4. Keep dialogs small enough to fit the iframe, or push genuinely modal decisions
   (apply/discard) to inline confirmation rows rather than centered windows.
5. Delete every `pve-meta-*` class except the one Monaco sizing rule.

If, after (1)–(3), the tab still reads as foreign in side-by-side review, Option B is a real
fallback — but it costs the Rust core, so it should be a decision made on evidence, not
pre-emptively.
