# Proxmox Conventions

How Proxmox *writes* things — Rust, Perl, JavaScript, packaging — so pve-meta can adopt them
verbatim instead of inventing house style.

Companion document: `docs/design/PDM-DESIGN-LANGUAGE.md` (how PDM *composes pages*).

Citation roots:

| Short name | Path |
|---|---|
| `$SB` | `/private/tmp/claude-501/-Users-arki-Documents-proxmox/afff609e-fb1e-4d06-8a92-09f2b57a5c90/scratchpad/upstream` |
| `PDM` | `$SB/proxmox-datacenter-manager` |
| `COMP` | `$SB/proxmox-yew-comp` |
| `PWT` | `$SB/proxmox-yew-widget-toolkit` |
| `node1` | lab node `10.10.10.154`, PVE 9.2.11 — paths are absolute on that host |

---

## 1. Rust — tooling

### 1.1 rustfmt

Every Proxmox repo has a `rustfmt.toml`, and it is two lines
(`$SB/proxmox-datacenter-manager/rustfmt.toml:1-2`, `$SB/proxmox-datacenter-manager/ui/rustfmt.toml:1-2`,
`$SB/proxmox/rustfmt.toml:1-2`, `$SB/proxmox-yew-widget-toolkit/rustfmt.toml:1-2`):

```toml
edition = "2024"
style_edition = "2024"
```

Nothing else — no `max_width`, no `group_imports`, no `imports_granularity`. **Import grouping
is done by hand**, not by rustfmt. `proxmox-yew-comp` has no `rustfmt.toml` at all.

> **Adopt:** add `ui/rustfmt.toml` and a top-level `rustfmt.toml` with exactly those two lines.
> Note pve-meta's crates currently declare `edition = "2021"` in `Cargo.toml`
> (`ui/Cargo.toml:4`); `style_edition = "2024"` in rustfmt is independent of that and is what
> Proxmox uses, so set it regardless.

### 1.2 clippy

No `clippy.toml` anywhere. Lints are per-crate, targeted, and only ever *relax* — never a
blanket deny (`$SB/proxmox-yew-comp/Cargo.toml:116-119`):

```toml
[lints.clippy]
too_many_arguments = "allow"
enum_variant_names = "allow"
large_enum_variant = "allow"
```

and where a `cfg` needs whitelisting (`$SB/proxmox-datacenter-manager/server/Cargo.toml:94-96`):

```toml
[lints.rust.unexpected_cfgs]
level = "warn"
check-cfg = ['cfg(remote_config, values("faked"))']
```

### 1.3 Crate attributes

The UI crates have **zero** crate-level lint attributes — no `#![deny(...)]`, no
`#![warn(...)]`. Non-UI library crates in the `proxmox` workspace add only
(`$SB/proxmox/proxmox-schema/src/lib.rs:9-10`):

```rust
#![deny(unsafe_op_in_unsafe_fn)]
#![cfg_attr(docsrs, feature(doc_cfg, doc_auto_cfg))]
```

with `#![deny(missing_docs)]` / `#![forbid(unsafe_code, missing_docs)]` on the stricter ones
(`$SB/proxmox/proxmox-s3-client/src/lib.rs:9-11`, `$SB/proxmox/proxmox-ini/src/lib.rs:8`).

### 1.4 Cargo source replacement

Identical in every repo, for Debian-packaged dependency builds
(`$SB/proxmox-datacenter-manager/.cargo/config.toml:1-4`):

```toml
[source]
[source.debian-packages]
directory = "/usr/share/cargo/registry"
[source.crates-io]
replace-with = "debian-packages"
```

### 1.5 Licensing

**No copyright headers and no SPDX lines in any `.rs` file** across `COMP`, `PWT` and `PDM/ui`
(zero grep hits). The license is declared once in `Cargo.toml`:
`COMP` → `license = "AGPLv3"` (`Cargo.toml:5`), `PWT` → `"MIT OR Apache-2.0"` (`Cargo.toml:9`),
`PDM/ui` → `"AGPL-3"` (`Cargo.toml:5`). Third-party code carries a plain inline attribution
comment, not a header (`$SB/proxmox-yew-widget-toolkit/src/web_sys_ext.rs:1`):

```rust
// copied from https://raw.githubusercontent.com/jetli/yew-hooks/main/crates/yew-hooks/src/web_sys_ext.rs
```

---

## 2. Rust — module and file layout

### 2.1 One component per file, `lib.rs` is a flat `mod`/`pub use` ledger

`$SB/proxmox-yew-comp/src/lib.rs:1-17`:

```rust
pub mod acme;

mod acl_context;
pub use acl_context::{AclContext, AclContextProvider};

mod api_load_callback;
pub use api_load_callback::{ApiLoadCallback, IntoApiLoadCallback};

#[cfg(feature = "apt")]
mod apt_package_manager;
#[cfg(feature = "apt")]
pub use apt_package_manager::{AptPackageManager, ProxmoxAptPackageManager};
```

Note the pairing: `mod` is private, the types are re-exported flat, and a `#[cfg(feature)]`
gate is repeated on **both** the `mod` and the `pub use`.

Sub-directories get a terse `mod.rs` of the same shape, with
`pub(crate) mod X; pub use X::Y;` (`$SB/proxmox-yew-comp/src/acl/mod.rs:1-8`):

```rust
pub(crate) mod acl_edit;
pub use acl_edit::AclEdit;

pub(crate) mod acl_path_selector;
pub use acl_path_selector::AclPathSelector;

pub(crate) mod acl_view;
pub use acl_view::AclView;
```

A feature area's `mod.rs` may also *contain* the small assembly component for that area rather
than only re-exporting — `$SB/proxmox-datacenter-manager/ui/src/remotes/mod.rs:1-94` lists the
submodules and then defines `#[function_component(RemotesPanel)]` at line 50.

Module docs are `//!` at the top of `mod.rs` **and** on ordinary large feature files
(`$SB/proxmox-datacenter-manager/ui/src/guests.rs:1-8`):

```rust
//! Central, cross-remote list of all guests (QEMU VMs and LXC containers).
//!
//! Provides a single filterable view over the guests of every remote PDM
//! manages, reusing the cached `/resources/list` aggregation. ...
```

### 2.2 Naming

