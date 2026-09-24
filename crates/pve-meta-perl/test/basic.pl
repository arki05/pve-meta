#!/usr/bin/perl

# Exercises `PVE::RS::Meta` against a temporary PVE_META_ROOT and prefix
# directory: the lifecycle hooks in full, and the perlmod boundary (native
# Perl structures, JSON-string `data`, 1/0 booleans, "NNN: message" errors)
# for the rest. The API rules themselves are pinned once, in Rust, by
# `crates/pve-meta-core/tests/api.rs`.

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

sub api_error_status {
    my ($code) = @_;
    return qr/^\Q$code\E: /;
}

# Runs $code, asserting it dies with a "$status: message" error (the
# Rust->Perl error contract) -- the recurring eval/ok/like triple.
sub dies_with {
    my ($status, $code, $label) = @_;
    my $res = eval { $code->() };
    ok(!defined($res), "$label dies");
    like($@, api_error_status($status), "$label with $status:");
    return $@;
}

my $res;

# --- version() ---------------------------------------------------------
like(PVE::RS::Meta::version(), qr/^\d+\.\d+\.\d+$/, 'version() looks like a semver string');

# --- split_tags() ------------------------------------------------------
# The rule is pinned in Rust (`tags::split_tags`); this is the boundary:
# undef in, an array ref out, as parse_tags hands it on.
is_deeply(PVE::RS::Meta::split_tags(undef), [], 'split_tags(undef) is no tags');
is_deeply(PVE::RS::Meta::split_tags('a;b,,c d'), [qw(a b c d)], 'split_tags splits the tag string');

# =========================================================================
# Lifecycle hooks (docs/DESIGN.md §7, docs/LIFECYCLE.md).
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

# on_create / on_destroy: the two hooks patched into PVE::AbstractConfig. Both
# clear a vmid's document and every snapshot copy; only the call site differs.
write_file('9300.yaml', "traefik:\n  host: old.example\n");
PVE::RS::Meta::on_snapshot(9300, 'snapA');
ok(file_exists('9300.yaml'), 'the doomed guest has a document');
ok(file_exists('9300.snapA.yaml'), '... and a snapshot copy');

is(PVE::RS::Meta::on_destroy(9300), 2, 'on_destroy removes the document and its snapshots');
ok(!file_exists('9300.yaml'), '... the document is gone');
ok(!file_exists('9300.snapA.yaml'), '... and so is the snapshot copy');
is(PVE::RS::Meta::on_destroy(9300), 0, 'on_destroy is idempotent');

# The reuse case a periodic sweep cannot see (docs/decisions/009-no-sweeper.md):
# the vmid comes straight back, so only a hook at creation clears the leftover.
write_file('9301.yaml', "traefik:\n  host: stale.example\n");
PVE::RS::Meta::on_snapshot(9301, 'snapB');
is(PVE::RS::Meta::on_create(9301), 2, 'on_create clears a leftover document and its snapshots');
ok(!file_exists('9301.yaml'), 'a guest created at a recycled vmid inherits nothing');
ok(!file_exists('9301.snapB.yaml'), '... not even an old snapshot copy');
is(PVE::RS::Meta::on_create(9301), 0, 'on_create on a clean vmid is a no-op');

# The vmid crosses the boundary as whatever scalar the caller holds: qemu-server
# passes the API parameter through as a string, pve-container as a number. Both
# must work, or every hook is a silent no-op for one guest type (`Vmid`, src/lib.rs).
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

# =========================================================================
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

is(PVE::RS::Meta::on_destroy(999500), 2, 'on_destroy removes a stale document and its snapshot copy');
is_deeply(PVE::RS::Meta::stored_vmids(), [9100, 9200], '... and the vmid leaves the list');
ok(file_exists('datacenter.yaml'), 'a stray file with no vmid in its name is never a guest');
is_deeply(PVE::RS::Meta::unknown_files(), ['datacenter.yaml'],
    '... and unknown_files names it, the row `pve-meta ls` prints as unknown/<file>');

unlink("$root/datacenter.yaml", "$root/9100.yaml", "$root/9100.keep.yaml", "$root/9200.old.yaml");

# =========================================================================
# export_for_backup / notes_import -- the two ends of a backup (docs/DESIGN.md §7):
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
# The perlmod boundary: native hashes and arrays, and truthiness.
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
unlink("$root/9200.yaml");

# =========================================================================
# api_*: one call per export, checking the shape that crosses -- native
# hash/array, JSON-string `data`, 1/0 booleans, an "NNN: message" error. The
# rules themselves (ACL policy, digest CAS, merge/replace semantics, the
# lint, enforcement, notes, unreadable-document handling, prefix resolution)
# are pinned once, in crates/pve-meta-core/tests/api.rs.
# =========================================================================

