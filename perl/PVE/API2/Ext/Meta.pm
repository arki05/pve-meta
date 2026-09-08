package PVE::API2::Ext::Meta;

use strict;
use warnings;

use JSON;

use PVE::Cluster;
use PVE::Exception qw(raise raise_param_exc);
use PVE::JSONSchema qw(get_standard_option);
use PVE::RESTHandler;
use PVE::RPCEnvironment;

use PVE::RS::Meta;

use base qw(PVE::RESTHandler);

# `PVE::API2::Ext::Meta`: the native `/meta/...` API tree (`docs/DESIGN.md`
# §3), a thin `PVE::RESTHandler` subclass whose methods call straight into
# `PVE::RS::Meta`'s `api_*` functions (`crates/pve-meta-perl`, implemented in
# `pve_meta_core::api`). This module does parameters, PVE ACL checks, the
# `scopes` lookup ("grants"), the vmlist and the per-document write lock;
# everything else -- view extraction, prefix stripping, merge/replace,
# write authorization, touched-path computation, YAML/JSON rendering and
# digesting -- happens in Rust.
#
# No `proxyto`: the store lives under `/etc/pve/meta`, so it is cluster-wide
# and any node can answer. Read methods are `protected => 0` (run in
# pveproxy, which can read `/etc/pve/meta/*` as `www-data`); write methods
# are `protected => 1` (run in pvedaemon, as root) and run inside
# `PVE::Cluster::cfs_lock_domain` (see `_locked`).
#
# Loaded by `PVE::API2::Ext` (see `pve-ext/perl/PVE/API2/Ext.pm`), which
# scans `/usr/share/perl5/PVE/API2/Ext/*.pm` and mounts each one at the path
# its `ext_path` class method declares.
sub ext_path { return 'meta' }

