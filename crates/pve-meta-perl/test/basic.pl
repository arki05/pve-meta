#!/usr/bin/perl

# Exercises every `PVE::RS::Meta` function against a temporary
# `PVE_META_ROOT`. Run via `make check` (from the crate root, which
# sed-patches a test copy of `Proxmox::Lib::PVEMeta` to load
# `target/{debug,release}/libpve_meta_rs.so` directly -- see `-I.` above and
# `Makefile`'s `all` target).

use strict;
use warnings;

use File::Temp qw(tempdir);
use JSON::PP;
use Test::More;

use PVE::RS::Meta;

my $root = tempdir(CLEANUP => 1);
$ENV{PVE_META_ROOT} = $root;
$ENV{PVE_META_PVE_ROOT} = $root;
$ENV{PVE_META_VMLIST} = "$root/.vmlist";

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

# --- api_* exports (PVE::API2::Meta, docs/NATIVE-API-SPEC.md) --------------
#
# These are exercised again, live, through the real pveproxy in the native
# API agent's own manual test pass; this section only checks the Rust-level
# contract in isolation: return shapes, dry_run, and the "NNN: message"
# error-prefix behavior the Perl layer (PVE::API2::Meta::_call) parses to
# pick an HTTP status.
#
# Fresh store: the lifecycle tests above leave documents behind (9002, 9006)
# that a global "list everything"/"store is empty" assertion would trip
# over. `write_file`/`read_file` close over `$root` directly, so
# reassigning it here (rather than introducing a second variable) keeps
# those helpers working unchanged for the rest of the file.
$root = tempdir(CLEANUP => 1);
$ENV{PVE_META_ROOT} = $root;
$ENV{PVE_META_PVE_ROOT} = $root;
$ENV{PVE_META_VMLIST} = "$root/.vmlist";

# helper: dies with a message matching /^NNN: .../ for a specific status
sub api_error_status {
    my ($code) = @_;
    return qr/^\Q$code\E: /;
}

# --- api_version() / api_health() ---------------------------------------
my $v1 = PVE::RS::Meta::api_version();
like($v1->{token}, qr/^[0-9a-f]{64}$/, 'api_version token looks like a sha256 hex digest');
is($v1->{changed}, 0, 'api_version changed is 0 for an empty store');

my $health = PVE::RS::Meta::api_health();
is($health->{store}{root}, $root, 'api_health reports the configured store root');
is($health->{store}{files}, 0, 'api_health sees no guest documents yet');
is_deeply($health->{hooks}, {}, 'api_health hooks is always {}');
like($health->{version}, qr/^\d+\.\d+\.\d+$/, 'api_health version looks like a semver string');

# --- api_list_guests() / api_get() / api_patch() on an empty store -------
is_deeply(PVE::RS::Meta::api_list_guests(undef), [], 'api_list_guests is empty for an empty store');

$res = eval { PVE::RS::Meta::api_get('9101', 0, 0) };
ok(!defined($res), 'api_get dies for a guest with no document');
like($@, api_error_status(404), 'api_get not-found error is prefixed 404:');

$res = eval { PVE::RS::Meta::api_get('not-a-vmid', 0, 0) };
ok(!defined($res), 'api_get dies for an id that is neither a vmid nor "datacenter"');
like($@, api_error_status(400), 'api_get bad-id error is prefixed 400:');

my $doc = PVE::RS::Meta::api_patch(
    '9101', encode_json({ traefik => { spec => { host => 'a.example' } } }), undef, 0,
);
is($doc->{id}, '9101', 'api_patch creates the document with the right id');
is($doc->{format}, 'yaml', 'api_patch creates the document in the default (yaml) format');
is_deeply(
    decode_json($doc->{data_json}),
    { traefik => { spec => { host => 'a.example' } } },
    'api_patch data_json round-trips through JSON',
);
is_deeply(
    $doc->{touched},
    [{ path => 'traefik', op => 'set' }],
    'api_patch touched reports the top-level key it created',
);

