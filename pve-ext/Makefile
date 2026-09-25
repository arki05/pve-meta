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
	cp patches/pve-manager.json $(DESTDIR)$(PREFIX)/share/pve-ext/patches/
	cp patches/*.diff $(DESTDIR)$(PREFIX)/share/pve-ext/patches/
	mkdir -p $(DESTDIR)$(PREFIX)/share/pve-ext/pages
	mkdir -p $(DESTDIR)$(PREFIX)/share/perl5/PVE/API2/Ext

# lintian is fatal under $CI and advisory locally, as for pve-meta's own packages.
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
