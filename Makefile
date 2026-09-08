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
#      already set) fixes this regardless of what HOME is.
export RUSTUP_HOME ?= /root/.rustup
export CARGO_HOME ?= /root/.cargo
CARGO ?= $(firstword $(wildcard $(CARGO_HOME)/bin/cargo $(HOME)/.cargo/bin/cargo /root/.cargo/bin/cargo) cargo)

DESTDIR ?=
PREFIX ?= /usr

UI_DIR := ui
UI_DIST := $(UI_DIR)/dist

.PHONY: build ui deb install clean check test

build:
	$(MAKE) -C crates/pve-meta-perl BUILD_MODE=release

# Builds the wasm editor UI with trunk. Falls back to a minimal static placeholder if `trunk`
# is not installed, so packaging/install/the API-only smoke test still work without the full
# wasm toolchain (the UI itself is specified in docs/DESIGN.md section 6).
#
# `npm install` fetches Monaco, which trunk copies into dist/vs -- the editor is shipped in
# the package, never loaded from a CDN (see ui/README.md).
ui:
	@if command -v trunk >/dev/null 2>&1; then \
		cd $(UI_DIR) && npm install && trunk build --release; \
	else \
		echo "warning: trunk not found, shipping a placeholder UI (see docs/UI-SPEC.md)" >&2; \
		mkdir -p $(UI_DIST); \
		printf '<!doctype html><html><head><title>pve-meta</title></head><body><p>pve-meta UI not built (trunk unavailable at package build time).</p></body></html>' > $(UI_DIST)/index.html; \
	fi

