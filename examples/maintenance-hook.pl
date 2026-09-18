#!/usr/bin/perl

# Example PVE hookscript: refuses to start a guest whose metadata sets
# homelab.maintenance, via `pve-meta` (a local reader, no token, no daemon,
# works even with pveproxy stopped; docs/DESIGN.md §8). Install with:
#
#   cp maintenance-hook.pl /var/lib/vz/snippets/maintenance-hook.pl
#   chmod +x /var/lib/vz/snippets/maintenance-hook.pl
#   pct set 201 --hookscript local:snippets/maintenance-hook.pl
#   pvesh set /meta/guests/201 --view homelab.maintenance --data '"disk replacement"'
#
# `pct start 201` then fails with the reason until the key is removed.

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