| Thing | Convention | Example |
|---|---|---|
| File | `snake_case` of the public props struct | `status_row.rs` |
| Public `Properties` struct | plain `CamelCase`, **no prefix** | `StatusRow`, `ConfirmButton`, `RemoteSelector` |
| Internal `Component` struct | **product prefix** + same name, `#[doc(hidden)]` | `ProxmoxStatusRow` (COMP), `PdmRemoteSelector` (PDM), `PwtToolbar` (PWT) |

Cites: `COMP/src/status_row.rs:61`, `COMP/src/confirm_button.rs:114`,
`PDM/ui/src/widget/remote_selector.rs:58`, `PWT/src/widget/toolbar.rs:39`.

### 2.3 Use-block ordering (hand-maintained, blank line between groups)

1. `std`
2. external crates (`anyhow`, `serde`, `serde_json`, `yew`, `wasm_bindgen`, `gloo_*`, `web_sys`)
3. proxmox / product API crates (`proxmox_*`, `pdm_*`, `pve_api_types`)
4. `pwt` / `pwt_macros`
5. `crate::...`

Verified against `PDM/ui/src/pve/lxc/mod.rs:1-22`, `COMP/src/markdown_editor.rs:1-18`,
`PDM/ui/src/guests.rs:9-30`.

### 2.4 Item order inside a component file

1. `use` block (grouped as above).
2. free helper functions / consts, if any
   (`COMP/src/confirm_button.rs:9-14`, `COMP/src/markdown_editor.rs:22`).
3. `#[widget(comp=…, @element, @container, @input)]` → `#[derive(Properties, …)]` →
   `#[builder]` (derive trait order is *not* fixed in practice).
4. `pub struct Foo { … }`, each field doc-commented, with
   `#[prop_or_default]` / `#[prop_or(true)]` / `#[builder(...)]` / `#[builder_cb(...)]`.
5. `impl Default for Foo { fn default() -> Self { Self::new() } }` when `new()` takes no args.
6. `impl Foo { pub fn new(...) -> Self { yew::props!(Self { … }) } … builder setters … }`.
7. `pub enum Msg { … }` (often `#[doc(hidden)]`).
8. `#[doc(hidden)] pub struct ProxmoxFoo { … }` — the internal state.
9. `impl Component for ProxmoxFoo` with `type Message`, `type Properties`, then
   `create` → `update` → `changed` → `view` → `rendered`, in that order.
10. `impl From<Foo> for VNode { … }` **only** when the component is not a `#[widget]` (the
    macro generates `Into<Html>`/`ToHtml` itself). Needed for `LoadableComponentMaster`-backed
    components (`COMP/src/tfa/tfa_view.rs:420-425`):

```rust
impl From<TfaView> for VNode {
    fn from(val: TfaView) -> Self {
        let comp = VComp::new::<LoadableComponentMaster<ProxmoxTfaView>>(Rc::new(val), None);
        VNode::from(comp)
    }
}
```

### 2.5 Reference file to copy

`COMP/src/status_row.rs:1-103` is the whole convention in 103 lines — read it before writing any
new component. Its `impl` block shows the builder-setter pair style verbatim
(`status_row.rs:24-57`):

```rust
impl StatusRow {
    /// Create a new instance.
    pub fn new(title: impl Into<AttrValue>) -> Self {
        yew::props!(Self { title: title.into() })
    }

    /// Builder style method to set the icon class.
    pub fn icon_class(mut self, icon_class: impl Into<Classes>) -> Self {
        self.set_icon_class(icon_class);
        self
    }

    /// Method to set the icon class.
    pub fn set_icon_class(&mut self, icon_class: impl Into<Classes>) {
        self.icon_class = Some(icon_class.into());
    }
}
```

---

## 3. Rust — the builder / props macros

`#[widget(...)]` — `PWT/pwt-macros/src/lib.rs:44-52`:

```rust
/// `#[widget(crate=foo, comp=bar, @element, @svg, ...)]`
/// * `crate=foo` is optional and designates where to find the `pwt` crate
/// * `comp=bar` is also optional and describes the `Component` to use
/// * The desired types are prefixed with `@` and simply appended as a comma seperated list
```

Markers: `@element` (adds listeners + `EventSubscriber`), `@container` (adds
`children: Vec<Html>` + `ContainerBuilder`), `@input` (adds `input_props: FieldStdProps` +
`FieldBuilder`), `@svg`.

`#[builder]` on a field generates the `set_x` / `x` pair
(`PWT/pwt-macros/src/lib.rs:317-329`):

```rust
pub fn set_some_field(&mut self, some_field: impl IntoPropValue<f32>) {
    self.some_field = some_field.into_prop_value();
}
pub fn some_field(mut self, some_field: impl IntoPropValue<f32>) -> Self {
    self.set_some_field(some_field);
    self
}
```

`#[builder_cb(IntoEventCallback, into_event_callback, T)]` for callbacks
(`PWT/pwt-macros/src/lib.rs:393-401`). Every event prop is
`#[prop_or_default] pub on_x: Option<Callback<T>>` — **never a bare non-optional `Callback<T>`
prop**.

Canonical field-attribute mix (`COMP/src/confirm_button.rs:19-60`):

```rust
#[widget(comp=ProxmoxConfirmButton)]
#[derive(Properties, PartialEq, Clone)]
#[builder]
pub struct ConfirmButton {
    #[prop_or_default]
    pub text: Option<AttrValue>,
    #[prop_or_default]
    #[builder(IntoPropValue, into_prop_value)]
    pub tabindex: Option<i32>,
    #[prop_or_default]
    #[builder]
    pub autofocus: bool,
    #[prop_or_default]
    #[builder_cb(IntoEventCallback, into_event_callback, ())]
    pub on_activate: Option<Callback<()>>,
}
```

Helper macros:

* `pwt::impl_yew_std_props_builder!()` adds `key`/`set_key`
  (`PWT/src/props/mod.rs:229-241`).
* `pwt::impl_deref_mut_property!(Ty, field, FieldTy)` makes a component's state
  `Deref`/`DerefMut` into an embedded sub-state — this is how every `LoadableComponent`
  reaches `LoadableComponentState` (`PWT/src/lib.rs:415-423`, used at
  `COMP/src/user_panel.rs:179`).

---

## 4. Rust — data, state, errors, i18n

### 4.1 String types

| Type | When |
|---|---|
| `AttrValue` | prop text that flows straight into `html!`/DOM (`StatusRow.title`) |
| `String` | owned computed/mutable values — error messages, ids from responses |
| `Rc<Vec<T>>` / `Rc<str>` | shared collections cloned every render (columns, remote lists) |

