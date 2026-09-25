package PVE::Meta::Hooks;

# What the lifecycle patch (patches/lifecycle/) calls from stock PVE code,
# beyond PVE::RS::Meta itself (docs/LIFECYCLE.md).

use strict;
use warnings;

use PVE::Cluster;

# Runs $code under the guest document's own lock -- `pve-meta-<vmid>`, the
# name PVE::API2::Ext::Meta::lock_domain_for gives it and every writer takes
# -- and warns "pve-meta: $what failed: ..." instead of dying: metadata must
# never be able to fail a guest operation. Without the lock a hook races an
# in-flight API write: the write passes its checks, the hook purges or copies
# the file, and the stalled write lands on top.
#
# Unlike the API's _locked, which dies, this is soft-fail by design.
# `cfs_lock_domain` reports its callback's die, and a lock it could not take,
# through `$@` rather than raising, so `$@` is read inside the eval, before
# leaving it clears it. It does not die itself; the eval keeps a guest
# operation going if a later PVE ever did.
sub locked_warn {
    my ($vmid, $what, $code) = @_;
    my $err;
    eval {
        PVE::Cluster::cfs_lock_domain("pve-meta-$vmid", 10, $code);
        $err = $@;
    };
    $err = $@ if !$err;
    warn "pve-meta: $what failed: $err" if $err;
    return;
}

1;
