#!/usr/bin/perl

# An example PVE hook script: refuse to start a guest that its metadata says is
# under maintenance.
#
#   pve-meta get 201 homelab.maintenance   ->  reason, or exit 2 for "not set"
#
# Install it on a guest with:
#
#   cp maintenance-hook.pl /var/lib/vz/snippets/maintenance-hook.pl
#   chmod +x /var/lib/vz/snippets/maintenance-hook.pl
#   pct set 201 --hookscript local:snippets/maintenance-hook.pl
#   # then, to park the guest:
#   pvesh set /meta/guests/201 --view homelab.maintenance --data '"disk replacement"'
#
# and `pct start 201` fails with the reason until the key is removed.
#
# The point of the example is what it demonstrates, not what it does:
#
# * **Metadata is useful with no operator anywhere.** No token, no permission
#   file, no daemon -- a key, a hook script and `pve-meta`. That is the smallest
#   complete use of this system (docs/DESIGN.md section 10).
# * **`pve-meta` is a local reader.** PVE runs a hook script as root on the node,
#   at a moment when the API may not be reachable at all -- during boot, or with
#   pveproxy stopped. This never opens a socket.
# * **Exit 2 means "not set".** "No maintenance key" and "the value is the empty
#   string" are different answers, and the exit status is what separates them
#   without parsing anything.
#
# A caution worth stating, because the obvious next example is the dangerous
# one: a hook script that *executes* something it reads from metadata turns
# every principal with a write permission on that prefix into root on the node.
# Metadata is data an operator token can write; a hook script runs as root. Read
# values and decide with them, as here. Do not run them.

use strict;
use warnings;

my ($vmid, $phase) = @ARGV;
exit 0 if !defined($phase) || $phase ne 'pre-start';

# `pve-meta get` prints the value and exits 0, or exits 2 when the key is not
# there. Anything else is a real failure, and a hook script that cannot read the
# metadata should not silently let the guest start.
my $reason = `/usr/sbin/pve-meta get $vmid homelab.maintenance 2>/dev/null`;
my $status = $? >> 8;

if ($status == 2) {
    exit 0; # not under maintenance
}
if ($status != 0) {
    print STDERR "hook: cannot read metadata for $vmid (pve-meta exit $status)\n";
    exit 1;
}

chomp $reason;
$reason = 'no reason given' if $reason eq '';
print STDERR "hook: refusing to start $vmid: under maintenance ($reason)\n";
print STDERR "hook: clear it with: pvesh delete /meta/guests/$vmid --view homelab.maintenance\n";
exit 1;
