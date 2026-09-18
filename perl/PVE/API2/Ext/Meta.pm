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
# §8), a thin `PVE::RESTHandler` subclass whose methods call straight into
# `PVE::RS::Meta`'s `api_*` functions (`crates/pve-meta-perl`, implemented in
# `pve_meta_core::api`). This module does parameters, PVE ACL checks, the
# vmlist, the guests' tags and the per-document write lock; everything else --
# view extraction, prefix stripping, merge/replace, write authorization, the
# lint, touched-path computation, YAML/JSON rendering and digesting -- happens
# in Rust.
#
# Everything crosses the boundary as a **native structure** (`docs/DESIGN.md`
# §8): the caller's ACL hash goes in, documents and results come back as Perl
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
# `docs/DESIGN.md` §4: PVE's ACLs and nothing else. `read` = `VM.Audit` on
# `/vms/<vmid>`, `write` = `VM.Config.Options`; the tags travel with the ACL
# because a prefix's selector matches against them, and the guest's node comes
# along too, because a prefix may override its schema/enforce/hidden for that
# node (`docs/DESIGN.md` §3). A registry document (a prefix file) is readable
# by every authenticated user and written with `Sys.Modify` on `/`.

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
        node => _guest_node($vmid),
    };
}

# A registry document -- one prefix definition (docs/DESIGN.md §3) -- is an
# administrator's to edit and nobody else's.
#
# Read is open to every authenticated user, because it has to agree with the
# list endpoint below: it already returns the same files' content to
# everyone, and a document read that was stricter than the list of the same
# thing would be a rule with two answers. Write is Sys.Modify on '/'.
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
# (`docs/DESIGN.md` §7): every API write is a read-modify-write of a file on
# pmxcfs, which is shared by every node, and the writes are deliberately not
# `proxyto`'d -- so without this two nodes can both read, both write, and
# silently lose one update. The digest
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

# `docs/DESIGN.md` §8: "PUT and DELETE return 404 for a vmid that is not in
# the vmlist; GET of such a vmid is 404 too." Without this, "every guest
# document" is really "every u32": any caller with VM.Config.Options on some
# vmid could create unbounded files under `/etc/pve/meta` (replicated
# cluster-wide by pmxcfs, which has a hard size budget), and a guest later
# created at that vmid would silently inherit the metadata.
sub _assert_guest_exists {
    my ($vmid) = @_;
    return if _vmlist_ids()->{$vmid};
    raise("guest '$vmid' does not exist\n", code => 404);
}