# -- grants -----------------------------------------------------------------
#
# `docs/DESIGN.md` §2: two grant paths, evaluated per request. `full_read`/
# `full_write` come straight from PVE ACLs; `scopes` is looked up once (it
# is the *same* list for every guest document -- scopes are not per-vmid in
# this revision) and reused across an entire request. All three are handed
# to Rust as one `grants_json` string
# (`{"full_read":bool,"full_write":bool,"scopes":[{"prefix":..,"mode":..}]}`);
# see `PVE::RS::Meta::api_get`/`api_put`/`api_delete`/`api_list_guests`.
#
# Built directly as a string (not via encode_json on a Perl hash) to avoid
# Perl's boolean-vs-JSON-number ambiguity: encode_json would render a plain
# 1/0 as a JSON *number*, not the `true`/`false` literal the Rust side's
# `serde` deserializer requires for `full_read`/`full_write`.
sub _grants_json {
    my ($full_read, $full_write, $scopes) = @_;
    my $fr = $full_read ? 'true' : 'false';
    my $fw = $full_write ? 'true' : 'false';
    my $scopes_json = encode_json($scopes // []);
    return qq({"full_read":$fr,"full_write":$fw,"scopes":$scopes_json});
}

# The datacenter document's `scopes` entries for $authuser, decoded from
# `PVE::RS::Meta::api_grants` (itself a JSON string -- see that function's
# doc comment). The same list applies to every guest document
# (`docs/DESIGN.md` §2).
#
# The lookup is lenient (`docs/DESIGN.md` §8): a malformed entry belonging to
# a *different* principal is skipped with a warning on the Rust side, so one
# admin typo can never take the whole guest API down for everybody.
sub _scopes_for {
    my ($authuser) = @_;
    return decode_json(_call(\&PVE::RS::Meta::api_grants, $authuser));
}

# A guest's grants: `VM.Audit` / `VM.Config.Options` on `/vms/$vmid`, plus
# the caller's scopes.
#
# `$orphan` is set when the vmid is *not* in the vmlist (`docs/DESIGN.md` §9):
# the document is an orphan, or there is nothing there at all. Then the
# permission is the *datacenter* ACL and nothing else -- `Sys.Audit` /
# `Sys.Modify` on `/` -- and scopes do not apply, because a scope grants "the
# prefix on every guest document" and this document has no guest.
#
# `/vms/$vmid` is deliberately not consulted for an orphan (review pass 3 §5,
# `Meta.pm:86`). PVE permits ACLs on `/vms/<n>` with no guest behind them, and
# the orphan scenarios are exactly the ones where `remove_vm_access` never ran
# -- a guest destroyed while its node was down, a config removed by hand -- so
# a stale grant could otherwise delete an orphan or partially write it through
# `DELETE ?view=<prefix>`. `docs/DESIGN.md` §9 and `delete_guest`'s own
# permission text both say datacenter write; this is that, for read and write
# alike, and it is the same answer `GET /meta/access?vmid=<orphan>` gives.
#
# Callers that must not act on an orphan at all (PUT) never get here;
# `_orphan_or_404` is the gate.
sub _guest_grants_json {
    my ($rpcenv, $authuser, $vmid, $scopes, $orphan) = @_;
    if ($orphan) {
        return _grants_json(
            $rpcenv->check($authuser, '/', ['Sys.Audit'], 1),
            $rpcenv->check($authuser, '/', ['Sys.Modify'], 1),
            [],
        );
    }
    my $full_read = $rpcenv->check($authuser, "/vms/$vmid", ['VM.Audit'], 1);
    my $full_write = $rpcenv->check($authuser, "/vms/$vmid", ['VM.Config.Options'], 1);
    return _grants_json($full_read, $full_write, $scopes);
}

# The datacenter document's own grants: `Sys.Audit` / `Sys.Modify` on `/`,
# and *no* scopes -- scopes apply "on every guest document"
# (`docs/DESIGN.md` §2), not to the datacenter document itself. This is
# also what makes "only Sys.Modify may edit scopes" (§2) hold automatically:
# with no scope fallback, the Rust side requires `full_write` (i.e. real
# `Sys.Modify`) for the view *and* every touched path of a datacenter write,
# `scopes` included. It is also why a caller without `Sys.Audit` gets a 403
# from a datacenter GET rather than an empty document (§8).
sub _datacenter_grants_json {
    my ($rpcenv, $authuser) = @_;
    my $full_read = $rpcenv->check($authuser, '/', ['Sys.Audit'], 1);
    my $full_write = $rpcenv->check($authuser, '/', ['Sys.Modify'], 1);
    return _grants_json($full_read, $full_write, []);
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
# (`docs/DESIGN.md` §8): every API write is a read-modify-write of a file on
# pmxcfs, which is shared by every node, and the writes are deliberately not
# `proxyto`'d -- so without this two nodes can both read, both write, and
# silently lose one update (observed live; review F12). The digest
# precondition is re-checked *inside* the critical section, because the whole
# Rust call happens in here and `cfs_lock_domain` runs `cfs_update()` before
# invoking $code.
#
# `cfs_lock_domain` reports through `$@` rather than dying: a PVE::Exception
# from our own code is re-raised verbatim (that is how the Rust layer's
# 400/403/404/409 statuses survive), while a failure of the locking itself
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

# `docs/DESIGN.md` §8: the API never creates documents for guests that do not
# exist. Without this, "every guest document" is really "every u32": any
# caller with one rw scope could create unbounded files under `/etc/pve/meta`
# (replicated cluster-wide by pmxcfs, which has a hard size budget), invisible
# to `GET /meta/guests`, and a guest later created at that vmid would silently
# inherit the metadata (review F20).
sub _assert_guest_exists {
    my ($vmid) = @_;
    return if _vmlist_ids()->{$vmid};
    raise("guest '$vmid' does not exist\n", code => 404);
}

# The write-side vmlist bookkeeping for one guest id, in one place: is there a
# guest, and if not, is this an orphan the caller may act on
# (`docs/DESIGN.md` §9)?
#
# Returns 1 for an orphan (no vmlist entry, a document on disk, and the caller
# has datacenter read); returns 0 for a live guest; raises 404 otherwise --
# for a vmid that is neither a guest nor an orphan, and for an orphan the
# caller cannot see, which is the same answer `GET /meta/guests` gives them.
# F20 closed unbounded document creation and, with it, the only API path that
# could ever remove such a document; this is that path, restricted to the
# datacenter administrator (review P5).
sub _orphan_or_404 {
    my ($rpcenv, $authuser, $vmid) = @_;
    return 0 if _vmlist_ids()->{$vmid};
    # `int()`: `has_document` takes a `u32` through perlmod, which refuses a
    # string scalar ("invalid type: string, expected u32"). A `{vmid}` *path*
    # parameter arrives numified, but `/meta/access?vmid=` is an ordinary
    # optional parameter and does not -- so the same helper answered
    # correctly for DELETE and died for GET until this call site existed to
    # show it.
    raise("guest '$vmid' does not exist\n", code => 404)
        if !PVE::RS::Meta::has_document(int($vmid))
        || !$rpcenv->check($authuser, '/', ['Sys.Audit'], 1);
    return 1;
}

# Decodes an `api_get`/`api_put`/`api_delete` result's `data_json` (present
# when `format=json`) into a real `data` key, in place; `text`
# (`format=yaml`) needs no decoding. `JSON::PP`-backed `decode_json` (via
# `use JSON;`) produces `JSON::PP::Boolean` objects for JSON `true`/`false`,
# so a document's own booleans round-trip correctly when pveproxy
# re-encodes the response.
#
# `data` is an *unordered* object once it is a Perl hash (`docs/DESIGN.md`
# §8): Perl randomises hash order, so the document's own key order cannot
# survive this. Clients that need the order read `keys` (an ordered array
# the Rust side returns alongside) or ask for `format=yaml`.
sub _inflate_view {
    my ($doc) = @_;
    if (defined(my $data_json = delete $doc->{data_json})) {
        $doc->{data} = decode_json($data_json);
    }
    return $doc;
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

my $COMMENTS_SCHEMA = {
    type => 'boolean',
    optional => 1,
    default => 1,
    description => 'Keep comment keys ("key__"/"__").',
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
    description => "The caller's datacenter-configured prefix scopes (docs/DESIGN.md §2).",
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
        keys => {
            type => 'array',
            items => { type => 'string' },
            description => "The visible value's top-level keys, in document order. "
                . "'data' is an unordered object; this is the order-preserving view of it.",
        },
        data => { type => 'object', optional => 1, description => "Present when format=json (unordered)." },
        text => { type => 'string', optional => 1, description => "Present when format=yaml (ordered)." },
        parse_error => {
            type => 'string',
            optional => 1,
            description => "Present only when the stored document is not valid YAML "
                . "(docs/DESIGN.md §9): the parser's message. 'data'/'text' then describe "
                . "the empty document and 'keys' is empty, but 'digest' is the real digest "
                . "of the bytes on disk, so the document can be repaired with a "
                . "whole-document PUT (no 'view', mode=replace) or removed with DELETE.",
        },
        raw => {
            type => 'string',
            optional => 1,
            description => "The document's raw text. Present only alongside 'parse_error', "
                . "and only for a caller who may read the whole document (VM.Audit / "
                . "Sys.Audit) -- it is what they need to write the repair.",
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

# `$id` is a vmid or the literal "datacenter"; `$grants_json` is that
# resource's grants (see above).
my $get_view = sub {
    my ($id, $param, $grants_json) = @_;
    my $doc = _call(
        \&PVE::RS::Meta::api_get,
        $id, $param->{view}, $param->{format} // 'json', ($param->{comments} // 1) ? 1 : 0, $grants_json,
    );
    return _inflate_view($doc);
};

my $put_view = sub {
    my ($id, $param, $grants_json) = @_;
    my ($format, $payload) = _put_payload($param);
    return _call(
        \&PVE::RS::Meta::api_put,
        $id, $param->{view}, $format, $payload, $param->{mode} // 'replace',
        $param->{digest}, ($param->{dry_run} ? 1 : 0), $grants_json,
    );
};

my $delete_view = sub {
    my ($id, $param, $grants_json) = @_;
    return _call(\&PVE::RS::Meta::api_delete, $id, $param->{view}, $param->{digest}, $grants_json);
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
        return [map { { subdir => $_ } } qw(version access guests datacenter)];
    },
});

# -- version / access ---------------------------------------------------

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
    description => "The caller's effective grants for one document (docs/DESIGN.md §8): "
        . "'read'/'write' are the ACL answers for that document (VM.Audit / "
        . "VM.Config.Options with 'vmid'; Sys.Audit / Sys.Modify with 'dc'), and "
        . "'scopes' lists the caller's datacenter-configured prefix scopes, which "
        . "apply to every guest document but never to the datacenter document. "
        . "For an orphan vmid (a document whose guest is no longer in the vmlist) "
        . "'read'/'write' are Sys.Audit / Sys.Modify on / and 'scopes' is empty, "
        . "matching what a GET/DELETE of that document actually allows "
        . "(docs/DESIGN.md §9); a vmid that is neither a guest nor a visible "
        . "orphan is 404. "
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
            # An orphan is a document the same caller can list, GET and
            # DELETE, so answering 404 here made the one document a
            # datacenter admin most needs to clean up open read-only in the
            # editor (which treats any failure of this call as the malformed-
            # scopes case). Same gate as `delete_guest`, same answer as
            # `_guest_grants_json`'s orphan branch: datacenter ACL only, no
            # scopes -- there is no guest for a scope to apply to
            # (`docs/DESIGN.md` §9, review pass 3 R8).
            my $orphan = _orphan_or_404($rpcenv, $authuser, $vmid);
            return {
                read => $rpcenv->check($authuser, '/', ['Sys.Audit'], 1) ? 1 : 0,
                write => $rpcenv->check($authuser, '/', ['Sys.Modify'], 1) ? 1 : 0,
                scopes => [],
            } if $orphan;

            return {
                read => $rpcenv->check($authuser, "/vms/$vmid", ['VM.Audit'], 1) ? 1 : 0,
                write => $rpcenv->check($authuser, "/vms/$vmid", ['VM.Config.Options'], 1) ? 1 : 0,
                scopes => _scopes_for($authuser),
            };
        }

        # The datacenter document: scopes never apply to it, so an empty list
        # is the honest answer rather than the caller's guest scopes.
        my $dc = {
            read => $rpcenv->check($authuser, '/', ['Sys.Audit'], 1) ? 1 : 0,
            write => $rpcenv->check($authuser, '/', ['Sys.Modify'], 1) ? 1 : 0,
        };
        return { %$dc, scopes => $param->{dc} ? [] : _scopes_for($authuser) };
    },
});