`PDM/ui/src/widget/remote_selector.rs:25,44` shows both in one struct.

### 4.2 The `thread_local!` column cache

The canonical way to build `DataTable` columns once (`COMP/src/tfa/tfa_view.rs:362-419`,
consumed at `:302` as `COLUMNS.with(Rc::clone)`):

```rust
thread_local! {
    static COLUMNS: Rc<Vec<DataTableHeader<TfaEntry>>> = Rc::new(vec![
        DataTableColumn::new(tr!("User"))
            .width("200px")
            .render(|item: &TfaEntry| { html!{item.user_id.clone()} })
            .sorter(|a: &TfaEntry, b: &TfaEntry| a.user_id.cmp(&b.user_id))
            .into(),
    ]);
}
```

### 4.3 Loading and errors

`loading` is a **`usize` counter**, `last_load_error` is a **`String`**
(`COMP/src/loadable_component.rs:375-412`):

```rust
pub struct LoadableComponentState<V: PartialEq> {
    loading: usize,
    last_load_error: Option<String>,
}
impl<V: PartialEq> LoadableComponentState<V> {
    pub fn loading(&self) -> bool { self.loading > 0 }
    pub fn last_load_errors(&self) -> Option<&str> { self.last_load_error.as_deref() }
}
```

Async loaders return `anyhow::Error` boxed as a plain future
(`COMP/src/loadable_component.rs:150-153`):

```rust
fn load(&self, ctx: &LoadableComponentContext<Self>)
    -> Pin<Box<dyn Future<Output = Result<(), Error>>>>;
```

The error is stringified at the point it is stored, and the **first** failure escalates to a
modal while later ones stay inline (`COMP/src/loadable_component.rs:476-489`):

```rust
Err(err) => {
    let this_is_the_first_error = self.state.last_load_error.is_none();
    self.state.last_load_error = Some(err.to_string());
    if this_is_the_first_error {
        self.state.view_state = ViewState::Error(tr!("Load failed"), err.to_string(), false);
    }
}
```

### 4.4 `tr!()`

Provided by `pwt` (`PWT/src/tr.rs:81-110`), backed by the `gettext = "0.4"` crate
(`PWT/Cargo.toml:74`). Catalogs are `.mo` files fetched at runtime — `catalog-{lang}.mo`
(`PWT/src/widget/catalog_loader.rs:36,58`), wired by
`DesktopApp::catalog_url_builder(...)` (`PDM/ui/src/main.rs:349-350`). No `.po`/`.pot` files
or `xtr` step live in these checkouts.

Forms in use:

```rust
tr!("Add") + ": " + &tr!("User")                      // COMP/src/user_panel.rs:405
tr!("Remote '{0}'", props.remote)                     // PDM/ui/src/pve/remote/mod.rs:47
tr!("Could not delete '{0}': '{1}'", id, err)         // PDM/ui/src/configuration/views.rs:237
tr!("{n} Guest" | "{n} Guests" % total)               // PDM/ui/src/guests.rs:390
tr!("context" => "message")                           // documented, PWT/src/tr.rs:60-70
```

### 4.5 User-facing string style

* Error titles: `"Unable to <verb> <object>"`, sentence case, **no trailing period**. The
  detail comes from the `Display` of the underlying error, so the `tr!` string stays short and
  generic — `tr!("Unable to delete user")` (`COMP/src/user_panel.rs:225`),
  `tr!("Unable to apply changes")` (`COMP/src/configuration/network_view.rs:184`),
  `tr!("Unable to revert changes")` (`:173`).
* Confirmations: full sentence, trailing `?`, single-quoted `{0}` interpolation —
  `tr!("Are you sure you want to remove '{0}'", key)`
  (`PDM/ui/src/configuration/views.rs:319`),
  `tr!("Are you sure you want to remove entry {0}", name)`
  (`COMP/src/confirm_button.rs:11`).
* Long warnings are wrapped with a trailing `\` continuation inside the `tr!`, keeping one
  msgid (`PDM/ui/src/remotes/auto_installer/token_panel.rs:215-218`).
* Doc comments: every public builder method gets a one-liner, literally
  `/// Builder style method to set X.` and `/// Method to set X.`
  (`COMP/src/status_row.rs:24,31,37`).

---

## 5. Perl — `PVE::API2::*` module conventions

All paths in this section are on `node1` (PVE 9.2.11); they are the shipped, canonical files.

### 5.1 File header

`package`, blank, `use strict; use warnings;`, blank, grouped `use` block, then
`use base qw(PVE::RESTHandler);` — `/usr/share/perl5/PVE/API2/Pool.pm:1-17`:

```perl
package PVE::API2::Pool;

use strict;
use warnings;

use PVE::AccessControl;
use PVE::Cluster qw (cfs_read_file cfs_write_file);
use PVE::Exception qw(raise_param_exc);
use PVE::INotify;
use PVE::Storage;

use PVE::SafeSyslog;

use PVE::API2Tools;
use PVE::RESTHandler;

use base qw(PVE::RESTHandler);
```

`use` statements are *clustered*, not one alphabetical block. The fullest form is
`/usr/share/perl5/PVE/API2/Nodes.pm:6-67`: core Perl modules (`Digest::MD5`, `Fcntl`,
`HTTP::Status`, `JSON`, `List::Util`, `POSIX`, `Socket`, `IO::Socket::SSL`) → an alphabetized
`PVE::*` block (`:18-43`) → a separate alphabetized `PVE::API2::*` block (`:45-65`) →
`use base` (`:67`).

`use PVE::JSONSchema qw(get_standard_option);` is imported wherever `get_standard_option()` is
used (`Firewall/VM.pm:7`, `LXC/Config.pm:20`, `Nodes.pm:28`). API2 modules **consume** standard
options; they never call `register_standard_option` themselves — that lives in the central
schema modules.

One file may hold several packages: `/usr/share/perl5/PVE/API2/Firewall/VM.pm` defines
`VMBase`, `::VM` and `::CT`, each with its own header, where the leaf subclasses call a shared
`register_handlers('vm'|'ct')` (`:336`, `:360`).

### 5.2 `register_method` anatomy

`/usr/share/perl5/PVE/API2/Pool.pm:270-314`:

