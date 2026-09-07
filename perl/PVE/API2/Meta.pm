package PVE::API2::Meta;

use strict;
use warnings;

use JSON;

use PVE::Exception qw(raise raise_param_exc);
use PVE::JSONSchema qw(get_standard_option);
use PVE::RESTHandler;
use PVE::RPCEnvironment;

use PVE::RS::Meta;

use base qw(PVE::RESTHandler);

# `PVE::API2::Meta`: the native `/meta/...` API tree (see
# `docs/NATIVE-API-SPEC.md` and `docs/API.md`), a thin `PVE::RESTHandler`
# subclass whose methods call straight into `PVE::RS::Meta`'s `api_*`
# functions (`crates/pve-meta-perl`). No `proxyto` on any method: the store
# lives under `/etc/pve/meta`, so it is cluster-wide and any node can
# answer. Read methods are `protected => 0` (run in pveproxy, which can
# read `/etc/pve/meta/*` as `www-data`); write methods are `protected => 1`
# (run in pvedaemon, as root).

# -- helpers ---------------------------------------------------------------

# Snapshot names: mirrors `pve_meta_core::store::is_valid_snapshot_name`
# (`^[A-Za-z][A-Za-z0-9_-]*$`) for nicer API-viewer docs; the Rust side is
# still the actual authority (a name that somehow slips past this pattern
# dies there with a readable "invalid name" error).
my $SNAPSHOT_NAME_SCHEMA = {
    type => 'string',
    pattern => '^[A-Za-z][A-Za-z0-9_-]*$',
    description => "Snapshot name.",
};

# Calls a `PVE::RS::Meta::api_*` function, catching its "NNN: message" die
# (the Rust->Perl error contract from `docs/NATIVE-API-SPEC.md`) and
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

# Decodes a document hash's `data_json` field (JSON-encoded on the Rust
# side to preserve number/boolean fidelity, see `docs/NATIVE-API-SPEC.md`)
# into a real `data` key, in place. `JSON::PP`-backed `decode_json` (via
# `use JSON`) produces `JSON::PP::Boolean` objects for JSON
# `true`/`false`, so a document's own booleans round-trip correctly when
# pveproxy re-encodes the response.
sub _inflate_document {
    my ($doc) = @_;
    $doc->{data} = decode_json(delete $doc->{data_json});
    return $doc;
}

# `GET /meta/guests/{vmid}` / `GET /meta/datacenter`.
my $get_document = sub {
    my ($id, $param) = @_;
    my $doc = _call(
        \&PVE::RS::Meta::api_get, $id, ($param->{comments} ? 1 : 0), ($param->{raw} ? 1 : 0),
    );
    return _inflate_document($doc);
};

# `GET .../subtree?path=...`.
my $get_subtree = sub {
    my ($id, $param) = @_;
    my $result = _call(\&PVE::RS::Meta::api_subtree, $id, $param->{path});
    $result->{data} = decode_json(delete $result->{data_json});
    return $result;
};

# `PUT /meta/guests/{vmid}` / `PUT /meta/datacenter`: merge-patch. `patch`
# is already a JSON-encoded string on the wire (see `docs/API.md`), so it
# is passed straight through to `api_patch`'s `$patch_json` argument with
# no decode/re-encode round trip.
my $patch_document = sub {
    my ($id, $param) = @_;
    my $doc = _call(
        \&PVE::RS::Meta::api_patch, $id, $param->{patch}, $param->{digest}, ($param->{dry_run} ? 1 : 0),
    );
    return _inflate_document($doc);
};

# `PUT .../raw`: full-text replace.
my $put_raw_document = sub {
    my ($id, $param) = @_;
    my $doc = _call(
        \&PVE::RS::Meta::api_put_raw, $id, $param->{content}, $param->{format}, $param->{digest},
        ($param->{dry_run} ? 1 : 0),
    );
    return _inflate_document($doc);
};

# `POST .../convert`.
my $convert_document = sub {
    my ($id, $param) = @_;
    my $doc = _call(\&PVE::RS::Meta::api_convert, $id, $param->{format}, $param->{digest});
    return _inflate_document($doc);
};

# `DELETE /meta/guests/{vmid}` / `DELETE /meta/datacenter`.
my $delete_document = sub {
    my ($id) = @_;
    _call(\&PVE::RS::Meta::api_delete, $id);
    return undef;
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
        return [map { { subdir => $_ } } qw(version health inventory guests datacenter registry schemas)];
    },
});

