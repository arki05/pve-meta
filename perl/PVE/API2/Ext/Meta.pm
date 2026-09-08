package PVE::API2::Ext::Meta;

use strict;
use warnings;

use PVE::Cluster;
use PVE::Exception qw(raise raise_param_exc);
use PVE::JSONSchema qw(get_standard_option);
use PVE::RESTHandler;
use PVE::RPCEnvironment;

use PVE::RS::Meta;

use base qw(PVE::RESTHandler);

# `PVE::API2::Ext::Meta`: the native `/meta/...` API tree (`docs/DESIGN.md`
# §5), a thin `PVE::RESTHandler` subclass whose methods call straight into
# `PVE::RS::Meta`'s `api_*` functions (`crates/pve-meta-perl`, implemented in
# `pve_meta_core::api`). This module does parameters, PVE ACL checks, the
# vmlist, the guests' tags and the per-document write lock; everything else --
# resolving the caller's scopes against the operator registrations, view
# extraction, prefix stripping, merge/replace, write authorization, the lint,
# touched-path computation, YAML/JSON rendering and digesting -- happens in
# Rust.
#
# Everything crosses the boundary as a **native structure** (`docs/DESIGN.md`
# §5): the caller's ACL hash goes in, documents and results come back as Perl
# hashes and arrays. The one exception is the client's `data` parameter, which
# is a JSON string because that is what the REST parameter is; Rust decodes it
# once. There is no JSON encoding or decoding in this file.
#
# No `proxyto`: the store lives under `/etc/pve/meta`, so it is cluster-wide
# and any node can answer. Read methods are `protected => 0` (run in pveproxy,
# which can read `/etc/pve/meta/*` as `www-data`); write methods are
# `protected => 1` (run in pvedaemon, as root) and run inside
# `PVE::Cluster::cfs_lock_domain` (see `_locked`).
#
# Loaded by `PVE::API2::Ext` (see `pve-ext/perl/PVE/API2/Ext.pm`), which
# scans `/usr/share/perl5/PVE/API2/Ext/*.pm` and mounts each one at the path
# its `ext_path` class method declares.
sub ext_path { return 'meta' }

# -- the caller's ACL ------------------------------------------------------
#
# `docs/DESIGN.md` §3: `full_read` = `VM.Audit` on `/vms/<vmid>`,
# `full_write` = `VM.Config.Options` (datacenter: `Sys.Audit` / `Sys.Modify`
# on `/`). Rust adds the scopes from the operator registrations whose authid
# is the caller and whose selector matches the guest's tags -- which is why
# the tags travel with the ACL.
#
# Scopes apply to guest documents only; the datacenter document is governed by
# ACLs alone, and Rust enforces that rather than trusting an empty list here.

# A guest's PVE tags, as an array ref. `get_guest_config_properties` is the
# cluster-wide cached property fetch `list_guests` already used for the display
# names, so this costs nothing extra; tags are a `;`-separated string in the
# guest config.
sub _parse_tags {
    my ($raw) = @_;
    return [] if !defined($raw) || $raw eq '';
    return [grep { length($_) } split(/[;,\s]+/, $raw)];
}

