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

.PHONY: build ui deb install install-cli install-daemon clean check test

build:
	$(CARGO) build --release -p pve-metad -p pve-meta-cli
	$(MAKE) -C crates/pve-meta-perl BUILD_MODE=release

# Builds the wasm editor UI with trunk. Falls back to a minimal static placeholder if `trunk`
# is not installed, so packaging/install/the API-only smoke test still work without the full
# wasm toolchain (the UI itself is specified separately in docs/UI-SPEC.md).
ui:
	@if command -v trunk >/dev/null 2>&1; then \
		cd $(UI_DIR) && trunk build --release; \
	else \
		echo "warning: trunk not found, shipping a placeholder UI (see docs/UI-SPEC.md)" >&2; \
		mkdir -p $(UI_DIST); \
		printf '<!doctype html><html><head><title>pve-meta</title></head><body><p>pve-meta UI not built (trunk unavailable at package build time).</p></body></html>' > $(UI_DIST)/index.html; \
	fi

# Per docs/NATIVE-API-SPEC.md the API is served natively by pveproxy/pvedaemon
# (PVE::API2::Meta); pve-metad (the standalone HTTPS daemon) is a separate, optional
# package. Three binary packages come from this source (see debian/control):
#   - `pve-meta`:            install-cli (below)
#   - `pve-metad`:           install-daemon (below)
#   - `libpve-meta-rs-perl`: crates/pve-meta-perl's own `install` target, invoked with
#     its own DESTDIR directly from debian/rules (see crates/pve-meta-perl/PACKAGING.md).
install: install-cli install-daemon

# Everything in the `pve-meta` package: the CLI, the native PVE::API2::Meta module (if
# it's been generated yet -- see docs/NATIVE-API-SPEC.md, written alongside this by
# another part of the project), the wasm editor UI (served by pveproxy, not pve-metad),
# and the web UI / guest-lifecycle integration patch tools.
install-cli:
	install -D -m 0755 target/release/pve-meta $(DESTDIR)$(PREFIX)/bin/pve-meta
	if [ -f perl/PVE/API2/Meta.pm ]; then \
		install -D -m 0644 perl/PVE/API2/Meta.pm $(DESTDIR)$(PREFIX)/share/perl5/PVE/API2/Meta.pm; \
	else \
		echo "warning: perl/PVE/API2/Meta.pm not present yet, skipping" >&2; \
	fi
	mkdir -p $(DESTDIR)$(PREFIX)/share/pve-manager/js/pve-meta-ui
	if [ -d $(UI_DIST) ]; then cp -a $(UI_DIST)/. $(DESTDIR)$(PREFIX)/share/pve-manager/js/pve-meta-ui/; fi
	# pve-manager web UI "Metadata" tab injection (see pve-manager-patch/).
	install -D -m 0755 pve-manager-patch/pve-meta-patch $(DESTDIR)$(PREFIX)/sbin/pve-meta-patch
	install -D -m 0644 pve-manager-patch/pve-meta-loader.js $(DESTDIR)$(PREFIX)/share/pve-meta/pve-meta-loader.js
	# Guest lifecycle hook + native-API-registration patch (see
	# pve-manager-patches/lifecycle/): the *.diff files always ship;
	# pve-meta-lifecycle-patch itself is tolerant of not (yet) existing.
	mkdir -p $(DESTDIR)$(PREFIX)/share/pve-meta/lifecycle
	cp pve-manager-patches/lifecycle/*.diff $(DESTDIR)$(PREFIX)/share/pve-meta/lifecycle/ 2>/dev/null || true
	if [ -f pve-manager-patches/lifecycle/pve-meta-lifecycle-patch ]; then \
		install -D -m 0755 pve-manager-patches/lifecycle/pve-meta-lifecycle-patch $(DESTDIR)$(PREFIX)/sbin/pve-meta-lifecycle-patch; \
	else \
		echo "warning: pve-manager-patches/lifecycle/pve-meta-lifecycle-patch not present yet, skipping" >&2; \
	fi

# The `pve-metad` package: the standalone, optional HTTPS daemon + its systemd unit.
# Nothing else -- it does not need the CLI, the UI, or either patch tool.
install-daemon:
	install -D -m 0755 target/release/pve-metad $(DESTDIR)$(PREFIX)/sbin/pve-metad
	install -D -m 0644 debian/pve-metad.service $(DESTDIR)$(PREFIX)/lib/systemd/system/pve-metad.service

deb:
	dpkg-buildpackage -b -us -uc -d

check:
	$(CARGO) clippy --workspace --exclude pve-meta-ui -- -D warnings

test:
	$(CARGO) test -p pve-meta-core -p pve-meta-api

clean:
	$(CARGO) clean
	rm -rf $(UI_DIST)
