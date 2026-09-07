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
# `PVE::RS::Meta`'s `api_*` functions (`crates/pve-meta-perl`). This module
# does parameters, PVE ACL checks and `scopes` lookup ("grants"); everything
# else -- view extraction, prefix stripping, merge/replace, touched-path
# computation, YAML/JSON rendering and digesting -- happens in Rust.
#
# No `proxyto`: the store lives under `/etc/pve/meta`, so it is cluster-wide
# and any node can answer. Read methods are `protected => 0` (run in
# pveproxy, which can read `/etc/pve/meta/*` as `www-data`); write methods
# are `protected => 1` (run in pvedaemon, as root).
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
sub _scopes_for {
    my ($authuser) = @_;
    return decode_json(_call(\&PVE::RS::Meta::api_grants, $authuser));
}

# A guest's grants: `VM.Audit` / `VM.Config.Options` on `/vms/$vmid`, plus
# the caller's scopes.
sub _guest_grants_json {
    my ($rpcenv, $authuser, $vmid, $scopes) = @_;
    my $full_read = $rpcenv->check($authuser, "/vms/$vmid", ['VM.Audit'], 1);
    my $full_write = $rpcenv->check($authuser, "/vms/$vmid", ['VM.Config.Options'], 1);
    return _grants_json($full_read, $full_write, $scopes);
}

# The datacenter document's own grants: `Sys.Audit` / `Sys.Modify` on `/`,
# and *no* scopes -- scopes apply "on every guest document"
# (`docs/DESIGN.md` §2), not to the datacenter document itself. This is
# also what makes "only Sys.Modify may edit scopes" (§2) hold automatically:
# with no scope fallback, `Grants::check_write` requires `full_write` (i.e.
# real `Sys.Modify`) for *every* touched path of a datacenter write,
# `scopes` included.
sub _datacenter_grants_json {
    my ($rpcenv, $authuser) = @_;
    my $full_read = $rpcenv->check($authuser, '/', ['Sys.Audit'], 1);
    my $full_write = $rpcenv->check($authuser, '/', ['Sys.Modify'], 1);
    return _grants_json($full_read, $full_write, []);
}

# -- helpers ------------------------------------------------------------

# Calls a `PVE::RS::Meta::api_*` function, catching its "NNN: message" die
# (the Rust->Perl error contract, `crates/pve-meta-perl/src/api.rs`) and
# re-raising through `PVE::Exception` so clients get the right HTTP status.
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

# Decodes an `api_get`/`api_put`/`api_delete` result's `data_json` (present
# when `format=json`) into a real `data` key, in place; `text`
# (`format=yaml`) needs no decoding. `JSON::PP`-backed `decode_json` (via
# `use JSON;`) produces `JSON::PP::Boolean` objects for JSON `true`/`false`,
# so a document's own booleans round-trip correctly when pveproxy
# re-encodes the response.
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
        "'replace' (default) replaces the view's subtree with the payload wholesale; "
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

my $VIEW_RETURNS = {
    type => 'object',
    properties => {
        id => { type => 'string' },
        view => { type => 'string' },
        digest => { type => 'string' },
        data => { type => 'object', optional => 1, description => "Present when format=json." },
        text => { type => 'string', optional => 1, description => "Present when format=yaml." },
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
    returns => { type => 'object', additionalProperties => 1 },
    code => sub {
        return _call(\&PVE::RS::Meta::api_version);
    },
});