# The node a guest is on right now, from the vmlist, or undef. Which node's prefix
# files apply to the guest is decided per request from this, so a migrated guest
# simply gets the other node's (docs/DESIGN.md §3).
sub _guest_node {
    my ($vmid) = @_;
    return (_vmlist_ids()->{$vmid} // {})->{node};
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
    description => "Store the result even where a prefix declares 'enforce: true' and "
        . "the write would leave its subtree not matching that prefix's schema "
        . "(docs/DESIGN.md §7). Without it such a write is a 422 naming the paths. "
        . "This is what the editor's \"Save anyway\" tick sends.",
};

my $COMMENTS_SCHEMA = {
    type => 'boolean',
    optional => 1,
    default => 0,
    description => "Include comment keys (a key ending in '__', a note about its sibling; "
        . "docs/DESIGN.md §2). Without it a read leaves them out at any depth, 'text' is "
        . "the canonical YAML of what is left, and a 'view' naming one is a 400; a "
        . "'replace' may carry none and keeps every stored note whose subject it keeps "
        . "(in a list, the notes of an unchanged member). "
        . "With it a read returns them and a 'replace' payload is the subtree, notes "
        . "included. 'merge' is the same either way.",
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
            description => "Present only when the stored document's content could not be "
                . "recovered (docs/DESIGN.md §7): it is not valid YAML, it is above the "
                . "store's read cap, or it parses to something that is not a mapping. "
                . "'text' is the raw file only for a full reader with format=yaml and "
                . "'comments'; otherwise the same condition is a 422. It is repaired with a "
                . "whole-document PUT (no 'view', mode=replace) or removed with DELETE -- "
                . "nothing narrower is accepted.",
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
    description => "The store's current change-version token, unscoped: one hash over "
        . "every document and every prefix file. Cheap; poll it every few seconds.",
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
    description => "The caller's access to one document (docs/DESIGN.md §4): PVE's ACLs "
        . "and nothing else. 'read'/'write' are VM.Audit / VM.Config.Options on a guest; "
        . "for a prefix file, read is open to every authenticated user and write is "
        . "Sys.Modify on /. With no 'id', 'read'/'write' describe the registry as a "
        . "whole -- what the prefix list asks before offering Add. Used by the editor "
        . "UI to decide what to offer and whether to enable Apply.",
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
            _assert_guest_exists($id);
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
        description => "Readable by every authenticated user: a prefix declares "
            . "that a prefix exists and what shape it has, which is not sensitive "
            . "(docs/DESIGN.md §1) and is what the editor needs to render typed rows.",
        user => 'all',
    },
    description => "Prefixes (docs/DESIGN.md §3): the files in "
        . "/usr/share/pve-meta/prefixes and /etc/pve/meta.d/prefixes. The file name is "
        . "the prefix. Without 'id', every file as it is: a cluster file overrides the "
        . "packaged file of the same name, one row per name, and a row may carry a "
        . "'nodes' map of per-node overrides as the file declares them. With 'id', the "
        . "prefixes reaching that guest, resolved against its tags and its current node "
        . "(docs/DESIGN.md §6): selector-matched, its node's override already applied -- "
        . "no 'nodes' map, 'enforce'/'hidden'/'schema' already effective. Either way, "
        . "sorted most-specific first, which is the order that resolves which prefix "
        . "governs a path -- longest prefix wins and schemas never merge. A file that did "
        . "not load -- unreadable, or not valid as a prefix -- still appears here: it is "
        . "named ('prefix' is its file name) and carries 'error' instead of "
        . "'selector'/'schema', so it can be found and repaired at /meta/prefixes/{name} "
        . "rather than quietly not existing.",
    parameters => {
        additionalProperties => 0,
        properties => {
            id => get_standard_option('pve-vmid', {
                optional => 1,
                description => "The guest whose prefixes to resolve: its tags and its "
                    . "current node are read from the vmlist per request, so a migrated "
                    . "or re-tagged guest gets the other set.",
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
                    description => "Present only on an entry for a file that did not load: "
                        . "the parser's or the filesystem's own message. 'prefix' is still "
                        . "the file name and 'origin' still says which file it is, so the "
                        . "row can be opened and repaired the same way a loaded one can.",
                },
            },
        },
    },
    code => sub {
        my ($param) = @_;
        my $id = $param->{id};
        return _call(\&PVE::RS::Meta::api_prefixes, undef, undef) if !defined($id);
        _assert_guest_exists($id);
        return _call(\&PVE::RS::Meta::api_prefixes, _guest_node($id), _guest_tags($id));
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
    description => "The prefix file format as a schema (docs/DESIGN.md §6), keyed "
        . "'prefix', in the same PVE::JSONSchema dialect a prefix uses to describe a "
        . "guest's subtree. The editor renders a prefix document with this the way it "
        . "renders a guest document with the prefixes that reach it. This is an "
        . "affordance, not the validator: what is storable is decided by the parser "
        . "on the way in.",
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
# Every kind of document -- a guest's or a prefix file -- is
# read, written and removed by the same three calls into
# Rust ($get_view / $put_view / $delete_view). What differs is how the request
# names the document, which ACL answers describe the caller on it, what lock a
# write holds, and what the API docs say. Those are a spec, and the three
# methods are generated from it: a duplicated read/write pair is where this
# project's wrong-result bugs come from.
#
#   name      the method-name suffix: get_<name>, put_<name>, delete_<name>
#   path      the REST path, with its parameter placeholder if it has one
#   params    that parameter's schema
#   id        $param -> the document id Rust addresses
#   lock      $param -> the cfs_lock_domain suffix every write holds
#   acl       ($rpcenv, $authuser, $param) -> the caller's ACL hash
#   check     optional, $param -> dies unless the document may be addressed.
#             Runs before a read and *inside* the lock before a write, so a
#             guest destroyed in between is a 404 and not a resurrected file;
#             and before the ACL is computed, so a 404 costs no ACL lookups.
#   check_put optional, $param -> the same, for PUT alone and after `check`: what
#             only creating a file needs.
#   describe  { get, put, delete } -> the method descriptions
#   perms     { get, put, delete } -> the permission descriptions
sub _register_document_methods {
    my ($spec) = @_;
    my ($name, $path, $params) = @$spec{qw(name path params)};
    my $check = $spec->{check} // sub { };
    my $check_put = $spec->{check_put} // sub { };

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
            return _locked($spec->{lock}->($param), sub {
                $check->($param);
                $check_put->($param);
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
            return _locked($spec->{lock}->($param), sub {
                $check->($param);
                return $delete_view->($spec->{id}->($param), $param, $caller->($param));
            });
        },
    });
}

# -- registry documents ------------------------------------------------------

# The file name, which for a prefix *is* its prefix -- so it is dotted when
# the prefix is nested. The same shape `pve_meta_core::registry::is_valid_file_name`
# accepts -- the pattern AND the length (`MAX_FILE_NAME_LEN`) -- which is what
# the Rust layer re-checks when it parses the id: this schema is the friendly
# 400, that check is the real one, and the editor asks the same function.
my $REGISTRY_NAME_SCHEMA = {
    type => 'string',
    pattern => '[A-Za-z0-9_@!-]+(\.[A-Za-z0-9_@!-]+)*',
    maxLength => 128,
    description => "The file's name, without the '.yaml' suffix. For a prefix "
        . "definition that name *is* the prefix (docs/DESIGN.md §3): the file "
        . "'homelab.docker.yaml' declares 'homelab.docker'.",
};

_register_document_methods({
    name => 'prefix',
    path => 'prefixes/{name}',
    params => { name => $REGISTRY_NAME_SCHEMA },
    id => sub { "prefixes/$_[0]->{name}" },
    lock => sub { "prefix-$_[0]->{name}" },
    acl => sub { _registry_acl($_[0], $_[1]) },
    perms => {
        get => "Readable by every authenticated user, exactly as the "
            . "GET /meta/prefixes listing is (docs/DESIGN.md §1).",
        put => "Requires Sys.Modify on / (docs/DESIGN.md §4).",
        delete => "Requires Sys.Modify on / for the view and every touched "
            . "path, same as PUT.",
    },
    describe => {
        get => "Gets one prefix file as a document (or a view/prefix of it). "
            . "Unlike the GET /meta/prefixes listing, which returns what the loader "
            . "parsed, this returns the file itself -- including a file the loader "
            . "would skip, so a malformed one can be seen and repaired.",
        put => "Writes one prefix file (or a view/prefix of it), in "
            . "/etc/pve/meta.d/prefixes, overriding the packaged file of the same name "
            . "in /usr/share/pve-meta/prefixes if there is one. The result must parse "
            . "as a prefix: a file the loader would skip is refused with a 400 rather "
            . "than written, because a write that made the prefix silently disappear "
            . "would otherwise answer 200.",
        delete => "Removes one prefix file, or the subtree at 'view'. A packaged "
            . "prefix is never removed: deleting the cluster file that overrode it "
            . "reverts to the packaged one, which is then what a following GET "
            . "returns.",
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
                    . "dotted path. Naming a comment key is a 400: a note is not data.",
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

        my $idlist = _vmlist_ids();

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
                tags => _parse_tags($p->{tags}),
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
    lock => sub { $_[0]->{vmid} },
    acl => sub { _guest_acl($_[0], $_[1], $_[2]->{vmid}) },
    check => sub { _assert_guest_exists($_[0]->{vmid}) },
    perms => {
        get => "Requires VM.Audit on the guest; a caller without it is refused with "
            . "403 for the whole document and any 'view' alike. A vmid that is not "
            . "in the vmlist is 404 (docs/DESIGN.md §8).",
        put => "Requires VM.Config.Options on the guest, and nothing else -- there is "
            . "no per-path check and no requirement to also hold VM.Audit "
            . "(docs/DESIGN.md §4). A document that cannot be read back is no "
            . "exception: repairing it as a whole still only needs VM.Config.Options. "
            . "Unknown vmids are 404, not created.",
        delete => "Requires VM.Config.Options on the guest, same as PUT. Removes only "
            . "the current document -- snapshot copies belong to the guest lifecycle "
            . "and are never touched from here. Unknown vmids are 404.",
    },
    describe => {
        get => "Gets a guest's metadata document (or a view/prefix of it).",
        put => "Writes a guest's metadata document (or a view/prefix of it).",
        delete => "Removes a guest's document, or the subtree at 'view'.",
    },
});

1;
