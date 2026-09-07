#!/usr/bin/perl

# Exercises every `PVE::RS::Meta` function against a temporary
# `PVE_META_ROOT`. Run via `make check` (from the crate root, which
# sed-patches a test copy of `Proxmox::Lib::PVEMeta` to load
# `target/{debug,release}/libpve_meta_rs.so` directly -- see `-I.` above and
# `Makefile`'s `all` target).

use strict;
use warnings;

use File::Temp qw(tempdir);
use Test::More;

use PVE::RS::Meta;

my $root = tempdir(CLEANUP => 1);
$ENV{PVE_META_ROOT} = $root;

sub write_file {
    my ($name, $content) = @_;
    open(my $fh, '>', "$root/$name") or die "failed to write $root/$name: $!\n";
    print {$fh} $content;
    close($fh);
}

sub read_file {
    my ($name) = @_;
    open(my $fh, '<', "$root/$name") or die "failed to read $root/$name: $!\n";
    local $/ = undef;
    my $content = <$fh>;
    close($fh);
    return $content;
}

sub file_exists { return -f "$root/$_[0]"; }

# --- version() ---------------------------------------------------------
like(PVE::RS::Meta::version(), qr/^\d+\.\d+\.\d+$/, 'version() looks like a semver string');

# --- has_document() / list_snapshots() on an empty store ---------------
is(PVE::RS::Meta::has_document(9001), 0, 'has_document is false for an unknown guest');
is_deeply(PVE::RS::Meta::list_snapshots(9001), [], 'list_snapshots is empty for an unknown guest');

# --- on_snapshot() -------------------------------------------------------
is(PVE::RS::Meta::on_snapshot(9001, 'before'), 0, 'on_snapshot is a no-op without a document');

write_file('9001.yaml', "name: web01\ntags:\n  - prod\n");
is(PVE::RS::Meta::has_document(9001), 1, 'has_document is true once a document exists');

is(PVE::RS::Meta::on_snapshot(9001, 'before'), 1, 'on_snapshot copies the document');
ok(file_exists('9001.before.yaml'), 'snapshot file was created');
is_deeply(PVE::RS::Meta::list_snapshots(9001), ['before'], 'list_snapshots sees the new snapshot');

# --- on_rollback(): restore -------------------------------------------
write_file('9001.yaml', "name: web01-modified\n");
is(PVE::RS::Meta::on_rollback(9001, 'before'), 'restored', 'on_rollback restores the snapshot');
is(read_file('9001.yaml'), "name: web01\ntags:\n  - prod\n", 'restored content matches the snapshot');

# --- on_delsnap() --------------------------------------------------------
is(PVE::RS::Meta::on_delsnap(9001, 'before'), 1, 'on_delsnap removes an existing snapshot');
ok(!file_exists('9001.before.yaml'), 'snapshot file is gone');
is(PVE::RS::Meta::on_delsnap(9001, 'before'), 0, 'on_delsnap is idempotent');

# --- on_rollback(): removed (doc exists, snapshot does not) -------------
is(PVE::RS::Meta::on_rollback(9001, 'gone'), 'removed', 'on_rollback removes the document when no snapshot exists');
ok(!file_exists('9001.yaml'), 'document file is gone after rollback-removed');

# --- on_rollback(): none (neither exists) -------------------------------
is(PVE::RS::Meta::on_rollback(9001, 'gone'), 'none', 'on_rollback is a no-op when nothing exists');

# --- on_clone() -----------------------------------------------------------
write_file('9002.yaml', "name: template\n");
write_file('9004.yaml', "name: stale-from-a-destroy-race\n");
is(PVE::RS::Meta::on_clone(9003, 9004), 0, 'on_clone is a no-op without a source document');
ok(!file_exists('9004.yaml'), 'on_clone removes a stale target document when the source has none');

is(PVE::RS::Meta::on_clone(9002, 9003), 1, 'on_clone copies the document');
is(read_file('9003.yaml'), "name: template\n", 'cloned content matches the source');

# Overwrites an existing target instead of dying (mirrors clone_vmfw_conf).
write_file('9002.yaml', "name: template-v2\n");
is(PVE::RS::Meta::on_clone(9002, 9003), 1, 'on_clone overwrites an existing target document');
is(read_file('9003.yaml'), "name: template-v2\n", 'overwritten content matches the new source');

# --- on_destroy() ---------------------------------------------------------
is(PVE::RS::Meta::on_snapshot(9003, 'snap'), 1, 'snapshot 9003 before destroying it');
is(PVE::RS::Meta::on_destroy(9003), 2, 'on_destroy removes the document and its snapshot');
is(PVE::RS::Meta::has_document(9003), 0, 'document is gone after on_destroy');
is_deeply(PVE::RS::Meta::list_snapshots(9003), [], 'snapshots are gone after on_destroy');
is(PVE::RS::Meta::on_destroy(9003), 0, 'on_destroy is idempotent (nothing left to remove)');

# --- export_for_backup() / import_from_backup(): round trip -------------
is(PVE::RS::Meta::export_for_backup(9005), undef, 'export_for_backup is undef without a document');

write_file('9002.yaml', "name: template\ntags:\n  - golden\n");
my $blob = PVE::RS::Meta::export_for_backup(9002);
is(
    $blob,
    "#pve-meta-format: yaml\nname: template\ntags:\n  - golden\n",
    'export_for_backup prepends the format header to the raw document',
);

is(PVE::RS::Meta::import_from_backup(9006, $blob), 1, 'import_from_backup returns 1');
is(read_file('9006.yaml'), "name: template\ntags:\n  - golden\n", 'imported content matches the export');

# import_from_backup replaces an existing document
write_file('9006.yaml', "name: stale\n");
is(PVE::RS::Meta::import_from_backup(9006, $blob), 1, 'import_from_backup replaces an existing document');
is(read_file('9006.yaml'), "name: template\ntags:\n  - golden\n", 'replaced content matches the export');

# --- error -> die behaviour ------------------------------------------------
my $res = eval { PVE::RS::Meta::on_snapshot(9002, 'not a valid name') };
ok(!defined($res), 'on_snapshot dies on an invalid snapshot name');
like($@, qr/invalid name/i, 'invalid-name error is readable');

$res = eval { PVE::RS::Meta::import_from_backup(9007, "no header here\n") };
ok(!defined($res), 'import_from_backup dies without a format header');
like($@, qr/header/i, 'missing-header error is readable');

$res = eval { PVE::RS::Meta::import_from_backup(9007, "#pve-meta-format: yaml\na: ~\n") };
ok(!defined($res), 'import_from_backup dies on content that fails the core lint');
is(PVE::RS::Meta::has_document(9007), 0, 'no document was written for the rejected import');

done_testing();