# -- guests -------------------------------------------------------------

__PACKAGE__->register_method({
    name => 'list_guests',
    path => 'guests',
    method => 'GET',
    permissions => {
        description => "Anybody may call this; the list is filtered to guests the "
            . "caller can read anything of (full VM.Audit, or any datacenter-configured "
            . "scope, which applies to every guest). 'node' and 'name' are returned "
            . "only for guests the caller has VM.Audit on. Orphan documents (whose "
            . "guest is no longer in the vmlist) are listed with 'orphan' set, for "
            . "callers with Sys.Audit on / only.",
        user => 'all',
    },
    description => "Lists every guest in the vmlist the caller can read anything of, "
        . "plus (with Sys.Audit on /) any orphan document whose guest is gone.",
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
        # Open-shaped: `{ vmid, node, type, name, digest, keys }` per guest,
        # plus `orphan => 1` on a document whose guest is gone
        # (docs/DESIGN.md §9). Rust omits what the caller may not see.
        items => { type => 'object', additionalProperties => 1 },
    },
    code => sub {
        my ($param) = @_;

        my $rpcenv = PVE::RPCEnvironment::get();
        my $authuser = $rpcenv->get_user();
        my $scopes = _scopes_for($authuser);

        my $idlist = _vmlist_ids();

        # One vmlist read, one display-name lookup, both here: Rust never
        # opens `/etc/pve/.vmlist` or a guest config, so the two can no
        # longer disagree and guest-config parsing is not re-implemented in
        # a second language (review §5, api.rs:304). `hostname` is the LXC
        # name field, `name` the qemu one.
        my $names = eval { PVE::Cluster::get_guest_config_properties([qw(name hostname)]) } || {};
        warn "pve-meta: could not read guest names: $@" if $@;

        my $guests = [];
        for my $vmid (sort { $a <=> $b } keys %$idlist) {
            my $info = $idlist->{$vmid};
            my $props = $names->{$vmid} // {};
            my $full_read = $rpcenv->check($authuser, "/vms/$vmid", ['VM.Audit'], 1);
            push @$guests, {
                vmid => int($vmid),
                node => $info->{node},
                type => $info->{type},
                name => $props->{name} // $props->{hostname},
                grants => _grants_json($full_read, 0, $scopes),
            };
        }

        # Orphans (documents whose vmid left the vmlist) are the datacenter
        # administrator's business: without this they are invisible to every
        # listing while still replicated by pmxcfs and still waiting to be
        # inherited by a future guest at that vmid (docs/DESIGN.md §9).
        my $orphans = $rpcenv->check($authuser, '/', ['Sys.Audit'], 1) ? 1 : 0;

        return _call(
            \&PVE::RS::Meta::api_list_guests, encode_json($guests), $param->{has}, $orphans,
        );
    },
});