```perl
__PACKAGE__->register_method({
    name => 'update_pool',
    protected => 1,
    path => '',
    method => 'PUT',
    permissions => {
        description => "You also need the right to modify permissions on any object you add/delete.",
        check => ['perm', '/pool/{poolid}', ['Pool.Allocate']],
    },
    description => "Update pool.",
    parameters => {
        additionalProperties => 0,
        properties => {
            poolid => { type => 'string', format => 'pve-poolid' },
            comment => { type => 'string', optional => 1 },
            vms => {
                description => 'List of guest VMIDs to add or remove from this pool.',
                type => 'string', format => 'pve-vmid-list', optional => 1,
            },
            'allow-move' => { type => 'boolean', optional => 1, default => 0 },
            delete => { type => 'boolean', optional => 1, default => 0 },
        },
    },
    returns => { type => 'null' },
    code => sub { ... return; },
});
```

Key order: `name`, `protected` (immediately after `name` when present), `path`, `method`, then
`description` / `permissions` / `proxyto` in whichever order reads best (compare
`Firewall/VM.pm:71-75`, which orders `description`, `proxyto`, `permissions`), then
`parameters`, `returns`, and `code` **always last**.

Rules that are never broken:

* `additionalProperties => 0` on every `parameters` block (`Pool.pm:30`,
  `Firewall/VM.pm:38,77,110`, `LXC/Config.pm:33,122`). `returns` may use
  `additionalProperties => 1` for permissive nested items (`Pool.pm:60,420`).
* Property keys carry `type`, `description`, `optional => 1`, `format => 'pve-poolid'`,
  `maxLength => 40` (`LXC/Config.pm:143`), `default => 0`, `enum => [...]`,
  `requires => 'poolid'` (`Pool.pm:41`).
* Standard options: `node => get_standard_option('pve-node')`,
  `vmid => get_standard_option('pve-vmid')` (`Firewall/VM.pm:40-41`), or with a completion
  hook: `get_standard_option('pve-vmid', { completion => \&PVE::LXC::complete_ctid })`
  (`LXC/Config.pm:35-36`).
* Permissions: `check => ['perm', '/pool/{poolid}', ['Pool.Allocate']]` (`Pool.pm:165-167`);
  `user => 'all'` for open endpoints (`Pool.pm:24-28`, `Firewall/VM.pm:35`); OR-of-privileges
  via `check => ['perm', '/vms/{vmid}', $vm_config_perm_list, any => 1]`
  (`LXC/Config.pm:118`); a `description` alongside `check` explains caveats
  (`Pool.pm:275-278`).

### 5.3 Errors

`raise_param_exc({...})` for field-keyed validation errors — the key is the offending
parameter name (`Firewall/VM.pm:141-142`, `LXC/Config.pm:78-81`):

```perl
raise_param_exc({ delete => "no such option '$opt'" })
    if !$option_properties->{$opt};

raise_param_exc({
    snapshot => "cannot use 'snapshot' parameter with 'current'",
    current  => "cannot use 'snapshot' parameter with 'current'",
});
```

Bare `die "...\n"` for business-logic errors — the trailing `\n` suppresses Perl's
"at FILE line N" suffix (`Pool.pm:106,191,196`):

```perl
die "pool '$poolid' does not exist\n" if !$pool_config;
die "pool '$pool' already exists\n" if $usercfg->{pools}->{$pool};
die "pool name must start with a letter\n" if $leaf !~ m!^[A-Za-z]!;
```

`PVE::Exception`'s `raise` / `raise_perm_exc` are imported as conventional siblings even when
unused in a given file (`Nodes.pm:23`:
`use PVE::Exception qw(raise raise_perm_exc raise_param_exc);`).

### 5.4 Description-string style

Sentence case, trailing period, terse: `"List pools or get pool configuration."`
(`Pool.pm:23`), `"Create new pool."` (`:168`), `"Update pool."` (`:280`),
`"Directory index."` (`Firewall/VM.pm:36`), `"Get container configuration."`
(`LXC/Config.pm:28`). Field descriptions are full sentences too
(`"List of guest VMIDs to add or remove from this pool."`, `Pool.pm:235`); very short
fragments may drop the period (`"Line number"`, `Firewall/VM.pm:209`).

**No `Note:`-prefixed lines** in any of the sampled files — caveats are woven into the sentence:
`"Update pool data (deprecated, no support for nested pools - use 'PUT /pools/?poolid={poolid}' instead)."`
(`Pool.pm:227-228`). Enum values are not listed in prose; the schema's `enum` carries them and
the text names the concept (`"Only list references of specified type."` beside
`enum => ['alias','ipset']`, `Firewall/VM.pm:257-260`).

### 5.5 Nesting and index methods

There is **no** `register_handler_class` in these files. Nesting is
`register_method({ subclass => ..., path => ... })` (`Nodes.pm:112-115`, and 117-208 for the
rest; `Firewall/VM.pm:321-324`; `Nodes.pm:2903-2906` for `{node}`):

```perl
__PACKAGE__->register_method({
    subclass => "PVE::API2::Qemu",
    path => 'qemu',
});
```

An `index` is `name => 'index', path => '', method => 'GET'` returning an array of `{name}`
objects with a `child` link (`Firewall/VM.pm:31-64`):

```perl
returns => {
    type => 'array',
    items => { type => "object", properties => {} },
    links => [{ rel => 'child', href => "{name}" }],
},
code => sub {
    my $result = [
        { name => 'rules' }, { name => 'aliases' }, { name => 'ipset' },
        { name => 'refs' }, { name => 'options' },
    ];
    return $result;
},
```

### 5.6 Config-update pattern (digest + lock)

`/usr/share/perl5/PVE/API2/LXC/Config.pm:150,170-268` — synchronous, `protected => 1`, digest
extracted from `$param` and re-checked **inside** the lock after reloading:

```perl
my $digest = extract_param($param, 'digest');
...
my $code = sub {
    my $conf = PVE::LXC::Config->load_config($vmid);
    PVE::LXC::Config->check_lock($conf);
    PVE::Tools::assert_if_modified($digest, $conf->{digest});
    ...
    PVE::LXC::check_ct_modify_config_perm(
        $rpcenv, $authuser, $vmid, undef, $conf, {}, [@delete], $unprivileged,
    );
    my $errors = PVE::LXC::Config->update_pct_config($vmid, $conf, $running, $param, \@delete, \@revert);
    raise_param_exc($errors) if scalar(keys %$errors);
    PVE::LXC::Config->write_config($vmid, $conf);
};
PVE::LXC::Config->lock_config($vmid, $code);
return undef;
```

