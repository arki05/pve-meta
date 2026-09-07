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

# NOTE (important, do not "simplify" the .so install back into a plain copy):
# `libpve_meta_rs.so` is `mmap`ed by every running pveproxy/pvedaemon process
# (`DynaLoader::dl_load_file` from `Proxmox/Lib/PVEMeta.pm`). Its text pages are
# file-backed and MAP_PRIVATE but never written to, so they are *never* copied on
# write: rewriting the bytes of that same inode swaps the code out from under
# every process that has it open. Those daemons never call back into the library
# once they are up, so nothing goes wrong until they exit -- at which point
# glibc's `__run_exit_handlers` has ld.so's `_dl_fini` call the library's
# `DT_FINI_ARRAY` destructor, the one and only piece of our code that runs at
# shutdown, and it executes bytes belonging to a different build. That is the
# `pveproxy worker[...]: segfault ... in libpve_meta_rs.so` / `pvedaemon.service:
# Main process exited, code=killed, status=11/SEGV` crash: purely a deployment
# artefact of writing through the installed file, reproducible on demand with a
# plain `cp new.so /usr/lib/.../auto/libpve_meta_rs.so` on a live node.
#
# The destination must therefore always be *replaced* (a new inode), never
# written through. `install`(1) unlinks the destination first and `dpkg` unpacks
# to `.dpkg-new` and renames, so both are already safe; installing via a
# temporary file plus `mv` (rename(2)) makes that guarantee explicit and holds
# even for a non-GNU `install`. Any ad-hoc deploy onto a live node
# (`cp`/`scp`/`rsync --inplace`/`cat >`) must do the same -- see docs/BUILD.md.
.PHONY: install
install: ../../target/release/libpve_meta_rs.so Proxmox/Lib/PVEMeta.pm $(PERLMOD_PACKAGE_FILES)
	install -d -m755 $(DESTDIR)$(PERL_INSTALLVENDORARCH)/auto
	install -m644 ../../target/release/libpve_meta_rs.so $(DESTDIR)$(PERL_INSTALLVENDORARCH)/auto/libpve_meta_rs.so.new
	mv -f $(DESTDIR)$(PERL_INSTALLVENDORARCH)/auto/libpve_meta_rs.so.new $(DESTDIR)$(PERL_INSTALLVENDORARCH)/auto/libpve_meta_rs.so
	install -d -m755 $(DESTDIR)$(PERL_INSTALLVENDORLIB)
	install -d -m755 $(DESTDIR)$(PERL_INSTALLVENDORLIB)/Proxmox/Lib
	install -m644 Proxmox/Lib/PVEMeta.pm $(DESTDIR)$(PERL_INSTALLVENDORLIB)/Proxmox/Lib/PVEMeta.pm
	find $(PM_DIR) \! -type d -print -exec install -Dm644 '{}' $(DESTDIR)$(PERL_INSTALLVENDORLIB)'/{}' ';'

.PHONY: clean
clean:
	rm -rf PVE Proxmox test/Proxmox
	cargo clean -p pve-meta-rs