# Per docs/DESIGN.md section 5, pve-meta is a consumer of pve-ext's three generic
# seams (see pve-ext/README.md), not a package that patches PVE itself:
#   - API module:    perl/PVE/API2/Ext/Meta.pm, discovered by PVE::API2::Ext at
#                     pvedaemon/pveproxy startup -- no registration diff needed.
#   - UI page:       pages/pve-meta.json, discovered by pve-ext-loader.js.
#   - Managed patch: patches/lifecycle.toml + patches/lifecycle/*.diff, applied by
#                     `pve-ext-patch apply pve-meta-lifecycle` from debian/pve-meta.postinst.
# Two binary packages come from this source (see debian/control):
#   - `pve-meta`:            install (below)
#   - `libpve-meta-rs-perl`: crates/pve-meta-perl's own `install` target, invoked with
#     its own DESTDIR directly from debian/rules (see crates/pve-meta-perl/PACKAGING.md).
#
# Everything in the `pve-meta` package: the native PVE::API2::Ext::Meta module (if
# it's been generated yet -- see docs/DESIGN.md section 3), the wasm editor UI
# (served by pveproxy), and the pve-ext page/patch manifests.
install:
	if [ -f perl/PVE/API2/Ext/Meta.pm ]; then \
		install -D -m 0644 perl/PVE/API2/Ext/Meta.pm $(DESTDIR)$(PREFIX)/share/perl5/PVE/API2/Ext/Meta.pm; \
	else \
		echo "warning: perl/PVE/API2/Ext/Meta.pm not present yet, skipping" >&2; \
	fi
	# Note: dist/ itself contains a "js" subdirectory (ui/index.html's
	# `data-target-path="js"` link for js/pve-meta-monaco.js), so this
	# lands at .../pve-manager/js/pve-meta-ui/js/pve-meta-monaco.js -- a
	# visually doubled "js" segment, but not a bug: index.html's own
	# `<script src="js/pve-meta-monaco.js">` is relative to itself, and
	# both files move together. Not worth reshaping (would mean changing
	# ui/index.html's copy-file target path) for a cosmetic doubling.
	mkdir -p $(DESTDIR)$(PREFIX)/share/pve-manager/js/pve-meta-ui
	if [ -d $(UI_DIST) ]; then cp -a $(UI_DIST)/. $(DESTDIR)$(PREFIX)/share/pve-manager/js/pve-meta-ui/; fi
	# pve-ext UI-page manifest (see pages/pve-meta.json, pve-ext/README.md).
	install -D -m 0644 pages/pve-meta.json $(DESTDIR)$(PREFIX)/share/pve-ext/pages/pve-meta.json
	# pve-ext managed-patch manifest for the guest-lifecycle snapshot hooks
	# (see patches/lifecycle.toml, patches/lifecycle/, pve-ext/README.md;
	# lifecycle is snapshot-only, see docs/LIFECYCLE-PATCHES.md).
	# Installed under its own declared `id` (patches/lifecycle.toml's
	# top-level `id = "..."` field), not its checkout filename -- so that
	# pve-ext-patch's claim identity, read from the manifest's own content
	# rather than whatever path/basename it was invoked with, is the same
	# whether run against this checkout or the installed package (see
	# pve-ext/bin/pve-ext-patch's header comment, "manifest_id"). Resolved
	# via pve-ext-patch's own "manifest-id" subcommand rather than a second,
	# independently-drifting awk parser of the same TOML rule (this
	# Makefile's own copy used to lack the diff tool's trailing-comment
	# strip and single-quote support -- see docs/REVIEW-2026-09-08-pass3.md,
	# Makefile:79 finding).
	lifecycle_id="$$(pve-ext/bin/pve-ext-patch manifest-id patches/lifecycle.toml)" || exit 1; \
	install -D -m 0644 patches/lifecycle.toml $(DESTDIR)$(PREFIX)/share/pve-ext/patches/$$lifecycle_id.toml
	mkdir -p $(DESTDIR)$(PREFIX)/share/pve-ext/patches/lifecycle
	cp patches/lifecycle/*.diff $(DESTDIR)$(PREFIX)/share/pve-ext/patches/lifecycle/
	# The manual GC broom (see docs/DESIGN.md section 6). Guest create and
	# destroy now clear metadata themselves, so nothing runs this on a timer;
	# it stays for the one case the hooks cannot cover -- a config removed
	# out of band, or a destroy that never ran because its node was down --
	# and an administrator runs it by hand.
	install -D -m 0755 libexec/gc $(DESTDIR)$(PREFIX)/libexec/pve-meta/gc
	# Packaged example operator registrations (docs/DESIGN.md section 3);
	# none are required for pve-meta to work, so this directory may be
	# empty in a checkout that hasn't added any yet -- `mkdir -p` plus a
	# tolerant glob copy, never a hard failure.
	mkdir -p $(DESTDIR)$(PREFIX)/share/pve-meta/operators
	if [ -d operators ] && ls operators/*.yaml >/dev/null 2>&1; then \
		cp operators/*.yaml $(DESTDIR)$(PREFIX)/share/pve-meta/operators/; \
	fi
	# pve-ext UI-page manifest for the ExtJS editor (see
	# pages/pve-meta-extjs.json, docs/DESIGN.md section 8) and its static
	# files, served the same way as the wasm UI. ui-extjs/ is a sibling
	# project directory maintained separately; tolerate it not existing
	# yet (or not being built) exactly like $(UI_DIST) above.
	install -D -m 0644 pages/pve-meta-extjs.json $(DESTDIR)$(PREFIX)/share/pve-ext/pages/pve-meta-extjs.json
	mkdir -p $(DESTDIR)$(PREFIX)/share/pve-manager/js/pve-meta-extjs
	if ls ui-extjs/*.js >/dev/null 2>&1; then \
		cp ui-extjs/*.js $(DESTDIR)$(PREFIX)/share/pve-manager/js/pve-meta-extjs/; \
	fi
	# ui-extjs/vendor/: js-yaml's dist bundle plus its LICENSE, loaded lazily
	# by pve-meta-tree.js from .../js/pve-meta-extjs/vendor/ (see
	# debian/copyright's js-yaml stanza and ui-extjs/README.md).
	mkdir -p $(DESTDIR)$(PREFIX)/share/pve-manager/js/pve-meta-extjs/vendor
	if [ -d ui-extjs/vendor ]; then \
		cp -a ui-extjs/vendor/. $(DESTDIR)$(PREFIX)/share/pve-manager/js/pve-meta-extjs/vendor/; \
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
# docs/design/PROXMOX-CONVENTIONS.md section 7.6/8, docs/DESIGN.md section 9,
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

check:
	# `ui/` is its own separate Cargo workspace (see the root Cargo.toml's
	# `exclude = ["ui"]`), never a member of this one -- `--exclude
	# pve-meta-ui` used to name a package that can never match anything
	# here (cargo only warns and ignores it), so there is nothing to
	# exclude; run `cargo clippy` from inside ui/ separately to lint it.
	$(CARGO) clippy --workspace -- -D warnings

test:
	$(CARGO) test -p pve-meta-core

clean:
	$(CARGO) clean
	rm -rf $(UI_DIST)