Long-running variants wrap the closure instead: `return $rpcenv->fork_worker('startall', undef,
$authuser, $code);` (`Nodes.pm:2293`, also `'stopall'` `:2454`, `'suspendall'` `:2586`,
`'migrateall'` `:2791`).

Imperative permission checks inside `code` (distinct from the declarative `permissions` key)
use `$rpcenv->check(...)`; a trailing `1` makes it return a boolean instead of throwing:

```perl
$rpcenv->check($authuser, "/pool/$poolid", ['Pool.Audit'], 1);   # Pool.pm:97
```

### 5.7 Return schemas

Array-of-objects with a `child` link (`Pool.pm:45-86`); `type => 'null'` for action endpoints
(`Pool.pm:182,262,314,470,494`, `LXC/Config.pm:169`); object returns often delegate to a shared
property builder rather than a literal hash (`LXC/Config.pm:56-71`):

```perl
returns => {
    type => "object",
    properties => PVE::LXC::Config->json_config_properties({
        lxc => { description => "...", type => 'array', items => {...}, optional => 1 },
        digest => { type => 'string', description => 'SHA1 digest of configuration file...' },
    }),
},
```

Numeric returns carry UI `renderer` hints: `cpu => { ..., renderer => 'fraction_as_percentage' }`,
`mem => { ..., renderer => 'bytes' }` (`Nodes.pm:2926-2937`).

---

## 6. JavaScript — general ExtJS conventions

Sources: `/usr/share/pve-manager/js/pvemanagerlib.js` (74,704 lines, 506 `Ext.define('PVE.*')`)
and `/usr/share/javascript/proxmox-widget-toolkit/proxmoxlib.js` (26,489 lines,
156 `Ext.define('Proxmox.*')`).

### 6.1 Namespacing and aliases

`PVE.*` = app-specific (`PVE.form.*`, `PVE.lxc.*`, `PVE.qemu.*`, `PVE.panel.*`);
`Proxmox.*` = the shared toolkit reused by PVE/PBS/PMG. Alias is `widget.pveXxx` camelCase
(`pvemanagerlib.js:4197` `'widget.pveConsoleButton'`, `:5015` `'widget.pveTwoColumnContainer'`);
some grids use the dotted form `alias: ['widget.PVE.qemu.Options']` (`:58970`).

Mixins + MVVM (`pvemanagerlib.js:5330-5344`):

```js
Ext.define('PVE.form.SizeField', {
    extend: 'Ext.form.FieldContainer',
    alias: 'widget.pveSizeField',
    mixins: ['Proxmox.Mixin.CBind'],
    viewModel: {
        data: { unit: 'MiB', unitPostfix: '' },
        formulas: { unitlabel: (get) => get('unit') + get('unitPostfix') },
    },
```

`Ext.app.ViewController` + declarative `control:` exists but is the minority style (~20
classes); the dominant style is still imperative `initComponent` + `Ext.apply(me, {...})` +
`me.callParent()` (`pvemanagerlib.js:15660-15685`).

### 6.2 Edit window

20 classes `extend: 'Proxmox.window.Edit'`. Shape: compute `url`/`method` in `initComponent`
from `isCreate`, build the inner panel, `Ext.apply(me, { subject, isAdd, items })`,
`me.callParent()`, then `me.load({ success })` for edit mode
(`pvemanagerlib.js:13761-13824`):

```js
Ext.define('PVE.FirewallRuleEdit', {
    extend: 'Proxmox.window.Edit',
    initComponent: function () {
        var me = this;
        me.isCreate = me.rule_pos === undefined;
        if (me.isCreate) { me.url = '/api2/extjs' + me.base_url; me.method = 'POST'; }
        else { me.url = '/api2/extjs' + me.base_url + '/' + me.rule_pos.toString(); me.method = 'PUT'; }
        var ipanel = Ext.create('PVE.FirewallRulePanel', { isCreate: me.isCreate, ... });
        Ext.apply(me, { subject: gettext('Rule'), isAdd: true, items: [ipanel] });
        me.callParent();
        if (!me.isCreate) {
            me.load({ success: function (response, options) {
                ipanel.setValues(response.result.data);
            }});
        }
    },
});
```

### 6.3 Options page — `PendingObjectGrid`

`PVE.lxc.Options` (`pvemanagerlib.js:42408-42500,42630-42669`); `PVE.qemu.Options` is the same
shape at `:58968`:

```js
Ext.define('PVE.lxc.Options', {
    extend: 'Proxmox.grid.PendingObjectGrid',
    alias: ['widget.pveLxcOptions'],
    onlineHelp: 'pct_options',
    initComponent: function () {
        var me = this;
        var caps = Ext.state.Manager.get('GuiCap');
        var rows = {
            onboot: {
                header: gettext('Start at boot'),
                defaultValue: '',
                renderer: Proxmox.Utils.format_boolean,
                editor: caps.vms['VM.Config.Options']
                    ? { xtype: 'proxmoxWindowEdit', subject: gettext('Start at boot'),
                        items: { xtype: 'proxmoxcheckbox', name: 'onboot',
                                 uncheckedValue: 0, defaultValue: 0,
                                 fieldLabel: gettext('Start at boot') } }
                    : undefined,
            },
        };
        Ext.apply(me, {
            url: '/api2/json/nodes/' + nodename + '/lxc/' + vmid + '/pending',
            selModel: sm, interval: 5000, tbar: [edit_btn, revert_btn],
            rows: rows, editorConfig: { url: '/api2/extjs/' + baseurl },
            listeners: { itemdblclick: me.run_editor, selectionchange: set_button_status },
        });
        me.callParent();
    },
});
```

Note the permission idiom: `editor` is set to `undefined` when
`Ext.state.Manager.get('GuiCap')` says the user may not edit — the row stays visible but
read-only. This is used everywhere.

### 6.4 Utilities and i18n

* `Proxmox.Utils.*` — shared formatters (`format_boolean`, `render_timestamp`,
  `parse_task_upid`, `defaultText`) and `API2Request`.
* `PVE.Utils.*` — app-specific (`kvm_ostypes`, `get_health_icon`, `render_kvm_startup`).
* `gettext(...)` — 3,707 call sites in `pvemanagerlib.js`. Universal.