__PACKAGE__->register_method({
    name => 'get_guest',
    path => 'guests/{vmid}',
    method => 'GET',
    permissions => {
        description => "The response is filtered to what the caller may read (full "
            . "VM.Audit, or a datacenter-configured scope). A caller with neither is "
            . "refused with 403, as is a 'view' outside the caller's read access. "
            . "For a vmid that is not in the vmlist (an orphan document, or nothing "
            . "at all) the permission is Sys.Audit on / alone -- a stale /vms/<vmid> "
            . "ACL left behind by a destroy is not consulted, and scopes do not "
            . "apply, since there is no guest for either to apply to "
            . "(docs/DESIGN.md §9). GET never 404s.",
        user => 'all',
    },
    description => "Gets a guest's metadata document (or a view/prefix of it).",
    parameters => {
        additionalProperties => 0,
        properties => {
            vmid => get_standard_option('pve-vmid'),
            view => $VIEW_SCHEMA,
            format => $FORMAT_SCHEMA,
            comments => $COMMENTS_SCHEMA,
        },
    },
    returns => $VIEW_RETURNS,
    code => sub {
        my ($param) = @_;

        my $rpcenv = PVE::RPCEnvironment::get();
        my $authuser = $rpcenv->get_user();
        my $vmid = $param->{vmid};

        # One vmlist read, and it only decides *which* ACL answers: a GET
        # never 404s on an unknown vmid (a missing document is the empty
        # document, `docs/DESIGN.md` §2).
        my $orphan = _vmlist_ids()->{$vmid} ? 0 : 1;
        my $grants_json =
            _guest_grants_json($rpcenv, $authuser, $vmid, _scopes_for($authuser), $orphan);
        return $get_view->("$vmid", $param, $grants_json);
    },
});