my $list = PVE::RS::Meta::api_list_guests(undef);
is(scalar(@$list), 1, 'api_list_guests now sees the one document');
is($list->[0]{vmid}, 9101, 'api_list_guests vmid is numeric');
is_deeply($list->[0]{namespaces}, ['traefik'], 'api_list_guests namespaces lists the top-level key');

is(scalar(@{ PVE::RS::Meta::api_list_guests('traefik.spec') }), 1, 'api_list_guests --has traefik.spec matches');
is(scalar(@{ PVE::RS::Meta::api_list_guests('traefik.spec.port') }), 0, 'api_list_guests --has traefik.spec.port does not match');

# --- api_subtree() -------------------------------------------------------
my $sub = PVE::RS::Meta::api_subtree('9101', 'traefik.spec');
is_deeply(decode_json($sub->{data_json}), { host => 'a.example' }, 'api_subtree returns the right subtree');
is($sub->{digest}, $doc->{digest}, 'api_subtree digest matches the document digest');

$res = eval { PVE::RS::Meta::api_subtree('9101', 'traefik.missing') };
ok(!defined($res), 'api_subtree dies when nothing is at the path');
like($@, api_error_status(404), 'api_subtree missing-path error is prefixed 404:');

# --- api_patch() digest / dry_run ----------------------------------------
$res = eval { PVE::RS::Meta::api_patch('9101', encode_json({ a => 1 }), 'deadbeef', 0) };
ok(!defined($res), 'api_patch dies on a digest mismatch');
like($@, api_error_status(409), 'api_patch digest-mismatch error is prefixed 409:');

my $dry = PVE::RS::Meta::api_patch('9101', encode_json({ traefik => { spec => { host => 'dry.example' } } }), undef, 1);
is_deeply(
    decode_json($dry->{data_json})->{traefik}{spec},
    { host => 'dry.example' },
    'api_patch dry_run reports the would-be result',
);
is(
    decode_json(PVE::RS::Meta::api_get('9101', 0, 0)->{data_json})->{traefik}{spec}{host},
    'a.example',
    'api_patch dry_run does not actually write anything',
);

# a dry_run patch that would 404 (top-level delete against a document that
# does not exist) must fail identically to a real write, not silently
# "succeed" by pretending to create an empty document (docs/API.md).
$res = eval { PVE::RS::Meta::api_patch('9199', encode_json({ a => undef }), undef, 1) };
ok(!defined($res), 'api_patch dry_run on a top-level delete against a missing document dies');
like($@, api_error_status(404), 'that dry_run error is prefixed 404: (same as a real write would be)');
is(PVE::RS::Meta::has_document(9199), 0, 'the dry_run above did not create a document');

# --- api_put_raw() / api_convert() ---------------------------------------
my $raw_doc = PVE::RS::Meta::api_put_raw('9101', "traefik:\n  spec:\n    host: b.example\n", undef, undef, 0);
is(
    decode_json($raw_doc->{data_json})->{traefik}{spec}{host},
    'b.example',
    'api_put_raw replaces the document content',
);

my $converted = PVE::RS::Meta::api_convert('9101', 'toml', undef);
is($converted->{format}, 'toml', 'api_convert switches the format');
is_deeply(decode_json($converted->{data_json}), decode_json($raw_doc->{data_json}), 'api_convert preserves the data');

# --- api_snapshots() / api_snapshot() / api_rollback() / api_clone() -----
is_deeply(PVE::RS::Meta::api_snapshots(9101), [], 'api_snapshots is empty before any snapshot');

my $created = PVE::RS::Meta::api_snapshot(9101, 'before');
is($created->{created}, 1, 'api_snapshot reports created => 1');
is_deeply(PVE::RS::Meta::api_snapshots(9101), ['before'], 'api_snapshots now sees it');

# 9101 is in 'toml' format since the api_convert() call above; switch back
# to 'yaml' explicitly here rather than relying on the (now-toml) current
# format, since this raw content is YAML syntax.
PVE::RS::Meta::api_put_raw('9101', "traefik:\n  spec:\n    host: c.example\n", 'yaml', undef, 0);
my $outcome = PVE::RS::Meta::api_rollback(9101, 'before');
is($outcome->{outcome}, 'restored', 'api_rollback reports outcome => restored');
is(
    decode_json(PVE::RS::Meta::api_get('9101', 0, 0)->{data_json})->{traefik}{spec}{host},
    'b.example',
    'api_rollback actually restored the snapshot content',
);

