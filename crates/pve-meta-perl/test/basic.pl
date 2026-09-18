#!/usr/bin/perl

# Exercises every `PVE::RS::Meta` export against a temporary
# `PVE_META_ROOT` and a temporary operator drop-directory. Run via
# `make check` (from the crate root, which sed-patches a test copy of
# `Proxmox::Lib::PVEMeta` to load `target/{debug,release}/libpve_meta_rs.so`
# directly -- see `-I.` above and `Makefile`'s `all` target).
#
# The point of this file, beyond the contract itself, is the **boundary**
# (`docs/DESIGN.md` §8): ACLs, guest lists and results cross as native Perl
# hashes and arrays, and only the client's `data` parameter is a JSON string.
# So everything below passes real hash refs and inspects real hash refs; the
# only `encode_json` here is for that one parameter.

use strict;
use warnings;

use File::Temp qw(tempdir);
use JSON::PP;
use Test::More;

use PVE::RS::Meta;

my $root = tempdir(CLEANUP => 1);
my $nsdir = tempdir(CLEANUP => 1);
$ENV{PVE_META_ROOT} = $root;
$ENV{PVE_META_PREFIX_DIRS} = $nsdir;
my $rundir = tempdir(CLEANUP => 1);
$ENV{PVE_META_RUN_DIR} = $rundir;

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

# The file name is the prefix (docs/DESIGN.md §3).
sub write_prefix {
    my ($name, $content) = @_;
    open(my $fh, '>', "$nsdir/$name.yaml") or die "failed to write $nsdir/$name.yaml: $!\n";
    print {$fh} $content;
    close($fh);
}

sub file_exists { return -f "$root/$_[0]"; }

# helper: dies with a message matching /^NNN: .../ for a specific status
sub api_error_status {
    my ($code) = @_;
    return qr/^\Q$code\E: /;
}

my $res;

# --- version() ---------------------------------------------------------
like(PVE::RS::Meta::version(), qr/^\d+\.\d+\.\d+$/, 'version() looks like a semver string');

# =========================================================================
# Snapshot hooks -- the only lifecycle exports (docs/DESIGN.md §9).
# =========================================================================

is(PVE::RS::Meta::on_snapshot(9001, 'before'), 0, 'on_snapshot is a no-op without a document');

write_file('9001.yaml', "name: web01\ntags:\n  - prod\n");
is(PVE::RS::Meta::on_snapshot(9001, 'before'), 1, 'on_snapshot copies the document');
ok(file_exists('9001.before.yaml'), 'snapshot file was created');

write_file('9001.yaml', "name: web01-modified\n");
is(PVE::RS::Meta::on_rollback(9001, 'before'), 'restored', 'on_rollback restores the snapshot');
is(read_file('9001.yaml'), "name: web01\ntags:\n  - prod\n", 'restored content matches the snapshot');

is(PVE::RS::Meta::on_delsnap(9001, 'before'), 1, 'on_delsnap removes an existing snapshot');
ok(!file_exists('9001.before.yaml'), 'snapshot file is gone');
is(PVE::RS::Meta::on_delsnap(9001, 'before'), 0, 'on_delsnap is idempotent');

is(PVE::RS::Meta::on_rollback(9001, 'gone'), 'removed',
    'on_rollback removes the document when no snapshot exists');
ok(!file_exists('9001.yaml'), 'document file is gone after rollback-removed');
is(PVE::RS::Meta::on_rollback(9001, 'gone'), 'none', 'on_rollback is a no-op when nothing exists');

# --- on_create / on_destroy ---------------------------------------------
#
# The two hooks patched into PVE::AbstractConfig (docs/DESIGN.md §9). Both clear a
# vmid's document and every snapshot copy; only the call site differs.
write_file('9300.yaml', "traefik:\n  host: old.example\n");
PVE::RS::Meta::on_snapshot(9300, 'snapA');
ok(file_exists('9300.yaml'), 'the doomed guest has a document');
ok(file_exists('9300.snapA.yaml'), '... and a snapshot copy');

is(PVE::RS::Meta::on_destroy(9300), 2, 'on_destroy removes the document and its snapshots');
ok(!file_exists('9300.yaml'), '... the document is gone');
ok(!file_exists('9300.snapA.yaml'), '... and so is the snapshot copy');
is(PVE::RS::Meta::on_destroy(9300), 0, 'on_destroy is idempotent');

# The reuse case a periodic sweep cannot see: the vmid comes straight back, so it is
# never "missing from the vmlist" -- only a hook at creation clears the leftover.
write_file('9301.yaml', "traefik:\n  host: stale.example\n");
PVE::RS::Meta::on_snapshot(9301, 'snapB');
is(PVE::RS::Meta::on_create(9301), 2, 'on_create clears a leftover document and its snapshots');
ok(!file_exists('9301.yaml'), 'a guest created at a recycled vmid inherits nothing');
ok(!file_exists('9301.snapB.yaml'), '... not even an old snapshot copy');
is(PVE::RS::Meta::on_create(9301), 0, 'on_create on a clean vmid is a no-op');

# The vmid crosses the boundary as whatever scalar the caller holds: qemu-server
# passes the API parameter through as a string, pve-container as a number. Both
# must work, or every hook is a silent no-op for one guest type.
write_file('9302.yaml', "k: v\n");
is(PVE::RS::Meta::on_snapshot("9302", 'str'), 1, 'a vmid given as a string is accepted (on_snapshot)');
is(PVE::RS::Meta::on_delsnap("9302", 'str'), 1, '... and on_delsnap');
is(PVE::RS::Meta::on_create("9302"), 1, '... and on_create');
ok(!file_exists('9302.yaml'), '... acting on the right vmid');
$res = eval { PVE::RS::Meta::on_create("not-a-vmid") };
ok(!defined($res) && $@ =~ /not a vmid/, 'a string that is not a vmid dies');

# --- the restore marker ---------------------------------------------------
# create_and_lock_config leaves one, the next write_config takes it, destroy
# removes it. Node-local, under $PVE_META_RUN_DIR here.
is(PVE::RS::Meta::take_created(9303), 0, 'no marker until a create');
PVE::RS::Meta::mark_created("9303");
ok(-f "$rundir/9303", 'mark_created leaves the marker file');
is(PVE::RS::Meta::take_created(9303), 1, 'take_created takes it');
is(PVE::RS::Meta::take_created(9303), 0, '... once');
PVE::RS::Meta::mark_created(9303);
PVE::RS::Meta::on_destroy(9303);
ok(!-f "$rundir/9303", 'on_destroy removes a marker a failed create left');

# Error -> die behaviour.
$res = eval { PVE::RS::Meta::on_snapshot(9001, 'not a valid name') };
ok(!defined($res), 'on_snapshot dies on an invalid snapshot name');
like($@, qr/invalid name/i, 'invalid-name error is readable');

