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
# resolving the caller's scopes against the permission files, view
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
# on `/`). Rust adds the scopes from the permission files whose authid
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

# A registry document -- one prefix definition or permission file (docs/DESIGN.md §3) --
# is an administrator's to edit and nobody else's.
#
# Read is open to every authenticated user, because it has to agree with the
# two list endpoints below: they already return the same files' content to
# everyone, and a document read that was stricter than the list of the same
# thing would be a rule with two answers. Write is Sys.Modify on '/', the same
# as the datacenter document.
#
# There is deliberately no scope path here at all: `api::effective` gives a
# registry document no scopes, so an operator holding `rw` on some prefix
# cannot edit the permission file that gave it that prefix, nor the prefix that
# declares it. Self-registration is refused by there being no way to express it.
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
        . "permission files (docs/DESIGN.md §3.2), with selectors already resolved.",
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
        return [map { { subdir => $_ } } qw(version access guests datacenter prefixes permissions schemas)];
    },
});

# -- version / access / prefixes / permissions -----------------------------

__PACKAGE__->register_method({
    name => 'version',
    path => 'version',
    method => 'GET',
    permissions => { user => 'all' },
    description => "The store's current change-version token. Cheap; poll it every few "
        . "seconds. With 'detail', also every document's own digest, so a caller that saw "
        . "the token move can tell which documents to re-read instead of re-listing the "
        . "store. Digests are not filtered per caller (docs/DESIGN.md §1).",
    parameters => {
        additionalProperties => 0,
        properties => {
            detail => {
                type => 'boolean',
                optional => 1,
                default => 0,
                description => "Also return each document's digest.",
            },
            id => {
                type => 'string',
                optional => 1,
                description => "Watch just this document: a vmid, 'datacenter', "
                    . "'prefixes/<name>' or 'permissions/<name>'. The token then covers "
                    . "that document plus the prefix and permission directories, and "
                    . "nothing else -- which is what an open editor watches, at a cost "
                    . "that does not grow with the number of guests. Tokens from "
                    . "different 'id' are not comparable with each other or with the "
                    . "unscoped one; poll with a fixed 'id' and compare against your own "
                    . "previous answer.",
            },
        },
    },
    returns => {
        type => 'object',
        properties => {
            token => { type => 'string', description => "Changes whenever any document's content changes." },
            changed => { type => 'integer', description => "Newest document mtime, as a unix timestamp." },
            documents => {
                type => 'array',
                optional => 1,
                description => "With 'detail': [{ id, digest }] for every document, sorted by id -- "
                    . "or, with 'id', for the documents that scoped token covers: that one plus "
                    . "the registry documents. Snapshot copies are not documents and are never "
                    . "listed, though they do move an unscoped 'token'.",
                items => {
                    type => 'object',
                    properties => {
                        id => { type => 'string', description => "A vmid, or 'datacenter'." },
                        digest => { type => 'string' },
                    },
                },
            },
        },
    },
    code => sub {
        my ($param) = @_;
        # No ACL check, and none is needed: a token is a hash over content the
        # 'detail' listing already hands to every authenticated user unfiltered
        # (docs/DESIGN.md §1), and a bad id is refused by the one id parser
        # rather than falling back to the whole store.
        return _call(\&PVE::RS::Meta::api_version, $param->{detail} ? 1 : 0, $param->{id});
    },
});