# -- version / health / inventory --------------------------------------------

__PACKAGE__->register_method({
    name => 'version',
    path => 'version',
    method => 'GET',
    permissions => { user => 'all' },
    description => "The store's current change-version token. Cheap; poll it "
        . "every few seconds. 'wait'/'since' are accepted for client "
        . "compatibility with the optional standalone pve-metad daemon (which "
        . "can long-poll on them) but are ignored here: the native module "
        . "always returns immediately.",
    parameters => {
        additionalProperties => 0,
        properties => {
            wait => {
                type => 'integer',
                optional => 1,
                minimum => 0,
                maximum => 60,
                description => "Ignored (see above).",
            },
            since => {
                type => 'string',
                optional => 1,
                description => "Ignored (see above).",
            },
        },
    },
    returns => { type => 'object', additionalProperties => 1 },
    code => sub {
        return _call(\&PVE::RS::Meta::api_version);
    },
});

__PACKAGE__->register_method({
    name => 'health',
    path => 'health',
    method => 'GET',
    permissions => { user => 'all' },
    description => "Store health/summary: root path, file/byte counts, and the "
        . "pve-meta-rs crate version.",
    parameters => {
        additionalProperties => 0,
        properties => {},
    },
    returns => { type => 'object', additionalProperties => 1 },
    code => sub {
        return _call(\&PVE::RS::Meta::api_health);
    },
});

__PACKAGE__->register_method({
    name => 'inventory',
    path => 'inventory',
    method => 'GET',
    permissions => { check => ['perm', '/', ['Sys.Audit']] },
    description => "Guests from /etc/pve/.vmlist, enriched with the guest's "
        . "display name and whether/how it has a metadata document.",
    parameters => {
        additionalProperties => 0,
        properties => {},
    },
    returns => {
        type => 'array',
        items => { type => 'object', additionalProperties => 1 },
    },
    code => sub {
        return _call(\&PVE::RS::Meta::api_inventory);
    },
});

# -- guests -------------------------------------------------------------