`API2Request` shape (`pvemanagerlib.js:4331-4341`):

```js
Proxmox.Utils.API2Request({
    url: this.apiurl || view.editorConfig.url,
    waitMsgTarget: view,
    selModel: view.getSelectionModel(),
    method: 'PUT',
    params: { revert: keys.join(',') },
    callback: () => view.reload(),
    failure: (response) => Ext.Msg.alert('Error', response.htmlStatus),
});
```

Errors: `Ext.Msg.alert(gettext('Error'), response.htmlStatus);` (dozens of sites).
Confirmations (`pvemanagerlib.js:4384-4396`):

```js
Ext.MessageBox.defaultButton = me.dangerous ? 2 : 1;
Ext.Msg.show({
    title: gettext('Confirm'),
    icon: me.dangerous ? Ext.Msg.WARNING : Ext.Msg.QUESTION,
    msg: msg, buttons: Ext.Msg.YESNO,
    callback: function (btn) { if (btn !== 'yes') { return; } me.realHandler(button, event, rec); },
});
```

(Note the `dangerous` flag → `WARNING` icon + default button "No" — the same semantic
`ConfirmButton::dangerous(true)` carries in pwt.)

### 6.5 Overrides

There is no literal `Ext.define('Proxmox.Override...')`. The mechanism is the `override:` key
on an arbitrarily named class — 1 site in `pvemanagerlib.js`, ~15 in `proxmoxlib.js`
(`:2116,2122,2134,2170,2225,2349,2361,2398,2432,2437,2598,2605,2619,2634,2673,2679,2685,2691,2697`).
Each carries a one-line comment saying *why*:

```js
Ext.define('PVE.form.field.Display', {
    override: 'Ext.form.field.Display',
    setSubmitValue: function (value) {
        // do nothing, this is only to allow generalized bindings for the:
        // `me.isCreate ? 'textfield' : 'displayfield'` cases we have.
    },
});
```
`/usr/share/pve-manager/js/pvemanagerlib.js:4081-4088`

```js
// we always want the number in x.y format and never in, e.g., x,y
Ext.define('PVE.form.field.Number', {
    override: 'Ext.form.field.Number',
    submitLocaleSeparator: false,
});
```
`/usr/share/javascript/proxmox-widget-toolkit/proxmoxlib.js:2114-2117`

### 6.6 Guest tabs

`PVE.lxc.Config` (`pvemanagerlib.js:39427`, `extend: 'PVE.panel.Config'`,
`alias: 'widget.pveLXCConfig'`, `onlineHelp: 'chapter_pct'`); `PVE.qemu.Config` at `:54127`.
Tab entries (`:39669-39724`):

```js
items: [
    { title: gettext('Summary'), xtype: 'pveGuestSummary', iconCls: 'fa fa-book', itemId: 'summary' },
],
...
me.items.push(
    { title: gettext('Resources'), itemId: 'resources', expandedOnInit: true,
      iconCls: 'fa fa-cube', xtype: 'pveLxcRessourceView' },
    { title: gettext('Network'), iconCls: 'fa fa-exchange', itemId: 'network',
      xtype: 'pveLxcNetworkView' },
    { title: gettext('Options'), itemId: 'options', iconCls: 'fa fa-gear', xtype: 'pveLxcOptions' },
    { title: gettext('Task History'), itemId: 'tasks', iconCls: 'fa fa-list-alt',
      xtype: 'proxmoxNodeTasks', nodename: nodename, preFilter: { vmid } },
);
```

Every entry is `{ title: gettext(...), itemId, iconCls: 'fa fa-...', xtype }`. Tabs are pushed
conditionally on ACL caps (`:39730-39742`). `onlineHelp` is set once on the `Config` class, not
per tab.

> **This is the exact shape pve-meta's `pve-manager-patch` must produce** for its Metadata tab.

### 6.7 Theme (ExtJS side)

`ls /usr/share/pve-manager/css/` → `ext6-pve.css` only.
`ls /usr/share/javascript/proxmox-widget-toolkit/themes/` → `theme-proxmox-dark.css` only.

Two names plus auto (`proxmoxlib.js:123-146`):

```js
theme_map: { crisp: 'Light theme', 'proxmox-dark': 'Proxmox Dark' },
```

Selection is a cookie plus a full page reload (`proxmoxlib.js:20421-20469`,
`Proxmox.window.ThemeEditWindow`, `cookieName: 'PVEThemeCookie'`), because the decision is made
**server-side** at page render (`/usr/share/perl5/PVE/Service/pveproxy.pm:212-221`):

```perl
if (my $newtheme = ($cookie =~ /(?:^|\s)PVEThemeCookie=([^;]*)/)[0]) {
    if ($newtheme =~ m/^[a-z]{1,10}(-[a-z]{1,10}){0,5}$/) { $theme = $newtheme; }
}
```

with `$theme = "auto"` as the default (`:212`); the template then links the dark overlay either
unconditionally or wrapped in `@media (prefers-color-scheme: dark)`.

The dark overlay defines its own CSS custom properties — **note the `--pwt-` prefix collision
with the Rust widget toolkit, they are unrelated namespaces** (`theme-proxmox-dark.css:1`):

```css
:root{
  --pwt-panel-background: #262626;
  --pwt-text-color: #f2f2f2;
  --pwt-gauge-default: #0060a4;
  --pwt-gauge-back: #333;
  --pwt-gauge-warn: #ffae0b;
  --pwt-gauge-crit: #ce3c3c;
  --pwt-chart-primary: #0060a4;
  --pwt-chart-grid-stroke: #4d4d4d;
}
```

plus `.x-body{color:#f2f2f2;background-color:#1a1a1a}`. These are read live via
`getComputedStyle` in `checkThemeColors` (`proxmoxlib.js:10636-10653`) and re-applied to charts
on a `matchMedia` change listener (`:10769-10776`) — so charts track OS theme changes live even
though the rest of the skin only updates on reload.

---

## 7. Build and packaging

### 7.1 Makefile variables

Version data is **never hand-written** — it comes from `debian/changelog` via
`/usr/share/dpkg/pkg-info.mk` (or `default.mk`). `$SB/proxmox-perl-rs/pve-rs/Makefile:1-19`:

