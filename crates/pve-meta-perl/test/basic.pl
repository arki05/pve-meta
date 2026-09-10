#!/usr/bin/perl

# Exercises every `PVE::RS::Meta` export against a temporary
# `PVE_META_ROOT` and a temporary operator drop-directory. Run via
# `make check` (from the crate root, which sed-patches a test copy of
# `Proxmox::Lib::PVEMeta` to load `target/{debug,release}/libpve_meta_rs.so`
# directly -- see `-I.` above and `Makefile`'s `all` target).
#
# The point of this file, beyond the contract itself, is the **boundary**
# (`docs/DESIGN.md` §5): permissions, guest lists and results cross as native Perl
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
my $grantdir = tempdir(CLEANUP => 1);
$ENV{PVE_META_ROOT} = $root;
$ENV{PVE_META_PREFIX_DIRS} = $nsdir;
$ENV{PVE_META_PERMISSION_DIRS} = $grantdir;

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

# The file name is the prefix (docs/DESIGN.md §3.1).
sub write_prefix {
    my ($name, $content) = @_;
    open(my $fh, '>', "$nsdir/$name.yaml") or die "failed to write $nsdir/$name.yaml: $!\n";
    print {$fh} $content;
    close($fh);
}

sub write_permission {
    my ($name, $content) = @_;
    open(my $fh, '>', "$grantdir/$name.yaml") or die "failed to write $grantdir/$name.yaml: $!\n";
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
# Snapshot hooks -- the only lifecycle exports (docs/DESIGN.md §6).
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
# The two hooks patched into PVE::AbstractConfig (docs/DESIGN.md §6). Both clear a
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

# Error -> die behaviour.
$res = eval { PVE::RS::Meta::on_snapshot(9001, 'not a valid name') };
ok(!defined($res), 'on_snapshot dies on an invalid snapshot name');
like($@, qr/invalid name/i, 'invalid-name error is readable');

# The lifecycle exports revision 5 removed (docs/DESIGN.md §10) are gone. Two names are
# deliberately *not* in this list: `on_destroy` came back with the create/destroy hooks
# (§6), and `api_permissions` came back in revision 6 meaning something else entirely -- the
# grant-file listing behind GET /meta/permissions, not revision 5's caller-scope lookup.
for my $gone (qw(on_clone export_for_backup import_from_backup
                 list_snapshots has_document)) {
    ok(!defined(&{"PVE::RS::Meta::$gone"}), "PVE::RS::Meta::$gone is not exported any more");
}

# gc() -- what replaces the destroy hook and the whole orphan concept.
# =========================================================================

write_file('9100.yaml', "traefik:\n  host: live\n");
write_file('999500.yaml', "traefik:\n  host: gone\n");
write_file('datacenter.yaml', "note: keep me\n");
PVE::RS::Meta::on_snapshot(9100, 'keep');
PVE::RS::Meta::on_snapshot(999500, 'snapA');
PVE::RS::Meta::on_snapshot(999500, 'snapB');

is(PVE::RS::Meta::gc([9100]), 3, 'gc removes the stale document and both its snapshot copies');
ok(file_exists('9100.yaml'), 'a guest still in the vmlist is untouched');
ok(file_exists('9100.keep.yaml'), '... and so are its snapshots');
ok(!file_exists('999500.yaml'), 'the stale document is gone');
ok(!file_exists('999500.snapA.yaml'), 'the stale snapshot copies are gone');
ok(!file_exists('999500.snapB.yaml'), '... both of them');
ok(file_exists('datacenter.yaml'), 'the datacenter document is never a guest');

is(PVE::RS::Meta::gc([9100]), 0, 'gc is idempotent');
is(PVE::RS::Meta::gc([9100, 999500]), 0, 'a vmid back in the vmlist is not removed');
# A whole sweep is a vmid the store does not have -- an empty vmlist is
# refused, because it is also what a process that skipped cfs_update() sees.
$res = eval { PVE::RS::Meta::gc([]) };
ok(!defined($res), 'gc refuses an empty vmlist, like gc_purge');
like($@, api_error_status(500), 'the empty-vmlist refusal is prefixed 500:');
is(PVE::RS::Meta::gc([999999]), 2, 'a sweep against a vmlist with no stored vmid removes everything stale');
ok(file_exists('datacenter.yaml'), '... still never the datacenter document');

# The two-phase GC /usr/libexec/pve-meta/gc actually runs: nominate under
# 'pve-meta-gc', then purge one vmid at a time under "pve-meta-$vmid" with the
# vmlist re-read *inside* that lock. Without the re-check, a document written
# after the outer vmlist read is deleted with its PUT already answered 200.
write_file('9100.yaml', "traefik:\n  host: live\n");
PVE::RS::Meta::on_snapshot(9100, 'keep');
is_deeply(PVE::RS::Meta::gc_candidates([9100]), [], 'nothing is stale against the live vmlist');

# ... the guest at 999500 is created and its metadata written afterwards.
write_file('999500.yaml', "traefik:\n  host: fresh\n");
is_deeply(PVE::RS::Meta::gc_candidates([9100]), [999500],
    'gc_candidates nominates the vmid missing from the snapshot');
is(PVE::RS::Meta::gc_purge(999500, [9100, 999500]), 0,
    'gc_purge keeps a vmid the re-read vmlist has');
ok(file_exists('999500.yaml'), 'the document written after the snapshot survives');

$res = eval { PVE::RS::Meta::gc_purge(999500, []) };
ok(!defined($res), 'gc_purge refuses an empty vmlist rather than purging');
like($@, qr/empty vmlist/, '... and says why');
ok(file_exists('999500.yaml'), '... and removed nothing');

is(PVE::RS::Meta::gc_purge(999500, [9100]), 1, 'gc_purge removes a vmid that really is gone');
ok(!file_exists('999500.yaml'), '... the document is gone');
is(PVE::RS::Meta::gc_purge(999500, [9100]), 0, 'gc_purge is idempotent');
ok(file_exists('9100.yaml'), 'a live guest is never touched by gc_purge');
ok(file_exists('datacenter.yaml'), '... and neither is the datacenter document');

unlink("$root/datacenter.yaml", "$root/9100.yaml", "$root/9100.keep.yaml");

# =========================================================================
# The perlmod boundary: native hashes and arrays.
# =========================================================================

# `$acl` is a native hash. perlmod converts a Perl scalar to a Rust bool by
# *truthiness*, so 1/0/''/undef all mean what a Perl programmer expects --
# this is the bug class the old hand-built `_permissions_json` existed to avoid
# (encode_json rendered 1/0 as JSON numbers, and serde wanted true/false).
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
# So seed one of each thing the walk has to tell apart: a guest document, the
# datacenter document, and a snapshot copy, which is *not* a document.
write_file('datacenter.yaml', "note: keep me\n");
write_file('9100.yaml', "a: 1\n");
write_file('9100.keep.yaml', "a: 1\n");

my $v = PVE::RS::Meta::api_version(0, undef);
like($v->{token}, qr/^[0-9a-f]{64}$/, 'api_version token is a sha256 hex string');
ok($v->{changed} >= 0, 'api_version changed is a unix timestamp');
ok(!defined($v->{documents}), 'no `documents` without detail');

my $vd = PVE::RS::Meta::api_version(1, undef);
is($vd->{token}, $v->{token}, 'detail does not change the token');
ok(ref($vd->{documents}) eq 'ARRAY', 'detail returns a documents array');
is_deeply([sort map { $_->{id} } @{ $vd->{documents} }], ['9100', '9200', 'datacenter'],
    'every guest and the datacenter document are listed by id, and the snapshot copy is not');

# Scoped to one document: its own file plus the registry directories, and
# nothing else. This is the form the editor polls, so the arity has to work
# across the perlmod boundary as well as the semantics.
my $scoped_v = PVE::RS::Meta::api_version(0, '9100');
like($scoped_v->{token}, qr/^[0-9a-f]{64}$/, 'api_version takes an id');
isnt($scoped_v->{token}, $v->{token}, 'a scoped token is its own token, not the store-wide one');
# `detail` lists what the token covers, which for a scoped token is this
# document plus the registry documents -- never the other guests.
is_deeply(
    [map { $_->{id} } @{ PVE::RS::Meta::api_version(1, '9100')->{documents} }],
    ['9100'],
    'detail with an id lists that document and no other guest',
);
write_file('9200.yaml', "moved: yes\n");
is(PVE::RS::Meta::api_version(0, '9100')->{token}, $scoped_v->{token},
    'another guest changing does not move a scoped token');
isnt(PVE::RS::Meta::api_version(0, undef)->{token}, $v->{token},
    '... though it does move the store-wide one');
write_file('9100.yaml', "a: 2\n");
isnt(PVE::RS::Meta::api_version(0, '9100')->{token}, $scoped_v->{token},
    'and the document itself changing does move it');

eval { PVE::RS::Meta::api_version(0, 'not-an-id') };
like($@, api_error_status(400), 'a garbage id is a 400, not a silent whole-store poll');

unlink("$root/datacenter.yaml", "$root/9100.yaml", "$root/9100.keep.yaml");

# A missing document is the empty document with digest "".
my $missing = PVE::RS::Meta::api_get('9101', undef, 'json', $FULL);
is($missing->{digest}, '', 'api_get digest is "" for a nonexistent document');
is_deeply($missing->{data}, {}, 'api_get data is {} for a nonexistent document -- a native hash');
ok(!defined($missing->{data_json}), 'there is no data_json field any more');
ok(!defined($missing->{keys}), 'and no `keys` wire field (docs/DESIGN.md §10)');

$res = eval { PVE::RS::Meta::api_get('not-a-vmid', undef, 'json', $FULL) };
ok(!defined($res), 'api_get dies for an id that is neither a vmid nor "datacenter"');
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

# Types survive the native round trip (this is what the JSON-string
# convention used to be justified by).
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

# reads require a permission
$res = eval { PVE::RS::Meta::api_get('9101', undef, 'json', $NONE) };
ok(!defined($res), 'a caller with no permission at all cannot read a document');
like($@, api_error_status(403), 'that read is refused with 403:');

# =========================================================================
# Prefixes, permissions, selectors and tags (docs/DESIGN.md §3).
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

write_permission('scoped', <<'YAML');
authid: scoped@pve!t1
description: The scoped test principal
rules:
  - prefix: traefik
    mode: rw
    selector: { tag: traefik }
  - prefix: netbird
    mode: ro
    selector: { all: true }
YAML

my $gs = PVE::RS::Meta::api_permissions();
is(scalar(@$gs), 1, 'api_permissions lists the permission');
is($gs->[0]->{name}, 'scoped', 'with the file name as its name');
is($gs->[0]->{authid}, 'scoped@pve!t1', 'and the authid');
is($gs->[0]->{description}, 'The scoped test principal', 'and the description');
is_deeply($gs->[0]->{rules}->[0]->{selector}, { tag => 'traefik' },
    'and the selector, as a native hash');
ok($gs->[0]->{rules}->[1]->{selector}->{all},
    '... and { all: true } is a hash spelled the way the file spells it, not a bare string');
is($gs->[0]->{rules}->[1]->{prefix}, 'netbird', 'and every rule');
ok(!exists $gs->[0]->{rules}->[0]->{schema}, 'a permission carries no schema');

sub scoped_acl {
    my (@tags) = @_;
    return { authid => 'scoped@pve!t1', read => 0, write => 0, tags => [@tags] };
}

PVE::RS::Meta::api_put('9400', undef, 'json',
    encode_json({
        traefik => { spec => { host => 'ct.example' } },
        netbird => { groups => ['lan'] },
        other => 1,
    }),
    'replace', undef, 0, $FULL);

# Tag off: only the all-guests netbird scope applies.
my $untagged = PVE::RS::Meta::api_access('9400', scoped_acl());
is(scalar(@{ $untagged->{scopes} }), 1, 'without the tag only the all: true scope applies');
is($untagged->{scopes}->[0]->{prefix}, 'netbird', '... the netbird one');
is_deeply(PVE::RS::Meta::api_get('9400', undef, 'json', scoped_acl())->{data},
    { netbird => { groups => ['lan'] } }, 'and the read sees only that subtree');
$res = eval { PVE::RS::Meta::api_put('9400', 'traefik.spec', 'json', '{"host":"x"}',
        'replace', undef, 0, scoped_acl()) };
ok(!defined($res), 'a write into the tag-gated prefix is refused while the tag is off');
like($@, api_error_status(403), 'that write is refused with 403:');

# Tag on: the traefik rw scope appears.
my $tagged = PVE::RS::Meta::api_access('9400', scoped_acl('traefik'));
is_deeply([map { $_->{prefix} } @{ $tagged->{scopes} }], ['traefik', 'netbird'],
    'with the tag, both scopes apply');
is_deeply([map { $_->{mode} } @{ $tagged->{scopes} }], ['rw', 'ro'], '... with their modes');

# The tags come back with the access answer, so an editor never has to read
# every document in the cluster (GET /meta/guests) to learn one guest's. They
# are filtered exactly as that endpoint filters them: no VM.Audit, no tags.
is_deeply($tagged->{tags}, [], 'a caller without VM.Audit is told its scopes but not the tags');
is_deeply(
    PVE::RS::Meta::api_access('9400',
        { authid => 'root@pam', read => 1, write => 1, tags => ['traefik', 'web'] })->{tags},
    ['traefik', 'web'],
    'a caller with VM.Audit gets the guest tags back',
);
is_deeply(PVE::RS::Meta::api_access('datacenter', $FULL)->{tags}, [],
    'the datacenter document has no tags');
is_deeply(PVE::RS::Meta::api_get('9400', undef, 'json', scoped_acl('traefik'))->{data},
    { traefik => { spec => { host => 'ct.example' } }, netbird => { groups => ['lan'] } },
    'and the read now carries both subtrees, but never `other`');
ok(defined(eval { PVE::RS::Meta::api_put('9400', 'traefik.spec', 'json', '{"host":"y"}',
            'replace', undef, 0, scoped_acl('traefik')) }),
    'a write into the tag-gated rw prefix now succeeds');

# A read-only scope is still read-only, and an unscoped view is refused.
$res = eval { PVE::RS::Meta::api_put('9400', 'netbird', 'json', '{"groups":["wan"]}',
        'replace', undef, 0, scoped_acl('traefik')) };
ok(!defined($res), 'a write into a read-only scope is refused');
like($@, api_error_status(403), 'that write is refused with 403:');

$res = eval { PVE::RS::Meta::api_get('9400', 'other', 'json', scoped_acl('traefik')) };
ok(!defined($res), 'a scoped principal cannot read a view outside its scopes');
like($@, api_error_status(403), 'that read is refused with 403:');

# A scope-only principal still cannot write the root view -- not because the root
# needs full write any more, but because it cannot *read* the whole document, and
# authorizing a write by what it changes needs the caller to be able to say what
# the document is (docs/DESIGN.md 3.4).
$res = eval { PVE::RS::Meta::api_put('9400', undef, 'json', '{"other":2}', 'merge', undef, 0,
        scoped_acl('traefik')) };
ok(!defined($res), 'a scope-only principal cannot write the root view');
like($@, qr/read the whole document/, 'the 403 explains that it cannot read the whole document');

# A caller with no write permission at all cannot write anything, even a change
# that touches no path: key order is not a path, so nothing else would stop it.
my $auditor = { authid => 'auditor@pve', read => 1, write => 0, tags => [] };
$res = eval { PVE::RS::Meta::api_put('9400', undef, 'json', '{"traefik":{"spec":{}}}',
        'replace', undef, 0, $auditor) };
ok(!defined($res), 'a read-only auditor cannot write at all');
like($@, qr/no write access/, '... and the 403 says it has no write access at all');

# -------------------------------------------------------------------------
# A write is authorized by what it *changes*, not by the view it names.
# -------------------------------------------------------------------------
#
# This principal has VM.Audit (so it can read and therefore compose a whole
# document) and no VM.Config.Options; what it may change is its rw scopes.
sub audit_scoped_acl {
    my (@tags) = @_;
    return { authid => 'scoped@pve!t1', read => 1, write => 0, tags => [@tags] };
}

write_permission('tworw', <<'YAML');
authid: scoped@pve!t1
rules:
  - prefix: traefik
    mode: rw
    selector: { all: true }
  - prefix: netbird
    mode: rw
    selector: { all: true }
YAML

# Literal JSON throughout this block, not `encode_json`: a Perl hash has no key
# order, and both the touched list and the stored key order are the assertions.
my $DOC_A = '{"traefik":{"host":"a"},"netbird":{"groups":["lan"]},"homelab":{"owner":"arki"}}';
PVE::RS::Meta::api_put('9401', undef, 'json', $DOC_A, 'replace', undef, 0, $FULL);

# One write spanning two granted prefixes. The narrowest view covering both is
# the document root, which used to be a flat 403.
$res = eval { PVE::RS::Meta::api_put('9401', undef, 'json',
    '{"traefik":{"host":"b"},"netbird":{"groups":["wan"]},"homelab":{"owner":"arki"}}',
    'replace', undef, 0, audit_scoped_acl()) };
ok(defined($res), 'one root write may span two granted prefixes');
is_deeply([map { $_->{path} } @{ $res->{touched} }], ['traefik.host', 'netbird.groups'],
    '... and it answers for exactly the two paths it changed');

# The same write, reaching one key further, is refused -- and changes nothing.
$res = eval { PVE::RS::Meta::api_put('9401', undef, 'json',
    '{"traefik":{"host":"c"},"netbird":{"groups":["wan"]},"homelab":{"owner":"mallory"}}',
    'replace', undef, 0, audit_scoped_acl()) };
ok(!defined($res), 'a root write that changes an ungranted key is refused');
like($@, api_error_status(403), '... with 403:');
is(PVE::RS::Meta::api_get('9401', 'homelab.owner', 'json', $FULL)->{data}, 'arki',
    '... and wrote nothing');

# Dropping an ungranted key is a change too -- this is the shape that loses data.
$res = eval { PVE::RS::Meta::api_put('9401', undef, 'json',
    '{"traefik":{"host":"b"},"netbird":{"groups":["wan"]}}',
    'replace', undef, 0, audit_scoped_acl()) };
ok(!defined($res), 'a root write that drops an ungranted key is refused');
is_deeply(PVE::RS::Meta::api_get('9401', 'homelab', 'json', $FULL)->{data}, { owner => 'arki' },
    '... and the key is still there');

# Reordering the keys changes no path, so it is allowed -- and it is a real
# change to the file, so it is stored.
$res = eval { PVE::RS::Meta::api_put('9401', undef, 'json',
    '{"homelab":{"owner":"arki"},"netbird":{"groups":["wan"]},"traefik":{"host":"b"}}',
    'replace', undef, 0, audit_scoped_acl()) };
ok(defined($res), 'reordering keys is a write a scoped principal may make');
is_deeply($res->{touched}, [], '... and it touches no path');
like(read_file('9401.yaml'), qr/\Ahomelab:/, '... and the new order is what is on disk');

unlink("$grantdir/tworw.yaml");
PVE::RS::Meta::api_delete('9401', undef, undef, $FULL);

# An empty merge at an arbitrary prefix creates nothing.
my $before_hack = PVE::RS::Meta::api_get('9400', undef, 'json', $FULL);
for my $hack_view ('zzz_hacked', 'zzz_hacked.deep') {
    $res = eval { PVE::RS::Meta::api_put('9400', $hack_view, 'json', '{}', 'merge', undef, 0,
            scoped_acl('traefik')) };
    ok(!defined($res), "an empty merge at $hack_view is refused");
    like($@, api_error_status(403), "the empty merge at $hack_view is refused with 403:");
}
is_deeply(PVE::RS::Meta::api_get('9400', undef, 'json', $FULL)->{data}, $before_hack->{data},
    'no empty merge created any structure');

# A scope on `traefik` covers the sibling comment key `traefik__` -- the one
# comment-key rule left (docs/DESIGN.md §3).
PVE::RS::Meta::api_put('9400', 'traefik__', 'json', '"the ingress config"', 'replace', undef, 0,
    scoped_acl('traefik'));
is(PVE::RS::Meta::api_get('9400', 'traefik__', 'json', scoped_acl('traefik'))->{data},
    'the ingress config', 'a scoped principal can write its own comment key');
$res = eval { PVE::RS::Meta::api_put('9400', 'netbird__', 'json', '"nope"', 'replace', undef, 0,
        scoped_acl('traefik')) };
ok(!defined($res), '... but not another key\'s comment (netbird is ro)');
PVE::RS::Meta::api_delete('9400', 'traefik__', undef, scoped_acl('traefik'));

# Scopes never apply to the datacenter document.
my $dc_scoped = PVE::RS::Meta::api_access('datacenter', scoped_acl('traefik'));
is_deeply($dc_scoped->{scopes}, [], 'no registration ever reaches the datacenter document');
$res = eval { PVE::RS::Meta::api_get('datacenter', 'traefik', 'json', scoped_acl('traefik')) };
ok(!defined($res), 'and a scoped datacenter read is refused');
like($@, api_error_status(403), 'that read is refused with 403:');

# A malformed file is skipped with a warning and contributes nothing; it never
# takes another file's grants away. Both directories, independently.
write_permission('broken', "authid: nope-not-an-authid\n");
write_permission('alsobroken', "authid: a\@pve\nrules:\n  - prefix: x\n    mode: sideways\n");
write_prefix('brokenns', "selector: { nonsense: true }\n");
write_prefix('a b', "selector: { all: true }\n"); # not a valid prefix, so not a definition
is(scalar(@{ PVE::RS::Meta::api_permissions() }), 1, 'a malformed permission file is skipped');
is(scalar(@{ PVE::RS::Meta::api_prefixes() }), 3, 'a malformed prefix file is skipped');
is(scalar(@{ PVE::RS::Meta::api_access('9400', scoped_acl('traefik'))->{scopes} }), 2,
    '... and the valid ones still grant exactly what they did');
unlink("$grantdir/broken.yaml", "$grantdir/alsobroken.yaml",
    "$nsdir/brokenns.yaml", "$nsdir/a b.yaml");

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
        write => $opts{write} // 0,
    };
}

