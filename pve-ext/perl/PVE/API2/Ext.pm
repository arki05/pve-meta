package PVE::API2::Ext;

use strict;
use warnings;

use JSON;
use Digest::SHA;

use PVE::JSONSchema qw(get_standard_option);
use PVE::RESTHandler;
use PVE::Tools qw(file_get_contents);

use base qw(PVE::RESTHandler);

# The generic Proxmox VE extension layer (see pve-ext/README.md).
# `require`d, then explicitly driven by one `PVE::API2::Ext->register_all();`
# call, from the very end of stock `PVE/API2.pm` (see patches/pve-manager.toml).
# register_all() mounts this module at `ext` and scans
# `/usr/share/perl5/PVE/API2/Ext/*.pm`, mounting each at its own `ext_path`.

my $EXT_MODULE_DIR = '/usr/share/perl5/PVE/API2/Ext';
my $EXT_PAGE_DIR = '/usr/share/pve-ext/pages';

# 'ext' is this module's own mount point; no other module may claim it.
my $RESERVED_EXT_PATHS = { ext => 1 };

# Set once register_all() has run, so a second call is a no-op.
my $registered = 0;

# Populated once by register_all(); GET /ext/modules reports exactly this.
my $loaded_modules = [];

# /usr/share/perl5/PVE/API2/Ext/Foo/Bar.pm -> "PVE::API2::Ext::Foo::Bar".
sub _class_from_file {
    my ($file) = @_;

    my $rel = $file;
    return undef if $rel !~ s{^\Q/usr/share/perl5/\E}{};
    return undef if $rel !~ s{\.pm$}{};

    $rel =~ s{/}{::}g;
    return $rel;
}

# Registers $class at $path. A colliding path (core or another extension)
# is a `warn`, never a `die` -- one broken/colliding module must never take
# pvedaemon/pveproxy down with it.
sub _register_guarded {
    my ($class, $path, $what) = @_;

    if ($RESERVED_EXT_PATHS->{$path}) {
        warn "pve-ext: skipping $what: ext_path '$path' is reserved\n";
        return 0;
    }

    return _register_unchecked($class, $path, $what);
}

# Same as above with no reserved-path check, for pve-ext's own 'ext' mount.
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
        # pvedaemon/pveproxy run under `perl -T`; glob()'s results are
        # tainted, so re-derive an untainted $file via a regex match
        # against exactly this scan's own glob pattern.
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

# The single entry point stock PVE/API2.pm calls, once, after all of
# PVE::API2's own core `register_method` calls -- so a core/extension
# ext_path collision always resolves in core's favor (see patches/pve-manager.toml).
sub register_all {
    my ($class) = @_;

    if ($registered) {
        warn "pve-ext: register_all() called more than once, ignoring\n";
        return;
    }
    $registered = 1;

    _register_unchecked(__PACKAGE__, 'ext', "pve-ext's own API ('ext')");

    _scan_and_register();

    return;
}

# Reads and validates one page manifest (see pve-ext/README.md). Returns
# the manifest hash, or undef (with a warning) if it isn't valid JSON or
# doesn't have the fields the loader needs -- one malformed manifest must
# never break the whole GET /ext/pages response.
sub _load_page_manifest {
    my ($file) = @_;

    my $raw = eval { file_get_contents($file) };
    if ($@) {
        warn "pve-ext: skipping page manifest '$file': could not read: $@";
        return undef;
    }

    my $manifest = eval { decode_json($raw) };
    if ($@ || ref($manifest) ne 'HASH') {
        warn "pve-ext: skipping page manifest '$file': not a valid JSON object\n";
        return undef;
    }

    for my $key (qw(id title script xtype)) {
        if (!defined($manifest->{$key}) || $manifest->{$key} eq '') {
            warn "pve-ext: skipping page manifest '$file': missing '$key'\n";
            return undef;
        }
    }
    if (ref($manifest->{targets}) ne 'ARRAY' || !scalar(@{ $manifest->{targets} })) {
        warn "pve-ext: skipping page manifest '$file': 'targets' must be a non-empty array\n";
        return undef;
    }

    # Cache-busting fingerprint of the file `script` resolves to (the
    # loader appends it as `?ver=`) -- see _asset_fingerprint below.
    $manifest->{fingerprint} = _asset_fingerprint($manifest->{script});

    return $manifest;
}

# A short content fingerprint of the static file `$url` resolves to, or
# undef when it does not name one we can stat. pveproxy serves static
# files with `Last-Modified` and no `Cache-Control`/`ETag`, and dpkg clamps
# mtimes for reproducible builds, so two builds of the same package
# version are byte-different files a browser cannot tell apart without
# this. `/pve2/js/...` is served from `/usr/share/pve-manager/js/...`
# (pveproxy's `add_dirs()`); anything else gets no fingerprint.
sub _asset_fingerprint {
    my ($url) = @_;

    return undef if !defined($url) || $url eq '';
    my ($path) = split(/[?#]/, $url, 2);
    return undef if $path !~ m|^/pve2/js/(.+)$|;
    my $file = "/usr/share/pve-manager/js/$1";
    return undef if !-f $file;

    my $digest = eval {
        open(my $fh, '<', $file) or die "open failed\n";
        binmode($fh);
        my $ctx = Digest::SHA->new(256);
        $ctx->addfile($fh);
        close($fh);
        substr($ctx->hexdigest, 0, 16);
    };
    if (my $err = $@) {
        warn "pve-ext: could not fingerprint '$file': $err";
        return undef;
    }
    return $digest;
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
        . "/usr/share/pve-ext/pages/*.json (see pve-ext/README.md): a "
        . "native ExtJS panel class ('script'+'xtype'), instantiated as "
        . "the tab content by js/pve-ext-loader.js. Each page's own "
        . "backend API is responsible for its own access control. "
        . "Re-reads the directory on every call, so dropping in a new "
        . "manifest takes effect immediately, without a service restart.",
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
                script => { type => 'string' },
                xtype => { type => 'string' },
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
#    extension module happens in register_all(), above. ------------------

1;
