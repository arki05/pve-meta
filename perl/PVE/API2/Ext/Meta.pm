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
# §6), a thin `PVE::RESTHandler` subclass whose methods call straight into
# `PVE::RS::Meta`'s `api_*` functions; everything but parameters, PVE ACL
# checks, the vmlist and the per-document write lock happens in Rust.
#
# Loaded by `PVE::API2::Ext` (`pve-ext/perl/PVE/API2/Ext.pm`), which scans
# `/usr/share/perl5/PVE/API2/Ext/*.pm` and mounts each one at the path its
# `ext_path` class method declares.
sub ext_path { return 'meta' }

# -- facts shared with `bin/pve-meta`, which calls these directly ----------
#
# The CLI is a local reader with no ticket and no pveproxy; it authorizes
# nothing, so it needs only the lookups below, never `_guest_acl` itself.

# A guest's PVE tags, split from the `;`-separated config string. The same
# rule in Rust is `pve_meta_core::tags::split_tags`, which is what every
# consumer of a document splits with (pve-meta-guest-files and the operators);
# both split on `;`, `,` and whitespace and drop empty runs.
sub parse_tags {
    my ($raw) = @_;
    return [] if !defined($raw) || $raw eq '';
    return [grep { length($_) } split(/[;,\s]+/, $raw)];
}