my $rows = [guest_row(9400, read => 1, tags => ['traefik']), guest_row(9401, read => 1)];
my $listed = PVE::RS::Meta::api_list_guests('root@pam', $rows, undef);
is(scalar(@$listed), 2, 'api_list_guests lists every guest the caller fully reads');
my ($g9400) = grep { $_->{vmid} == 9400 } @$listed;
my ($g9401) = grep { $_->{vmid} == 9401 } @$listed;
is($g9400->{node}, 'n1', 'api_list_guests reports the node Perl passed in');
is($g9400->{type}, 'lxc', 'and the type');
is($g9400->{name}, 'guest-9400', 'and the display name');
is_deeply($g9400->{tags}, ['traefik'], 'and the tags (docs/DESIGN.md §5)');
is($g9401->{digest}, '', 'digest is "" for a guest with no document');

# A caller with no ACL and no matching registration never sees a guest.
is(scalar(@{ PVE::RS::Meta::api_list_guests('nobody@pve', [guest_row(9400)], undef) }), 0,
    'api_list_guests omits a guest the caller can read nothing of');

# A scoped caller sees the guests its selectors match, without node/name/tags.
my $scoped_rows = [guest_row(9400, tags => ['traefik']), guest_row(9401)];
my $scoped_list = PVE::RS::Meta::api_list_guests('scoped@pve!t1', $scoped_rows, undef);
is(scalar(@$scoped_list), 2, 'the all: true netbird scope makes every guest listed');
is($scoped_list->[0]->{node}, undef, 'api_list_guests hides the node without VM.Audit');
is($scoped_list->[0]->{name}, undef, '... the name');
is($scoped_list->[0]->{tags}, undef, '... and the tags');

