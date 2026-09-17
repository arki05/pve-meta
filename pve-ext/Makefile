# Build orchestration for pve-ext (see README.md).
#
# Pure Perl + shell + JS: nothing to compile, Architecture: all. `install`
# is the only thing debian/rules calls; `check`/`test` are for local/CI use.

DESTDIR ?=
PREFIX ?= /usr

.PHONY: build install deb check test clean

build:
	@: # nothing to build

install:
	install -D -m 0644 perl/PVE/API2/Ext.pm $(DESTDIR)$(PREFIX)/share/perl5/PVE/API2/Ext.pm
	mkdir -p $(DESTDIR)$(PREFIX)/share/perl5/PVE/API2/Ext
	install -D -m 0644 js/pve-ext-loader.js $(DESTDIR)$(PREFIX)/share/pve-manager/js/pve-ext-loader.js
	install -D -m 0755 bin/pve-ext-patch $(DESTDIR)$(PREFIX)/sbin/pve-ext-patch
	install -D -m 0644 man/pve-ext-patch.8 $(DESTDIR)$(PREFIX)/share/man/man8/pve-ext-patch.8
	mkdir -p $(DESTDIR)$(PREFIX)/share/pve-ext/patches
	cp patches/pve-manager.toml $(DESTDIR)$(PREFIX)/share/pve-ext/patches/
	cp patches/*.diff $(DESTDIR)$(PREFIX)/share/pve-ext/patches/
	mkdir -p $(DESTDIR)$(PREFIX)/share/pve-ext/pages
	mkdir -p $(DESTDIR)$(PREFIX)/share/perl5/PVE/API2/Ext

# lintian runs as part of this target, not as a separate step (see
# docs/reference/PROXMOX-CONVENTIONS.md section 7.6/8, docs/DESIGN.md section 9,
# README.md "Building and packaging"), mirroring upstream pve-rs's plain
# `lintian $(DEBS)` (no --fail-on override, so only an E: tag fails the
# build; a W: is printed, not fatal, same as upstream). `|| true` for a
# local/dev build (unset $CI); unconditionally fatal when $CI is set
# (matches .github/workflows/build.yml, which sets it automatically).
# dpkg-buildpackage drops this package's artifacts in the parent of this
# directory -- the repo root, or one level further up when this target is
# invoked from the root Makefile's own `deb` (which then lintians the same
# .deb again alongside pve-meta's own two).
deb:
	dpkg-buildpackage -b -us -uc -d
	if [ -n "$$CI" ]; then \
		lintian ../pve-ext_*.deb; \
	else \
		lintian ../pve-ext_*.deb || true; \
	fi

# Local/CI sanity checks -- not run automatically by dpkg-buildpackage
# (see debian/rules' override_dh_auto_test): perl -c needs the PVE perl
# modules installed (only true on a real PVE host / build host), and
# shellcheck may not be present in every build environment.
check:
	shellcheck bin/pve-ext-patch
	perl -c perl/PVE/API2/Ext.pm

test: check

clean:
	@: # nothing to clean