__PACKAGE__->register_method({
    name => 'access',
    path => 'access',
    method => 'GET',
    permissions => { user => 'all' },
    description => "The caller's effective access for one document (docs/DESIGN.md §3): "
        . "'read'/'write' are the ACL answers for that document (VM.Audit / "
        . "VM.Config.Options with 'vmid'; Sys.Audit / Sys.Modify with 'dc'), and "
        . "'scopes' lists the prefix scopes the permission files give the caller "
        . "on it, with selectors already resolved against the guest's tags. Scopes "
        . "apply to guest documents only, never to the datacenter document. "
        . "With no parameter at all, 'read'/'write' describe the datacenter document. "
        . "Used by the editor UI to decide what to offer and whether to enable Apply.",
    parameters => {
        additionalProperties => 0,
        properties => {
            id => {
                type => 'string',
                optional => 1,
                description => "The document to ask about, as an id: a vmid, "
                    . "'datacenter', 'prefixes/<name>' or 'permissions/<name>'. Prefer this "
                    . "over 'vmid'/'dc', which predate registry documents and cannot "
                    . "name one.",
            },
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
            tags => {
                type => 'array',
                description => "The guest's PVE tags, which is what a permission's or "
                    . "prefix's 'selector: {tag: t}' matches against. Empty for any other "
                    . "document, and for a caller without VM.Audit on the guest -- the "
                    . "same filter GET /meta/guests applies to the same field. Here so "
                    . "the editor does not have to read every document in the cluster "
                    . "(GET /meta/guests) to learn one guest's tags.",
                items => { type => 'string' },
            },
        },
    },
    code => sub {
        my ($param) = @_;

        my $rpcenv = PVE::RPCEnvironment::get();
        my $authuser = $rpcenv->get_user();

        my $given = grep { $_ } (defined($param->{id}), defined($param->{vmid}), $param->{dc});
        raise_param_exc({ id => "'id', 'vmid' and 'dc' are mutually exclusive" }) if $given > 1;

        # One place that maps a document id to the ACL answers for that *kind* of
        # document. The three endpoint families each knew their own mapping; this
        # endpoint used to infer it from which parameter was set, which could not
        # name a registry document at all -- so the editor asked about the
        # datacenter document instead and got Sys.Audit for a file every
        # authenticated user may read (docs/DESIGN.md §3.5).
        my $id = $param->{id};
        if (!defined($id)) {
            $id = defined($param->{vmid}) ? "$param->{vmid}" : 'datacenter';
        }

        if ($id =~ m{^(prefixes|permissions)/}) {
            return _call(\&PVE::RS::Meta::api_access, $id, _registry_acl($rpcenv, $authuser));
        }
        if ($id =~ m{^\d+$}) {
            _assert_guest_exists($id);
            return _call(
                \&PVE::RS::Meta::api_access, $id, _guest_acl($rpcenv, $authuser, $id),
            );
        }
        # 'datacenter' -- or anything else, which the id parser refuses with a 400
        # rather than this endpoint growing a second opinion about what an id is.
        return _call(
            \&PVE::RS::Meta::api_access, $id, _datacenter_acl($rpcenv, $authuser),
        );
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
    description => "Every prefix (docs/DESIGN.md §3.1): the files in "
        . "/usr/share/pve-meta/prefixes and /etc/pve/meta.d/prefixes, with a cluster "
        . "file overriding the packaged one of the same name. The file name is the "
        . "prefix. Sorted most-specific first, which is the order that resolves which "
        . "prefix governs a path -- longest prefix wins and schemas never merge. A file "
        . "that did not load -- unreadable, or not valid as a prefix -- still appears "
        . "here: it is named ('prefix' is its file name) and carries 'error' instead of "
        . "'selector'/'schema', so it can be found and repaired at /meta/prefixes/{name} "
        . "rather than quietly not existing.",
    parameters => {
        additionalProperties => 0,
        properties => {},
    },
    returns => {
        type => 'array',
        # `{ prefix, description?, selector, schema? }` for a loaded prefix --
        # `schema` is a free-form PVE::JSONSchema-dialect subtree -- or
        # `{ prefix, origin, error }` for one that did not load.
        items => {
            type => 'object',
            additionalProperties => 1,
            properties => {
                error => {
                    type => 'string',
                    optional => 1,
                    description => "Present only on an entry for a file that did not load: "
                        . "the parser's or the filesystem's own message. 'prefix' is still "
                        . "the file name and 'origin' still says packaged or cluster, so the "
                        . "row can be opened and repaired the same way a loaded one can.",
                },
            },
        },
    },
    code => sub {
        return _call(\&PVE::RS::Meta::api_prefixes);
    },
});

__PACKAGE__->register_method({
    name => 'permissions',
    path => 'permissions',
    method => 'GET',
    permissions => {
        description => "Readable by every authenticated user: a permission file says who may "
            . "touch which prefix, which is exactly what the editor's Access column "
            . "shows for every row, and listings are out of scope (docs/DESIGN.md §1).",
        user => 'all',
    },
    description => "Every permission file (docs/DESIGN.md §3.2): the files in "
        . "/etc/pve/meta.d/permissions. Cluster-only on purpose -- there is deliberately no "
        . "packaged permissions directory, because an operator's own package may ship a "
        . "prefix definition (what it expects) but must never ship its own. A file that did "
        . "not load -- unreadable, or not valid as a permission -- still appears here: it is "
        . "named ('name' is its file name) and carries 'error' instead of 'authid'/'rules'. "
        . "It grants nothing (a malformed permission file must never grant anything), and it "
        . "can be found and repaired at /meta/permissions/{name} rather than quietly not "
        . "existing.",
    parameters => {
        additionalProperties => 0,
        properties => {},
    },
    returns => {
        type => 'array',
        # `{ name, authid, description?, rules: [{ prefix, mode, selector }] }`
        # for a loaded permission, or `{ name, origin, error }` for one that
        # did not load.
        items => {
            type => 'object',
            additionalProperties => 1,
            properties => {
                error => {
                    type => 'string',
                    optional => 1,
                    description => "Present only on an entry for a file that did not load: "
                        . "the parser's or the filesystem's own message. 'name' is still "
                        . "the file name and 'origin' still says packaged or cluster, so the "
                        . "row can be opened and repaired the same way a loaded one can.",
                },
            },
        },
    },
    code => sub {
        return _call(\&PVE::RS::Meta::api_permissions);
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
    description => "The two registry file formats as schemas (docs/DESIGN.md §3.6), "
        . "keyed 'prefix' and 'permission', in the same PVE::JSONSchema dialect a "
        . "prefix uses to describe a guest's subtree. The editor renders a "
        . "prefix or permission document with these the way it renders a guest document "
        . "with the prefixes that reach it. This is an affordance, not the "
        . "validator: what is storable is decided by the parser on the way in.",
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

# -- registry documents ------------------------------------------------------

# The file name, which for a prefix *is* its prefix -- so it is dotted when
# the prefix is nested. The same shape `pve_meta_core::registry::is_valid_file_name`
# accepts, which is what the Rust layer re-checks when it parses the id: this
# pattern is the friendly 400, that check is the real one.
my $REGISTRY_NAME_SCHEMA = {
    type => 'string',
    pattern => '[A-Za-z0-9_@!-]+(\.[A-Za-z0-9_@!-]+)*',
    maxLength => 128,
    description => "The file's name, without the '.yaml' suffix. For a prefix "
        . "definition that name *is* the prefix (docs/DESIGN.md §3.1): the file "
        . "'homelab.docker.yaml' declares 'homelab.docker'.",
};

# The six endpoints below are generated rather than written twice: a prefix
# and a permission file are the same document to everything but the parser that validates
# what is written (`api::check_registry_shape`), and two copies of a read/write
# pair is how this project has produced every wrong-result bug it has had.
for my $kind (['prefixes', 'prefix'], ['permissions', 'permission']) {
    my ($dir, $one) = @$kind;
    my $where = $one eq 'prefix'
        ? "/etc/pve/meta.d/prefixes, overriding the packaged file of the same name in "
          . "/usr/share/pve-meta/prefixes if there is one"
        : "/etc/pve/meta.d/permissions";

    __PACKAGE__->register_method({
        name => "get_$one",
        path => "$dir/{name}",
        method => 'GET',
        permissions => {
            description => "Readable by every authenticated user, exactly as the "
                . "GET /meta/$dir listing is (docs/DESIGN.md §1).",
            user => 'all',
        },
        description => "Gets one $one file as a document (or a view/prefix of it). "
            . "Unlike the GET /meta/$dir listing, which returns what the loader "
            . "parsed, this returns the file itself -- including a file the loader "
            . "would skip, so a malformed one can be seen and repaired.",
        parameters => {
            additionalProperties => 0,
            properties => {
                name => $REGISTRY_NAME_SCHEMA,
                view => $VIEW_SCHEMA,
                format => $FORMAT_SCHEMA,
            },
        },
        returns => $VIEW_RETURNS,
        code => sub {
            my ($param) = @_;

            my $rpcenv = PVE::RPCEnvironment::get();
            my $authuser = $rpcenv->get_user();

            return $get_view->(
                "$dir/$param->{name}", $param, _registry_acl($rpcenv, $authuser),
            );
        },
    });

    __PACKAGE__->register_method({
        name => "put_$one",
        protected => 1,
        path => "$dir/{name}",
        method => 'PUT',
        permissions => {
            description => "Requires Sys.Modify on / (docs/DESIGN.md §3).",
            user => 'all',
        },
        description => "Writes one $one file (or a view/prefix of it), in $where. "
            . "The result must parse as a $one: a file the loader would skip is "
            . "refused with a 400 rather than written, because a write that made the "
            . "$one silently disappear would otherwise answer 200.",
        parameters => {
            additionalProperties => 0,
            properties => {
                name => $REGISTRY_NAME_SCHEMA,
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

            return _locked("$one-$param->{name}", sub {
                return $put_view->(
                    "$dir/$param->{name}", $param, _registry_acl($rpcenv, $authuser),
                );
            });
        },
    });

    __PACKAGE__->register_method({
        name => "delete_$one",
        protected => 1,
        path => "$dir/{name}",
        method => 'DELETE',
        permissions => {
            description => "Requires Sys.Modify on / for the view and every touched "
                . "path, same as PUT.",
            user => 'all',
        },
        description => "Removes one $one file, or the subtree at 'view'. "
            . ($one eq 'prefix'
                ? "A packaged prefix is never removed: deleting the cluster file "
                  . "that overrode it reverts to the packaged one, which is then what "
                  . "a following GET returns."
                : "Permission files are cluster-only, so this removes the file."),
        parameters => {
            additionalProperties => 0,
            properties => {
                name => $REGISTRY_NAME_SCHEMA,
                view => $VIEW_SCHEMA,
                digest => get_standard_option('pve-config-digest'),
            },
        },
        returns => $PUT_RETURNS,
        code => sub {
            my ($param) = @_;

            my $rpcenv = PVE::RPCEnvironment::get();
            my $authuser = $rpcenv->get_user();

            return _locked("$one-$param->{name}", sub {
                return $delete_view->(
                    "$dir/$param->{name}", $param, _registry_acl($rpcenv, $authuser),
                );
            });
        },
    });
}

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
        # `tags` resolves the permission files' and prefix definitions' selectors (docs/DESIGN.md §3).
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
                # Constant, not the real ACL answer: a listing never consults full_write
                # (api.rs's `list_guests` gates only on read), so asking PVE for it would be
                # one $rpcenv->check() per guest for a value nothing reads.
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
            . "or a granted scope whose selector matches this guest). A caller with "
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
        description => "Anybody may call this. What authorizes the write is what it "
            . "*changes*: every path it touches -- values changed, keys added, keys "
            . "removed -- must be covered by VM.Config.Options or by a granted rw "
            . "scope, otherwise 403. The 'view' is where the write is aimed, not what "
            . "it may do, so one write may span two granted prefixes even though the "
            . "view covering both is the whole document. On top of that the caller "
            . "must be able to read the named view and must hold some write permission "
            . "on the document (docs/DESIGN.md 3.4). A document that cannot be read "
            . "back is the exception: repairing it as a whole requires "
            . "VM.Config.Options, since there is no stored content to check the change "
            . "against. Unknown vmids are 404, not created.",
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