# These exports are gone. `on_destroy` and `export_for_backup` are not in
# this list: those names exist again, as the create/destroy hook and the
# backup side of the notes block, respectively. `api_permissions` is gone
# along with the permission files it listed (docs/DESIGN.md §4): access is
# PVE's ACLs alone.
for my $gone (qw(on_clone import_from_backup list_snapshots has_document api_permissions)) {
    ok(!defined(&{"PVE::RS::Meta::$gone"}), "PVE::RS::Meta::$gone is not exported any more");
}

# stored_vmids() -- what `pve-meta ls --orphans` and `pve-meta rm` are built on.
# =========================================================================

write_file('9100.yaml', "traefik:\n  host: live\n");
write_file('999500.yaml', "traefik:\n  host: gone\n");
write_file('datacenter.yaml', "note: keep me\n");
PVE::RS::Meta::on_snapshot(9100, 'keep');
PVE::RS::Meta::on_snapshot(999500, 'snapA');
# A snapshot copy with no live document still names a vmid the store holds.
write_file('9200.old.yaml', "a: 1\n");

is_deeply(PVE::RS::Meta::stored_vmids(), [9100, 9200, 999500],
    'stored_vmids lists every guest vmid with any file, sorted, never a stray file');

# The only removal path for a stale vmid is the destroy hook, run by `pve-meta rm`
# under the document's own write lock (docs/DESIGN.md section 10).
is(PVE::RS::Meta::on_destroy(999500), 2, 'on_destroy removes a stale document and its snapshot copy');
is_deeply(PVE::RS::Meta::stored_vmids(), [9100, 9200], '... and the vmid leaves the list');
ok(file_exists('datacenter.yaml'), 'a stray file with no vmid in its name is never a guest');

for my $gone (qw(gc gc_candidates gc_purge)) {
    ok(!defined(&{"PVE::RS::Meta::$gone"}), "PVE::RS::Meta::$gone is not exported any more");
}

unlink("$root/datacenter.yaml", "$root/9100.yaml", "$root/9100.keep.yaml", "$root/9200.old.yaml");

# =========================================================================
# export_for_backup / notes_import -- the two ends of a backup (docs/DESIGN.md §9):
# the block vzdump's patched `assemble` appends to the archive's copy of the notes,
# and what the patched `write_config` (mode 'restore') and `pve-meta scan-notes`
# (mode 'install') do with it on the way back.
# =========================================================================

is(PVE::RS::Meta::export_for_backup(9400), undef, 'export_for_backup is undef without a document');
my $backed_up = "traefik:\n  spec: {host: web.example}\n";
write_file('9400.yaml', $backed_up);
my $block = PVE::RS::Meta::export_for_backup(9400);
like(
    $block,
    qr/^\[pve-meta v1 vmid=9400 time=\d{4}-\d\d-\d\dT\d\d:\d\d:\d\dZ sha256=[0-9a-f]{64}\]\n````yaml\ntraefik:\n  spec: \{host: web\.example\}\n````\n\[\/pve-meta\]$/,
    'the block is the document text under a marker, verbatim'
);

# Restore: to another vmid, over a document that was there -- the backup wins.
write_file('9401.yaml', "old: true\n");
$res = PVE::RS::Meta::notes_import(9401, "web01\n\n$block", 'restore');
is($res->{action}, 'imported', 'restore imports the block');
is($res->{description}, 'web01', '... and hands back the notes without it');
is(read_file('9401.yaml'), $backed_up, '... the document is the backed-up text');

$res = PVE::RS::Meta::notes_import(9401, 'plain notes', 'restore');
is($res->{action}, 'none', 'notes without a block are nothing to do');
ok(!defined($res->{description}), '... and the notes are left alone');

# Install: a document already there is kept, the block still goes.
$res = PVE::RS::Meta::notes_import(9401, $block, 'install');
is($res->{action}, 'stripped', 'install keeps a document that is there');
is($res->{description}, '', '... and strips the block, to empty notes here');
is(read_file('9401.yaml'), $backed_up, '... the document is untouched');

$res = PVE::RS::Meta::notes_import(9402, "kept\n$block\nalso kept", 'install');
is($res->{action}, 'imported', 'install imports where there is no document');
is($res->{description}, "kept\nalso kept", '... and the notes around the block survive');
is(read_file('9402.yaml'), $backed_up, '... into the store');