# `has` filters against the caller's own visible data.
is(scalar(@{ PVE::RS::Meta::api_list_guests('root@pam', $rows, 'traefik.spec') }), 1,
    'has=traefik.spec matches the guest that has it');
is(scalar(@{ PVE::RS::Meta::api_list_guests('root@pam', $rows, 'traefik.spec.port') }), 0,
    'has=traefik.spec.port does not match');
is(scalar(@{ PVE::RS::Meta::api_list_guests('scoped@pve!t1', $scoped_rows, 'other') }), 0,
    'has cannot see through a caller\'s own missing scope');

# =========================================================================
# The one lint (docs/DESIGN.md §4), and parse failures.
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
    for my $who (['FULL', $FULL], ['SCOPED', scoped_acl('traefik')]) {
        my ($label, $a) = @$who;
        $res = eval { PVE::RS::Meta::api_put('9400', $view, 'json', $payload, $mode, undef, 0, $a) };
        ok(!defined($res), "[$label] a $mode of $payload at $view is refused");
        like($@, api_error_status(400), "[$label] ... with 400:");
        like($@, $re, "[$label] ... naming the rule");
    }
}

# There is no redaction: the 400 names the offending path, whoever asks.
write_file('9502.yaml', "traefik:\n  host: x\nsecret_area:\n  customer name: acme\n");
for my $who (['FULL', $FULL], ['SCOPED', scoped_acl('traefik')]) {
    my ($label, $a) = @$who;
    $res = eval { PVE::RS::Meta::api_put('9502', 'traefik', 'json', '{"host":"y"}', 'replace',
            undef, 0, $a) };
    ok(!defined($res), "[$label] an out-of-band bad key blocks the write");
    like($@, qr/customer name/, "[$label] ... and the 400 names it (docs/DESIGN.md §1)");
}
unlink("$root/9502.yaml");