__PACKAGE__->register_method({
    name => 'put_guest',
    protected => 1,
    path => 'guests/{vmid}',
    method => 'PUT',
    permissions => {
        description => "Anybody may call this; the caller must be able to write the "
            . "named view (full VM.Config.Options, or a datacenter-configured rw "
            . "scope covering it) and every path the write touches -- otherwise 403. "
            . "Writing the whole document (no 'view') requires VM.Config.Options. "
            . "Unknown vmids are 404, not created.",
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
            # Unconditional: an orphan document is never written, only read
            # or removed (`docs/DESIGN.md` §9). Writing one would resurrect
            # exactly the unbounded-creation problem F20 closed.
            _assert_guest_exists($vmid);
            my $grants_json =
                _guest_grants_json($rpcenv, $authuser, $vmid, _scopes_for($authuser), 0);
            return $put_view->("$vmid", $param, $grants_json);
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
            . "and are never touched from here. An orphan document (whose guest is "
            . "no longer in the vmlist) may be removed with Sys.Audit + Sys.Modify "
            . "on /; every other unknown vmid is 404, and no scope ever grants this.",
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
            my $orphan = _orphan_or_404($rpcenv, $authuser, $vmid);
            my $grants_json =
                _guest_grants_json($rpcenv, $authuser, $vmid, _scopes_for($authuser), $orphan);
            return $delete_view->("$vmid", $param, $grants_json);
        });
    },
});

# -- datacenter -----------------------------------------------------------

__PACKAGE__->register_method({
    name => 'get_datacenter',
    path => 'datacenter',
    method => 'GET',
    permissions => {
        description => "Requires Sys.Audit on / (docs/DESIGN.md §2 -- scopes do not "
            . "apply to the datacenter document); anyone else is refused with 403.",
        user => 'all',
    },
    description => "Gets the datacenter metadata document (or a view/prefix of it).",
    parameters => {
        additionalProperties => 0,
        properties => {
            view => $VIEW_SCHEMA,
            format => $FORMAT_SCHEMA,
            comments => $COMMENTS_SCHEMA,
        },
    },
    returns => $VIEW_RETURNS,
    code => sub {
        my ($param) = @_;

        my $rpcenv = PVE::RPCEnvironment::get();
        my $authuser = $rpcenv->get_user();

        my $grants_json = _datacenter_grants_json($rpcenv, $authuser);
        return $get_view->('datacenter', $param, $grants_json);
    },
});

__PACKAGE__->register_method({
    name => 'put_datacenter',
    protected => 1,
    path => 'datacenter',
    method => 'PUT',
    permissions => {
        description => "Requires Sys.Modify on / for the view and every touched path, "
            . "'scopes' included (docs/DESIGN.md §2). A write that touches 'scopes' is "
            . "additionally validated and refused with 400 naming a malformed entry.",
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
            my $grants_json = _datacenter_grants_json($rpcenv, $authuser);
            return $put_view->('datacenter', $param, $grants_json);
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
            my $grants_json = _datacenter_grants_json($rpcenv, $authuser);
            return $delete_view->('datacenter', $param, $grants_json);
        });
    },
});

1;