$res = PVE::RS::Meta::notes_import("9405", $block, 'restore');
is($res->{action}, 'imported', 'notes_import takes a string vmid too');
like(PVE::RS::Meta::export_for_backup("9405"), qr/^\[pve-meta v1 vmid=9405 /, 'export_for_backup takes a string vmid too');

$res = eval { PVE::RS::Meta::notes_import(9402, $block, 'bogus') };
ok(!defined($res), 'notes_import dies on an unknown mode');

my $bad = "[pve-meta v1 vmid=1 sha256=0]\n````yaml\n- not: a map\n````\n[/pve-meta]";
$res = eval { PVE::RS::Meta::notes_import(9403, $bad, 'restore') };
ok(!defined($res), 'a block the store would refuse dies rather than importing');
ok(!file_exists('9403.yaml'), '... and writes nothing');

write_file('9404.yaml', "k: [unclosed\n");
$res = eval { PVE::RS::Meta::export_for_backup(9404) };
ok(!defined($res) && $@ =~ /does not parse/, 'export_for_backup dies for a document that does not parse');

unlink("$root/9400.yaml", "$root/9401.yaml", "$root/9402.yaml", "$root/9404.yaml", "$root/9405.yaml");

# =========================================================================
# The perlmod boundary: native hashes and arrays.
# =========================================================================

# `$acl` is a native hash. perlmod converts a Perl scalar to a Rust bool by
# *truthiness*, so 1/0/''/undef all mean what a Perl programmer expects.
sub acl {
    my (%opts) = @_;
    return {
        authid => $opts{authid} // 'root@pam',
        read => $opts{read},
        write => $opts{write},
        tags => $opts{tags} // [],
    };
}

my $FULL = acl(read => 1, write => 1);
my $NONE = acl(authid => 'nobody@pve', read => 0, write => 0);

write_file('9200.yaml', "traefik:\n  spec:\n    host: a.example\n");

for my $case (
    [1, 1, 'the integer 1'],
    ['1', 1, "the string '1'"],
    ['x', 1, "a non-empty string"],
    [0, 0, 'the integer 0'],
    ['', 0, 'the empty string'],
    [undef, 0, 'undef'],
) {
    my ($value, $expected, $label) = @$case;
    my $a = { authid => 'probe@pve', read => $value, write => 0, tags => [] };
    my $got = PVE::RS::Meta::api_access('9200', $a);
    is($got->{read} ? 1 : 0, $expected, "perlmod reads $label as " . ($expected ? 'true' : 'false'));
}

# =========================================================================
# api_* : reads and writes
# =========================================================================

# The GC section above emptied the store, and a `documents` assertion against an
# empty list passes for the wrong reason -- `grep` over nothing found nothing.
# So seed one of each thing the walk has to tell apart: a guest document, a
# snapshot copy, and a stray file with no vmid in its name -- the last two are
# *not* documents (a `datacenter.yaml` is what an earlier release left behind).
write_file('datacenter.yaml', "note: keep me\n");
write_file('9100.yaml', "a: 1\n");
write_file('9100.keep.yaml', "a: 1\n");

# One unscoped token, over every document and every prefix file
# (docs/DESIGN.md §6): no `id`, no `detail`, no per-document digests.
my $v = PVE::RS::Meta::api_version();
like($v->{token}, qr/^[0-9a-f]{64}$/, 'api_version token is a sha256 hex string');
is_deeply([sort keys %$v], ['token'], 'api_version answers exactly { token }');

write_file('9100.yaml', "a: 2\n");
isnt(PVE::RS::Meta::api_version()->{token}, $v->{token}, 'a write moves the token');

unlink("$root/datacenter.yaml", "$root/9100.yaml", "$root/9100.keep.yaml");

# A missing document is the empty document with digest "".
my $missing = PVE::RS::Meta::api_get('9101', undef, 'json', $FULL);
is($missing->{digest}, '', 'api_get digest is "" for a nonexistent document');
is_deeply($missing->{data}, {}, 'api_get data is {} for a nonexistent document -- a native hash');
ok(!defined($missing->{data_json}), 'there is no data_json field any more');
ok(!defined($missing->{keys}), 'and no `keys` wire field (docs/DESIGN.md §10)');

$res = eval { PVE::RS::Meta::api_get('not-a-vmid', undef, 'json', $FULL) };
ok(!defined($res), 'api_get dies for an id that is neither a vmid nor a registry id');
like($@, api_error_status(400), 'api_get bad-id error is prefixed 400:');

# replace mode creates the document; `data` is the one JSON string parameter.
my $put1 = PVE::RS::Meta::api_put(
    '9101', undef, 'json', encode_json({ traefik => { spec => { host => 'a.example' } } }),
    'replace', undef, 0, $FULL,
);
is($put1->{id}, '9101', 'api_put creates the document with the right id');
is_deeply($put1->{touched}, [{ path => 'traefik', op => 'set' }],
    'api_put touched reports the top-level key for a brand-new subtree');

my $doc1 = PVE::RS::Meta::api_get('9101', undef, 'json', $FULL);
is_deeply($doc1->{data}, { traefik => { spec => { host => 'a.example' } } },
    'api_get returns `data` as a native hash, nested structure intact');
is($doc1->{digest}, $put1->{digest}, 'api_get digest matches api_put digest');

# Types survive the native round trip.
PVE::RS::Meta::api_put('9101', 'types', 'json',
    '{"n":8080,"f":1.5,"t":true,"f2":false,"s":"x","list":[1,"two"]}',
    'replace', undef, 0, $FULL);
my $types = PVE::RS::Meta::api_get('9101', 'types', 'json', $FULL)->{data};
is($types->{n}, 8080, 'an integer survives the native boundary');
is($types->{f}, 1.5, 'a float survives it');
ok($types->{t} && !$types->{f2}, 'booleans survive it');
is_deeply($types->{list}, [1, 'two'], 'a mixed list survives it');
PVE::RS::Meta::api_delete('9101', 'types', undef, $FULL);

# view + merge mode
my $put2 = PVE::RS::Meta::api_put(
    '9101', 'traefik.spec', 'json', encode_json({ port => 8080 }), 'merge',
    PVE::RS::Meta::api_get('9101', undef, 'json', $FULL)->{digest}, 0, $FULL,
);
is_deeply($put2->{touched}, [{ path => 'traefik.spec.port', op => 'set' }],
    'api_put merge touched is scoped under the view');
is_deeply(PVE::RS::Meta::api_get('9101', 'traefik.spec', 'json', $FULL)->{data},
    { host => 'a.example', port => 8080 }, 'api_put merge preserved the sibling key host');

# view + replace mode (wholesale, not merged)
PVE::RS::Meta::api_put('9101', 'traefik.spec', 'json', encode_json({ host => 'b.example' }),
    'replace', undef, 0, $FULL);
is_deeply(PVE::RS::Meta::api_get('9101', 'traefik.spec', 'json', $FULL)->{data},
    { host => 'b.example' }, 'api_put replace mode wholesale-replaces the view (port is gone)');

# yaml vs json format
my $as_yaml = PVE::RS::Meta::api_get('9101', 'traefik.spec', 'yaml', $FULL);
ok(!defined($as_yaml->{data}), 'api_get format=yaml does not set data');
is($as_yaml->{text}, "host: b.example\n", 'api_get format=yaml renders the view as YAML text');
ok(!defined(PVE::RS::Meta::api_get('9101', 'traefik.spec', 'json', $FULL)->{text}),
    'api_get format=json does not set text');

# digest mismatch / dry_run
$res = eval { PVE::RS::Meta::api_put('9101', undef, 'json', '{"a":1}', 'merge', 'deadbeef', 0, $FULL) };
ok(!defined($res), 'api_put dies on a digest mismatch');
like($@, api_error_status(409), 'api_put digest-mismatch error is prefixed 409:');

my $before_dry = PVE::RS::Meta::api_get('9101', undef, 'json', $FULL);
my $dry = PVE::RS::Meta::api_put('9101', 'traefik.spec', 'json',
    encode_json({ host => 'dry.example' }), 'merge', undef, 1, $FULL);
is_deeply($dry->{touched}, [{ path => 'traefik.spec.host', op => 'set' }],
    'api_put dry_run reports what would touch');
is_deeply(PVE::RS::Meta::api_get('9101', undef, 'json', $FULL)->{data}, $before_dry->{data},
    'api_put dry_run does not actually write anything');

# merge + null deletes; replace with {} stores an empty map
PVE::RS::Meta::api_put('9101', 'traefik.spec', 'json', '{"port":7}', 'merge', undef, 0, $FULL);
PVE::RS::Meta::api_put('9101', 'traefik.spec', 'json', '{"port":null}', 'merge', undef, 0, $FULL);
is_deeply(PVE::RS::Meta::api_get('9101', 'traefik.spec', 'json', $FULL)->{data},
    { host => 'b.example' }, 'merge with null deleted the key');
PVE::RS::Meta::api_put('9101', 'traefik.empty', 'json', '{}', 'replace', undef, 0, $FULL);
is_deeply(PVE::RS::Meta::api_get('9101', 'traefik', 'json', $FULL)->{data}->{empty}, {},
    'replace with {} stores an empty map');
PVE::RS::Meta::api_delete('9101', 'traefik.empty', undef, $FULL);

# the documented GET-then-PUT create flow
my $fresh = PVE::RS::Meta::api_get('9300', undef, 'json', $FULL);
is($fresh->{digest}, '', 'a nonexistent document reports digest ""');
isnt(PVE::RS::Meta::api_put('9300', 'traefik', 'json', '{"host":"new"}', 'replace',
        $fresh->{digest}, 0, $FULL)->{digest},
    '', 'PUT with the empty digest creates the document instead of 409-ing forever');
PVE::RS::Meta::api_delete('9300', undef, undef, $FULL);

# reads require VM.Audit
$res = eval { PVE::RS::Meta::api_get('9101', undef, 'json', $NONE) };
ok(!defined($res), 'a caller with no read access at all cannot read a document');
like($@, api_error_status(403), 'that read is refused with 403:');

# =========================================================================
# Prefixes, selectors and tags (docs/DESIGN.md §3).
# =========================================================================

# The file name is the prefix; there is no `prefix:` field to disagree with it.
write_prefix('traefik', <<'YAML');
description: Traefik dynamic configuration
selector: { tag: traefik }
schema:
  type: object
  properties:
    spec: { type: object }
YAML
write_prefix('homelab.docker', <<'YAML');
selector: { all: true }
schema: { type: object }
YAML
write_prefix('homelab', <<'YAML');
selector: { all: true }
schema: { type: object }
YAML

my $ns = PVE::RS::Meta::api_prefixes();
is(scalar(@$ns), 3, 'api_prefixes lists every prefix');
is($ns->[0]->{prefix}, 'homelab.docker',
    'sorted most-specific first, which is the order that resolves who governs a path');
is_deeply([map { $_->{prefix} } @$ns], ['homelab.docker', 'homelab', 'traefik'],
    '... longest prefix first, then by name');
my ($traefik_ns) = grep { $_->{prefix} eq 'traefik' } @$ns;
is($traefik_ns->{description}, 'Traefik dynamic configuration', 'the description survives');
is_deeply($traefik_ns->{selector}, { tag => 'traefik' }, 'and the selector, as a native hash');
ok($traefik_ns->{schema}, 'and the schema, passed through verbatim');
ok(!exists $traefik_ns->{authid}, 'a prefix names no principal');

sub read_only {
    return { authid => 'auditor@pve', read => 1, write => 0, tags => [] };
}

sub write_only {
    return { authid => 'writer@pve', read => 0, write => 1, tags => [] };
}

PVE::RS::Meta::api_put('9400', undef, 'json',
    encode_json({
        traefik => { spec => { host => 'ct.example' } },
        netbird => { groups => ['lan'] },
        other => 1,
    }),
    'replace', undef, 0, $FULL);

# Access is PVE's ACLs alone (docs/DESIGN.md §4): read/write are exactly the
# ACL's own booleans, with no partial view and no scopes.
is_deeply(PVE::RS::Meta::api_access('9400', $FULL), { read => 1, write => 1 },
    'api_access reflects a full ACL');
is_deeply(PVE::RS::Meta::api_access('9400', $NONE), { read => 0, write => 0 },
    '... and an empty one');
is_deeply(PVE::RS::Meta::api_access('9400', read_only()), { read => 1, write => 0 },
    '... and read-only');
is_deeply(PVE::RS::Meta::api_access('9400', write_only()), { read => 0, write => 1 },
    '... and write-only');

# Read access is a boolean: any reader sees the whole document, the same as a
# full ACL -- there is no narrower structure to filter to.
is_deeply(PVE::RS::Meta::api_get('9400', undef, 'json', read_only())->{data},
    PVE::RS::Meta::api_get('9400', undef, 'json', $FULL)->{data},
    'a read-only caller sees exactly what a full one does');
$res = eval { PVE::RS::Meta::api_get('9400', undef, 'json', $NONE) };
ok(!defined($res), 'a caller with no read access cannot read a document');
like($@, api_error_status(403), 'that read is refused with 403:');

# Write access does not require read access (docs/DESIGN.md §4): there is no
# "must read what you write" rule any more, not even for the root view.
ok(defined(eval { PVE::RS::Meta::api_put('9400', undef, 'json', '{"other":2}', 'merge',
        undef, 0, write_only()) }),
    'write access alone is enough to merge into the root view');
is(PVE::RS::Meta::api_get('9400', 'other', 'json', $FULL)->{data}, 2, '... and it landed');

# A caller with no write access at all cannot write anything, even a change
# that touches no path: key order is not a path, so nothing else would stop it.
$res = eval { PVE::RS::Meta::api_put('9400', undef, 'json',
        encode_json({ traefik => { spec => { host => 'ct.example' } },
            netbird => { groups => ['lan'] }, other => 2 }),
        'replace', undef, 0, read_only()) };
ok(!defined($res), 'a read-only caller cannot write at all');
like($@, qr/no write access/, '... and the 403 says it has no write access at all');

# Reordering the keys changes no path, so it is allowed -- and it is a real
# change to the file, so it is stored. Literal JSON, not `encode_json`: a
# Perl hash has no key order, and the stored order is the assertion.
$res = PVE::RS::Meta::api_put('9400', undef, 'json',
    '{"other":2,"netbird":{"groups":["lan"]},"traefik":{"spec":{"host":"ct.example"}}}',
    'replace', undef, 0, $FULL);
is_deeply($res->{touched}, [], 'a reordering write touches no path');
like(read_file('9400.yaml'), qr/\Aother:/, '... and the new order is what is on disk');

# An empty merge at an arbitrary prefix creates nothing, for a caller with no
# write access to reach it with.
my $before_hack = PVE::RS::Meta::api_get('9400', undef, 'json', $FULL);
for my $hack_view ('zzz_hacked', 'zzz_hacked.deep') {
    $res = eval { PVE::RS::Meta::api_put('9400', $hack_view, 'json', '{}', 'merge', undef, 0, $NONE) };
    ok(!defined($res), "an empty merge at $hack_view is refused");
    like($@, api_error_status(403), "the empty merge at $hack_view is refused with 403:");
}
is_deeply(PVE::RS::Meta::api_get('9400', undef, 'json', $FULL)->{data}, $before_hack->{data},
    'no empty merge created any structure');

# Comment keys are notes (docs/DESIGN.md §2, §7): a read leaves them out unless
# `$comments`, and a replace without it carries none and keeps the stored ones.
write_file('9403.yaml', "__: the guest\nweb:\n  host__: public name\n  host: a\n  port: 80\n");
is_deeply(PVE::RS::Meta::api_get('9403', undef, 'json', $FULL)->{data},
    { web => { host => 'a', port => 80 } }, 'api_get leaves comment keys out by default');
is(PVE::RS::Meta::api_get('9403', undef, 'yaml', $FULL)->{text}, "web:\n  host: a\n  port: 80\n",
    '... and format=yaml is the canonical dump of what is left');
is(PVE::RS::Meta::api_get('9403', undef, 'yaml', $FULL, 1)->{text},
    "__: the guest\nweb:\n  host__: public name\n  host: a\n  port: 80\n",
    'with $comments a full reader gets the file itself');
$res = eval { PVE::RS::Meta::api_get('9403', 'web.host__', 'json', $FULL) };
like($@, api_error_status(400), 'a view naming a comment key without $comments is 400:');
my $kept = PVE::RS::Meta::api_put('9403', 'web', 'json', '{"host":"b"}', 'replace', undef, 0, $FULL);
is_deeply([sort map { "$_->{op} $_->{path}" } @{ $kept->{touched} }],
    ['delete web.port', 'set web.host'], 'a replace without $comments reports only what it changed');
is(read_file('9403.yaml'), "__: the guest\nweb:\n  host__: public name\n  host: b\n",
    '... and keeps the notes whose subject it kept');
$res = eval { PVE::RS::Meta::api_put('9403', 'web', 'json', '{"host":"c","host__":"x"}', 'replace',
        undef, 0, $FULL) };
like($@, api_error_status(400), 'a replace without $comments may not carry a comment key');
PVE::RS::Meta::api_put('9403', 'web', 'json', '{"host":"c"}', 'replace', undef, 0, $FULL, 0, 1);
is(read_file('9403.yaml'), "__: the guest\nweb:\n  host: c\n",
    'with $comments the payload is the subtree, notes included');
write_file('9403.yaml', "web__: the site\nweb:\n  host: c\n");
PVE::RS::Meta::api_delete('9403', 'web', undef, $FULL);
is(read_file('9403.yaml'), "{}\n", 'a DELETE of a view takes its note along');
$res = eval { PVE::RS::Meta::api_list_guests([{ vmid => 9403, read => 1 }], 'web__') };
like($@, api_error_status(400), 'has= naming a comment key is 400:');
unlink("$root/9403.yaml");

# A registry document uses only the ACL it is given, same as a guest.
is_deeply(PVE::RS::Meta::api_access('prefixes/traefik', $NONE), { read => 0, write => 0 },
    'a registry document is not readable by an empty ACL');
$res = eval { PVE::RS::Meta::api_get('prefixes/traefik', 'traefik', 'json', $NONE) };
ok(!defined($res), 'and a read of one with no read bit is refused');
like($@, api_error_status(403), 'that read is refused with 403:');

# A malformed file is skipped with a warning and defines nothing; it is not
# invisible: the listing carries a row for it too, named, with 'error' set
# and nothing else -- the one place an administrator can find out a file
# stopped loading at all, instead of only a log line nobody reads
# (docs/DESIGN.md §1).
write_prefix('brokenns', "selector: { nonsense: true }\n");
write_prefix('a b', "selector: { all: true }\n"); # not a valid file name, so not addressable at all

my $prefixes_all = PVE::RS::Meta::api_prefixes();
is(scalar(@$prefixes_all), 4, 'api_prefixes lists the three good ones plus the malformed one');
my @ns_failed = grep { exists $_->{error} } @$prefixes_all;
is(scalar(@ns_failed), 1, '... one of them carries an error');
is($ns_failed[0]->{prefix}, 'brokenns', '... named by prefix, the same key a loaded prefix uses');
ok(!exists $ns_failed[0]->{path}, '... no filesystem path is on the wire');
ok(!exists $ns_failed[0]->{selector} && !exists $ns_failed[0]->{schema},
    '... nothing a loaded prefix promises');
ok(!(grep { defined($_->{prefix}) && $_->{prefix} eq 'a b' } @$prefixes_all),
    "'a b' cannot be addressed at all, so it is not even a failure row");

unlink("$nsdir/brokenns.yaml", "$nsdir/a b.yaml");

# =========================================================================
# api_list_guests: native rows in, native rows out.
# =========================================================================

sub guest_row {
    my ($vmid, %opts) = @_;
    return {
        vmid => int($vmid),
        node => 'n1',
        type => 'lxc',
        name => "guest-$vmid",
        tags => $opts{tags} // [],
        read => $opts{read} // 0,
    };
}

my $rows = [guest_row(9400, read => 1, tags => ['traefik']), guest_row(9401, read => 1)];
my $listed = PVE::RS::Meta::api_list_guests($rows, undef);
is(scalar(@$listed), 2, 'api_list_guests lists every guest the caller has VM.Audit on');
my ($g9400) = grep { $_->{vmid} == 9400 } @$listed;
my ($g9401) = grep { $_->{vmid} == 9401 } @$listed;
is($g9400->{node}, 'n1', 'api_list_guests reports the node Perl passed in');
is($g9400->{type}, 'lxc', 'and the type');
is($g9400->{name}, 'guest-9400', 'and the display name');
is_deeply($g9400->{tags}, ['traefik'], 'and the tags (docs/DESIGN.md §4)');
is($g9401->{digest}, '', 'digest is "" for a guest with no document');

# A guest without VM.Audit is omitted entirely -- never included with its
# fields hidden, since there is no partial visibility any more.
is(scalar(@{ PVE::RS::Meta::api_list_guests([guest_row(9400)], undef) }), 0,
    'api_list_guests omits a guest the caller cannot read');

# `has` filters against the guest's document, and only among guests already
# included by `read`.
is(scalar(@{ PVE::RS::Meta::api_list_guests($rows, 'traefik.spec') }), 1,
    'has=traefik.spec matches the guest that has it');
is(scalar(@{ PVE::RS::Meta::api_list_guests($rows, 'traefik.spec.port') }), 0,
    'has=traefik.spec.port does not match');
is(scalar(@{ PVE::RS::Meta::api_list_guests([guest_row(9400, tags => ['traefik'])], 'traefik.spec') }), 0,
    'has cannot see through a caller\'s own missing read access');

# =========================================================================
# The one lint (docs/DESIGN.md §7), and parse failures.
# =========================================================================

for my $case (
    ['traefik__', 'replace', '5', qr/comment key value must be a string/],
    ['traefik', 'replace', '{"bad key":1}', qr/invalid key/],
    ['traefik', 'replace', '{"deep":{"a.b":1}}', qr/no dots/],
    ['traefik', 'replace', '{"list":[{"bad key":1}]}', qr/invalid key/],
    ['traefik', 'merge', '{"x__":5}', qr/comment key value must be a string/],
    ['traefik', 'replace', '{"nul":null}', qr/null values are not allowed/],
    # A view *through* a comment key needs no rule of its own: the lint on the
    # planned document is what refuses the map it would have to materialise.
    ['traefik.q__.r', 'replace', '1', qr/comment key value must be a string/],
) {
    my ($view, $mode, $payload, $re) = @$case;
    for my $who (['FULL', $FULL], ['WRITE-ONLY', write_only()]) {
        my ($label, $a) = @$who;
        $res = eval { PVE::RS::Meta::api_put('9400', $view, 'json', $payload, $mode, undef, 0, $a, 0, 1) };
        ok(!defined($res), "[$label] a $mode of $payload at $view is refused");
        like($@, api_error_status(400), "[$label] ... with 400:");
        like($@, $re, "[$label] ... naming the rule");
    }
}

# There is no redaction: the 400 names the offending path, whoever asks.
write_file('9502.yaml', "traefik:\n  host: x\nsecret_area:\n  customer name: acme\n");
for my $who (['FULL', $FULL], ['WRITE-ONLY', write_only()]) {
    my ($label, $a) = @$who;
    $res = eval { PVE::RS::Meta::api_put('9502', 'traefik', 'json', '{"host":"y"}', 'replace',
            undef, 0, $a) };
    ok(!defined($res), "[$label] an out-of-band bad key blocks the write");
    like($@, qr/customer name/, "[$label] ... and the 400 names it (docs/DESIGN.md §1)");
}
unlink("$root/9502.yaml");

# An unparseable document: yaml + parse_error for a full reader asking for
# comments, 422 for json and for anyone else, repaired by a root replace (docs/DESIGN.md §7).
for my $broken ("a: 1\n\tb: 2\n", "a: &anc 1\nb: *anc\n", "a: 1\n  b: 2\n", "a: [\n") {
    (my $label = $broken) =~ s/\n/\\n/g;
    write_file('9500.yaml', $broken);

    my $doc = PVE::RS::Meta::api_get('9500', undef, 'yaml', $FULL, 1);
    ok(defined($doc->{parse_error}), "[$label] a full reader gets parse_error");
    is($doc->{text}, $broken, "[$label] ... with the raw text to repair from");
    isnt($doc->{digest}, '', "[$label] ... and the real digest");

    $res = eval { PVE::RS::Meta::api_get('9500', undef, 'json', $FULL) };
    ok(!defined($res), "[$label] format=json is refused");
    like($@, api_error_status(422), "[$label] ... with 422:");
    $res = eval { PVE::RS::Meta::api_get('9500', undef, 'yaml', $FULL) };
    like($@, api_error_status(422), "[$label] ... and so is yaml without comments");

    $res = eval { PVE::RS::Meta::api_get('9500', undef, 'json', read_only()) };
    ok(!defined($res), "[$label] any reader gets the same 422 for format=json");
    like($@, api_error_status(422), "[$label] ... 422 there too");

    # A narrower write would plan against the empty document: refused.
    $res = eval { PVE::RS::Meta::api_put('9500', 'x', 'json', '{"a":1}', 'replace', undef, 0, $FULL) };
    ok(!defined($res), "[$label] a view write against it is refused");
    like($@, qr/repaired as a whole/, "[$label] the 400 says how to repair it");
    is(read_file('9500.yaml'), $broken, "[$label] and nothing was written");

    # The documented repair, with the digest precondition.
    PVE::RS::Meta::api_put('9500', undef, 'yaml', "traefik:\n  host: fixed\n", 'replace',
        $doc->{digest}, 0, $FULL);
    is(read_file('9500.yaml'), "traefik:\n  host: fixed\n", "[$label] a root replace repairs it");

    # ... and a root DELETE is the other repair shape.
    write_file('9500.yaml', $broken);
    PVE::RS::Meta::api_delete('9500', undef, undef, $FULL);
    ok(!file_exists('9500.yaml'), "[$label] a root DELETE removes it");
}

# A document that parses fine but is not a mapping -- an empty or
# comment-only file above all -- is the same condition as an unparseable one:
# there is no structure a narrower write could preserve (docs/DESIGN.md §7).
for my $text ("", "# only a comment\n", "- a\n- b\n", "just a scalar\n") {
    (my $label = $text) =~ s/\n/\\n/g;
    write_file('9505.yaml', $text);

    my $doc = PVE::RS::Meta::api_get('9505', undef, 'yaml', $FULL, 1);
    ok(defined($doc->{parse_error}), "[$label] a full reader is told it is not a document");
    is($doc->{text}, $text, "[$label] ... with the raw text to repair from");

    $res = eval { PVE::RS::Meta::api_get('9505', undef, 'json', $FULL) };
    ok(!defined($res), "[$label] format=json is refused");
    like($@, api_error_status(422), "[$label] ... with 422:");

    for my $shape ([ 'x', 'replace' ], [ 'x', 'merge' ], [ undef, 'merge' ]) {
        my ($view, $mode) = @$shape;
        $res = eval { PVE::RS::Meta::api_put('9505', $view, 'json', '{"a":1}', $mode, undef, 0, $FULL) };
        ok(!defined($res), "[$label] a $mode narrower than a whole-file replace is refused");
        like($@, qr/repaired as a whole/, "[$label] ... and says how to repair it");
    }
    is(read_file('9505.yaml'), $text, "[$label] and nothing was written");

    PVE::RS::Meta::api_put('9505', undef, 'yaml', "a: 1\n", 'replace', $doc->{digest}, 0, $FULL);
    is(read_file('9505.yaml'), "a: 1\n", "[$label] a root replace repairs it");

    write_file('9505.yaml', $text);
    PVE::RS::Meta::api_delete('9505', undef, undef, $FULL);
    ok(!file_exists('9505.yaml'), "[$label] a root DELETE removes it");
}

# A document above the read cap: reported, never rendered, and repairable by
# exactly the same two whole-file shapes. Its bytes are never read -- not by
# the GET, not by the listing, and not by the version poll.
write_file('9504.yaml', "a: \"" . ('x' x (4 * 1024 * 1024)) . "\"\n");
for my $fmt (qw(json yaml)) {
    $res = eval { PVE::RS::Meta::api_get('9504', undef, $fmt, $FULL) };
    ok(!defined($res), "a document above the read cap is reported, not rendered ($fmt)");
    like($@, api_error_status(422), "... with 422: ($fmt)");
    like($@, qr/too large/, "... and says why ($fmt)");
}
my ($listed_big) = grep { $_->{vmid} == 9504 }
    @{ PVE::RS::Meta::api_list_guests([guest_row(9504, read => 1)], undef) };
ok(defined($listed_big), 'one oversized document does not take the listing down');
isnt($listed_big->{digest}, '', '... and it is listed with an identity of its own');
ok(defined(PVE::RS::Meta::api_version()->{token}), '... nor the version poll');

$res = eval { PVE::RS::Meta::api_put('9504', 'x', 'json', '{"a":1}', 'replace', undef, 0, $FULL) };
ok(!defined($res), 'a view write against an oversized document is refused');
like($@, qr/repaired as a whole/, '... and says how to repair it');

PVE::RS::Meta::api_put('9504', undef, 'json', '{"a":1}', 'replace', $listed_big->{digest}, 0, $FULL);
is_deeply(PVE::RS::Meta::api_get('9504', undef, 'json', $FULL)->{data}, { a => 1 },
    'and a root replace repairs it, against the digest the listing reported');
PVE::RS::Meta::api_delete('9504', undef, undef, $FULL);

# A write that changes no path and would put back the bytes already on disk
# is skipped: `version()`'s token does not move for it.
write_file('9506.yaml', "traefik:\n  spec:\n    host: x\n");
my $noop_before = PVE::RS::Meta::api_version();
my $noop = PVE::RS::Meta::api_put('9506', 'traefik.spec', 'json', '{}', 'merge', undef, 0, $FULL);
is_deeply($noop->{touched}, [], 'a no-op merge touches nothing');
is_deeply(PVE::RS::Meta::api_version(), $noop_before, '... and moves neither token');
is(read_file('9506.yaml'), "traefik:\n  spec:\n    host: x\n", '... and rewrites nothing');
unlink("$root/9506.yaml");

# A document another caller removed under us is a 404-shaped absence, never a
# 500: reads are unlocked, so a DELETE or the GC can win any race.
write_file('9507.yaml', "traefik:\n  host: x\n");
my $vanishing = PVE::RS::Meta::api_get('9507', undef, 'json', $FULL);
unlink("$root/9507.yaml");
is_deeply(PVE::RS::Meta::api_get('9507', undef, 'json', $FULL)->{data}, {},
    'a document that vanished reads as the empty document');
is(PVE::RS::Meta::api_delete('9507', undef, undef, $FULL)->{digest}, '',
    'and deleting it again is that request satisfied');
$res = eval { PVE::RS::Meta::api_put('9507', undef, 'yaml', "a: 1\n", 'replace',
    $vanishing->{digest}, 0, $FULL) };
ok(!defined($res), 'its stale digest is still a precondition failure');
like($@, api_error_status(409), '... a 409, not a 500');

# api_delete: a view removes a subtree, the root removes the file.
my $before_del = PVE::RS::Meta::api_get('9400', undef, 'json', $FULL);
my $del = PVE::RS::Meta::api_delete('9400', 'netbird', $before_del->{digest}, $FULL);
is_deeply($del->{touched}, [{ path => 'netbird.groups', op => 'delete' }],
    'api_delete reports the removed leaf');
is(PVE::RS::Meta::api_get('9400', undef, 'json', $FULL)->{data}->{netbird}, undef,
    'api_delete removed the netbird subtree');

$res = eval { PVE::RS::Meta::api_delete('9400', 'traefik', 'deadbeef', $FULL) };
ok(!defined($res), 'api_delete dies on a digest mismatch');
like($@, api_error_status(409), 'api_delete digest-mismatch error is prefixed 409:');

my $wipe = PVE::RS::Meta::api_delete('9400', undef,
    PVE::RS::Meta::api_get('9400', undef, 'json', $FULL)->{digest}, $FULL);
is($wipe->{digest}, '', 'api_delete without a view leaves digest "" (the file is gone)');
ok(!file_exists('9400.yaml'), 'api_delete without a view actually removes the file');

# -- the meta-schema ---------------------------------------------------------

my $schemas = PVE::RS::Meta::api_schemas();
is(ref($schemas), 'HASH', 'api_schemas returns a native hash');
is_deeply([sort keys %$schemas], ['prefix'], '... one schema, for the one registry kind');
is($schemas->{prefix}->{properties}->{selector}->{type}, 'object',
    'the prefix schema describes its selector');
ok(!defined($schemas->{prefix}->{properties}->{selector}->{optional}),
    '... as required, which is what the parser enforces');
ok($schemas->{prefix}->{properties}->{schema}->{optional},
    'a prefix schema is optional');
ok(!defined($schemas->{prefix}->{properties}->{schema}->{properties}),
    '... and free-form: no properties, so the editor offers text rather than a form');
my $sel = $schemas->{prefix}->{properties}->{selector}->{properties};
is_deeply([sort keys %$sel], ['all', 'tag'], 'the selector describes both alternatives');
ok($sel->{all}->{optional} && $sel->{tag}->{optional},
    '... each individually optional: "exactly one of" is a rule the dialect cannot hold');
# ... so the parser is the only thing that enforces it, on the way in.
$res = eval { PVE::RS::Meta::api_put('prefixes/bothsel', undef, 'yaml',
    "selector: {all: true, tag: web}\n", 'replace', '', 0, $FULL) };
ok(!defined($res), 'a selector with both alternatives is refused');
like($@, api_error_status(400), '... with a 400');

# -- registry documents: a prefix is a document too ------------------------
#
# Same three functions, a second kind of id (`prefixes/<name>`), and one rule
# it does not share with a guest document: what is written has to parse as a
# prefix, because the loader *skips* a file it cannot parse. A 200 on a
# write that made the prefix disappear from api_prefixes() would be the
# worst possible answer, so it is a 400 instead.

my $ADMIN = { authid => 'root@pam', read => 1, write => 1, tags => [] };

my $ns_put = PVE::RS::Meta::api_put(
    'prefixes/labtest', undef, 'yaml',
    "selector:\n  all: true\ndescription: Home lab\n",
    'replace', '', 0, $ADMIN,
);
is($ns_put->{id}, 'prefixes/labtest', 'api_put creates a prefix document');
ok(-f "$nsdir/labtest.yaml", 'the file lands in the prefix directory, not the store root');
ok(!file_exists('labtest.yaml'), '... and nothing appeared beside the guest documents');

my ($labtest) = grep { $_->{prefix} eq 'labtest' } @{ PVE::RS::Meta::api_prefixes() };
ok($labtest, 'the loader picks up what the write produced');
is($labtest->{description}, 'Home lab', '... with the description it was given');

my $ns_doc = PVE::RS::Meta::api_get('prefixes/labtest', undef, 'json', $ADMIN);
is($ns_doc->{digest}, $ns_put->{digest}, 'api_get of a prefix agrees with the write');
is(ref($ns_doc->{data}->{selector}), 'HASH',
    'api_get returns it as a native hash like any other document');
# A YAML boolean crosses the boundary as a plain Perl truth value, not as a
# JSON::PP object -- the same convention the `types` round trip above checks.
ok($ns_doc->{data}->{selector}->{all} && !ref($ns_doc->{data}->{selector}->{all}),
    'and `selector: { all: true }` arrives as a plain true scalar');

# A view write reaches into it, with the same digest compare-and-swap.
PVE::RS::Meta::api_put('prefixes/labtest', 'schema.type', 'json', '"object"',
    'replace', $ns_doc->{digest}, 0, $ADMIN);
($labtest) = grep { $_->{prefix} eq 'labtest' } @{ PVE::RS::Meta::api_prefixes() };
is($labtest->{schema}->{type}, 'object', 'a view write reached into the prefix file');

# The gate: a prefix with no selector is one the loader would skip.
$res = eval { PVE::RS::Meta::api_put('prefixes/broken', undef, 'yaml',
    "description: nothing else\n", 'replace', '', 0, $ADMIN) };
ok(!defined($res), 'api_put refuses a prefix the loader could not read back');
like($@, api_error_status(400), '... with a 400');
ok(!-e "$nsdir/broken.yaml", '... and wrote nothing');

# A nested prefix is a dotted file name, and has to be addressable: the
# file name *is* the prefix, and `homelab.docker` was declared above.
my $nested = PVE::RS::Meta::api_get('prefixes/homelab.docker', undef, 'json', $ADMIN);
is($nested->{id}, 'prefixes/homelab.docker', 'a nested prefix is addressable by its file name');
is_deeply($nested->{data}->{schema}, { type => 'object' },
    '... and reads back the file the loader reads');

# An id that could address a file outside the directory is not an id, and
# neither is a registry kind that no longer exists.
for my $bad ('prefixes/../../etc/passwd', 'prefixes/a/b', 'prefixes/a..b', 'operators/traefik',
    'permissions/ops') {
    $res = eval { PVE::RS::Meta::api_get($bad, undef, 'json', $ADMIN) };
    ok(!defined($res), "api_get refuses the id '$bad'");
    like($@, api_error_status(400), "... with a 400");
}

# -- a node's schema override, inside the prefix file (docs/DESIGN.md §3) ------
#
# `nodes: { <node>: { schema?, enforce?, hidden? } }` inside a prefix file
# replaces the top-level fields, whole, for a guest on that node -- never a
# separate document, never a separate directory. The node crosses as `node`
# in the ACL hash; a guest's tags decide whether the prefix reaches it at all.

PVE::RS::Meta::api_put(
    'prefixes/gpu', undef, 'yaml',
    "selector: {all: true}\nenforce: true\n" .
    "schema: {type: object, properties: {count: {type: integer}}}\n" .
    "nodes:\n  pve1: {enforce: false}\n",
    'replace', '', 0, $ADMIN,
);

my ($gpu_raw) = grep { $_->{prefix} eq 'gpu' } @{ PVE::RS::Meta::api_prefixes() };
ok($gpu_raw, 'the raw listing carries the new prefix');
is_deeply($gpu_raw->{nodes}, { pve1 => { enforce => 0 } },
    '... with its nodes map as parsed');

my ($gpu_pve1) = grep { $_->{prefix} eq 'gpu' } @{ PVE::RS::Meta::api_prefixes('pve1', []) };
ok(!exists($gpu_pve1->{nodes}), 'a resolved row carries no nodes map');
ok(!$gpu_pve1->{enforce}, "pve1's override turns enforcement off");
my ($gpu_pve2) = grep { $_->{prefix} eq 'gpu' } @{ PVE::RS::Meta::api_prefixes('pve2', []) };
ok($gpu_pve2->{enforce}, 'another node keeps the top-level default');

for my $bad ('a b', '..', 'pve1/../x') {
    $res = eval { PVE::RS::Meta::api_prefixes($bad, []) };
    ok(!defined($res), "api_prefixes refuses the node '$bad'");
    like($@, api_error_status(400), "... with a 400");
}

# The node crosses as a checked node name: the ACL hash refuses one that is not.
$res = eval { PVE::RS::Meta::api_put('9600', 'x', 'json', '1', 'replace', undef, 0, { %$ADMIN, node => '../pve1' }) };
ok(!defined($res), 'an ACL hash whose node is not a node name is refused');
ok(!exists(PVE::RS::Meta::api_access('9600', { %$ADMIN, node => 'pve1' })->{node}),
    'api_access does not hand the node back: the prefix listing resolves it by id');

# enforce follows the guest's node.
my $on_pve1 = { %$ADMIN, node => 'pve1' };
my $on_pve2 = { %$ADMIN, node => 'pve2' };
ok(defined(eval { PVE::RS::Meta::api_put('9600', 'gpu.count', 'json', '"two"', 'replace', undef, 0, $on_pve1) }),
    "pve1's override turns enforcement off for its guests");
$res = eval { PVE::RS::Meta::api_put('9601', 'gpu.count', 'json', '"two"', 'replace', undef, 0, $on_pve2) };
ok(!defined($res), 'the top-level default still enforces for a guest on another node');
like($@, api_error_status(422), '... with a 422');

# =========================================================================
# A store that is not there (docs/DESIGN.md §7): every export refuses, with
# a 503 where the API has a status, instead of answering "no documents".
# =========================================================================

write_file('9700.yaml', "a: 1\n");
{
    local $ENV{PVE_META_CLUSTER_MARKER} = "$root/local";
    for my $case (
        ['api_get', sub { PVE::RS::Meta::api_get('9700', undef, 'json', $FULL) }],
        ['api_put', sub { PVE::RS::Meta::api_put('9700', 'b', 'json', '1', 'replace', undef, 0, $FULL) }],
        ['api_delete', sub { PVE::RS::Meta::api_delete('9700', undef, undef, $FULL) }],
        ['api_version', sub { PVE::RS::Meta::api_version() }],
        ['api_list_guests', sub { PVE::RS::Meta::api_list_guests([{ vmid => 9700, read => 1 }], undef) }],
        ['api_access', sub { PVE::RS::Meta::api_access('9700', $FULL) }],
        ['api_prefixes', sub { PVE::RS::Meta::api_prefixes(undef, undef) }],
    ) {
        my ($name, $call) = @$case;
        $res = eval { $call->() };
        ok(!defined($res), "$name refuses while the cluster marker is missing");
        like($@, qr/^503: cluster filesystem not available/, "... with a 503:");
    }
    $res = eval { PVE::RS::Meta::stored_vmids() };
    like($@, qr/cluster filesystem not available/, 'stored_vmids refuses too, rather than listing nothing');
    is(read_file('9700.yaml'), "a: 1\n", 'and nothing was written or removed');

    symlink($root, "$root/local") or die "symlink: $!\n";
    is_deeply(PVE::RS::Meta::api_get('9700', undef, 'json', $FULL)->{data}, { a => 1 },
        'with the marker in place the same store answers');
    unlink("$root/local");
}
unlink("$root/9700.yaml");

done_testing();
