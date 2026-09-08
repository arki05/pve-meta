package PVE::API2::Ext;

use strict;
use warnings;

use JSON;

use PVE::JSONSchema qw(get_standard_option);
use PVE::RESTHandler;
use PVE::Tools qw(file_get_contents);

use base qw(PVE::RESTHandler);

# `PVE::API2::Ext`: the generic Proxmox VE extension layer (see
# `pve-ext/README.md`). `require`d, then explicitly driven by one
# `PVE::API2::Ext->register_all();` call, from the very end of stock
# `PVE/API2.pm` -- i.e. *after* every one of PVE::API2's own runtime
# `register_method` calls have already run (see `patches/pve-manager.toml`
# and the comment on `register_all()` below for why order matters here).
# Two things happen, once, inside that explicit `register_all()` call:
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
# A module that fails to `require`, that has no `ext_path`, or whose
# `ext_path` collides with a path some other module (core or extension)
# already holds -- including `ext` itself, always reserved for this
# module -- is skipped with a `warn`, never a `die`: pvedaemon/pveproxy
# load `PVE::API2` once at startup, so one broken or colliding extension
# must never take either daemon down with it. Nothing here is refreshed
# at runtime: the scan happens exactly once, when `register_all()` runs
# (i.e. once per pvedaemon/pveproxy worker startup); installing a new
# extension module (or page manifest -- see `GET /ext/pages` below, which
# *does* re-read its directory on every request) needs a service restart
# to be picked up, same as any other PVE::API2 module.

my $EXT_MODULE_DIR = '/usr/share/perl5/PVE/API2/Ext';
my $EXT_PAGE_DIR = '/usr/share/pve-ext/pages';

my $VALID_TARGETS = { lxc => 1, qemu => 1, node => 1, dc => 1 };

# Path names no extension module may ever claim, regardless of whether
# anything has registered them yet: `ext` is this module's own mount
# point (registered by register_all() itself, guarded the same way as
# any other module below), so a third-party module racing to claim it
# first must never be allowed to win.
my $RESERVED_EXT_PATHS = { ext => 1 };

# Set once register_all() has run, so a second call (there should never
# be one -- see the comment there) is a no-op instead of double-scanning.
my $registered = 0;

# Populated once by register_all(), below; `GET /ext/modules` reports
# exactly this list, it never re-scans.
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

# Registers $class (a subclass, e.g. an extension module) at $path,
# refusing anything in $RESERVED_EXT_PATHS (the reserved list is for
# *other* modules -- pve-ext's own self-registration goes through
# _register_unchecked below instead, which skips this check by
# construction) and anything already taken by anything else in the API
# root's method table
# (core PVE::API2 registrations, or an earlier extension module in this
# same scan). Returns true on success; on any failure it warns (using
# $what to describe what was being registered) and returns false -- never
# dies, so the caller can keep going.
sub _register_guarded {
    my ($class, $path, $what) = @_;

    if ($RESERVED_EXT_PATHS->{$path}) {
        warn "pve-ext: skipping $what: ext_path '$path' is reserved\n";
        return 0;
    }

    return _register_unchecked($class, $path, $what);
}

# The actual register_method call, with only the "already taken" guard --
# no reserved-path check, since the one caller allowed to bypass it
# (pve-ext registering itself at 'ext', the path the reserved list exists
# to protect) needs exactly this. Not called directly for extension
# modules; they always go through _register_guarded above.
sub _register_unchecked {
    my ($class, $path, $what) = @_;

    my $ok = eval { PVE::API2->register_method({ subclass => $class, path => $path }); 1 };
    if (!$ok) {
        warn "pve-ext: skipping $what: path '$path' is already registered: $@";
        return 0;
    }

    return 1;
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

        next if !_register_guarded($class, $path, "extension module '$class' ($file)");

        push @$loaded_modules, { module => $class, path => $path, file => $file };
    }

    return;
}

# The single entry point stock PVE/API2.pm calls, once, from its own very
# end -- i.e. after all of PVE::API2's own runtime `register_method` calls
# have already populated the API root's method table (see
# `patches/pve-manager.toml`'s pve-manager_API2.pm.diff). This is what
# makes collisions safe in the direction that matters: an extension's
# ext_path colliding with a *core* path always loses (core registered
# first, so _register_guarded's eval-wrapped register_method call for the
# extension fails and is warned-and-skipped) instead of the reverse.
#
# Previously this module registered itself and ran its scan as top-level
# statements, executed the moment `use PVE::API2::Ext;` compiled this file
# in -- which happens at BEGIN time, i.e. before *any* of PVE/API2.pm's
# own runtime register_method calls, regardless of where in that file the
# `use` line sits. That made every extension win every collision against
# core, which is exactly backwards. `require` (not `use`) plus this
# explicit call, placed at the true end of PVE/API2.pm, fixes the
# ordering; folding the two side effects that used to run at `use` time
# into this one guarded call is what makes a colliding ext_path (`ext`
# itself included) a `warn`, never a fatal `die` that takes pvedaemon and
# pveproxy down with it.
sub register_all {
    my ($class) = @_;

    if ($registered) {
        warn "pve-ext: register_all() called more than once, ignoring\n";
        return;
    }
    $registered = 1;

    # Mount ourselves first, at the one path the reserved list exists to
    # keep everyone else off of -- so this goes through
    # _register_unchecked (skip the reserved-path check, which would
    # otherwise refuse 'ext' to pve-ext itself), still guarded against an
    # actual collision with a core path literally named 'ext' (not
    # expected, but must never be fatal either).
    _register_unchecked(__PACKAGE__, 'ext', "pve-ext's own API ('ext')");

    _scan_and_register();

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
        my $seen_ids = {};
        for my $file (sort glob("$EXT_PAGE_DIR/*.json")) {
            my $manifest = _load_page_manifest($file);
            next if !$manifest;

            my $id = $manifest->{id};
            if ($seen_ids->{$id}) {
                warn "pve-ext: skipping page manifest '$file': duplicate id '$id'"
                    . " (already provided by '$seen_ids->{$id}')\n";
                next;
            }
            $seen_ids->{$id} = $file;

            push @manifests, $manifest;
        }
        return \@manifests;
    },
});

# -- mounting ourselves at 'ext' and discovering/mounting every other
#    extension module happens in register_all(), above, called explicitly
#    from the very end of stock PVE/API2.pm -- nothing runs as a side
#    effect of `require`ing this file. ------------------------------------

1;
