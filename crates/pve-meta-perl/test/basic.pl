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
is_deeply(decode_json(PVE::RS::Meta::api_grants('svc@pve!tok')), [], 'api_grants is empty without a datacenter document');

PVE::RS::Meta::api_put(
    'datacenter', undef, 'json',
    encode_json({
        scopes => {
            'svc@pve!tok' => [
                { prefix => 'traefik', mode => 'rw' },
                { prefix => 'netbird', mode => 'ro' },
            ],
        },
    }),
    'replace', undef, 0, $FULL,
);
is_deeply(
    decode_json(PVE::RS::Meta::api_grants('svc@pve!tok')),
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

# a root-view write is refused outright without full write access
# (docs/DESIGN.md section 8, review F1)
$res = eval {
    PVE::RS::Meta::api_put('9101', undef, 'json', encode_json({ other => 2 }), 'merge', undef, 0, $scoped)
};
ok(!defined($res), 'a scoped principal cannot write the root view');
like($@, api_error_status(403), 'that write is refused with 403:');
like($@, qr/full write access/, 'the 403 explains that the root view needs full write access');

# a write into an unreadable prefix is refused *generically*: the message must
# not confirm a guessed key or value (review F3)
$res = eval {
    PVE::RS::Meta::api_put('9101', 'other', 'json', encode_json({ x => 2 }), 'replace', undef, 0, $scoped)
};
ok(!defined($res), 'a write outside every scope is refused');
like($@, api_error_status(403), 'that write is refused with 403:');
unlike($@, qr/other/, 'the 403 does not name a path the caller cannot read');

# an empty merge at an arbitrary prefix creates nothing (review F1/F2)
my $before_hack = PVE::RS::Meta::api_get('9101', undef, 'json', 1, $FULL);
for my $hack_view ('zzz_hacked', 'zzz_hacked.deep') {
    $res = eval { PVE::RS::Meta::api_put('9101', $hack_view, 'json', '{}', 'merge', undef, 0, $scoped) };
    ok(!defined($res), "an empty merge at $hack_view is refused");
    like($@, api_error_status(403), "the empty merge at $hack_view is refused with 403:");
}
is_deeply(
    decode_json(PVE::RS::Meta::api_get('9101', undef, 'json', 1, $FULL)->{data_json}),
    decode_json($before_hack->{data_json}),
    'no empty merge created any structure',
);

# `merge` + `null` deletes, end to end (review F8)
PVE::RS::Meta::api_put('9101', 'traefik.spec', 'json', encode_json({ port => 7 }), 'merge', undef, 0, $scoped);
PVE::RS::Meta::api_put('9101', 'traefik.spec', 'json', '{"port": null}', 'merge', undef, 0, $scoped);
is_deeply(
    decode_json(PVE::RS::Meta::api_get('9101', 'traefik.spec', 'json', 1, $scoped)->{data_json}),
    { host => 'scoped.example' },
    'merge with null deleted the key',
);

# `replace` with `{}` stores an empty map instead of deleting (review F9)
PVE::RS::Meta::api_put('9101', 'traefik.empty', 'json', '{}', 'replace', undef, 0, $scoped);
is_deeply(
    decode_json(PVE::RS::Meta::api_get('9101', 'traefik', 'json', 1, $scoped)->{data_json})->{empty},
    {},
    'replace with {} stores an empty map',
);
PVE::RS::Meta::api_delete('9101', 'traefik.empty', undef, $scoped);

# a scope on `traefik` also covers the sibling comment key `traefik__`
# (review F11)
PVE::RS::Meta::api_put('9101', 'traefik__', 'json', '"the ingress config"', 'replace', undef, 0, $scoped);
is(
    decode_json(PVE::RS::Meta::api_get('9101', 'traefik__', 'json', 1, $scoped)->{data_json}),
    'the ingress config',
    'a scoped principal can write its own comment key',
);
PVE::RS::Meta::api_delete('9101', 'traefik__', undef, $scoped);

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
#
# Perl now owns the vmlist and the display names and passes them in; Rust
# never reads `.vmlist` or a guest config (review section 5, api.rs:304). The
# input is a JSON array of `{vmid, node, type, name, grants}` where `grants`
# is that guest's grants as a JSON string.
sub guest_list_json {
    my (@rows) = @_;
    # `grants` crosses as a JSON *string* (see the wire contract), so it is
    # encode_json'd like any other string value -- exactly what
    # PVE::API2::Ext::Meta does.
    return encode_json([
        map {
            my ($vmid, $grants) = @$_;
            {
                vmid => int($vmid),
                node => 'n1',
                type => 'lxc',
                name => "guest-$vmid",
                grants => $grants,
            }
        } @rows
    ]);
}

PVE::RS::Meta::api_put(
    '200', undef, 'json',
    encode_json({ traefik => { spec => { host => 'ct200.example' } }, netbird => { groups => ['lan'] } }),
    'replace', undef, 0, $FULL,
);

# 201 has no document at all; full-access grants still list it (digest "").
my $full_rows = guest_list_json([200, $FULL], [201, $FULL]);
my $listed = PVE::RS::Meta::api_list_guests($full_rows, undef, 0);
is(scalar(@$listed), 2, 'api_list_guests lists every guest the caller fully reads, with or without a document');
my ($g200) = grep { $_->{vmid} == 200 } @$listed;
my ($g201) = grep { $_->{vmid} == 201 } @$listed;
is($g200->{node}, 'n1', 'api_list_guests reports the node Perl passed in');
is($g200->{type}, 'lxc', 'api_list_guests reports the type Perl passed in');
is($g200->{name}, 'guest-200', 'api_list_guests reports the display name Perl passed in');
is_deeply([sort @{ $g200->{keys} }], ['netbird', 'traefik'], 'api_list_guests keys lists every top-level key for full access');
is($g201->{digest}, '', 'api_list_guests digest is "" for a guest with no document');
is_deeply($g201->{keys}, [], 'api_list_guests keys is empty for a guest with no document');

# A caller with no grant at all on a guest never sees it (docs/DESIGN.md section 8).
is(
    scalar(@{ PVE::RS::Meta::api_list_guests(guest_list_json([200, $FULL], [201, $NONE]), undef, 0) }),
    1,
    'api_list_guests omits a guest the caller can read nothing of',
);

# A scoped principal (no full access, one rw scope) sees only its own keys,
# and gets neither node nor name (no VM.Audit).
my $scoped_traefik_only = grants_json(scopes => [{ prefix => 'traefik', mode => 'rw' }]);
my $scoped_rows = guest_list_json([200, $scoped_traefik_only], [201, $scoped_traefik_only]);
my $scoped_list = PVE::RS::Meta::api_list_guests($scoped_rows, undef, 0);
is(scalar(@$scoped_list), 2, 'a scope on "traefik" makes every guest listed (the scope applies to every document)');
my ($sg200) = grep { $_->{vmid} == 200 } @$scoped_list;
is_deeply($sg200->{keys}, ['traefik'], 'api_list_guests keys is filtered to the scope for a scoped caller');
is($sg200->{node}, undef, 'api_list_guests hides the node without VM.Audit');
is($sg200->{name}, undef, 'api_list_guests hides the name without VM.Audit');

# --has filters against the caller's own visible keys.
is(
    scalar(@{ PVE::RS::Meta::api_list_guests($full_rows, 'traefik.spec', 0) }),
    1,
    'api_list_guests --has traefik.spec matches the guest that has it',
);
is(
    scalar(@{ PVE::RS::Meta::api_list_guests($full_rows, 'traefik.spec.port', 0) }),
    0,
    'api_list_guests --has traefik.spec.port does not match',
);
is(
    scalar(@{ PVE::RS::Meta::api_list_guests($scoped_rows, 'netbird', 0) }),
    0,
    '--has cannot see through a caller\'s own missing scope (netbird is not in $scoped_traefik_only)',
);

# --- scopes are validated at write time (review F7) ------------------------
$res = eval {
    PVE::RS::Meta::api_put(
        'datacenter', 'scopes', 'json',
        encode_json({ 'bad@pve' => [{ prefix => 'x', mode => 'readwrite' }] }),
        'replace', undef, 0, $FULL,
    )
};
ok(!defined($res), 'a malformed scopes entry is refused');
like($@, api_error_status(400), 'the malformed scopes write is refused with 400:');
like($@, qr/bad\@pve/, 'the 400 names the offending entry');

# ... and a malformed entry already on disk never denies service to others
# (the lenient per-principal read).
write_file('datacenter.yaml', "scopes:\n  broken\@pve: not-a-list\n  good\@pve!tok:\n  - prefix: traefik\n    mode: rw\n");
is_deeply(
    decode_json(PVE::RS::Meta::api_grants('good@pve!tok')),
    [{ prefix => 'traefik', mode => 'rw' }],
    'a malformed entry for another principal is skipped, not fatal',
);
is_deeply(decode_json(PVE::RS::Meta::api_grants('root@pam')), [], 'an unrelated principal is unaffected too');

# --- `scopes` is admin-only, whatever the scopes say (review P1) -----------
write_file(
    'datacenter.yaml',
    "scopes:\n  good\@pve!tok:\n  - prefix: traefik\n    mode: rw\nother: 1\n",
);
my $before_scopes = read_file('datacenter.yaml');
my $on_scopes = grants_json(scopes => [{ prefix => 'scopes', mode => 'rw' }]);
for my $mode ('merge', 'replace') {
    $res = eval {
        PVE::RS::Meta::api_put(
            'datacenter', 'scopes', 'json',
            encode_json({ 'evil@pve' => [{ prefix => 'traefik', mode => 'rw' }] }),
            $mode, undef, 0, $on_scopes,
        )
    };
    ok(!defined($res), "an rw scope on 'scopes' cannot $mode the access-control map");
    like($@, api_error_status(403), "that $mode is refused with 403:");
}
$res = eval { PVE::RS::Meta::api_delete('datacenter', 'scopes', undef, $on_scopes) };
ok(!defined($res), 'an rw scope on "scopes" cannot delete the access-control map either');
like($@, api_error_status(403), 'that delete is refused with 403:');
is(read_file('datacenter.yaml'), $before_scopes, 'no scopes write got through');

# --- a scope prefix must be non-empty (review P1 / DESIGN section 9) -------
$res = eval {
    PVE::RS::Meta::api_put(
        'datacenter', 'scopes', 'json',
        encode_json({ 'ok@pve' => [{ prefix => '', mode => 'rw' }] }),
        'merge', undef, 0, $FULL,
    )
};
ok(!defined($res), 'an empty scope prefix is refused at write time');
like($@, api_error_status(400), 'the empty-prefix write is refused with 400:');
like($@, qr/must not be empty/, 'the 400 says why');

# ... and one already on disk grants nothing (a warning, not an outage).
write_file('datacenter.yaml', "scopes:\n  broad\@pve:\n  - prefix: ''\n    mode: rw\n");
is_deeply(decode_json(PVE::RS::Meta::api_grants('broad@pve')), [], 'an on-disk empty prefix grants nothing');

# --- reads never lint (review P2) ------------------------------------------
write_file(
    'datacenter.yaml',
    "scopes:\n  good\@pve!tok:\n  - prefix: traefik\n    mode: rw\nbad key: 1\nempty:\n",
);
is_deeply(
    decode_json(PVE::RS::Meta::api_grants('good@pve!tok')),
    [{ prefix => 'traefik', mode => 'rw' }],
    'an out-of-band invalid key elsewhere in the document does not break the scope lookup',
);
my $broken_dc = PVE::RS::Meta::api_get('datacenter', undef, 'yaml', 1, $FULL);
like($broken_dc->{text}, qr/bad key: 1/, 'an admin can read the invalid document to see what to fix');
PVE::RS::Meta::api_put(
    'datacenter', undef, 'yaml',
    "scopes:\n  good\@pve!tok:\n  - prefix: traefik\n    mode: rw\n",
    'replace', $broken_dc->{digest}, 0, $FULL,
);
is(
    read_file('datacenter.yaml'),
    "scopes:\n  good\@pve!tok:\n  - prefix: traefik\n    mode: rw\n",
    'and repair it with a root-level replace',
);

# --- a non-map `scopes` grants nothing instead of 400-ing (review P3) ------
write_file('datacenter.yaml', "scopes: oops\n");
is_deeply(decode_json(PVE::RS::Meta::api_grants('good@pve!tok')), [], 'a non-map scopes key grants nothing');
is_deeply(decode_json(PVE::RS::Meta::api_grants('root@pam')), [], '... for everybody else too');
PVE::RS::Meta::api_delete('datacenter', undef, undef, $FULL);

# --- the bare `__` map comment is not disclosed (review P4) ----------------
PVE::RS::Meta::api_put(
    '9400', undef, 'yaml',
    "__: top level note - secret-ish\ntraefik__: about traefik\ntraefik:\n  host: x\nnetbird:\n  groups:\n  - lan\n",
    'replace', undef, 0, $FULL,
);
my $traefik_only = grants_json(scopes => [{ prefix => 'traefik', mode => 'rw' }]);
is(
    PVE::RS::Meta::api_get('9400', undef, 'yaml', 1, $traefik_only)->{text},
    "traefik__: about traefik\ntraefik:\n  host: x\n",
    'a scoped read does not carry the document-root comment',
);
$res = eval { PVE::RS::Meta::api_get('9400', '__', 'yaml', 1, $traefik_only) };
ok(!defined($res), 'and the explicit view of it is refused');
like($@, api_error_status(403), 'that read is refused with 403:');
PVE::RS::Meta::api_delete('9400', undef, undef, $FULL);

# --- `scopes` is an opaque leaf for addressing (review P9) ------------------
$res = eval { PVE::RS::Meta::api_get('datacenter', 'scopes.good@pve!tok', 'json', 1, $FULL) };
ok(!defined($res), 'a single scopes entry is not path-addressable');
like($@, api_error_status(400), 'that view is refused with 400:');
like($@, qr/as a whole/, 'the 400 says to address the map as a whole');

# ... while a dotted authid, which no view could ever address, is a valid key.
PVE::RS::Meta::api_put(
    'datacenter', 'scopes', 'json',
    encode_json({ 'john.doe@pve' => [{ prefix => 'traefik', mode => 'ro' }] }),
    'replace', undef, 0, $FULL,
);
is_deeply(
    decode_json(PVE::RS::Meta::api_grants('john.doe@pve')),
    [{ prefix => 'traefik', mode => 'ro' }],
    'a dotted PVE authid can hold a scope',
);
PVE::RS::Meta::api_delete('datacenter', undef, undef, $FULL);

# --- orphan documents (review P5) ------------------------------------------
PVE::RS::Meta::api_put('999500', undef, 'json', encode_json({ traefik => { host => 'gone' } }), 'replace', undef, 0, $FULL);
my $rows_without_it = guest_list_json([200, $FULL]);
is(
    scalar(grep { $_->{vmid} == 999500 } @{ PVE::RS::Meta::api_list_guests($rows_without_it, undef, 0) }),
    0,
    'without datacenter read, a document whose guest is gone stays invisible',
);
my ($orphan) =
    grep { $_->{vmid} == 999500 } @{ PVE::RS::Meta::api_list_guests($rows_without_it, undef, 1) };
ok(defined($orphan), 'with datacenter read, it is listed');
is($orphan->{orphan}, 1, 'and marked as an orphan');
is_deeply($orphan->{keys}, ['traefik'], 'with its top-level keys');
PVE::RS::Meta::api_delete('999500', undef, undef, $FULL);
is(PVE::RS::Meta::has_document(999500), 0, 'a datacenter writer can remove it');

# --- the documented GET-then-PUT create flow (review F13) ------------------
my $fresh = PVE::RS::Meta::api_get('9300', undef, 'json', 1, $FULL);
is($fresh->{digest}, '', 'a nonexistent document reports digest ""');
my $created = PVE::RS::Meta::api_put(
    '9300', 'traefik', 'json', encode_json({ host => 'new.example' }), 'replace', $fresh->{digest}, 0, $FULL,
);
isnt($created->{digest}, '', 'PUT with the empty digest creates the document instead of 409-ing forever');
PVE::RS::Meta::api_delete('9300', undef, undef, $FULL);

# --- reads require a grant (review F21) ------------------------------------
$res = eval { PVE::RS::Meta::api_get('200', undef, 'json', 1, $NONE) };
ok(!defined($res), 'a caller with no grant at all cannot read a document');
like($@, api_error_status(403), 'that read is refused with 403:, not an empty document with a real digest');

# --- ordered keys alongside unordered JSON data (review F23) ---------------
PVE::RS::Meta::api_put('200', undef, 'json', encode_json({ zeta => 1 }), 'replace', undef, 0, $FULL);
PVE::RS::Meta::api_put('200', 'alpha', 'json', encode_json({ x => 1 }), 'replace', undef, 0, $FULL);
PVE::RS::Meta::api_put('200', 'mid', 'json', encode_json({ y => 1 }), 'replace', undef, 0, $FULL);
is_deeply(
    PVE::RS::Meta::api_get('200', undef, 'json', 1, $FULL)->{keys},
    ['zeta', 'alpha', 'mid'],
    'api_get returns the document\'s key order in `keys`, which `data` cannot carry',
);

PVE::RS::Meta::api_delete('200', undef, undef, $FULL);

# --- an unparseable document denies nobody (pass 3 R1) ---------------------
#
# Pass 2 moved the *lint* off the read path and left the *parse* fatal, which
# is the same cluster-wide outage one layer down: `api_grants` reads
# `datacenter.yaml` on every guest request, and both write handlers read the
# document before planning, so one tab in a hand-edited file 400'd every
# endpoint for every principal -- root included -- and could not be repaired
# through the API.
my $SCOPED = grants_json(scopes => [{ prefix => 'traefik', mode => 'rw' }]);
PVE::RS::Meta::api_put('9500', undef, 'json', encode_json({ traefik => { host => 'x' } }), 'replace', undef, 0, $FULL);

for my $broken ("a: 1\n\tb: 2\n", "a: &anc 1\nb: *anc\n", "a: 1\n  b: 2\n", "a: [\n") {
    (my $label = $broken) =~ s/\n/\\n/g;
    write_file('datacenter.yaml', $broken);

    is_deeply(decode_json(PVE::RS::Meta::api_grants('svc@pve!tok')), [],
        "[$label] the scope lookup grants nothing instead of failing");
    is_deeply(decode_json(PVE::RS::Meta::api_grants('root@pam')), [],
        "[$label] ... for the administrator too");

    ok(defined(eval { PVE::RS::Meta::api_get('9500', undef, 'yaml', 1, $FULL) }),
        "[$label] an unrelated guest read still works");
    ok(defined(eval {
            PVE::RS::Meta::api_put('9500', 'traefik', 'json', '{"host":"y"}', 'replace', undef, 0, $SCOPED)
        }),
        "[$label] an unrelated scoped guest write still works");

    my $dc = PVE::RS::Meta::api_get('datacenter', undef, 'yaml', 1, $FULL);
    ok(defined($dc->{parse_error}), "[$label] the document itself answers with parse_error");
    is($dc->{raw}, $broken, "[$label] a full reader gets the raw text to repair from");
    is($dc->{text}, "{}\n", "[$label] ... and no data, since nothing parsed");
    is_deeply($dc->{keys}, [], "[$label] ... and no keys");
    isnt($dc->{digest}, '', "[$label] ... but the real digest");

    my $scoped_dc = PVE::RS::Meta::api_get(
        'datacenter', undef, 'json', 1, grants_json(scopes => [{ prefix => 'traefik', mode => 'ro' }]),
    );
    ok(defined($scoped_dc->{parse_error}), "[$label] a scoped reader is told why it is empty");
    is($scoped_dc->{raw}, undef, "[$label] ... and never gets the bytes");

    # A narrower write would plan against the empty document and silently
    # drop the file's whole content: refused.
    $res = eval { PVE::RS::Meta::api_put('datacenter', 'x', 'json', '{"a":1}', 'replace', undef, 0, $FULL) };
    ok(!defined($res), "[$label] a view write against it is refused");
    like($@, api_error_status(400), "[$label] that write is refused with 400:");
    like($@, qr/repaired as a whole/, "[$label] the 400 says how to repair it");
    is(read_file('datacenter.yaml'), $broken, "[$label] and nothing was written");

    # The documented repair: a root-level replace, digest and all.
    PVE::RS::Meta::api_put(
        'datacenter', undef, 'yaml',
        "scopes:\n  svc\@pve!tok:\n  - prefix: traefik\n    mode: rw\n",
        'replace', $dc->{digest}, 0, $FULL,
    );
    is_deeply(
        decode_json(PVE::RS::Meta::api_grants('svc@pve!tok')),
        [{ prefix => 'traefik', mode => 'rw' }],
        "[$label] a root-level replace repairs it",
    );

    # ... and a root DELETE is the other repair shape.
    write_file('datacenter.yaml', $broken);
    PVE::RS::Meta::api_delete('datacenter', undef, undef, $FULL);
    ok(!file_exists('datacenter.yaml'), "[$label] a root DELETE removes it");
}

# --- a non-map document is empty for a scoped reader (pass 3 R2) -----------
#
# `view::filter`'s catch-all returned the whole value without consulting the
# prefixes; before pass 2 made reads lenient, lint rule 1 made that arm
# unreachable.
for my $text ("- a\n- secret\n", "just a scalar\n") {
    (my $label = $text) =~ s/\n/\\n/g;
    write_file('9501.yaml', $text);

    is(PVE::RS::Meta::api_get('9501', undef, 'yaml', 1, $SCOPED)->{text}, "{}\n",
        "[$label] a scope-only reader gets nothing");
    is_deeply(PVE::RS::Meta::api_get('9501', undef, 'yaml', 1, $SCOPED)->{keys}, [],
        "[$label] ... and no keys");
    is(PVE::RS::Meta::api_get('9501', undef, 'yaml', 1, $FULL)->{text}, $text,
        "[$label] a full reader still sees exactly what is on disk");
    is(
        scalar(@{ PVE::RS::Meta::api_list_guests(encode_json([{ vmid => 9501, grants => $SCOPED }]), 'traefik', 0) }),
        0,
        "[$label] ?has= is not a content oracle over it either",
    );

    # The write gate refuses to store it again.
    $res = eval { PVE::RS::Meta::api_put('9501', undef, 'yaml', $text, 'replace', undef, 0, $FULL) };
    ok(!defined($res), "[$label] storing it again is refused");
    like($@, api_error_status(400), "[$label] that write is refused with 400:");
    is(read_file('9501.yaml'), $text, "[$label] and nothing was written");
}
unlink("$root/9501.yaml");

# --- a lint 400 never names an unreadable path (pass 3 R6) -----------------
write_file('9502.yaml', "traefik:\n  host: x\nsecret_area:\n  customer name: acme\n");
ok(
    defined(eval {
        PVE::RS::Meta::api_put('9502', 'traefik', 'json', '{"host":"y"}', 'replace', undef, 0, $SCOPED)
    }),
    'an out-of-band bad key elsewhere does not block a scoped write',
);
like(read_file('9502.yaml'), qr/customer name/, 'and the scoped write did not drop it either');

$res = eval { PVE::RS::Meta::api_put('9502', 'traefik', 'json', '{"my bad":1}', 'replace', undef, 0, $SCOPED) };
ok(!defined($res), 'a scoped writer\'s own bad key is still refused');
like($@, api_error_status(400), 'that write is refused with 400:');
like($@, qr/my bad/, 'and the 400 names the key the caller wrote');
unlike($@, qr/customer name|secret_area/, 'but never a key the caller cannot read');

$res = eval { PVE::RS::Meta::api_put('9502', 'traefik', 'json', '{"host":"z"}', 'replace', undef, 0, $FULL) };
ok(!defined($res), 'a full-write caller is still gated on the whole document');
like($@, qr/customer name/, 'and does get the offending path spelled out');

# Narrowing the *scope* of the lint must not narrow its *rules*: every one of
# these was caught by the whole-document lint before, including the
# comment-key-value rule, which lives in the view's parent map.
for my $case (
    ['traefik__', 'replace', '5', qr/comment key value must be a string/],
    ['traefik', 'replace', '{"bad key":1}', qr/invalid key/],
    ['traefik', 'replace', '{"deep":{"a.b":1}}', qr/no dots/],
    ['traefik', 'replace', '{"list":[{"bad key":1}]}', qr/invalid key/],
    ['traefik', 'merge', '{"x__":5}', qr/comment key value must be a string/],
    ['traefik', 'replace', '{"nul":null}', qr/null values are not allowed/],
) {
    my ($view, $mode, $payload, $re) = @$case;
    $res = eval { PVE::RS::Meta::api_put('9502', $view, 'json', $payload, $mode, undef, 0, $SCOPED) };
    ok(!defined($res), "a scoped write of $payload at $view is still refused");
    like($@, $re, "... with the rule the whole-document lint used to report");
}
ok(
    defined(eval {
        PVE::RS::Meta::api_put('9502', 'traefik__', 'json', '"the ingress config"', 'replace', undef, 0, $SCOPED)
    }),
    'a legitimate comment-key write still goes through',
);
unlink("$root/9502.yaml");

# --- a nested delete marker is applied, not stored (pass 3 R7) -------------
PVE::RS::Meta::api_put('9503', undef, 'json', encode_json({ traefik => { host => 'x' } }), 'replace', undef, 0, $FULL);
my $noop = PVE::RS::Meta::api_put('9503', 'traefik', 'json', '{"sub":{"gone":null}}', 'merge', undef, 0, $SCOPED);
is_deeply($noop->{touched}, [], 'a nested delete of a not-yet-existing container touches nothing');
is_deeply(
    decode_json(PVE::RS::Meta::api_get('9503', 'traefik', 'json', 1, $FULL)->{data_json}),
    { host => 'x' },
    'and creates nothing',
);
PVE::RS::Meta::api_put('9503', 'traefik', 'json', '{"sub":{"port":1,"gone":null}}', 'merge', undef, 0, $SCOPED);
is_deeply(
    decode_json(PVE::RS::Meta::api_get('9503', 'traefik', 'json', 1, $FULL)->{data_json}),
    { host => 'x', sub => { port => 1 } },
    'a combined set+delete against an absent container stores only the set',
);
PVE::RS::Meta::api_delete('9503', undef, undef, $FULL);

# --- `touched` collapses a path inside `scopes` (pass 3 section 5) ---------
PVE::RS::Meta::api_put(
    'datacenter', 'scopes', 'json',
    encode_json({ 'john.doe@pve' => [{ prefix => 'traefik', mode => 'ro' }] }),
    'replace', undef, 0, $FULL,
);
is_deeply(
    PVE::RS::Meta::api_put(
        'datacenter', 'scopes', 'json',
        encode_json({ 'a.b@pve' => [{ prefix => 'netbird', mode => 'ro' }] }),
        'merge', undef, 0, $FULL,
    )->{touched},
    [{ path => 'scopes', op => 'set' }],
    'a write inside the scopes map reports the map, not another principal\'s authid',
);
PVE::RS::Meta::api_delete('datacenter', undef, undef, $FULL);

# --- an invalid scope prefix is refused at the wire boundary (section 5) ---
for my $bad ('', 'scopes.other@pve', '__') {
    my $g = grants_json(scopes => [{ prefix => $bad, mode => 'rw' }]);
    $res = eval { PVE::RS::Meta::api_get('9500', undef, 'json', 1, $g) };
    ok(!defined($res), "a scope prefix '$bad' is refused at the grants boundary");
    like($@, api_error_status(400), "that request is refused with 400:");
}

# --- a document above the read cap is refused, not hashed (section 5) ------
write_file('9504.yaml', "a: \"" . ('x' x (4 * 1024 * 1024)) . "\"\n");
$res = eval { PVE::RS::Meta::api_get('9504', undef, 'json', 1, $FULL) };
ok(!defined($res), 'a document above the read cap is refused');
like($@, api_error_status(400), 'that read is refused with 400:');
like($@, qr/too large/, 'and says why');
# It is still replaceable: the repair does not depend on reading it.
PVE::RS::Meta::api_put('9504', undef, 'json', encode_json({ a => 1 }), 'replace', undef, 0, $FULL);
is_deeply(
    decode_json(PVE::RS::Meta::api_get('9504', undef, 'json', 1, $FULL)->{data_json}),
    { a => 1 },
    'and a root replace still repairs it',
);
PVE::RS::Meta::api_delete('9504', undef, undef, $FULL);
PVE::RS::Meta::api_delete('9500', undef, undef, $FULL);

done_testing();
