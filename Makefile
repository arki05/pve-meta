# Top-level build orchestration for pve-meta.
#
# The build host uses rustup (~/.cargo/bin), not distro cargo/rustc packages, so `cargo`
# resolves to the rustup one when present. Two rustup-specific gotchas handled here, both hit
# under `dpkg-buildpackage`/`dh_auto_*` (debhelper compat 13 runs build steps with `HOME`
# rewritten to a fake, empty `debian/.debhelper/generated/_source/home` sandbox for
# reproducible builds):
#   1. Finding the `cargo` binary: $(wildcard ...), not `$(shell test -x $(HOME)/...)` --
#      under that fake HOME the wildcard just doesn't match and falls through to the
#      hardcoded `/root/.cargo/bin/cargo` (this project's build host only ever builds as
#      root).
#   2. Running it: rustup's `cargo` is a proxy that resolves its default toolchain from
#      `$RUSTUP_HOME` (default `$HOME/.rustup`) -- under that same fake HOME it finds no
#      `settings.toml` and fails with "no default toolchain configured" even though the
#      binary itself was found fine. Exporting RUSTUP_HOME/CARGO_HOME explicitly (only if not
#      already set) fixes this regardless of what HOME is. Same $(wildcard) trick as (1),
#      for the same reason: a developer's own ~/.rustup when it exists -- `make test` and
#      `make doc` do run on a workstation -- and the build host's /root otherwise, which
#      is also where the fake HOME lands. (`make check` stays build-host only: its clippy
#      run covers the whole workspace, and pve-meta-perl needs libperl-dev.)
export RUSTUP_HOME ?= $(firstword $(wildcard $(HOME)/.rustup /root/.rustup) /root/.rustup)
export CARGO_HOME ?= $(firstword $(wildcard $(HOME)/.cargo /root/.cargo) /root/.cargo)
CARGO ?= $(firstword $(wildcard $(CARGO_HOME)/bin/cargo $(HOME)/.cargo/bin/cargo /root/.cargo/bin/cargo) cargo)

DESTDIR ?=
PREFIX ?= /usr

UI_DIR := ui-extjs
MONACO := $(UI_DIR)/monaco/vs
# The browser build of pve-meta-core (crates/pve-meta-wasm): the same crate the
# perlmod bindings link, compiled for wasm32 and shipped next to the editor. Needs
# the target installed on the build host -- `rustup target add
# wasm32-unknown-unknown`, or Debian's `libstd-rust-dev-wasm32` with a distro rustc.
# No other tool: no wasm-bindgen, no wasm-pack, no npm (see docs/WASM-CORE.md).
WASM_TARGET := wasm32-unknown-unknown
WASM := target/$(WASM_TARGET)/wasm/pve_meta_wasm.wasm

.PHONY: build ui wasm deb install clean check check-perl test doc

build: wasm
	$(MAKE) -C crates/pve-meta-perl BUILD_MODE=release

wasm:
	$(CARGO) build -p pve-meta-wasm --target $(WASM_TARGET) --profile wasm

# Fetches Monaco into ui-extjs/monaco/vs. The editor is a plain JS panel with no build
# step of its own (see ui-extjs/README.md); the only thing to fetch is Monaco's minified
# AMD tree, which ships *in the package* and is never loaded from a CDN.
#
# Tolerant of `npm` being absent: a package built without it simply has no Text card and
# no diff dialog, which is a degraded editor rather than a failed build. `make deb` in CI
# has npm (see .github/workflows/build.yml).
ui:
	@if [ -d $(MONACO) ]; then \
		echo "monaco already vendored in $(MONACO)"; \
	elif command -v npm >/dev/null 2>&1; then \
		(cd $(UI_DIR) && npm install --no-audit --no-fund); \
		mkdir -p $(MONACO); \
		cp -a $(UI_DIR)/node_modules/monaco-editor/min/vs/. $(MONACO)/; \
	else \
		echo "warning: npm not found, packaging without Monaco (no Text card, no diff)" >&2; \
	fi