__PACKAGE__->register_method({
    name => 'list_guests',
    path => 'guests',
    method => 'GET',
    permissions => {
        description => "Anybody may call this; the list is filtered to guests "
            . "the caller has VM.Audit on.",
        user => 'all',
    },
    description => "Lists guest metadata documents.",
    parameters => {
        additionalProperties => 0,
        properties => {
            has => {
                type => 'string',
                optional => 1,
                description =>
                    "Only list guests whose document has data at this dotted path (top-level key or dotted prefix).",
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

        my $list = _call(\&PVE::RS::Meta::api_list_guests, $param->{has});
        # NOTE: copy vmid into a fresh lexical before interpolating it: string
        # interpolation caches a PV representation on the *actual* scalar it
        # reads, and interpolating $_->{vmid} directly would do that on the
        # entry's own vmid value -- which is then returned as part of the
        # JSON response -- silently turning the JSON encoder's later number
        # detection for that field into a string ("200" instead of 200).
        return [
            grep {
                my $vmid = $_->{vmid};
                $rpcenv->check($authuser, "/vms/$vmid", ['VM.Audit'], 1)
            } @$list
        ];
    },
});

__PACKAGE__->register_method({
    name => 'get_guest',
    path => 'guests/{vmid}',
    method => 'GET',
    permissions => { check => ['perm', '/vms/{vmid}', ['VM.Audit']] },
    description => "Gets a guest's metadata document.",
    parameters => {
        additionalProperties => 0,
        properties => {
            vmid => get_standard_option('pve-vmid'),
            comments => {
                type => 'boolean',
                optional => 1,
                default => 0,
                description => 'Keep comment keys ("key__"/"__").',
            },
            raw => {
                type => 'boolean',
                optional => 1,
                default => 0,
                description => "Include the raw file text.",
            },
        },
    },
    returns => { type => 'object', additionalProperties => 1 },
    code => sub {
        my ($param) = @_;
        return $get_document->("$param->{vmid}", $param);
    },
});

__PACKAGE__->register_method({
    name => 'patch_guest',
    path => 'guests/{vmid}',
    method => 'PUT',
    protected => 1,
    permissions => { check => ['perm', '/vms/{vmid}', ['VM.Config.Options']] },
    description => "Applies a merge patch to a guest's metadata document "
        . "(creating it in the default format if it does not exist).",
    parameters => {
        additionalProperties => 0,
        properties => {
            vmid => get_standard_option('pve-vmid'),
            patch => {
                type => 'string',
                description => "JSON-encoded merge patch (RFC 7386-like; null deletes a key).",
            },
            digest => get_standard_option('pve-config-digest'),
            dry_run => {
                type => 'boolean',
                optional => 1,
                default => 0,
                description => "Validate and diff without writing.",
            },
        },
    },
    returns => { type => 'object', additionalProperties => 1 },
    code => sub {
        my ($param) = @_;
        return $patch_document->("$param->{vmid}", $param);
    },
});

__PACKAGE__->register_method({
    name => 'get_guest_subtree',
    path => 'guests/{vmid}/subtree',
    method => 'GET',
    permissions => { check => ['perm', '/vms/{vmid}', ['VM.Audit']] },
    description =>
        "Gets a subtree of a guest's document at a dotted path (404 if nothing is there).",
    parameters => {
        additionalProperties => 0,
        properties => {
            vmid => get_standard_option('pve-vmid'),
            path => { type => 'string', description => "Dotted path into the document." },
        },
    },
    returns => { type => 'object', additionalProperties => 1 },
    code => sub {
        my ($param) = @_;
        return $get_subtree->("$param->{vmid}", $param);
    },
});

__PACKAGE__->register_method({
    name => 'put_guest_raw',
    path => 'guests/{vmid}/raw',
    method => 'PUT',
    protected => 1,
    permissions => { check => ['perm', '/vms/{vmid}', ['VM.Config.Options']] },
    description => "Replaces a guest's metadata document with raw text.",
    parameters => {
        additionalProperties => 0,
        properties => {
            vmid => get_standard_option('pve-vmid'),
            content => { type => 'string', description => "Full replacement file text." },
            format => {
                type => 'string',
                optional => 1,
                description => "Switch the document's format/extension (yaml/toml/json).",
            },
            digest => get_standard_option('pve-config-digest'),
            dry_run => {
                type => 'boolean',
                optional => 1,
                default => 0,
                description => "Validate and diff without writing.",
            },
        },
    },
    returns => { type => 'object', additionalProperties => 1 },
    code => sub {
        my ($param) = @_;
        return $put_raw_document->("$param->{vmid}", $param);
    },
});

__PACKAGE__->register_method({
    name => 'convert_guest',
    path => 'guests/{vmid}/convert',
    method => 'POST',
    protected => 1,
    permissions => { check => ['perm', '/vms/{vmid}', ['VM.Config.Options']] },
    description =>
        "Re-dumps a guest's document in another format (file comments are lost; comment keys survive).",
    parameters => {
        additionalProperties => 0,
        properties => {
            vmid => get_standard_option('pve-vmid'),
            format => { type => 'string', description => "Target format (yaml/toml/json)." },
            digest => get_standard_option('pve-config-digest'),
        },
    },
    returns => { type => 'object', additionalProperties => 1 },
    code => sub {
        my ($param) = @_;
        return $convert_document->("$param->{vmid}", $param);
    },
});

__PACKAGE__->register_method({
    name => 'delete_guest',
    path => 'guests/{vmid}',
    method => 'DELETE',
    protected => 1,
    permissions => { check => ['perm', '/vms/{vmid}', ['VM.Config.Options']] },
    description => "Deletes a guest's document and all of its snapshot copies.",
    parameters => {
        additionalProperties => 0,
        properties => {
            vmid => get_standard_option('pve-vmid'),
        },
    },
    returns => { type => 'null' },
    code => sub {
        my ($param) = @_;
        return $delete_document->("$param->{vmid}");
    },
});

__PACKAGE__->register_method({
    name => 'list_guest_snapshots',
    path => 'guests/{vmid}/snapshots',
    method => 'GET',
    permissions => { check => ['perm', '/vms/{vmid}', ['VM.Audit']] },
    description => "Lists a guest's snapshot names.",
    parameters => {
        additionalProperties => 0,
        properties => {
            vmid => get_standard_option('pve-vmid'),
        },
    },
    returns => {
        type => 'array',
        items => { type => 'string' },
    },
    code => sub {
        my ($param) = @_;
        return _call(\&PVE::RS::Meta::api_snapshots, $param->{vmid});
    },
});

__PACKAGE__->register_method({
    name => 'snapshot_guest',
    path => 'guests/{vmid}/snapshot',
    method => 'POST',
    protected => 1,
    permissions => { check => ['perm', '/vms/{vmid}', ['VM.Config.Options']] },
    description => "Snapshots a guest's current document (a no-op if it has none).",
    parameters => {
        additionalProperties => 0,
        properties => {
            vmid => get_standard_option('pve-vmid'),
            name => $SNAPSHOT_NAME_SCHEMA,
        },
    },
    returns => { type => 'object', additionalProperties => 1 },
    code => sub {
        my ($param) = @_;
        return _call(\&PVE::RS::Meta::api_snapshot, $param->{vmid}, $param->{name});
    },
});

__PACKAGE__->register_method({
    name => 'rollback_guest',
    path => 'guests/{vmid}/rollback',
    method => 'POST',
    protected => 1,
    permissions => { check => ['perm', '/vms/{vmid}', ['VM.Config.Options']] },
    description => "Rolls a guest's document back to a snapshot.",
    parameters => {
        additionalProperties => 0,
        properties => {
            vmid => get_standard_option('pve-vmid'),
            name => $SNAPSHOT_NAME_SCHEMA,
        },
    },
    returns => { type => 'object', additionalProperties => 1 },
    code => sub {
        my ($param) = @_;
        return _call(\&PVE::RS::Meta::api_rollback, $param->{vmid}, $param->{name});
    },
});

__PACKAGE__->register_method({
    name => 'delete_guest_snapshot',
    path => 'guests/{vmid}/snapshots/{name}',
    method => 'DELETE',
    protected => 1,
    permissions => { check => ['perm', '/vms/{vmid}', ['VM.Config.Options']] },
    description => "Deletes a guest's snapshot.",
    parameters => {
        additionalProperties => 0,
        properties => {
            vmid => get_standard_option('pve-vmid'),
            name => $SNAPSHOT_NAME_SCHEMA,
        },
    },
    returns => { type => 'null' },
    code => sub {
        my ($param) = @_;
        _call(\&PVE::RS::Meta::api_delete_snapshot, $param->{vmid}, $param->{name});
        return undef;
    },
});

__PACKAGE__->register_method({
    name => 'clone_guest',
    path => 'guests/{vmid}/clone',
    method => 'POST',
    protected => 1,
    permissions => { check => ['perm', '/vms/{vmid}', ['VM.Config.Options', 'VM.Clone']] },
    description => "Clones a guest's document (only) to another vmid, overwriting "
        . "any document that vmid already has.",
    parameters => {
        additionalProperties => 0,
        properties => {
            vmid => get_standard_option('pve-vmid'),
            newid => get_standard_option('pve-vmid', { description => "The new guest's vmid." }),
        },
    },
    returns => { type => 'object', additionalProperties => 1 },
    code => sub {
        my ($param) = @_;
        my $doc = _call(\&PVE::RS::Meta::api_clone, $param->{vmid}, $param->{newid});
        return _inflate_document($doc);
    },
});

# -- datacenter -----------------------------------------------------------

__PACKAGE__->register_method({
    name => 'get_datacenter',
    path => 'datacenter',
    method => 'GET',
    permissions => { check => ['perm', '/', ['Sys.Audit']] },
    description => "Gets the datacenter metadata document.",
    parameters => {
        additionalProperties => 0,
        properties => {
            comments => {
                type => 'boolean',
                optional => 1,
                default => 0,
                description => 'Keep comment keys ("key__"/"__").',
            },
            raw => {
                type => 'boolean',
                optional => 1,
                default => 0,
                description => "Include the raw file text.",
            },
        },
    },
    returns => { type => 'object', additionalProperties => 1 },
    code => sub {
        my ($param) = @_;
        return $get_document->('datacenter', $param);
    },
});

__PACKAGE__->register_method({
    name => 'patch_datacenter',
    path => 'datacenter',
    method => 'PUT',
    protected => 1,
    permissions => { check => ['perm', '/', ['Sys.Modify']] },
    description => "Applies a merge patch to the datacenter document "
        . "(creating it in the default format if it does not exist).",
    parameters => {
        additionalProperties => 0,
        properties => {
            patch => {
                type => 'string',
                description => "JSON-encoded merge patch (RFC 7386-like; null deletes a key).",
            },
            digest => get_standard_option('pve-config-digest'),
            dry_run => {
                type => 'boolean',
                optional => 1,
                default => 0,
                description => "Validate and diff without writing.",
            },
        },
    },
    returns => { type => 'object', additionalProperties => 1 },
    code => sub {
        my ($param) = @_;
        return $patch_document->('datacenter', $param);
    },
});

__PACKAGE__->register_method({
    name => 'get_datacenter_subtree',
    path => 'datacenter/subtree',
    method => 'GET',
    permissions => { check => ['perm', '/', ['Sys.Audit']] },
    description =>
        "Gets a subtree of the datacenter document at a dotted path (404 if nothing is there).",
    parameters => {
        additionalProperties => 0,
        properties => {
            path => { type => 'string', description => "Dotted path into the document." },
        },
    },
    returns => { type => 'object', additionalProperties => 1 },
    code => sub {
        my ($param) = @_;
        return $get_subtree->('datacenter', $param);
    },
});

__PACKAGE__->register_method({
    name => 'put_datacenter_raw',
    path => 'datacenter/raw',
    method => 'PUT',
    protected => 1,
    permissions => { check => ['perm', '/', ['Sys.Modify']] },
    description => "Replaces the datacenter document with raw text.",
    parameters => {
        additionalProperties => 0,
        properties => {
            content => { type => 'string', description => "Full replacement file text." },
            format => {
                type => 'string',
                optional => 1,
                description => "Switch the document's format/extension (yaml/toml/json).",
            },
            digest => get_standard_option('pve-config-digest'),
            dry_run => {
                type => 'boolean',
                optional => 1,
                default => 0,
                description => "Validate and diff without writing.",
            },
        },
    },
    returns => { type => 'object', additionalProperties => 1 },
    code => sub {
        my ($param) = @_;
        return $put_raw_document->('datacenter', $param);
    },
});

__PACKAGE__->register_method({
    name => 'convert_datacenter',
    path => 'datacenter/convert',
    method => 'POST',
    protected => 1,
    permissions => { check => ['perm', '/', ['Sys.Modify']] },
    description =>
        "Re-dumps the datacenter document in another format (file comments are lost; comment keys survive).",
    parameters => {
        additionalProperties => 0,
        properties => {
            format => { type => 'string', description => "Target format (yaml/toml/json)." },
            digest => get_standard_option('pve-config-digest'),
        },
    },
    returns => { type => 'object', additionalProperties => 1 },
    code => sub {
        my ($param) = @_;
        return $convert_document->('datacenter', $param);
    },
});

__PACKAGE__->register_method({
    name => 'delete_datacenter',
    path => 'datacenter',
    method => 'DELETE',
    protected => 1,
    permissions => { check => ['perm', '/', ['Sys.Modify']] },
    description => "Deletes the datacenter document.",
    parameters => {
        additionalProperties => 0,
        properties => {},
    },
    returns => { type => 'null' },
    code => sub {
        return $delete_document->('datacenter');
    },
});

# -- registry / schemas -------------------------------------------------

__PACKAGE__->register_method({
    name => 'registry',
    path => 'registry',
    method => 'GET',
    permissions => { check => ['perm', '/', ['Sys.Audit']] },
    description => "Convenience view of the datacenter document's 'operators' section.",
    parameters => {
        additionalProperties => 0,
        properties => {},
    },
    returns => {
        type => 'array',
        items => { type => 'object', additionalProperties => 1 },
    },
    code => sub {
        my $entries = _call(\&PVE::RS::Meta::api_registry);
        for my $entry (@$entries) {
            $entry->{schemas} = decode_json(delete $entry->{schemas_json});
        }
        return $entries;
    },
});

__PACKAGE__->register_method({
    name => 'schemas_for_guest',
    path => 'schemas/{vmid}',
    method => 'GET',
    permissions => { check => ['perm', '/vms/{vmid}', ['VM.Audit']] },
    description => "The JSON schemas applicable to a guest's document "
        . "(i.e. for namespaces it actually uses), keyed by namespace prefix. "
        . "Used by the UI form generator.",
    parameters => {
        additionalProperties => 0,
        properties => {
            vmid => get_standard_option('pve-vmid'),
        },
    },
    returns => { type => 'object', additionalProperties => 1 },
    code => sub {
        my ($param) = @_;
        my $json = _call(\&PVE::RS::Meta::api_schemas, $param->{vmid});
        return decode_json($json);
    },
});

1;