sub _guest_tags {
    my ($vmid) = @_;
    my $props = eval { PVE::Cluster::get_guest_config_properties(['tags'], $vmid) } || {};
    warn "pve-meta: could not read guest tags: $@" if $@;
    return _parse_tags(($props->{$vmid} // {})->{tags});
}

sub _guest_acl {
    my ($rpcenv, $authuser, $vmid, $tags) = @_;
    return {
        authid => $authuser,
        read => $rpcenv->check($authuser, "/vms/$vmid", ['VM.Audit'], 1) ? 1 : 0,
        write => $rpcenv->check($authuser, "/vms/$vmid", ['VM.Config.Options'], 1) ? 1 : 0,
        tags => $tags // _guest_tags($vmid),
    };
}

sub _datacenter_acl {
    my ($rpcenv, $authuser) = @_;
    return {
        authid => $authuser,
        read => $rpcenv->check($authuser, '/', ['Sys.Audit'], 1) ? 1 : 0,
        write => $rpcenv->check($authuser, '/', ['Sys.Modify'], 1) ? 1 : 0,
        tags => [],
    };
}

# -- helpers ------------------------------------------------------------

# Calls a `PVE::RS::Meta::api_*` function, catching its "NNN: message" die
# (the Rust->Perl error contract, `pve_meta_core::api`) and re-raising
# through `PVE::Exception` so clients get the right HTTP status.
# Anything else (a die that isn't "NNN: ...", i.e. a genuine unexpected
# failure) is re-thrown as-is.
sub _call {
    my ($func, @args) = @_;

    my $res = eval { $func->(@args) };
    if (my $err = $@) {
        my $msg = "$err";
        $msg =~ s/\s+$//;
        if ($msg =~ /^(\d{3}):\s*(.*)$/s) {
            raise("$2\n", code => $1 + 0);
        }
        die "$msg\n";
    }
    return $res;
}

# Runs $code under the cluster-wide lock for one document
# (`docs/DESIGN.md` §4): every API write is a read-modify-write of a file on
# pmxcfs, which is shared by every node, and the writes are deliberately not
# `proxyto`'d -- so without this two nodes can both read, both write, and
# silently lose one update (observed live on the lab cluster). The digest
# precondition is re-checked *inside* the critical section, because the whole
# Rust call happens in here and `cfs_lock_domain` runs `cfs_update()` before
# invoking $code.
#
# `cfs_lock_domain` reports through `$@` rather than dying: a PVE::Exception
# from our own code is re-raised verbatim (that is how the Rust layer's
# 400/403/404/409/422 statuses survive), while a failure of the locking itself
# becomes a clean 503.
sub _locked {
    my ($id, $code) = @_;

    my $res = PVE::Cluster::cfs_lock_domain("pve-meta-$id", 10, $code);
    if (my $err = $@) {
        die $err if ref($err); # a PVE::Exception raised by _call
        my $msg = "$err";
        $msg =~ s/\s+$//;
        raise("$msg\n", code => 503) if $msg =~ /^cfs-lock / || $msg =~ /no quorum/;
        die "$msg\n";
    }
    return $res;
}

# The current vmlist, keyed by vmid.
sub _vmlist_ids {
    my $vmlist = PVE::Cluster::get_vmlist() || {};
    return $vmlist->{ids} || {};
}

# `docs/DESIGN.md` §5: "PUT and DELETE return 404 for a vmid that is not in
# the vmlist; GET of such a vmid is 404 too." Without this, "every guest
# document" is really "every u32": any caller with one rw scope could create
# unbounded files under `/etc/pve/meta` (replicated cluster-wide by pmxcfs,
# which has a hard size budget), and a guest later created at that vmid would
# silently inherit the metadata.
sub _assert_guest_exists {
    my ($vmid) = @_;
    return if _vmlist_ids()->{$vmid};
    raise("guest '$vmid' does not exist\n", code => 404);
}

# Picks the PUT payload's wire format and text out of `data` (a JSON
# string) or `text` (YAML), exactly one of which must be given.
sub _put_payload {
    my ($param) = @_;
    if (defined($param->{text})) {
        raise_param_exc({ data => "only one of 'data' or 'text' may be given" })
            if defined($param->{data});
        return ('yaml', $param->{text});
    } elsif (defined($param->{data})) {
        return ('json', $param->{data});
    }
    raise_param_exc({ data => "one of 'data' or 'text' is required" });
}

my $VIEW_SCHEMA = {
    type => 'string',
    optional => 1,
    description => "Key-path prefix (dotted or slash-separated); omit for the whole document.",
};

my $FORMAT_SCHEMA = {
    type => 'string',
    enum => ['json', 'yaml'],
    optional => 1,
    default => 'json',
    description => "Wire format for the returned view.",
};

my $MODE_SCHEMA = {
    type => 'string',
    enum => ['replace', 'merge'],
    optional => 1,
    default => 'replace',
    description =>
        "'replace' (default) replaces the view's subtree with the payload wholesale "
        . "(an empty object stores an empty map; use DELETE to remove a view); "
        . "'merge' applies it as an RFC 7386-style merge patch relative to the view "
        . "(a JSON 'null' deletes a key).",
};

my $DATA_SCHEMA = {
    type => 'string',
    optional => 1,
    description => "The new view content, JSON-encoded. Exactly one of 'data'/'text' is required.",
};

my $TEXT_SCHEMA = {
    type => 'string',
    optional => 1,
    description => "The new view content, as YAML text. Exactly one of 'data'/'text' is required.",
};

my $SCOPES_RETURNS = {
    type => 'array',
    description => "The caller's prefix scopes for this document, from the operator "
        . "registrations (docs/DESIGN.md §3), with selectors already resolved.",
    items => {
        type => 'object',
        properties => {
            prefix => { type => 'string', description => "The key-path prefix the scope covers." },
            mode => { type => 'string', enum => ['ro', 'rw'], description => "What it grants." },
        },
    },
};

my $VIEW_RETURNS = {
    type => 'object',
    properties => {
        id => { type => 'string' },
        view => { type => 'string' },
        digest => { type => 'string' },
        data => { type => 'object', optional => 1, description => "Present when format=json." },
        text => { type => 'string', optional => 1, description => "Present when format=yaml." },
        parse_error => {
            type => 'string',
            optional => 1,
            description => "Present only when the stored document's content could not be "
                . "recovered (docs/DESIGN.md §4): it is not valid YAML, it is above the "
                . "store's read cap, or it parses to something that is not a mapping. "
                . "'text' is then the file's raw text, so an administrator can repair it "
                . "with a whole-document PUT (no 'view', mode=replace) or remove it with "
                . "DELETE -- nothing narrower is accepted. With format=json, for a caller "
                . "without full read, and whenever the bytes were never read at all, the "
                . "same condition is a 422 instead.",
        },
    },
};

my $PUT_RETURNS = {
    type => 'object',
    properties => {
        id => { type => 'string' },
        view => { type => 'string' },
        digest => { type => 'string' },
        touched => {
            type => 'array',
            items => {
                type => 'object',
                properties => {
                    path => { type => 'string' },
                    op => { type => 'string', enum => ['set', 'delete'] },
                },
            },
        },
    },
};

# `$id` is a vmid or the literal "datacenter"; `$acl` is that resource's ACL
# hash (see above).
my $get_view = sub {
    my ($id, $param, $acl) = @_;
    return _call(
        \&PVE::RS::Meta::api_get, $id, $param->{view}, $param->{format} // 'json', $acl,
    );
};

my $put_view = sub {
    my ($id, $param, $acl) = @_;
    my ($format, $payload) = _put_payload($param);
    return _call(
        \&PVE::RS::Meta::api_put,
        $id, $param->{view}, $format, $payload, $param->{mode} // 'replace',
        $param->{digest}, ($param->{dry_run} ? 1 : 0), $acl,
    );
};

my $delete_view = sub {
    my ($id, $param, $acl) = @_;
    return _call(\&PVE::RS::Meta::api_delete, $id, $param->{view}, $param->{digest}, $acl);
};

# -- directory index ---------------------------------------------------------

__PACKAGE__->register_method({
    name => 'index',
    path => '',
    method => 'GET',
    permissions => { user => 'all' },
    description => "Meta API directory index.",
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
        return [map { { subdir => $_ } } qw(version access guests datacenter operators)];
    },
});

# -- version / access / operators -------------------------------------------

__PACKAGE__->register_method({
    name => 'version',
    path => 'version',
    method => 'GET',
    permissions => { user => 'all' },
    description => "The store's current change-version token. Cheap; poll it every few seconds.",
    parameters => {
        additionalProperties => 0,
        properties => {},
    },
    returns => {
        type => 'object',
        properties => {
            token => { type => 'string', description => "Changes whenever any document's content changes." },
            changed => { type => 'integer', description => "Newest document mtime, as a unix timestamp." },
        },
    },
    code => sub {
        return _call(\&PVE::RS::Meta::api_version);
    },
});

__PACKAGE__->register_method({
    name => 'access',
    path => 'access',
    method => 'GET',
    permissions => { user => 'all' },
    description => "The caller's effective grants for one document (docs/DESIGN.md §3): "
        . "'read'/'write' are the ACL answers for that document (VM.Audit / "
        . "VM.Config.Options with 'vmid'; Sys.Audit / Sys.Modify with 'dc'), and "
        . "'scopes' lists the prefix scopes the operator registrations give the caller "
        . "on it, with selectors already resolved against the guest's tags. Scopes "
        . "apply to guest documents only, never to the datacenter document. "
        . "With neither parameter, 'read'/'write' describe the datacenter document. "
        . "Used by the editor UI to decide what to offer and whether to enable Apply.",
    parameters => {
        additionalProperties => 0,
        properties => {
            vmid => get_standard_option('pve-vmid', { optional => 1 }),
            dc => {
                type => 'boolean',
                optional => 1,
                description => "Ask about the datacenter document instead of a guest.",
            },
        },
    },
    returns => {
        type => 'object',
        properties => {
            read => { type => 'boolean', description => "May read the whole document (ACL)." },
            write => { type => 'boolean', description => "May write the whole document (ACL)." },
            scopes => $SCOPES_RETURNS,
        },
    },
    code => sub {
        my ($param) = @_;

        my $rpcenv = PVE::RPCEnvironment::get();
        my $authuser = $rpcenv->get_user();

        raise_param_exc({ vmid => "'vmid' and 'dc' are mutually exclusive" })
            if defined($param->{vmid}) && $param->{dc};

        if (defined(my $vmid = $param->{vmid})) {
            _assert_guest_exists($vmid);
            return _call(
                \&PVE::RS::Meta::api_access, "$vmid", _guest_acl($rpcenv, $authuser, $vmid),
            );
        }

        return _call(
            \&PVE::RS::Meta::api_access, 'datacenter', _datacenter_acl($rpcenv, $authuser),
        );
    },
});

__PACKAGE__->register_method({
    name => 'operators',
    path => 'operators',
    method => 'GET',
    permissions => {
        description => "Readable by every authenticated user: the registry is not "
            . "sensitive (docs/DESIGN.md §1) and the editor's ownership column needs it.",
        user => 'all',
    },
    description => "Every operator registration (docs/DESIGN.md §3): the files in "
        . "/usr/share/pve-meta/operators and /etc/pve/meta.d/operators, with a cluster "
        . "file overriding the packaged one of the same name. A malformed file is "
        . "skipped with a warning and does not appear here.",
    parameters => {
        additionalProperties => 0,
        properties => {},
    },
    returns => {
        type => 'array',
        # `{ name, authid, description, scopes: [{ prefix, mode, selector, grammar? }] }`
        # -- `grammar` is a free-form PVE::JSONSchema-dialect subtree.
        items => { type => 'object', additionalProperties => 1 },
    },
    code => sub {
        return _call(\&PVE::RS::Meta::api_operators);
    },
});

# -- guests -------------------------------------------------------------

__PACKAGE__->register_method({
    name => 'list_guests',
    path => 'guests',
    method => 'GET',
    permissions => {
        description => "Anybody may call this; the list is filtered to guests the "
            . "caller can read anything of (VM.Audit, or a registered scope whose "
            . "selector matches that guest). 'node', 'name' and 'tags' are returned "
            . "only for guests the caller has VM.Audit on.",
        user => 'all',
    },
    description => "Lists every guest in the vmlist the caller can read anything of.",
    parameters => {
        additionalProperties => 0,
        properties => {
            has => {
                type => 'string',
                optional => 1,
                description => "Only list guests whose *visible* data has something at this "
                    . "dotted path.",
            },
        },
    },
    returns => {
        type => 'array',
        # Open-shaped: `{ vmid, node, type, name, tags, digest }` per guest.
        # Rust omits what the caller may not see.
        items => { type => 'object', additionalProperties => 1 },
    },
    code => sub {
        my ($param) = @_;

        my $rpcenv = PVE::RPCEnvironment::get();
        my $authuser = $rpcenv->get_user();

        my $idlist = _vmlist_ids();

        # One vmlist read, one guest-property lookup, both here: Rust never
        # opens `/etc/pve/.vmlist` or a guest config, so the two can no longer
        # disagree and guest-config parsing is not re-implemented in a second
        # language. `hostname` is the LXC name field, `name` the qemu one;
        # `tags` resolves the registrations' selectors (docs/DESIGN.md §3).
        my $props = eval { PVE::Cluster::get_guest_config_properties([qw(name hostname tags)]) } || {};
        warn "pve-meta: could not read guest properties: $@" if $@;

        my $guests = [];
        for my $vmid (sort { $a <=> $b } keys %$idlist) {
            my $info = $idlist->{$vmid};
            my $p = $props->{$vmid} // {};
            push @$guests, {
                vmid => int($vmid),
                node => $info->{node},
                type => $info->{type},
                name => $p->{name} // $p->{hostname},
                tags => _parse_tags($p->{tags}),
                read => $rpcenv->check($authuser, "/vms/$vmid", ['VM.Audit'], 1) ? 1 : 0,
                write => 0,
            };
        }

        return _call(\&PVE::RS::Meta::api_list_guests, $authuser, $guests, $param->{has});
    },
});

__PACKAGE__->register_method({
    name => 'get_guest',
    path => 'guests/{vmid}',
    method => 'GET',
    permissions => {
        description => "The response is filtered to what the caller may read (VM.Audit, "
            . "or a registered scope whose selector matches this guest). A caller with "
            . "neither is refused with 403, as is a 'view' outside the caller's read "
            . "access. A vmid that is not in the vmlist is 404 (docs/DESIGN.md §5).",
        user => 'all',
    },
    description => "Gets a guest's metadata document (or a view/prefix of it).",
    parameters => {
        additionalProperties => 0,
        properties => {
            vmid => get_standard_option('pve-vmid'),
            view => $VIEW_SCHEMA,
            format => $FORMAT_SCHEMA,
        },
    },
    returns => $VIEW_RETURNS,
    code => sub {
        my ($param) = @_;

        my $rpcenv = PVE::RPCEnvironment::get();
        my $authuser = $rpcenv->get_user();
        my $vmid = $param->{vmid};

        _assert_guest_exists($vmid);
        return $get_view->("$vmid", $param, _guest_acl($rpcenv, $authuser, $vmid));
    },
});

__PACKAGE__->register_method({
    name => 'put_guest',
    protected => 1,
    path => 'guests/{vmid}',
    method => 'PUT',
    permissions => {
        description => "Anybody may call this; the caller must be able to write the "
            . "named view (VM.Config.Options, or a registered rw scope covering it) "
            . "and every path the write touches -- otherwise 403. Writing the whole "
            . "document (no 'view') requires VM.Config.Options. Unknown vmids are 404, "
            . "not created.",
        user => 'all',
    },
    description => "Writes a guest's metadata document (or a view/prefix of it).",
    parameters => {
        additionalProperties => 0,
        properties => {
            vmid => get_standard_option('pve-vmid'),
            view => $VIEW_SCHEMA,
            data => $DATA_SCHEMA,
            text => $TEXT_SCHEMA,
            mode => $MODE_SCHEMA,
            digest => get_standard_option('pve-config-digest'),
            dry_run => {
                type => 'boolean',
                optional => 1,
                default => 0,
                description => "Validate and diff without writing.",
            },
        },
    },
    returns => $PUT_RETURNS,
    code => sub {
        my ($param) = @_;

        my $rpcenv = PVE::RPCEnvironment::get();
        my $authuser = $rpcenv->get_user();
        my $vmid = $param->{vmid};

        return _locked($vmid, sub {
            _assert_guest_exists($vmid);
            return $put_view->("$vmid", $param, _guest_acl($rpcenv, $authuser, $vmid));
        });
    },
});

__PACKAGE__->register_method({
    name => 'delete_guest',
    protected => 1,
    path => 'guests/{vmid}',
    method => 'DELETE',
    permissions => {
        description => "Anybody may call this; same write rules as PUT. Removes only "
            . "the current document -- snapshot copies belong to the guest lifecycle "
            . "and are never touched from here. Unknown vmids are 404.",
        user => 'all',
    },
    description => "Removes a guest's document, or the subtree at 'view'.",
    parameters => {
        additionalProperties => 0,
        properties => {
            vmid => get_standard_option('pve-vmid'),
            view => $VIEW_SCHEMA,
            digest => get_standard_option('pve-config-digest'),
        },
    },
    returns => $PUT_RETURNS,
    code => sub {
        my ($param) = @_;

        my $rpcenv = PVE::RPCEnvironment::get();
        my $authuser = $rpcenv->get_user();
        my $vmid = $param->{vmid};

        return _locked($vmid, sub {
            _assert_guest_exists($vmid);
            return $delete_view->("$vmid", $param, _guest_acl($rpcenv, $authuser, $vmid));
        });
    },
});

# -- datacenter -----------------------------------------------------------

__PACKAGE__->register_method({
    name => 'get_datacenter',
    path => 'datacenter',
    method => 'GET',
    permissions => {
        description => "Requires Sys.Audit on / (docs/DESIGN.md §3 -- scopes never "
            . "apply to the datacenter document); anyone else is refused with 403.",
        user => 'all',
    },
    description => "Gets the datacenter metadata document (or a view/prefix of it).",
    parameters => {
        additionalProperties => 0,
        properties => {
            view => $VIEW_SCHEMA,
            format => $FORMAT_SCHEMA,
        },
    },
    returns => $VIEW_RETURNS,
    code => sub {
        my ($param) = @_;

        my $rpcenv = PVE::RPCEnvironment::get();
        my $authuser = $rpcenv->get_user();

        return $get_view->('datacenter', $param, _datacenter_acl($rpcenv, $authuser));
    },
});

__PACKAGE__->register_method({
    name => 'put_datacenter',
    protected => 1,
    path => 'datacenter',
    method => 'PUT',
    permissions => {
        description => "Requires Sys.Modify on / for the view and every touched path "
            . "(docs/DESIGN.md §3).",
        user => 'all',
    },
    description => "Writes the datacenter metadata document (or a view/prefix of it).",
    parameters => {
        additionalProperties => 0,
        properties => {
            view => $VIEW_SCHEMA,
            data => $DATA_SCHEMA,
            text => $TEXT_SCHEMA,
            mode => $MODE_SCHEMA,
            digest => get_standard_option('pve-config-digest'),
            dry_run => {
                type => 'boolean',
                optional => 1,
                default => 0,
                description => "Validate and diff without writing.",
            },
        },
    },
    returns => $PUT_RETURNS,
    code => sub {
        my ($param) = @_;

        my $rpcenv = PVE::RPCEnvironment::get();
        my $authuser = $rpcenv->get_user();

        return _locked('datacenter', sub {
            return $put_view->('datacenter', $param, _datacenter_acl($rpcenv, $authuser));
        });
    },
});

__PACKAGE__->register_method({
    name => 'delete_datacenter',
    protected => 1,
    path => 'datacenter',
    method => 'DELETE',
    permissions => {
        description => "Requires Sys.Modify on / for the view and every touched path, "
            . "same as PUT.",
        user => 'all',
    },
    description => "Removes the datacenter document, or the subtree at 'view'.",
    parameters => {
        additionalProperties => 0,
        properties => {
            view => $VIEW_SCHEMA,
            digest => get_standard_option('pve-config-digest'),
        },
    },
    returns => $PUT_RETURNS,
    code => sub {
        my ($param) = @_;

        my $rpcenv = PVE::RPCEnvironment::get();
        my $authuser = $rpcenv->get_user();

        return _locked('datacenter', sub {
            return $delete_view->('datacenter', $param, _datacenter_acl($rpcenv, $authuser));
        });
    },
});

1;