my $cloned = PVE::RS::Meta::api_clone(9101, 9102);
is($cloned->{id}, '9102', 'api_clone returns the new document, not the source');
is_deeply(decode_json($cloned->{data_json}), decode_json($raw_doc->{data_json}), 'api_clone copied the content');

is(PVE::RS::Meta::api_delete_snapshot(9101, 'before'), 1, 'api_delete_snapshot returns 1');
is_deeply(PVE::RS::Meta::api_snapshots(9101), [], 'the snapshot is gone');

# --- api_delete() ---------------------------------------------------------
is(PVE::RS::Meta::api_delete('9101'), 1, 'api_delete returns 1');
is(PVE::RS::Meta::api_delete('9102'), 1, 'api_delete returns 1 for the clone too');
$res = eval { PVE::RS::Meta::api_get('9101', 0, 0) };
ok(!defined($res), 'api_get dies for the now-deleted document');
like($@, api_error_status(404), 'api_get not-found error is prefixed 404: after delete');

# --- api_registry() / api_schemas() --------------------------------------
is_deeply(PVE::RS::Meta::api_registry(), [], 'api_registry is empty without a datacenter document');

PVE::RS::Meta::api_patch(
    'datacenter',
    encode_json({
        operators => {
            traefik => {
                claims => [{ prefix => 'traefik', scope => 'rw' }],
                schemas => { traefik => { type => 'object' } },
                description => 'Traefik router provider',
            },
        },
    }),
    undef,
    0,
);

my $registry = PVE::RS::Meta::api_registry();
is(scalar(@$registry), 1, 'api_registry now sees the one operator');
is($registry->[0]{name}, 'traefik', 'api_registry entry name');
is_deeply($registry->[0]{claims}, [{ prefix => 'traefik', scope => 'rw' }], 'api_registry entry claims');
is($registry->[0]{description}, 'Traefik router provider', 'api_registry entry description');
is_deeply(decode_json($registry->[0]{schemas_json}), { traefik => { type => 'object' } }, 'api_registry entry schemas_json');

PVE::RS::Meta::api_patch('9103', encode_json({ traefik => { spec => { host => 'd.example' } } }), undef, 0);
is_deeply(
    decode_json(PVE::RS::Meta::api_schemas(9103)),
    { traefik => { type => 'object' } },
    'api_schemas returns the schema for a namespace the guest actually uses',
);
PVE::RS::Meta::api_delete('9103');
PVE::RS::Meta::api_delete('datacenter');

# --- api_inventory() ------------------------------------------------------
write_file('.vmlist', encode_json({
    version => 1,
    ids => { 9104 => { node => 'n1', type => 'lxc', version => 1 } },
}));
mkdir("$root/nodes");
mkdir("$root/nodes/n1");
mkdir("$root/nodes/n1/lxc");
write_file('nodes/n1/lxc/9104.conf', "hostname: web01\n");

my $inventory = PVE::RS::Meta::api_inventory();
is(scalar(@$inventory), 1, 'api_inventory lists the one guest from .vmlist');
is($inventory->[0]{vmid}, 9104, 'api_inventory vmid');
is($inventory->[0]{node}, 'n1', 'api_inventory node');
is($inventory->[0]{type}, 'lxc', 'api_inventory type');
is($inventory->[0]{name}, 'web01', 'api_inventory reads the display name from the guest config');
is($inventory->[0]{has_meta}, 0, 'api_inventory has_meta is false without a document');
is($inventory->[0]{format}, undef, 'api_inventory format is undef without a document');

PVE::RS::Meta::api_patch('9104', encode_json({ a => 1 }), undef, 0);
$inventory = PVE::RS::Meta::api_inventory();
is($inventory->[0]{has_meta}, 1, 'api_inventory has_meta is true once a document exists');
is($inventory->[0]{format}, 'yaml', 'api_inventory format is set once a document exists');
PVE::RS::Meta::api_delete('9104');

done_testing();