# Per docs/DESIGN.md section 11, pve-meta is a consumer of pve-ext's three generic
# seams (see pve-ext/README.md), not a package that patches PVE itself:
#   - API module:    perl/PVE/API2/Ext/Meta.pm, discovered by PVE::API2::Ext at
#                     pvedaemon/pveproxy startup -- no registration diff needed.
#   - UI pages:      pages/pve-meta.json and pages/pve-meta-dc.json, discovered by
#                     pve-ext-loader.js.
#   - Managed patch: patches/lifecycle.toml + patches/lifecycle/*.diff, applied by
#                     `pve-ext-patch apply pve-meta-lifecycle` from debian/pve-meta.postinst.
# Two binary packages come from this source (see debian/control):
#   - `pve-meta`:            install (below)
#   - `libpve-meta-rs-perl`: crates/pve-meta-perl's own `install` target, invoked with
#     its own DESTDIR directly from debian/rules (see crates/pve-meta-perl/PACKAGING.md).
#
# Everything in the `pve-meta` package: the native PVE::API2::Ext::Meta module
# (docs/DESIGN.md section 8), the ExtJS editor tab with its core `.wasm` and
# vendored Monaco (served by pveproxy), the two CLIs, and the pve-ext page/patch
# manifests.
install:
	if [ -f perl/PVE/API2/Ext/Meta.pm ]; then \
		install -D -m 0644 perl/PVE/API2/Ext/Meta.pm $(DESTDIR)$(PREFIX)/share/perl5/PVE/API2/Ext/Meta.pm; \
	else \
		echo "warning: perl/PVE/API2/Ext/Meta.pm not present yet, skipping" >&2; \
	fi
	# pve-ext managed-patch manifest for the guest-lifecycle hooks
	# (see patches/lifecycle.toml, patches/lifecycle/, pve-ext/README.md;
	# the whole lifecycle is one patched file, see docs/LIFECYCLE-PATCHES.md).
	# Installed under its own declared `id` (patches/lifecycle.toml's
	# top-level `id = "..."` field), not its checkout filename -- so that
	# pve-ext-patch's claim identity, read from the manifest's own content
	# rather than whatever path/basename it was invoked with, is the same
	# whether run against this checkout or the installed package (see
	# pve-ext/bin/pve-ext-patch's header comment, "manifest_id"). Resolved
	# via pve-ext-patch's own "manifest-id" subcommand rather than a second,
	# independently-drifting awk parser of the same TOML rule (this
	# Makefile's own copy had already drifted: it lacked the diff tool's
	# trailing-comment strip and single-quote support when a review compared
	# the two).
	lifecycle_id="$$(pve-ext/bin/pve-ext-patch manifest-id patches/lifecycle.toml)" || exit 1; \
	install -D -m 0644 patches/lifecycle.toml $(DESTDIR)$(PREFIX)/share/pve-ext/patches/$$lifecycle_id.toml
	mkdir -p $(DESTDIR)$(PREFIX)/share/pve-ext/patches/lifecycle
	cp patches/lifecycle/*.diff $(DESTDIR)$(PREFIX)/share/pve-ext/patches/lifecycle/
	# The local reader (bin/pve-meta, docs/DESIGN.md section 10). In sbin because
	# it reads /etc/pve/meta directly and so is root's tool, not an API client's:
	# a hook script runs as root on the node, often before pveproxy is reachable.
	install -D -m 0755 bin/pve-meta $(DESTDIR)$(PREFIX)/sbin/pve-meta
	# The example hook script: what this system is for with no operator anywhere
	# near it. Not executable in place -- an administrator copies it into a
	# storage's snippets directory, which is where PVE looks for hookscripts.
	install -D -m 0644 examples/maintenance-hook.pl \
		$(DESTDIR)$(PREFIX)/share/doc/pve-meta/examples/maintenance-hook.pl
	# Packaged example prefixes (docs/DESIGN.md section 3); none are
	# required for pve-meta to work, so this directory may be empty in a
	# checkout that hasn't added any yet -- `mkdir -p` plus a tolerant glob
	# copy, never a hard failure.
	#
	# There is deliberately no packaged *permissions* directory: an operator's
	# package may ship a prefix (a declaration) but must never ship its
	# own permissions, and dpkg cannot write into pmxcfs (docs/DESIGN.md 3.2).
	mkdir -p $(DESTDIR)$(PREFIX)/share/pve-meta/prefixes
	if [ -d prefixes ] && ls prefixes/*.yaml >/dev/null 2>&1; then \
		cp prefixes/*.yaml $(DESTDIR)$(PREFIX)/share/pve-meta/prefixes/; \
	fi
	# pve-ext UI-page manifests for the editor (see pages/, docs/DESIGN.md
	# section 8) and its static files. Both are the `script`+`xtype` form: pve-ext's
	# loader defines the class and puts a native ExtJS panel in the tab, so there is
	# no iframe. Two manifests, one file: they name different `xtype`s
	# (a guest's document editor, and the datacenter's three sub-tabs) out of the
	# same script, which the loader fetches once.
	install -D -m 0644 pages/pve-meta.json $(DESTDIR)$(PREFIX)/share/pve-ext/pages/pve-meta.json
	install -D -m 0644 pages/pve-meta-dc.json $(DESTDIR)$(PREFIX)/share/pve-ext/pages/pve-meta-dc.json
	mkdir -p $(DESTDIR)$(PREFIX)/share/pve-manager/js/pve-meta-extjs
	if ls ui-extjs/*.js >/dev/null 2>&1; then \
		cp ui-extjs/*.js $(DESTDIR)$(PREFIX)/share/pve-manager/js/pve-meta-extjs/; \
	fi
	# The core, built for the browser by the `wasm` target and loaded lazily by
	# pve-meta-tree.js from .../js/pve-meta-extjs/pve-meta-core.wasm
	# (`PVE.meta.Core.SRC`). Not optional: without it the editor cannot read a
	# document, so a missing build is a failed install rather than a degraded one.
	install -D -m 0644 $(WASM) $(DESTDIR)$(PREFIX)/share/pve-manager/js/pve-meta-extjs/pve-meta-core.wasm
	# Monaco's minified AMD tree, vendored by the `ui` target above and served
	# from .../js/pve-meta-extjs/vs (pve-meta-tree.js's `VS` constant). Absent
	# when the build host had no npm; the panel degrades rather than breaking.
	if [ -d $(MONACO) ]; then \
		mkdir -p $(DESTDIR)$(PREFIX)/share/pve-manager/js/pve-meta-extjs/vs; \
		cp -a $(MONACO)/. $(DESTDIR)$(PREFIX)/share/pve-manager/js/pve-meta-extjs/vs/; \
	fi

# `make deb` builds every package this repo ships, in one call:
#   - pve-ext:                    its own source package, built via `make -C pve-ext deb`.
#     dpkg-buildpackage places pve-ext's artifacts in the parent of `pve-ext/`, i.e. this
#     directory -- move them up one more level so they land next to pve-meta's own output
#     (dpkg-buildpackage always drops artifacts in the parent of the source root it's
#     invoked from).
#   - pve-meta, libpve-meta-rs-perl: this source package's two binaries (see debian/control).
#
# lintian runs as part of this target, not as a separate step (see
# docs/design/PROXMOX-CONVENTIONS.md section 7.6/8, docs/DESIGN.md section 13,
# pve-ext/README.md "Building and packaging"), mirroring the plain
# `lintian $(DEBS)` upstream pve-rs uses (no --fail-on override: lintian's
# own default -- exit non-zero only on an E: tag -- is what "fatal" below
# means; a W: is printed but does not fail the build, same as upstream).
# `|| true` for a local/dev build (unset $CI) so an unrelated lintian nag
# never blocks iterating locally; unconditionally fatal when $CI is set
# (matches .github/workflows/build.yml, which sets it automatically) --
# CI is the actual gate. `pve-ext`'s own artifacts were already moved into
# ".." above, so all three packages' .debs are lintianed together here.
deb:
	$(MAKE) -C pve-ext deb
	for f in pve-ext_*.deb pve-ext_*.buildinfo pve-ext_*.changes; do [ -e "$$f" ] && mv -f "$$f" ..; done
	dpkg-buildpackage -b -us -uc -d
	if [ -n "$$CI" ]; then \
		lintian ../pve-meta_*.deb ../libpve-meta-rs-perl_*.deb ../pve-ext_*.deb; \
	else \
		lintian ../pve-meta_*.deb ../libpve-meta-rs-perl_*.deb ../pve-ext_*.deb || true; \
	fi

# A broken intra-doc link is a stale doc comment pointing at something that no longer
# exists -- exactly the rot nothing else catches, and how the one real broken link in
# this crate was found, under ten warnings about deliberate links to private items.
# Those are allowed crate-wide in lib.rs, so this only fires on a link resolving to
# nothing. Its own target as well as part of `check`: unlike clippy's whole-workspace
# run it needs no libperl, so it works on a workstation.
doc:
	RUSTDOCFLAGS="-D warnings" $(CARGO) doc --no-deps -p pve-meta-core

check: doc wasm
	$(CARGO) clippy --workspace -- -D warnings
	# The editor's offline suite loads the `.wasm` the package ships and drives
	# the editor's helpers through it: the codec round trip, the row builder,
	# the staging model and the schema findings, all answered by the core.
	@if command -v node >/dev/null 2>&1; then \
		node ui-extjs/testing/smoke.js; \
	else \
		echo "warning: node not found, skipping the ui-extjs smoke suite" >&2; \
	fi

# Every shell and Perl file, compiled but not run. The PVE modules the Perl
# files `use` are stubbed (scripts/perl-stubs) so this runs on a laptop and in
# CI's plain Debian container; on a PVE host `perl -c` without -I is stricter.
# Only PVE's own modules are stubbed: pve-ext's loader uses JSON, which is a
# real dependency (libjson-perl, always present on a PVE node) and has to be
# installed wherever this runs.
# pve-ext-patch rewrites files inside pve-manager and libpve-guest-common-perl,
# which is the strongest reason for it to be the one script shellcheck sees.
check-perl:
	shellcheck pve-ext/bin/pve-ext-patch
	@for f in perl/PVE/API2/Ext/Meta.pm pve-ext/perl/PVE/API2/Ext.pm \
	          bin/pve-meta examples/maintenance-hook.pl; do \
		perl -Iscripts/perl-stubs -c $$f || exit 1; \
	done

test:
	$(CARGO) test -p pve-meta-core -p pve-meta-wasm

clean:
	$(CARGO) clean