# A guest's tags, read fresh via the cached cluster property fetch.
sub guest_tags {
    my ($vmid) = @_;
    my $props = eval { PVE::Cluster::get_guest_config_properties(['tags'], $vmid) } || {};
    warn "pve-meta: could not read guest tags: $@" if $@;
    return parse_tags(($props->{$vmid} // {})->{tags});
}

# The vmlist's guest ids, keyed by vmid. $refresh runs `cfs_update()` first,
# for a short-lived process (the CLI) that has not seen one yet; API callers
# rely on `_locked`'s (`cfs_lock_domain` refreshes before its callback runs).
sub vmlist_ids {
    my ($refresh) = @_;
    PVE::Cluster::cfs_update() if $refresh;
    my $vmlist = PVE::Cluster::get_vmlist() || {};
    return $vmlist->{ids} || {};
}

# Dies 404 unless $vmid is in the vmlist (docs/DESIGN.md §6): otherwise any
# caller with VM.Config.Options on some vmid could create unbounded files
# under a cluster-replicated directory with a hard size budget.
sub assert_guest_exists {
    my ($vmid) = @_;
    return if vmlist_ids()->{$vmid};
    raise("guest '$vmid' does not exist\n", code => 404);
}

# The node a guest is on right now, or undef: which node's prefix overrides
# apply is decided per request from this, so a migrated guest simply gets
# the other node's (docs/DESIGN.md §3).
sub guest_node {
    my ($vmid) = @_;
    return (vmlist_ids()->{$vmid} // {})->{node};
}

# The `cfs_lock_domain` name for one document's write lock, held by the API
# and the CLI alike. $id is a vmid or 'prefixes/<name>'.
sub lock_domain_for {
    my ($id) = @_;
    return "pve-meta-$id" if $id =~ /^\d+$/;
    my (undef, $name) = split(m{/}, $id, 2);
    return "pve-meta-prefix-$name";
}

# Splits a `PVE::RS::Meta::api_*` die's "NNN: message" (the Rust->Perl error
# contract) into its HTTP status and message; with no such prefix, the status
# is undef and the message is the error as given.
sub parse_rs_error {
    my ($err) = @_;
    my $msg = "$err";
    $msg =~ s/\s+$//;
    return ($1 + 0, $2) if $msg =~ /^(\d{3}):\s*(.*)$/s;
    return (undef, $msg);
}

# Dies via $on_bad->(@given) unless exactly one of $opts's @keys is defined;
# otherwise returns that key's name. The "give exactly one payload source"
# rule, shared by the API's data/text and the CLI's data/text/file.
sub require_one_of {
    my ($opts, $keys, $on_bad) = @_;
    my @given = grep { defined $opts->{$_} } @$keys;
    $on_bad->(@given) if @given != 1;
    return $given[0];
}

sub _guest_acl {
    my ($rpcenv, $authuser, $vmid, $tags) = @_;
    return {
        authid => $authuser,
        read => $rpcenv->check($authuser, "/vms/$vmid", ['VM.Audit'], 1) ? 1 : 0,
        write => $rpcenv->check($authuser, "/vms/$vmid", ['VM.Config.Options'], 1) ? 1 : 0,
        tags => $tags // guest_tags($vmid),
        node => guest_node($vmid),
    };
}

# A registry document (a prefix file) is readable by every authenticated
# user, to agree with the list endpoint below, and written with Sys.Modify
# on '/'.
sub _registry_acl {
    my ($rpcenv, $authuser) = @_;
    return {
        authid => $authuser,
        read => 1,
        write => $rpcenv->check($authuser, '/', ['Sys.Modify'], 1) ? 1 : 0,
        tags => [],
    };
}

# -- helpers ------------------------------------------------------------

# Calls a `PVE::RS::Meta::api_*` function, re-raising its "NNN: message" die
# through `PVE::Exception` so clients get the right HTTP status. Anything
# else (a die with no such prefix) is re-thrown as-is.
sub _call {
    my ($func, @args) = @_;

    my $res = eval { $func->(@args) };
    if (my $err = $@) {
        my ($code, $msg) = parse_rs_error($err);
        raise("$msg\n", code => $code) if defined($code);
        die "$msg\n";
    }
    return $res;
}

# Runs $code under the cluster-wide lock for one document: every API write is
# a read-modify-write of a file on pmxcfs, and writes are deliberately not
# `proxyto`'d, so without this two nodes could both read, both write, and
# silently lose one update.
#
# `cfs_lock_domain` reports through `$@` rather than dying: a PVE::Exception
# from our own code is re-raised verbatim (how the Rust layer's
# 400/403/404/409/422 statuses survive), while a failure of the locking
# itself becomes a clean 503.
sub _locked {
    my ($id, $code) = @_;

    my $res = PVE::Cluster::cfs_lock_domain(lock_domain_for($id), 10, $code);
    if (my $err = $@) {
        die $err if ref($err); # a PVE::Exception raised by _call
        my $msg = "$err";
        $msg =~ s/\s+$//;
        raise("$msg\n", code => 503) if $msg =~ /^cfs-lock / || $msg =~ /no quorum/;
        die "$msg\n";
    }
    return $res;
}

# Picks the PUT payload's wire format and text out of `data` (JSON) or
# `text` (YAML).
sub _put_payload {
    my ($param) = @_;
    my $key = require_one_of($param, [qw(text data)], sub {
        raise_param_exc({
            data => @_ > 1
                ? "only one of 'data' or 'text' may be given"
                : "one of 'data' or 'text' is required",
        });
    });
    return $key eq 'text' ? ('yaml', $param->{text}) : ('json', $param->{data});
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
        . "(an empty object stores an empty map; use DELETE to remove a view), "
        . "'merge' applies it as an RFC 7386-style merge patch relative to the view "
        . "(a JSON 'null' deletes a key).",
};

my $DATA_SCHEMA = {
    type => 'string',
    optional => 1,
    description => "The new view content, JSON-encoded; exactly one of 'data'/'text' is required.",
};

my $TEXT_SCHEMA = {
    type => 'string',
    optional => 1,
    description => "The new view content, as YAML text; exactly one of 'data'/'text' is required.",
};

my $DRY_RUN_SCHEMA = {
    type => 'boolean',
    optional => 1,
    default => 0,
    description => "Validate and diff without writing.",
};

my $FORCE_SCHEMA = {
    type => 'boolean',
    optional => 1,
    default => 0,
    description => "Store the result even where it would leave an enforcing prefix's "
        . "schema not matching (a 422 naming the paths without this) -- what the "
        . "editor's \"Save anyway\" tick sends (docs/DESIGN.md §5).",
};

my $COMMENTS_SCHEMA = {
    type => 'boolean',
    optional => 1,
    default => 0,
    description => "Include comment keys (a key ending in '__', a note about its "
        . "sibling; docs/DESIGN.md §2): without it a read leaves them out and a "
        . "'replace' keeps each kept key's stored note, with it a read returns them "
        . "and a 'replace' payload is the subtree notes included ('merge' is the same "
        . "either way).",
};

my $VIEW_RETURNS = {
    type => 'object',
    properties => {
        id => { type => 'string' },
        view => { type => 'string' },
        digest => { type => 'string' },
        data => { type => 'object', optional => 1, description => "Present when format=json." },
        text => {
            type => 'string',
            optional => 1,
            description => "Present when format=yaml: the file's own text for the "
                . "whole document with 'comments', a canonical dump otherwise.",
        },
        parse_error => {
            type => 'string',
            optional => 1,
            description => "Present only when the stored document could not be read back "
                . "(not valid YAML, above the read cap, or not a mapping; docs/DESIGN.md "
                . "§5) -- 'text' is then the raw file for a full format=yaml+comments "
                . "read, a 422 otherwise -- and is only repaired with a whole-document "
                . "PUT or removed with DELETE, nothing narrower.",
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

# `$id` is a vmid or `prefixes/<name>`; `$acl` is that resource's ACL hash
# (see above).
my $get_view = sub {
    my ($id, $param, $acl) = @_;
    return _call(
        \&PVE::RS::Meta::api_get, $id, $param->{view}, $param->{format} // 'json', $acl,
        ($param->{comments} ? 1 : 0),
    );
};

my $put_view = sub {
    my ($id, $param, $acl) = @_;
    my ($format, $payload) = _put_payload($param);
    return _call(
        \&PVE::RS::Meta::api_put,
        $id, $param->{view}, $format, $payload, $param->{mode} // 'replace',
        $param->{digest}, ($param->{dry_run} ? 1 : 0), $acl, ($param->{force} ? 1 : 0),
        ($param->{comments} ? 1 : 0),
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
        return [map { { subdir => $_ } } qw(version access guests prefixes schemas)];
    },
});

# -- version / access / prefixes --------------------------------------------

__PACKAGE__->register_method({
    name => 'version',
    path => 'version',
    method => 'GET',
    permissions => { user => 'all' },
    description => "The store's current change-version token, unscoped (one hash over "
        . "every document and every prefix file) and cheap enough to poll every few "
        . "seconds.",
    parameters => {
        additionalProperties => 0,
        properties => {},
    },
    returns => {
        type => 'object',
        properties => {
            token => { type => 'string', description => "Changes whenever any document's content changes." },
        },
    },
    code => sub {
        # No ACL check, and none is needed: a token is a hash over content
        # every authenticated user may already see the shape of
        # (docs/DESIGN.md §1).
        return _call(\&PVE::RS::Meta::api_version);
    },
});

__PACKAGE__->register_method({
    name => 'access',
    path => 'access',
    method => 'GET',
    permissions => { user => 'all' },
    description => "The caller's access to one document (docs/DESIGN.md §4) -- PVE's "
        . "ACLs alone: VM.Audit/VM.Config.Options on a guest, or open read plus "
        . "Sys.Modify write on a prefix file -- or, with no 'id', the registry as a "
        . "whole, for the editor to decide what to offer.",
    parameters => {
        additionalProperties => 0,
        properties => {
            id => {
                type => 'string',
                optional => 1,
                description => "The document to ask about, as an id: a vmid or "
                    . "'prefixes/<name>'. Omit for the registry.",
            },
        },
    },
    returns => {
        type => 'object',
        properties => {
            read => { type => 'boolean', description => "May read the whole document (ACL)." },
            write => { type => 'boolean', description => "May write the whole document (ACL)." },
        },
    },
    code => sub {
        my ($param) = @_;

        my $rpcenv = PVE::RPCEnvironment::get();
        my $authuser = $rpcenv->get_user();

        # One place that maps a document id to the ACL answers for that *kind* of
        # document: the endpoint families each know their own mapping, and this
        # endpoint has to agree with them. The two kinds answer differently, and
        # only the write bit coincides -- mixing up the kind is silently
        # wrong for a caller holding Sys.Modify without Sys.Audit.
        my $id = $param->{id};
        if (!defined($id)) {
            my $acl = _registry_acl($rpcenv, $authuser);
            return { read => $acl->{read}, write => $acl->{write} };
        }
        if ($id =~ m{^prefixes/}) {
            return _call(\&PVE::RS::Meta::api_access, $id, _registry_acl($rpcenv, $authuser));
        }
        if ($id =~ m{^\d+$}) {
            assert_guest_exists($id);
            return _call(
                \&PVE::RS::Meta::api_access, $id, _guest_acl($rpcenv, $authuser, $id),
            );
        }
        # Anything else is refused by the one id parser with a 400, rather than
        # this endpoint growing a second opinion about what an id is.
        return _call(\&PVE::RS::Meta::api_access, $id, _registry_acl($rpcenv, $authuser));
    },
});

__PACKAGE__->register_method({
    name => 'prefixes',
    path => 'prefixes',
    method => 'GET',
    permissions => {
        description => "Readable by every authenticated user: what a prefix looks "
            . "like is not sensitive (docs/DESIGN.md §1), and the editor needs it to "
            . "render typed rows.",
        user => 'all',
    },
    description => "Prefixes (docs/DESIGN.md §3) from /usr/share/pve-meta/prefixes "
        . "and /etc/pve/meta.d/prefixes, sorted most-specific first: without 'id', "
        . "every file as declared (a cluster file overriding a packaged one of the "
        . "same name, 'nodes' as written); with 'id', those reaching that guest "
        . "resolved against its tags and current node (no 'nodes', "
        . "'enforce'/'hidden'/'schema' already effective); either way, a file that "
        . "failed to load still appears, named, with 'error' set instead of "
        . "'selector'/'schema'.",
    parameters => {
        additionalProperties => 0,
        properties => {
            id => get_standard_option('pve-vmid', {
                optional => 1,
                description => "The guest whose prefixes to resolve, its tags and node "
                    . "read from the vmlist per request so a migrated or re-tagged "
                    . "guest gets the other set.",
            }),
        },
    },
    returns => {
        type => 'array',
        # `{ prefix, description?, selector, enforce, hidden, schema?, nodes?, origin,
        # overrides }` for a loaded prefix -- `schema` is a free-form PVE::JSONSchema-
        # dialect subtree -- or `{ prefix, origin, error }` for one that did not load.
        items => {
            type => 'object',
            additionalProperties => 1,
            properties => {
                error => {
                    type => 'string',
                    optional => 1,
                    description => "Present only for a file that failed to load -- the "
                        . "parser's or the filesystem's message -- with 'prefix' and "
                        . "'origin' still naming the file, so it can be opened and "
                        . "repaired the same way as a loaded one.",
                },
            },
        },
    },
    code => sub {
        my ($param) = @_;
        my $id = $param->{id};
        return _call(\&PVE::RS::Meta::api_prefixes, undef, undef) if !defined($id);
        assert_guest_exists($id);
        return _call(\&PVE::RS::Meta::api_prefixes, guest_node($id), guest_tags($id));
    },
});

__PACKAGE__->register_method({
    name => 'schemas',
    path => 'schemas',
    method => 'GET',
    permissions => {
        description => "Readable by every authenticated user: it is a description of a "
            . "file format, the same one this package's own documentation carries.",
        user => 'all',
    },
    description => "The prefix file format as a schema (docs/DESIGN.md §3), keyed "
        . "'prefix' in the same dialect a prefix uses, for the editor to render a "
        . "prefix document the way it renders a guest's -- an affordance, not the "
        . "validator, since what is storable is decided by the parser.",
    parameters => {
        additionalProperties => 0,
        properties => {},
    },
    returns => {
        type => 'object',
        additionalProperties => 1,
    },
    code => sub {
        return _call(\&PVE::RS::Meta::api_schemas);
    },
});

# -- one document, three methods ---------------------------------------------
#
# Every kind of document -- a guest's or a prefix file -- is read, written
# and removed by the same three calls into Rust ($get_view / $put_view /
# $delete_view); a write's lock domain is `id` too, through `lock_domain_for`.
# What differs is how the request names the document, which ACL answers
# describe the caller on it, and what the API docs say -- a spec, with the
# three methods generated from it.
#
#   name      the method-name suffix: get_<name>, put_<name>, delete_<name>
#   path      the REST path, with its parameter placeholder if it has one
#   params    that parameter's schema
#   id        $param -> the document id Rust addresses, and the write lock's
#   acl       ($rpcenv, $authuser, $param) -> the caller's ACL hash
#   check     optional, $param -> dies unless the document may be addressed.
#             Runs before a read and *inside* the lock before a write, so a
#             guest destroyed in between is a 404 and not a resurrected file;
#             and before the ACL is computed, so a 404 costs no ACL lookups.
#   describe  { get, put, delete } -> the method descriptions
#   perms     { get, put, delete } -> the permission descriptions
sub _register_document_methods {
    my ($spec) = @_;
    my ($name, $path, $params) = @$spec{qw(name path params)};
    my $check = $spec->{check} // sub { };

    my $caller = sub {
        my ($param) = @_;
        my $rpcenv = PVE::RPCEnvironment::get();
        my $authuser = $rpcenv->get_user();
        return $spec->{acl}->($rpcenv, $authuser, $param);
    };

    __PACKAGE__->register_method({
        name => "get_$name",
        path => $path,
        method => 'GET',
        permissions => { description => $spec->{perms}{get}, user => 'all' },
        description => $spec->{describe}{get},
        parameters => {
            additionalProperties => 0,
            properties => {
                %$params,
                view => $VIEW_SCHEMA,
                format => $FORMAT_SCHEMA,
                comments => $COMMENTS_SCHEMA,
            },
        },
        returns => $VIEW_RETURNS,
        code => sub {
            my ($param) = @_;
            $check->($param);
            return $get_view->($spec->{id}->($param), $param, $caller->($param));
        },
    });

    __PACKAGE__->register_method({
        name => "put_$name",
        protected => 1,
        path => $path,
        method => 'PUT',
        permissions => { description => $spec->{perms}{put}, user => 'all' },
        description => $spec->{describe}{put},
        parameters => {
            additionalProperties => 0,
            properties => {
                %$params,
                view => $VIEW_SCHEMA,
                data => $DATA_SCHEMA,
                text => $TEXT_SCHEMA,
                mode => $MODE_SCHEMA,
                digest => get_standard_option('pve-config-digest'),
                dry_run => $DRY_RUN_SCHEMA,
                force => $FORCE_SCHEMA,
                comments => $COMMENTS_SCHEMA,
            },
        },
        returns => $PUT_RETURNS,
        code => sub {
            my ($param) = @_;
            return _locked($spec->{id}->($param), sub {
                $check->($param);
                return $put_view->($spec->{id}->($param), $param, $caller->($param));
            });
        },
    });

    __PACKAGE__->register_method({
        name => "delete_$name",
        protected => 1,
        path => $path,
        method => 'DELETE',
        permissions => { description => $spec->{perms}{delete}, user => 'all' },
        description => $spec->{describe}{delete},
        parameters => {
            additionalProperties => 0,
            properties => {
                %$params,
                view => $VIEW_SCHEMA,
                digest => get_standard_option('pve-config-digest'),
            },
        },
        returns => $PUT_RETURNS,
        code => sub {
            my ($param) = @_;
            return _locked($spec->{id}->($param), sub {
                $check->($param);
                return $delete_view->($spec->{id}->($param), $param, $caller->($param));
            });
        },
    });
}

# -- registry documents ------------------------------------------------------

# The same shape `pve_meta_core::registry::is_valid_file_name` accepts (pattern and
# `MAX_FILE_NAME_LEN`): this schema is the friendly 400, Rust's own re-check on parse
# is the real one.
my $REGISTRY_NAME_SCHEMA = {
    type => 'string',
    pattern => '[A-Za-z0-9_@!-]+(\.[A-Za-z0-9_@!-]+)*',
    maxLength => 128,
    description => "The file's name without '.yaml', which for a prefix definition "
        . "is the prefix itself (docs/DESIGN.md §3): 'homelab.docker.yaml' declares "
        . "'homelab.docker'.",
};

_register_document_methods({
    name => 'prefix',
    path => 'prefixes/{name}',
    params => { name => $REGISTRY_NAME_SCHEMA },
    id => sub { "prefixes/$_[0]->{name}" },
    acl => sub { _registry_acl($_[0], $_[1]) },
    perms => {
        get => "Readable by every authenticated user, exactly as the "
            . "GET /meta/prefixes listing is (docs/DESIGN.md §1).",
        put => "Requires Sys.Modify on / (docs/DESIGN.md §4).",
        delete => "Requires Sys.Modify on / for the view and every touched "
            . "path, same as PUT.",
    },
    describe => {
        get => "Gets one prefix file as a document (or a view of it) as stored -- "
            . "unlike the GET /meta/prefixes listing, this includes a file the "
            . "loader would skip, so a malformed one can be seen and repaired.",
        put => "Writes one prefix file (or a view of it) to /etc/pve/meta.d/prefixes, "
            . "overriding any packaged file of the same name; refused with a 400 if "
            . "the result would not parse as a prefix, rather than silently making it "
            . "disappear.",
        delete => "Removes one prefix file, or the subtree at 'view'; a packaged "
            . "prefix is never removed, only its cluster override, reverting to the "
            . "packaged file.",
    },
});

# -- guests -------------------------------------------------------------

__PACKAGE__->register_method({
    name => 'list_guests',
    path => 'guests',
    method => 'GET',
    permissions => {
        description => "Anybody may call this; the list is filtered to guests the "
            . "caller has VM.Audit on (docs/DESIGN.md §4).",
        user => 'all',
    },
    description => "Lists every guest in the vmlist the caller has VM.Audit on.",
    parameters => {
        additionalProperties => 0,
        properties => {
            has => {
                type => 'string',
                optional => 1,
                description => "Only list guests whose data has something at this "
                    . "dotted path, notes left out as everywhere a read does (docs/DESIGN.md §2).",
            },
        },
    },
    returns => {
        type => 'array',
        # `{ vmid, node, type, name, tags, digest }` per guest.
        items => { type => 'object', additionalProperties => 1 },
    },
    code => sub {
        my ($param) = @_;

        my $rpcenv = PVE::RPCEnvironment::get();
        my $authuser = $rpcenv->get_user();

        my $idlist = vmlist_ids();

        # One vmlist read, one guest-property lookup, both here: Rust never
        # opens `/etc/pve/.vmlist` or a guest config, so the two can no longer
        # disagree and guest-config parsing is not re-implemented in a second
        # language. `hostname` is the LXC name field, `name` the qemu one;
        # `tags` resolves a prefix's selector (docs/DESIGN.md §3).
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
                tags => parse_tags($p->{tags}),
                read => $rpcenv->check($authuser, "/vms/$vmid", ['VM.Audit'], 1) ? 1 : 0,
            };
        }

        return _call(\&PVE::RS::Meta::api_list_guests, $guests, $param->{has});
    },
});

_register_document_methods({
    name => 'guest',
    path => 'guests/{vmid}',
    params => { vmid => get_standard_option('pve-vmid') },
    id => sub { "$_[0]->{vmid}" },
    acl => sub { _guest_acl($_[0], $_[1], $_[2]->{vmid}) },
    check => sub { assert_guest_exists($_[0]->{vmid}) },
    perms => {
        get => "Requires VM.Audit on the guest (403 without it, for the whole "
            . "document and any 'view' alike); a vmid not in the vmlist is 404 "
            . "(docs/DESIGN.md §6).",
        put => "Requires VM.Config.Options on the guest and nothing else -- no "
            . "per-path check, no VM.Audit requirement, and no exception for a "
            . "document that cannot be read back (docs/DESIGN.md §4); an unknown "
            . "vmid is 404, never created.",
        delete => "Requires VM.Config.Options on the guest, same as PUT; removes "
            . "only the current document, never a snapshot copy, and an unknown "
            . "vmid is 404.",
    },
    describe => {
        get => "Gets a guest's metadata document (or a view/prefix of it).",
        put => "Writes a guest's metadata document (or a view/prefix of it).",
        delete => "Removes a guest's document, or the subtree at 'view'.",
    },
});

1;
