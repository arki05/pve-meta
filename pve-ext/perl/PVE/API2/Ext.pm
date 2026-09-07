package PVE::API2::Ext;

use strict;
use warnings;

use JSON;

use PVE::JSONSchema qw(get_standard_option);
use PVE::RESTHandler;
use PVE::Tools qw(file_get_contents);

use base qw(PVE::RESTHandler);

# `PVE::API2::Ext`: the generic Proxmox VE extension layer (see
# `pve-ext/README.md`). Loaded by a single `use PVE::API2::Ext;` line
# dpkg-diverted into stock `PVE/API2.pm` (see `patches/pve-manager.toml`).
# Two things happen, once, as a side effect of that `use`:
#
#   1. This module registers itself into the API root at `ext`
#      (`/api2/json/ext/...`), exposing three read-only endpoints of its
#      own: `GET /ext` (index), `GET /ext/modules` (which extension API
#      modules loaded, see below) and `GET /ext/pages` (the UI-tab
#      manifests `js/pve-ext-loader.js` fetches).
#   2. It scans `/usr/share/perl5/PVE/API2/Ext/*.pm` -- every other
#      package's extension API module -- `require`s each one and mounts
#      it directly into the API root at the path it declares via its own
#      `ext_path` class method (e.g. `PVE::API2::Ext::Meta` declaring
#      `sub ext_path { 'meta' }` ends up reachable at `/api2/json/meta`,
#      exactly like any other native PVE::API2 subclass).
#
# A module that fails to `require`, or that has no `ext_path`, is skipped
# with a `warn` -- pvedaemon/pveproxy load `PVE::API2` once at startup, so
# one broken extension must never take either daemon down. Nothing here
# is refreshed at runtime: the scan happens exactly once, when this file
# is first `require`d (i.e. once per pvedaemon/pveproxy worker startup);
# installing a new extension module (or page manifest -- see `GET
# /ext/pages` below, which *does* re-read its directory on every request)
# needs a service restart to be picked up, same as any other PVE::API2
# module.

my $EXT_MODULE_DIR = '/usr/share/perl5/PVE/API2/Ext';
my $EXT_PAGE_DIR = '/usr/share/pve-ext/pages';

my $VALID_TARGETS = { lxc => 1, qemu => 1, node => 1, dc => 1 };

# Populated once by _scan_and_register(), at the bottom of this file;
# `GET /ext/modules` reports exactly this list, it never re-scans.
my $loaded_modules = [];

# /usr/share/perl5/PVE/API2/Ext/Foo/Bar.pm -> "PVE::API2::Ext::Foo::Bar".
# Returns undef (never guesses) if $file isn't actually under the perl5
# root this module scans, or doesn't look like a .pm file.
sub _class_from_file {
    my ($file) = @_;

    my $rel = $file;
    return undef if $rel !~ s{^\Q/usr/share/perl5/\E}{};
    return undef if $rel !~ s{\.pm$}{};

    $rel =~ s{/}{::}g;
    return $rel;
}

sub _scan_and_register {
    my @files = sort glob("$EXT_MODULE_DIR/*.pm");

    for my $file (@files) {
        # pvedaemon/pveproxy run under `perl -T` (taint mode); glob()'s
        # results are tainted, and a bare `require $file` on a tainted
        # filename dies with "Insecure dependency in require while
        # running with -T switch". A regex match's captured group is not
        # tainted (standard Perl behaviour, see perlsec), so re-derive an
        # untainted $file by matching it against exactly the shape this
        # scan's own glob pattern can produce -- anything that doesn't
        # match that shape is refused, never blindly untainted.
        my ($untainted) = $file =~ m{^(\Q$EXT_MODULE_DIR\E/[A-Za-z0-9_]+\.pm)$};
        if (!$untainted) {
            warn "pve-ext: skipping '$file': unexpected filename shape, refusing to load it\n";
            next;
        }
        $file = $untainted;

        my $class = _class_from_file($file);
        if (!$class) {
            warn "pve-ext: skipping '$file': not a usable PVE::API2::Ext::* module path\n";
            next;
        }

        eval { require $file; };
        if ($@) {
            warn "pve-ext: skipping extension module '$class' ($file): failed to load: $@";
            next;
        }

        if (!$class->can('ext_path')) {
            warn "pve-ext: skipping extension module '$class' ($file): no ext_path() declared\n";
            next;
        }

        my $path = eval { $class->ext_path() };
        if ($@ || !defined($path) || $path eq '') {
            warn "pve-ext: skipping extension module '$class' ($file): ext_path() failed or returned nothing: $@";
            next;
        }

        eval { PVE::API2->register_method({ subclass => $class, path => $path }); };
        if ($@) {
            warn "pve-ext: skipping extension module '$class' ($file): register_method('$path') failed: $@";
            next;
        }

        push @$loaded_modules, { module => $class, path => $path, file => $file };
    }

    return;
}

