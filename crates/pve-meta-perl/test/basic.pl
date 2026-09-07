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

# --- api_* exports (docs/DESIGN.md revision 4) ------------------------
#
# These are exercised again, live, through the real pveproxy in the native
# API agent's own manual test pass; this section only checks the Rust-level
# contract in isolation: return shapes, dry_run, and the "NNN: message"
# error-prefix behavior the Perl layer (PVE::API2::Ext::Meta::_call) parses
# to pick an HTTP status.
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

# Builds a `grants_json` string directly (rather than through encode_json,
# whose plain 1/0 would serialize as JSON numbers, not the booleans serde
# requires for `full_read`/`full_write`).
sub grants_json {
    my (%opts) = @_;
    my $full_read = $opts{full_read} ? 'true' : 'false';
    my $full_write = $opts{full_write} ? 'true' : 'false';
    my @entries = map { qq({"prefix":"$_->{prefix}","mode":"$_->{mode}"}) } @{ $opts{scopes} // [] };
    my $scopes = '[' . join(',', @entries) . ']';
    return qq({"full_read":$full_read,"full_write":$full_write,"scopes":$scopes});
}

my $FULL = grants_json(full_read => 1, full_write => 1);
my $NONE = grants_json();

# --- api_version() -------------------------------------------------------
my $v1 = PVE::RS::Meta::api_version();
like($v1->{token}, qr/^[0-9a-f]{64}$/, 'api_version token looks like a sha256 hex digest');
is($v1->{changed}, 0, 'api_version changed is 0 for an empty store');

# --- api_grants() ----------------------------------------------------------
# api_grants returns a JSON-encoded string (see its doc comment), not a
# decoded Perl structure -- decode it before comparing, same as callers
# (PVE::API2::Ext::Meta) will.
is_deeply(decode_json(PVE::RS::Meta::api_grants('svc@pve!x')), [], 'api_grants is empty without a datacenter document');

PVE::RS::Meta::api_put(
    'datacenter', undef, 'json',
    encode_json({
        scopes => {
            'svc@pve!x' => [
                { prefix => 'traefik', mode => 'rw' },
                { prefix => 'netbird', mode => 'ro' },
            ],
        },
    }),
    'replace', undef, 0, $FULL,
);
is_deeply(
    decode_json(PVE::RS::Meta::api_grants('svc@pve!x')),
    [{ prefix => 'traefik', mode => 'rw' }, { prefix => 'netbird', mode => 'ro' }],
    'api_grants returns the scopes entry for the authid',
);
is_deeply(decode_json(PVE::RS::Meta::api_grants('nobody@pve')), [], 'api_grants is empty for an authid with no entry');
PVE::RS::Meta::api_delete('datacenter', undef, undef, $FULL);

# --- api_get() / api_put(): nonexistent doc is an empty doc, digest "" ---
my $missing = PVE::RS::Meta::api_get('9101', undef, 'json', 1, $FULL);
is($missing->{digest}, '', 'api_get digest is "" for a nonexistent document');
is_deeply(decode_json($missing->{data_json}), {}, 'api_get data is {} for a nonexistent document');

$res = eval { PVE::RS::Meta::api_get('not-a-vmid', undef, 'json', 1, $FULL) };
ok(!defined($res), 'api_get dies for an id that is neither a vmid nor "datacenter"');
like($@, api_error_status(400), 'api_get bad-id error is prefixed 400:');

# --- api_put(): replace mode creates the document ------------------------
my $put1 = PVE::RS::Meta::api_put(
    '9101', undef, 'json', encode_json({ traefik => { spec => { host => 'a.example' } } }), 'replace', undef, 0, $FULL,
);
is($put1->{id}, '9101', 'api_put creates the document with the right id');
is_deeply(
    $put1->{touched},
    [{ path => 'traefik', op => 'set' }],
    'api_put touched reports the top-level key for a brand-new subtree (matches patch::diff\'s '
        . '"replacing/creating a whole subtree yields its root path" convention)',
);

my $doc1 = PVE::RS::Meta::api_get('9101', undef, 'json', 1, $FULL);
is_deeply(decode_json($doc1->{data_json}), { traefik => { spec => { host => 'a.example' } } }, 'api_get sees the new document');
is($doc1->{digest}, $put1->{digest}, 'api_get digest matches api_put digest');

# --- api_put(): view + merge mode -----------------------------------------
my $put2 = PVE::RS::Meta::api_put(
    '9101', 'traefik.spec', 'json', encode_json({ port => 8080 }), 'merge', $doc1->{digest}, 0, $FULL,
);
is_deeply(
    $put2->{touched},
    [{ path => 'traefik.spec.port', op => 'set' }],
    'api_put merge touched is scoped under the view',
);
is_deeply(
    decode_json(PVE::RS::Meta::api_get('9101', 'traefik.spec', 'json', 1, $FULL)->{data_json}),
    { host => 'a.example', port => 8080 },
    'api_put merge preserved the sibling key host',
);

# --- api_put(): view + replace mode (wholesale, not merged) --------------
PVE::RS::Meta::api_put(
    '9101', 'traefik.spec', 'json', encode_json({ host => 'b.example' }), 'replace', undef, 0, $FULL,
);
is_deeply(
    decode_json(PVE::RS::Meta::api_get('9101', 'traefik.spec', 'json', 1, $FULL)->{data_json}),
    { host => 'b.example' },
    'api_put replace mode wholesale-replaces the view (port is gone)',
);

# --- api_get(): yaml vs json format ---------------------------------------
my $as_yaml = PVE::RS::Meta::api_get('9101', 'traefik.spec', 'yaml', 1, $FULL);
ok(!defined($as_yaml->{data_json}), 'api_get format=yaml does not set data_json');
is($as_yaml->{text}, "host: b.example\n", 'api_get format=yaml renders the view as YAML text');

my $as_json = PVE::RS::Meta::api_get('9101', 'traefik.spec', 'json', 1, $FULL);
ok(!defined($as_json->{text}), 'api_get format=json does not set text');
is_deeply(decode_json($as_json->{data_json}), { host => 'b.example' }, 'api_get format=json sets data_json');

# --- api_put(): digest mismatch / dry_run --------------------------------
$res = eval { PVE::RS::Meta::api_put('9101', undef, 'json', encode_json({ a => 1 }), 'merge', 'deadbeef', 0, $FULL) };
ok(!defined($res), 'api_put dies on a digest mismatch');
like($@, api_error_status(409), 'api_put digest-mismatch error is prefixed 409:');

my $before_dry = PVE::RS::Meta::api_get('9101', undef, 'json', 1, $FULL);
my $dry = PVE::RS::Meta::api_put(
    '9101', 'traefik.spec', 'json', encode_json({ host => 'dry.example' }), 'merge', undef, 1, $FULL,
);
is_deeply(
    $dry->{touched},
    [{ path => 'traefik.spec.host', op => 'set' }],
    'api_put dry_run reports what would touch',
);
is_deeply(
    decode_json(PVE::RS::Meta::api_get('9101', undef, 'json', 1, $FULL)->{data_json}),
    decode_json($before_dry->{data_json}),
    'api_put dry_run does not actually write anything',
);

# --- scoped principal: reads/writes only its own subtree -----------------
PVE::RS::Meta::api_put(
    '9101', undef, 'json',
    encode_json({ traefik => { spec => { host => 'b.example' } }, netbird => { groups => ['lan'] }, other => 1 }),
    'replace', undef, 0, $FULL,
);
my $scoped = grants_json(scopes => [{ prefix => 'traefik', mode => 'rw' }, { prefix => 'netbird', mode => 'ro' }]);

my $scoped_view = PVE::RS::Meta::api_get('9101', undef, 'json', 1, $scoped);
is_deeply(
    decode_json($scoped_view->{data_json}),
    { traefik => { spec => { host => 'b.example' } }, netbird => { groups => ['lan'] } },
    'a scoped principal without a view sees only its readable subtrees, not "other"',
);

$res = eval { PVE::RS::Meta::api_get('9101', 'other', 'json', 1, $scoped) };
ok(!defined($res), 'a scoped principal cannot read a view outside its scopes');
like($@, api_error_status(403), 'that read is refused with 403:');

# scoped rw write into its own prefix succeeds (replace and merge)
my $scoped_put = PVE::RS::Meta::api_put(
    '9101', 'traefik.spec', 'json', encode_json({ host => 'scoped.example' }), 'replace', undef, 0, $scoped,
);
is_deeply($scoped_put->{touched}, [{ path => 'traefik.spec.host', op => 'set' }], 'scoped rw replace into its own prefix succeeds');
PVE::RS::Meta::api_put('9101', 'traefik.spec', 'json', encode_json({ port => 9 }), 'merge', undef, 0, $scoped);
is_deeply(
    decode_json(PVE::RS::Meta::api_get('9101', 'traefik.spec', 'json', 1, $scoped)->{data_json}),
    { host => 'scoped.example', port => 9 },
    'scoped rw merge into its own prefix succeeds',
);

# a write into a read-only scope is refused
$res = eval {
    PVE::RS::Meta::api_put('9101', 'netbird', 'json', encode_json({ groups => ['wan'] }), 'replace', undef, 0, $scoped)
};
ok(!defined($res), 'a write into a read-only scope is refused');
like($@, api_error_status(403), 'that write is refused with 403:');

# a write touching a path outside every scope is refused, naming the path
$res = eval {
    PVE::RS::Meta::api_put('9101', undef, 'json', encode_json({ other => 2 }), 'merge', undef, 0, $scoped)
};
ok(!defined($res), 'a write outside every scope is refused');
like($@, api_error_status(403), 'that write is refused with 403:');
like($@, qr/other/, 'the 403 names the offending path');

# --- api_delete(): view removes a subtree; digest / grants enforced ------
my $before_del = PVE::RS::Meta::api_get('9101', undef, 'json', 1, $FULL);
my $del = PVE::RS::Meta::api_delete('9101', 'netbird', $before_del->{digest}, $FULL);
is_deeply($del->{touched}, [{ path => 'netbird.groups', op => 'delete' }], 'api_delete reports the removed leaf');
is_deeply(
    decode_json(PVE::RS::Meta::api_get('9101', undef, 'json', 1, $FULL)->{data_json})->{netbird},
    undef,
    'api_delete removed the netbird subtree',
);

$res = eval { PVE::RS::Meta::api_delete('9101', 'traefik', 'deadbeef', $FULL) };
ok(!defined($res), 'api_delete dies on a digest mismatch');
like($@, api_error_status(409), 'api_delete digest-mismatch error is prefixed 409:');

# a scoped principal may delete within its own rw scope...
PVE::RS::Meta::api_put('9101', 'traefik.extra', 'json', encode_json({ x => 1 }), 'replace', undef, 0, $FULL);
my $scoped_del = PVE::RS::Meta::api_delete('9101', 'traefik.extra', undef, $scoped);
is_deeply($scoped_del->{touched}, [{ path => 'traefik.extra.x', op => 'delete' }], 'a scoped principal may delete within its own rw scope');

# ...but not delete the whole document (would touch paths outside its scope).
$res = eval { PVE::RS::Meta::api_delete('9101', undef, undef, $scoped) };
ok(!defined($res), 'a scoped principal cannot delete the whole document');
like($@, api_error_status(403), 'that delete is refused with 403:');

# --- api_delete(): whole document removal (no view) ------------------------
my $doc_before_wipe = PVE::RS::Meta::api_get('9101', undef, 'json', 1, $FULL);
my $wipe = PVE::RS::Meta::api_delete('9101', undef, $doc_before_wipe->{digest}, $FULL);
is($wipe->{digest}, '', 'api_delete without a view leaves digest "" (the file is gone)');
is(PVE::RS::Meta::has_document(9101), 0, 'api_delete without a view actually removes the file');

my $again = PVE::RS::Meta::api_get('9101', undef, 'json', 1, $FULL);
is($again->{digest}, '', 'api_get after a full delete is the empty-document convention again');

# --- api_list_guests() -----------------------------------------------------
write_file('.vmlist', encode_json({
    version => 1,
    ids => {
        200 => { node => 'n1', type => 'lxc', version => 1 },
        201 => { node => 'n1', type => 'qemu', version => 1 },
    },
}));
mkdir("$root/nodes");
mkdir("$root/nodes/n1");
mkdir("$root/nodes/n1/lxc");
write_file('nodes/n1/lxc/200.conf', "hostname: web01\n");

PVE::RS::Meta::api_put(
    '200', undef, 'json',
    encode_json({ traefik => { spec => { host => 'ct200.example' } }, netbird => { groups => ['lan'] } }),
    'replace', undef, 0, $FULL,
);

# 201 has no document at all; full-access grants still list it (digest "").
my $grants_map_full = '{"200":' . $FULL . ',"201":' . $FULL . '}';
my $listed = PVE::RS::Meta::api_list_guests($grants_map_full, undef);
is(scalar(@$listed), 2, 'api_list_guests lists every guest the caller fully reads, with or without a document');
my ($g200) = grep { $_->{vmid} == 200 } @$listed;
my ($g201) = grep { $_->{vmid} == 201 } @$listed;
is($g200->{node}, 'n1', 'api_list_guests reports the node from .vmlist');
is($g200->{type}, 'lxc', 'api_list_guests reports the type from .vmlist');
is($g200->{name}, 'web01', 'api_list_guests reads the display name from the guest config');
is_deeply([sort @{ $g200->{keys} }], ['netbird', 'traefik'], 'api_list_guests keys lists every top-level key for full access');
is($g201->{digest}, '', 'api_list_guests digest is "" for a guest with no document');
is_deeply($g201->{keys}, [], 'api_list_guests keys is empty for a guest with no document');

# A vmid absent from the grants map (caller has no access at all) is omitted.
my $grants_map_partial = '{"200":' . $FULL . '}';
is(scalar(@{ PVE::RS::Meta::api_list_guests($grants_map_partial, undef) }), 1, 'api_list_guests omits a vmid missing from the grants map');

# A scoped principal (no full access, one rw scope) sees only its own keys.
my $scoped_traefik_only = grants_json(scopes => [{ prefix => 'traefik', mode => 'rw' }]);
my $grants_map_scoped = '{"200":' . $scoped_traefik_only . ',"201":' . $scoped_traefik_only . '}';
my $scoped_list = PVE::RS::Meta::api_list_guests($grants_map_scoped, undef);
is(scalar(@$scoped_list), 2, 'a scope on "traefik" makes every guest listed (the scope applies to every document)');
my ($sg200) = grep { $_->{vmid} == 200 } @$scoped_list;
is_deeply($sg200->{keys}, ['traefik'], 'api_list_guests keys is filtered to the scope for a scoped caller');

# --has filters against the caller's own visible keys.
is(
    scalar(@{ PVE::RS::Meta::api_list_guests($grants_map_full, 'traefik.spec') }),
    1,
    'api_list_guests --has traefik.spec matches the guest that has it',
);
is(
    scalar(@{ PVE::RS::Meta::api_list_guests($grants_map_full, 'traefik.spec.port') }),
    0,
    'api_list_guests --has traefik.spec.port does not match',
);
is(
    scalar(@{ PVE::RS::Meta::api_list_guests($grants_map_scoped, 'netbird') }),
    0,
    '--has cannot see through a caller\'s own missing scope (netbird is not in $scoped_traefik_only)',
);

PVE::RS::Meta::api_delete('200', undef, undef, $FULL);

done_testing();