__PACKAGE__->register_method({
    name => 'access',
    path => 'access',
    method => 'GET',
    permissions => { user => 'all' },
    description => "The caller's effective grants: 'full' is '*' (unrestricted, via "
        . "Sys.Audit on /) or the list of vmids the caller has VM.Audit on; 'scopes' "
        . "is their datacenter-configured prefix scopes (docs/DESIGN.md §2). Used by "
        . "the editor UI's 'view as' selector.",
    parameters => {
        additionalProperties => 0,
        properties => {},
    },
    returns => {
        type => 'object',
        properties => {
            full => { description => "'*', or an array of vmids." },
            scopes => { type => 'array' },
        },
    },
    code => sub {
        my $rpcenv = PVE::RPCEnvironment::get();
        my $authuser = $rpcenv->get_user();

        my $scopes = _scopes_for($authuser);

        my $full;
        if ($rpcenv->check($authuser, '/', ['Sys.Audit'], 1)) {
            $full = '*';
        } else {
            my $vmlist = PVE::Cluster::get_vmlist() || {};
            my $idlist = $vmlist->{ids} || {};
            $full = [
                sort { $a <=> $b }
                grep { $rpcenv->check($authuser, "/vms/$_", ['VM.Audit'], 1) }
                keys %$idlist
            ];
        }

        return { full => $full, scopes => $scopes };
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
            . "scope, which applies to every guest).",
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
        items => { type => 'object', additionalProperties => 1 },
    },
    code => sub {
        my ($param) = @_;

        my $rpcenv = PVE::RPCEnvironment::get();
        my $authuser = $rpcenv->get_user();
        my $scopes = _scopes_for($authuser);

        my $vmlist = PVE::Cluster::get_vmlist() || {};
        my $idlist = $vmlist->{ids} || {};

        my @entries;
        for my $vmid (sort keys %$idlist) {
            my $full_read = $rpcenv->check($authuser, "/vms/$vmid", ['VM.Audit'], 1);
            push @entries, qq("$vmid":) . _grants_json($full_read, 0, $scopes);
        }
        my $grants_map_json = '{' . join(',', @entries) . '}';

        return _call(\&PVE::RS::Meta::api_list_guests, $grants_map_json, $param->{has});
    },
});

__PACKAGE__->register_method({
    name => 'get_guest',
    path => 'guests/{vmid}',
    method => 'GET',
    permissions => {
        description => "Anybody may call this; the response is filtered to what the "
            . "caller may read (full VM.Audit, or a datacenter-configured scope). A "
            . "'view' outside the caller's read access is refused with 403.",
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

        my $grants_json =
            _guest_grants_json($rpcenv, $authuser, $vmid, _scopes_for($authuser));
        return $get_view->("$vmid", $param, $grants_json);
    },
});

__PACKAGE__->register_method({
    name => 'put_guest',
    path => 'guests/{vmid}',
    method => 'PUT',
    protected => 1,
    permissions => {
        description => "Anybody may call this; every touched path must be writable "
            . "per the caller's grants (full VM.Config.Options, or a "
            . "datacenter-configured rw scope) -- refused with 403 naming the first "
            . "path that is not.",
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

        my $grants_json =
            _guest_grants_json($rpcenv, $authuser, $vmid, _scopes_for($authuser));
        return $put_view->("$vmid", $param, $grants_json);
    },
});

__PACKAGE__->register_method({
    name => 'delete_guest',
    path => 'guests/{vmid}',
    method => 'DELETE',
    protected => 1,
    permissions => {
        description => "Anybody may call this; every touched path must be writable "
            . "per the caller's grants, same as PUT.",
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

        my $grants_json =
            _guest_grants_json($rpcenv, $authuser, $vmid, _scopes_for($authuser));
        return $delete_view->("$vmid", $param, $grants_json);
    },
});

# -- datacenter -----------------------------------------------------------

__PACKAGE__->register_method({
    name => 'get_datacenter',
    path => 'datacenter',
    method => 'GET',
    permissions => {
        description => "Requires Sys.Audit on / (docs/DESIGN.md §2 -- scopes do not "
            . "apply to the datacenter document).",
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
    path => 'datacenter',
    method => 'PUT',
    protected => 1,
    permissions => {
        description => "Requires Sys.Modify on / for every touched path, 'scopes' "
            . "included (docs/DESIGN.md §2).",
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

        my $grants_json = _datacenter_grants_json($rpcenv, $authuser);
        return $put_view->('datacenter', $param, $grants_json);
    },
});

__PACKAGE__->register_method({
    name => 'delete_datacenter',
    path => 'datacenter',
    method => 'DELETE',
    protected => 1,
    permissions => {
        description => "Requires Sys.Modify on / for every touched path, same as PUT.",
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

        my $grants_json = _datacenter_grants_json($rpcenv, $authuser);
        return $delete_view->('datacenter', $param, $grants_json);
    },
});

1;