# Reads and validates one page manifest (see pve-ext/README.md for the
# shape). Returns the manifest hash on success, or undef (with a warning)
# if the file isn't valid JSON or doesn't match the expected shape -- one
# malformed manifest must never break the whole `GET /ext/pages` response.
sub _load_page_manifest {
    my ($file) = @_;

    my $raw = eval { file_get_contents($file) };
    if ($@) {
        warn "pve-ext: skipping page manifest '$file': could not read: $@";
        return undef;
    }

    my $manifest = eval { decode_json($raw) };
    if ($@) {
        warn "pve-ext: skipping page manifest '$file': invalid JSON: $@";
        return undef;
    }

    if (ref($manifest) ne 'HASH') {
        warn "pve-ext: skipping page manifest '$file': not a JSON object\n";
        return undef;
    }

    for my $key (qw(id title targets url)) {
        if (!defined($manifest->{$key}) || $manifest->{$key} eq '') {
            warn "pve-ext: skipping page manifest '$file': missing or empty '$key'\n";
            return undef;
        }
    }

    if (ref($manifest->{targets}) ne 'ARRAY' || !scalar(@{ $manifest->{targets} })) {
        warn "pve-ext: skipping page manifest '$file': 'targets' must be a non-empty array\n";
        return undef;
    }
    for my $target (@{ $manifest->{targets} }) {
        if (!$VALID_TARGETS->{$target // ''}) {
            warn "pve-ext: skipping page manifest '$file': invalid target '"
                . ($target // '<undef>')
                . "' (must be one of: "
                . join(', ', sort keys %$VALID_TARGETS)
                . ")\n";
            return undef;
        }
    }

    if (defined($manifest->{requires}) && ref($manifest->{requires}) ne 'HASH') {
        warn "pve-ext: skipping page manifest '$file': 'requires' must be an object\n";
        return undef;
    }
    if (defined($manifest->{requires})) {
        for my $cap (keys %{ $manifest->{requires} }) {
            if (ref($manifest->{requires}->{$cap}) ne 'ARRAY') {
                warn "pve-ext: skipping page manifest '$file': 'requires.$cap' must be an array\n";
                return undef;
            }
        }
    }

    if (defined($manifest->{iconCls}) && ref($manifest->{iconCls})) {
        warn "pve-ext: skipping page manifest '$file': 'iconCls' must be a string\n";
        return undef;
    }

    return $manifest;
}

# -- our own API: GET /ext, /ext/modules, /ext/pages -----------------------

__PACKAGE__->register_method({
    name => 'index',
    path => '',
    method => 'GET',
    permissions => { user => 'all' },
    description => "Directory index of the pve-ext extension layer.",
    parameters => {
        additionalProperties => 0,
        properties => {},
    },
    returns => {
        type => 'array',
        items => {
            type => "object",
            properties => {
                subdir => { type => 'string' },
            },
        },
        links => [{ rel => 'child', href => "{subdir}" }],
    },
    code => sub {
        return [{ subdir => 'modules' }, { subdir => 'pages' }];
    },
});

__PACKAGE__->register_method({
    name => 'modules',
    path => 'modules',
    method => 'GET',
    permissions => { user => 'all' },
    description => "List the PVE::API2::Ext::* modules pve-ext loaded and "
        . "mounted into the API root at startup.",
    parameters => {
        additionalProperties => 0,
        properties => {},
    },
    returns => {
        type => 'array',
        items => {
            type => 'object',
            properties => {
                module => { type => 'string', description => 'The Perl package name.' },
                path => { type => 'string', description => 'API root path it is mounted at.' },
            },
        },
    },
    code => sub {
        return [map { { module => $_->{module}, path => $_->{path} } } @$loaded_modules];
    },
});

__PACKAGE__->register_method({
    name => 'pages',
    path => 'pages',
    method => 'GET',
    permissions => { user => 'all' },
    description => "List the UI-tab page manifests found under "
        . "/usr/share/pve-ext/pages/*.json (see pve-ext/README.md). This "
        . "endpoint only validates manifest *shape*; it does not enforce "
        . "the 'requires' privileges itself -- js/pve-ext-loader.js does "
        . "that client-side (so a user simply never sees a tab they can't "
        . "use), and each page's own backend API enforces access "
        . "server-side. Re-reads the directory on every call, so dropping "
        . "in a new manifest takes effect immediately, without a service "
        . "restart.",
    parameters => {
        additionalProperties => 0,
        properties => {},
    },
    returns => {
        type => 'array',
        items => {
            type => 'object',
            properties => {
                id => { type => 'string' },
                title => { type => 'string' },
                iconCls => { type => 'string', optional => 1 },
                targets => { type => 'array', items => { type => 'string' } },
                url => { type => 'string' },
                requires => { type => 'object', optional => 1 },
            },
        },
    },
    code => sub {
        my @manifests;
        for my $file (sort glob("$EXT_PAGE_DIR/*.json")) {
            my $manifest = _load_page_manifest($file);
            push @manifests, $manifest if $manifest;
        }
        return \@manifests;
    },
});

# -- mount ourselves at 'ext', then discover & mount every other
#    extension module at the path it declares -----------------------------

PVE::API2->register_method({ subclass => __PACKAGE__, path => 'ext' });

_scan_and_register();

1;
