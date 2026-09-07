# Builds `PVE::RS::Meta` (libpve-meta-rs-perl): the Rust cdylib plus its
# generated Perl glue, modelled on upstream `pve-rs/Makefile`.
#
# Requires (on the build host): a `perl` interpreter with `libperl-dev`
# installed (provides `<archlib>/CORE`, used by perlmod's build.rs), and a
# Rust toolchain (`cargo`/`rustc`).

PACKAGE=libpve-meta-rs-perl

PERL_INSTALLVENDORARCH != perl -MConfig -e 'print $$Config{installvendorarch};'
PERL_INSTALLVENDORLIB != perl -MConfig -e 'print $$Config{installvendorlib};'

DESTDIR=

PM_DIR := PVE

PERLMOD_GENPACKAGE := perl $(CURDIR)/genpackage.pl \
	--lib=pve_meta_rs \
	--lib-tag=pvemeta \
	--lib-package=Proxmox::Lib::PVEMeta \
	--lib-prefix=PVEMeta

PERLMOD_PACKAGES := PVE::RS::Meta

PERLMOD_PACKAGE_FILES := $(addsuffix .pm,$(subst ::,/,$(PERLMOD_PACKAGES)))

ifeq ($(BUILD_MODE), release)
CARGO_BUILD_ARGS += --release
TARGET_DIR=release
else
TARGET_DIR=debug
endif

.PHONY: all
all: PVE
	cargo build $(CARGO_BUILD_ARGS) -p pve-meta-rs
	mkdir -p test/Proxmox/Lib
	sed -r -e \
	  's@^sub libfile.*$$@sub libfile { "$(CURDIR)/../../target/$(TARGET_DIR)/libpve_meta_rs.so"; }@' \
	  Proxmox/Lib/PVEMeta.pm >test/Proxmox/Lib/PVEMeta.pm

Proxmox: Proxmox/Lib/PVEMeta.pm
PVE: $(PERLMOD_PACKAGE_FILES)
Proxmox/Lib/PVEMeta.pm $(PERLMOD_PACKAGE_FILES) &: genpackage.pl
	$(PERLMOD_GENPACKAGE) $(PERLMOD_PACKAGES)

.PHONY: check
check: all
	$(MAKE) -C test test

.PHONY: install
install: ../../target/release/libpve_meta_rs.so Proxmox/Lib/PVEMeta.pm $(PERLMOD_PACKAGE_FILES)
	install -d -m755 $(DESTDIR)$(PERL_INSTALLVENDORARCH)/auto
	install -m644 ../../target/release/libpve_meta_rs.so $(DESTDIR)$(PERL_INSTALLVENDORARCH)/auto/libpve_meta_rs.so
	install -d -m755 $(DESTDIR)$(PERL_INSTALLVENDORLIB)
	install -d -m755 $(DESTDIR)$(PERL_INSTALLVENDORLIB)/Proxmox/Lib
	install -m644 Proxmox/Lib/PVEMeta.pm $(DESTDIR)$(PERL_INSTALLVENDORLIB)/Proxmox/Lib/PVEMeta.pm
	find $(PM_DIR) \! -type d -print -exec install -Dm644 '{}' $(DESTDIR)$(PERL_INSTALLVENDORLIB)'/{}' ';'

.PHONY: clean
clean:
	rm -rf PVE Proxmox test/Proxmox
	cargo clean -p pve-meta-rs
