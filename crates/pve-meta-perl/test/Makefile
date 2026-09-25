.PHONY: test
test: Proxmox/Lib/PVEMeta.pm
	@echo "-- running pve-meta-rs (PVE::RS::Meta) tests --"
	perl -I. -I.. -I../perl ./basic.pl

# The test stub, we don't know where to look for the library from in here!
Proxmox/Lib/PVEMeta.pm:
	@echo "run 'make' in the pve-meta-perl/ dir first"
	@exit 1