```make
include /usr/share/dpkg/pkg-info.mk

PACKAGE=libpve-rs-perl
export PERLMOD_PRODUCT=PVE

ARCH:=$(shell dpkg-architecture -qDEB_BUILD_ARCH)
PERL_INSTALLVENDORARCH != perl -MConfig -e 'print $$Config{installvendorarch};'
PERL_INSTALLVENDORLIB  != perl -MConfig -e 'print $$Config{installvendorlib};'

MAIN_DEB=$(PACKAGE)_$(DEB_VERSION)_$(ARCH).deb
DBGSYM_DEB=$(PACKAGE)-dbgsym_$(DEB_VERSION)_$(ARCH).deb
DEBS=$(MAIN_DEB) $(DBGSYM_DEB)
DSC=$(PACKAGE)_$(DEB_VERSION_UPSTREAM_REVISION).dsc
BUILDDIR ?= $(PACKAGE)-$(DEB_VERSION_UPSTREAM)

DESTDIR=
```

PDM uses the superset `/usr/share/dpkg/default.mk` and adds
`CARGO ?= cargo` with `CARGO_BUILD_ARGS += --release` gated on `BUILD_MODE`
(`$SB/proxmox-datacenter-manager/Makefile:1-27`). `GITVERSION:=$(shell git rev-parse HEAD)`
appears in `proxmox-perl-rs/common/pkg/Makefile:6` but is vestigial.

### 7.2 Targets

Common vocabulary: `all`, `install`, `clean`, `deb`, `dsc`, `sbuild`, `upload`, `dinstall`.
`pve-rs/Makefile:91,106,110,125,131,140,145` declares `install dinstall upload deb dsc doc
doc-open` as `.PHONY`; PDM's top-level `Makefile:116-177` adds
`deb-ui dsc-ui upload-ui clean-deb distclean test tidy`.

### 7.3 The `install -Dm` idiom

`$SB/proxmox-perl-rs/pve-rs/Makefile:92-98`:

```make
.PHONY: install
install: target/release/libpve_rs.so Proxmox/Lib/PVE.pm $(PERLMOD_PACKAGE_FILES)
	install -d -m755 $(DESTDIR)$(PERL_INSTALLVENDORARCH)/auto
	install -m644 target/release/libpve_rs.so $(DESTDIR)$(PERL_INSTALLVENDORARCH)/auto/libpve_rs.so
	install -d -m755 $(DESTDIR)$(PERL_INSTALLVENDORLIB)/Proxmox/Lib
	install -m644 Proxmox/Lib/PVE.pm $(DESTDIR)$(PERL_INSTALLVENDORLIB)/Proxmox/Lib/PVE.pm
	find $(PM_DIR) \! -type d -print -exec install -Dm644 '{}' $(DESTDIR)$(PERL_INSTALLVENDORLIB)'/{}' ';'
```

`find … -exec install -Dm644` (creating parent dirs per file) is the bulk-install idiom, reused
at `common/pkg/Makefile:43-44`. For a small fixed manifest, PDM's UI uses explicit
`install -dm0755` + `install -m0644` lines instead
(`$SB/proxmox-datacenter-manager/ui/Makefile:55-78`).

### 7.4 `debian/rules`

A thin `%: dh $@` with an `override_dh_auto_configure` that *cross-checks `Cargo.toml`'s
version against `debian/changelog` and fails the build on mismatch*. No explicit
`--buildsystem=cargo` — `dh-cargo (>= 25)` auto-detects it.
`$SB/proxmox-perl-rs/pve-rs/debian/rules:1-25`:

```make
#!/usr/bin/make -f

include /usr/share/dpkg/pkg-info.mk
include /usr/share/rustc/architecture.mk

export BUILD_MODE=release
CARGO=/usr/share/cargo/bin/cargo
export CARGO_HOME = $(CURDIR)/debian/cargo_home
export DEB_CARGO_CRATE=pve-rs_$(DEB_VERSION_UPSTREAM)
export DEB_CARGO_PACKAGE=pve-rs

%:
	dh $@

override_dh_auto_configure:
	@perl -ne 'if (/^version\s*=\s*"(\d+(?:\.\d+)+)"/) { my $$v_cargo = $$1; my $$v_deb = "$(DEB_VERSION_UPSTREAM)"; \
	    die "ERROR: d/changelog <-> Cargo.toml version mismatch: $$v_cargo != $$v_deb\n" if $$v_cargo ne $$v_deb; exit(0); }' Cargo.toml
	$(CARGO) prepare-debian $(CURDIR)/debian/cargo_registry --link-from-system
	dh_auto_configure
```

PDM adds `override_dh_strip` (running `debian/scripts/elf-strip-unused-dependencies.sh`),
`override_dh_installsystemd --no-start --no-restart-after-upgrade --no-stop-on-upgrade`,
`override_dh_missing: dh_missing --fail-missing`, and `override_dh_compress: dh_compress -X.pdf`
(`proxmox-datacenter-manager/debian/rules:49-50`, `ui/debian/rules:46-47`).

**Relevant to pve-meta:** PDM's *UI* `debian/rules` guards the strict version check on
`ifneq ($(DEB_DISTRIBUTION),UNRELEASED)` so dev builds skip it, and patches
`debian/cargo_home/config.toml` for a `wasm32-unknown-unknown` lld linker target after
`cargo prepare-debian` (`ui/debian/rules:23-26`).

### 7.5 Cargo invocation

`$(CARGO) build $(CARGO_BUILD_ARGS)` with `--release` gated by `BUILD_MODE`
(`proxmox-datacenter-manager/Makefile:22-27,90`, `ui/Makefile:12-18`). `pve-rs/Makefile:77`
calls bare `cargo build $(CARGO_BUILD_ARGS)` because `CARGO`/`BUILD_MODE` are already exported
from `debian/rules`. **`--locked` is never used** — reproducibility comes from
`cargo prepare-debian … --link-from-system` plus the source replacement in
`.cargo/config.toml` (§1.4), which also sets `[profile.release] debug = true`.

### 7.6 Changelog and package targets

```
libpve-rs-perl (0.15.3) trixie; urgency=medium

  * rebuild against proxmox-ve-config 0.10.3, ...

 -- Proxmox Support Team <support@proxmox.com>  Thu, 21 May 2026 11:30:35 +0200
```
`$SB/proxmox-perl-rs/pve-rs/debian/changelog:1-13`

Distribution is the codename, or the literal `UNRELEASED` for in-progress work (which is what
the `debian/rules` guard keys off).