# api_version -> { token }, nothing else, a sha256 hex string.
my $v = PVE::RS::Meta::api_version();
is_deeply([sort keys %$v], ['token'], 'api_version answers exactly { token }');
like($v->{token}, qr/^[0-9a-f]{64}$/, '... a sha256 hex string');

dies_with(400, sub { PVE::RS::Meta::api_get('not-a-vmid', undef, 'json', $FULL) },
    'api_get for an id that is neither a vmid nor a registry id');

# api_put's `data` is the one JSON string; the payload and the result --
# including `touched`, a native array of { path, op } hashes -- are native
# structures both ways.
my $put1 = PVE::RS::Meta::api_put(
    '9101', undef, 'json', encode_json({ traefik => { spec => { host => 'a.example' } } }),
    'replace', undef, 0, $FULL,
);
is($put1->{id}, '9101', 'api_put creates the document with the right id');
is_deeply($put1->{touched}, [{ path => 'traefik', op => 'set' }],
    'api_put touched is a native array of { path, op } hashes');

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

# format=json vs format=yaml: two different return shapes for one export.
my $as_yaml = PVE::RS::Meta::api_get('9101', 'traefik.spec', 'yaml', $FULL);
ok(!defined($as_yaml->{data}), 'api_get format=yaml does not set data');
is($as_yaml->{text}, "host: a.example\n", 'api_get format=yaml renders the view as YAML text');
ok(!defined(PVE::RS::Meta::api_get('9101', 'traefik.spec', 'json', $FULL)->{text}),
    'api_get format=json does not set text');

# api_delete: a view removes a subtree and reports it, native array shape;
# the root removes the file, digest "" back.
my $del = PVE::RS::Meta::api_delete('9101', 'traefik.spec.host', undef, $FULL);
is_deeply($del->{touched}, [{ path => 'traefik.spec.host', op => 'delete' }],
    'api_delete touched is a native array of { path, op } hashes');
$del = PVE::RS::Meta::api_delete('9101', undef, $del->{digest}, $FULL);
is($del->{digest}, '', 'api_delete without a view leaves digest "" (the file is gone)');
ok(!file_exists('9101.yaml'), 'api_delete without a view actually removes the file');

# api_list_guests: native rows in, native rows out.
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
write_file('9410.yaml', "traefik:\n  spec: {host: x}\n");
my $listed = PVE::RS::Meta::api_list_guests([guest_row(9410, read => 1, tags => ['traefik'])], undef);
is(scalar(@$listed), 1, 'api_list_guests takes a native array of hashes and returns one');
is($listed->[0]->{node}, 'n1', '... with the fields Perl passed in');
is_deeply($listed->[0]->{tags}, ['traefik'], '... including tags, as a native array');
unlink("$root/9410.yaml");

# api_access -> exactly { read, write }, the ACL's own booleans.
is_deeply(PVE::RS::Meta::api_access('9410', $FULL), { read => 1, write => 1 },
    'api_access answers exactly { read, write }, native booleans');

# api_prefixes -> a native array of hashes; fields (description, selector,
# schema) pass through as given.
write_prefix('traefik', <<'YAML');
description: Traefik dynamic configuration
selector: { tag: traefik }
schema:
  type: object
  properties:
    spec: { type: object }
YAML
my $ns = PVE::RS::Meta::api_prefixes();
my ($traefik_ns) = grep { $_->{prefix} eq 'traefik' } @$ns;
is($traefik_ns->{description}, 'Traefik dynamic configuration', 'api_prefixes passes description through');
is_deeply($traefik_ns->{selector}, { tag => 'traefik' }, '... the selector, as a native hash');
ok($traefik_ns->{schema}, '... and the schema, passed through verbatim');

# api_schemas -> a native hash, keyed by registry kind.
my $schemas = PVE::RS::Meta::api_schemas();
is(ref($schemas), 'HASH', 'api_schemas returns a native hash');
is_deeply([sort keys %$schemas], ['prefix'], '... one schema, for the one registry kind');

# A YAML boolean crosses the boundary as a plain Perl truth value, not as a
# JSON::PP object -- the same convention the types round trip above checks.
write_prefix('booltest', "selector: {all: true}\n");
my $booltest = PVE::RS::Meta::api_get('prefixes/booltest', undef, 'json', $FULL);
ok($booltest->{data}->{selector}->{all} && !ref($booltest->{data}->{selector}->{all}),
    '`selector: { all: true }` arrives as a plain true scalar, not a JSON::PP::Boolean');
unlink("$nsdir/traefik.yaml", "$nsdir/booltest.yaml");

# =========================================================================
# A store that is not there (docs/DESIGN.md §5): every export refuses, with
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
        my $err = dies_with(503, $call, "$name while the cluster marker is missing");
        like($err, qr/cluster filesystem not available/, "... naming why");
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