# An unparseable document: yaml + parse_error for a full reader, 422 for json
# and for anyone else, repaired by a root replace (docs/DESIGN.md §4).
for my $broken ("a: 1\n\tb: 2\n", "a: &anc 1\nb: *anc\n", "a: 1\n  b: 2\n", "a: [\n") {
    (my $label = $broken) =~ s/\n/\\n/g;
    write_file('9500.yaml', $broken);

    my $doc = PVE::RS::Meta::api_get('9500', undef, 'yaml', $FULL);
    ok(defined($doc->{parse_error}), "[$label] a full reader gets parse_error");
    is($doc->{text}, $broken, "[$label] ... with the raw text to repair from");
    isnt($doc->{digest}, '', "[$label] ... and the real digest");

    $res = eval { PVE::RS::Meta::api_get('9500', undef, 'json', $FULL) };
    ok(!defined($res), "[$label] format=json is refused");
    like($@, api_error_status(422), "[$label] ... with 422:");

    $res = eval { PVE::RS::Meta::api_get('9500', undef, 'yaml', scoped_acl('traefik')) };
    ok(!defined($res), "[$label] and a scoped reader never gets the bytes");
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
# there is no structure a narrower write could preserve (docs/DESIGN.md §4).
for my $text ("", "# only a comment\n", "- a\n- b\n", "just a scalar\n") {
    (my $label = $text) =~ s/\n/\\n/g;
    write_file('9505.yaml', $text);

    my $doc = PVE::RS::Meta::api_get('9505', undef, 'yaml', $FULL);
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
    @{ PVE::RS::Meta::api_list_guests('root@pam', [guest_row(9504, read => 1)], undef) };
ok(defined($listed_big), 'one oversized document does not take the listing down');
isnt($listed_big->{digest}, '', '... and it is listed with an identity of its own');
ok(defined(PVE::RS::Meta::api_version(0, undef)->{token}), '... nor the version poll');

$res = eval { PVE::RS::Meta::api_put('9504', 'x', 'json', '{"a":1}', 'replace', undef, 0, $FULL) };
ok(!defined($res), 'a view write against an oversized document is refused');
like($@, qr/repaired as a whole/, '... and says how to repair it');

PVE::RS::Meta::api_put('9504', undef, 'json', '{"a":1}', 'replace', $listed_big->{digest}, 0, $FULL);
is_deeply(PVE::RS::Meta::api_get('9504', undef, 'json', $FULL)->{data}, { a => 1 },
    'and a root replace repairs it, against the digest the listing reported');
PVE::RS::Meta::api_delete('9504', undef, undef, $FULL);

# A write that changes no path and would put back the bytes already on disk
# is skipped: `version()`'s token does not move for it, so `changed` must not
# either.
write_file('9506.yaml', "traefik:\n  spec:\n    host: x\n");
my $noop_before = PVE::RS::Meta::api_version(0, undef);
my $noop = PVE::RS::Meta::api_put('9506', 'traefik.spec', 'json', '{}', 'merge', undef, 0, $FULL);
is_deeply($noop->{touched}, [], 'a no-op merge touches nothing');
is_deeply(PVE::RS::Meta::api_version(0, undef), $noop_before, '... and moves neither token nor changed');
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
is_deeply([sort keys %$schemas], ['permission', 'prefix'], '... one schema per registry kind');
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

# -- registry documents: prefixes and permissions are documents too --------------
#
# Same three functions, a third kind of id (`prefixes/<name>`), and one rule
# they do not share with the other two: what is written has to parse as the kind
# it claims to be, because the loader *skips* a file it cannot parse. A 200 on a
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

my $g_put = PVE::RS::Meta::api_put(
    'permissions/ops', undef, 'yaml',
    "authid: ops\@pve!t1\nrules:\n  - prefix: labtest\n    mode: rw\n    selector: {all: true}\n",
    'replace', '', 0, $ADMIN,
);
is($g_put->{id}, 'permissions/ops', 'api_put creates a permission document');
my ($ops) = grep { $_->{name} eq 'ops' } @{ PVE::RS::Meta::api_permissions() };
is($ops->{authid}, 'ops@pve!t1', 'the permission loader picks it up too');

# A partial delete is a write, and the same rule holds on that path.
$res = eval { PVE::RS::Meta::api_delete('permissions/ops', 'authid', undef, $ADMIN) };
ok(!defined($res), 'api_delete refuses to strip a permission of its authid');
like($@, api_error_status(400), '... with a 400');
($ops) = grep { $_->{name} eq 'ops' } @{ PVE::RS::Meta::api_permissions() };
ok($ops, 'the permission still loads');

is(PVE::RS::Meta::api_delete('permissions/ops', undef, undef, $ADMIN)->{digest}, '',
    'removing the whole file is an ordinary delete');
ok(!-e "$grantdir/ops.yaml", '... and the file is gone');

# A nested prefix is a dotted file name, and has to be addressable: the
# file name *is* the prefix, and `homelab.docker` was declared above.
my $nested = PVE::RS::Meta::api_get('prefixes/homelab.docker', undef, 'json', $ADMIN);
is($nested->{id}, 'prefixes/homelab.docker', 'a nested prefix is addressable by its file name');
is_deeply($nested->{data}->{schema}, { type => 'object' },
    '... and reads back the file the loader reads');

# An id that could address a file outside the directory is not an id.
for my $bad ('prefixes/../../etc/passwd', 'prefixes/a/b', 'prefixes/a..b', 'operators/traefik') {
    $res = eval { PVE::RS::Meta::api_get($bad, undef, 'json', $ADMIN) };
    ok(!defined($res), "api_get refuses the id '$bad'");
    like($@, api_error_status(400), "... with a 400");
}

done_testing();