Native-format `deb`/`dsc` (`pve-rs/Makefile:117-135`):

```make
.PHONY: deb
deb: $(DEBS)
$(DEBS) &: $(BUILDDIR)
	cd $(BUILDDIR); PATH="/usr/local/bin:/usr/bin" dpkg-buildpackage -b -us -uc
	lintian $(DEBS)

.PHONY: dsc
dsc: $(DSC)
$(DSC): $(BUILDDIR)
	cd $(BUILDDIR); PATH="/usr/local/bin:/usr/bin" dpkg-buildpackage -S -us -uc -d
	lintian $(DSC)
```

Note `lintian` is run as part of the target, not separately. Quilt-format projects build an
orig tarball first (`proxmox-datacenter-manager/Makefile:113-114,129-130`).

`upload` refuses to run on a dirty tree (`pve-rs/Makefile:110-115`):

```make
.PHONY: upload
upload: UPLOAD_DIST ?= $(DEB_DISTRIBUTION)
upload: $(DEBS)
	git diff --exit-code --stat && git diff --exit-code --stat --staged
	tar cf - $(DEBS) | ssh -X repoman@repo.proxmox.com upload --product pve --dist $(DEB_DISTRIBUTION)
```

### 7.7 perlmod bindings, as installed

One shared object per crate, plus thin generated shims. On node1:
`/usr/lib/x86_64-linux-gnu/perl5/5.40/auto/libpve_rs.so`, and
`/usr/share/perl5/PVE/RS/{CalendarEvent,Meta,NVML,OCI,OpenId,SDN,TFA}.pm` plus
`Firewall/`, `ResourceScheduling/`, `SDN/` — mirroring `PERLMOD_PACKAGES` in
`pve-rs/Makefile:28-40`. Each shim is three lines:

```perl
package PVE::RS::SDN;
use base 'Proxmox::Lib::PVE';
BEGIN { __PACKAGE__->bootstrap(); }
1;
```

with `/usr/share/perl5/Proxmox/Lib/PVE.pm` the generated base class implementing
`library()`/`find_lib()`/`load()`/`bootstrap()` over `DynaLoader`, installed by
`pve-rs/Makefile:96-97`.

---

## 8. Checklist for pve-meta

**Rust / UI**

- [ ] Add `rustfmt.toml` (`edition = "2024"`, `style_edition = "2024"`) at the repo root and in `ui/`.
- [ ] No crate-level `#![deny]`; relax specific clippy lints in `Cargo.toml` `[lints.clippy]` only if needed.
- [ ] No copyright/SPDX headers in `.rs` files; license lives in `Cargo.toml`.
- [ ] One component per file; `lib.rs` is a flat `mod X; pub use X::{…};` ledger.
- [ ] Public props struct = plain `CamelCase`; internal `Component` = `PveMeta`-prefixed, `#[doc(hidden)]`.
- [ ] Item order per §2.4; `create → update → changed → view → rendered`.
- [ ] Builder setters as `set_x`/`x` pairs, each with a `/// Builder style method to set X.` doc.
- [ ] Event props are `Option<Callback<T>>` + `#[builder_cb(IntoEventCallback, into_event_callback, T)]`.
- [ ] `loading: usize` counter, `last_load_error: Option<String>`, `anyhow::Error` in loaders.
- [ ] Wrap every user-visible string in `tr!()`; error titles `"Unable to <verb> <object>"`, confirmations end in `?`.
- [ ] `thread_local!` for `DataTable` column definitions.

**Perl (`PVE::API2::Ext::Meta`)**

- [ ] Header order: `package` / `use strict; use warnings;` / grouped `use` / `use base qw(PVE::RESTHandler);`.
- [ ] `register_method` key order; `additionalProperties => 0`; `returns` before `code`.
- [ ] `get_standard_option('pve-vmid'|'pve-node')` instead of hand-written schemas.
- [ ] `raise_param_exc({ field => "..." })` for validation, `die "...\n"` for logic errors.
- [ ] Descriptions in sentence case with a trailing period; no `Note:` lines.
- [ ] Digest round-trip: `extract_param($param,'digest')` + `PVE::Tools::assert_if_modified` inside `lock_config`.
- [ ] `permissions => { check => ['perm', '/vms/{vmid}', [...]] }` on every method.

**JS (`pve-ext` page manifest / loader)** — supersedes the pre-`pve-ext` `Ext.define(...,
{override: ...})`/`xtype: 'pveMetaTab'` approach below, which throws in real ExtJS 7
classic and silently kills the whole config panel (see `pve-ext-loader.js`'s own header
comment and `pve-ext/README.md`, "UI pages").

- [ ] Ship a page manifest (`pages/pve-meta.json`) with `id`, `title`, `iconCls`,
      `targets`, `url`, optional `requires` — see `pve-ext/README.md`, "UI pages".
- [ ] Gate the tab client-side via `requires`, checked against
      `Ext.state.Manager.get('GuiCap')` — a UX convenience only, never the access
      control; the backend API must enforce the real permission check regardless of
      whether the tab was shown.
- [ ] Patch `PVE.panel.Config.prototype.initComponent` by capturing the original
      function and calling it first, then adding the extra tab(s) — never via the
      global `Ext.override(cls, {...})` + `this.callParent(...)` shim.
- [ ] `escapeHtml(title)` and `sanitizeIconCls(iconCls)` on every manifest-sourced
      string before it reaches the DOM/ExtJS config.
- [ ] Every seam individually `try`/`catch`-guarded; a failure logs to the console
      (prefixed `[pve-ext]`) and degrades to "that one thing doesn't happen" — it must
      never be possible for a broken manifest to break the PVE UI itself.

**Packaging**

- [ ] Keep version data flowing from `debian/changelog` (`pkg-info.mk`/`default.mk`), never hand-set.
- [x] Add the `Cargo.toml` ⇄ `changelog` version cross-check to `override_dh_auto_configure`, guarded on `DEB_DISTRIBUTION != UNRELEASED`.
- [ ] `install -Dm644` / `find … -exec install -Dm644` for file installation.
- [x] Run `lintian` from the `deb`/`dsc` targets (both `Makefile:deb` and `pve-ext/Makefile:deb`; fatal when `$CI` is set, `|| true` locally — see `pve-ext/README.md`, "Managed patches" build notes, and `docs/DESIGN.md` §9).
- [ ] `upload` guarded by `git diff --exit-code`.
