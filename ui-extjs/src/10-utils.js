// ---------------------------------------------------------------------------
// Helpers: paths, the JSON data model, and YAML in and out.
// ---------------------------------------------------------------------------

// Plain FontAwesome classes, sized and coloured by PVE's own resource-tree
// stylesheet via `x-tree-icon-custom`, so no CSS ships with this file.
PVE.meta.Icons = {
    map: 'fa fa-folder',
    mapExpanded: 'fa fa-folder-open',
    leaf: 'fa fa-file-text-o',
};

PVE.meta.Utils = {
    // One finding as the warning banner lists it; an enforced one says so first.
    findingText: function (f) {
        return (f.enforced ? gettext('enforced') + ': ' : '') + f.path + ': ' + f.msg;
    },

    // A key ending in `__` documents its sibling; a bare `__` documents the map.
    isComment: (key) => key.length >= 2 && key.slice(-2) === '__',
    commentTarget: (key) => key.slice(0, -2),
    joinPath: (prefix, key) => (prefix ? prefix + '.' + key : key),
    // The path above `path`, or '' for a top-level key (and for the root itself).
    parentPath: (path) => (path && path.indexOf('.') !== -1 ? path.slice(0, path.lastIndexOf('.')) : ''),

    // Two document values are the same value: the core's own dump of each, compared.
    sameValue: (a, b) => PVE.meta.Core.call('same', a, b),

    // Why a key name is not one (`path::is_valid_segment`, via `key_path_check`),
    // or `null` if it is fine; the server refuses the same names on its own.
    keyPathError: function (text) {
        if (!PVE.meta.Core.loaded()) {
            return null; // no early answer, then; the server's is still the answer
        }
        let why = PVE.meta.Core.call('key_path_check', String(text === undefined || text === null ? '' : text));
        if (!why) {
            return null;
        }
        if (why.reason === 'empty') {
            return gettext('Key must not be empty');
        }
        if (why.reason === 'empty_segment') {
            return gettext('A dotted path must not have an empty segment');
        }
        return Ext.String.format(
            gettext("'{0}' is not allowed in a key ({1}). Keys are letters, digits, _ - @ ! -- and '.' separates them."),
            why.char === ' ' ? gettext('space') : why.char,
            why.segment,
        );
    },

    // Why a registry file name is not one (`registry::is_valid_file_name`), or `null`.
    fileNameError: function (text) {
        if (!PVE.meta.Core.loaded()) {
            return null; // as `keyPathError`
        }
        return PVE.meta.Core.call('file_name_valid', String(text || ''))
            ? null
            : gettext("A name is one or more key segments joined by '.', e.g. homelab.docker");
    },

    kindOf: function (value) {
        if (Ext.isArray(value)) {
            return 'array';
        } else if (value !== null && typeof value === 'object') {
            return 'map';
        } else if (typeof value === 'boolean') {
            return 'boolean';
        } else if (typeof value === 'number') {
            return 'number';
        }
        return 'string';
    },

    // What the Value column shows for a leaf. Arrays are one text leaf (DESIGN §8).
    displayValue: function (value, kind) {
        if (kind === 'map') {
            return '';
        } else if (kind === 'boolean') {
            // The API's JSON view renders YAML booleans as 1/0 (a Perl artifact).
            return Proxmox.Utils.format_boolean(value);
        }
        return kind === 'string' ? String(value) : Ext.encode(value);
    },

    // A grammar's `format` is a PVE::JSONSchema format name, wired to proxmoxlib's
    // own vtype for it rather than a second regex; an unmapped format constrains
    // nothing. The server never validates `format` (DESIGN §5), so this is the
    // only place it is checked at all.
    FORMAT_VTYPES: {
        'ip': 'IP64Address',
        'ipv4': 'IPAddress',
        'ipv6': 'IP6Address',
        'CIDR': 'IP64CIDRAddress',
        'CIDRv4': 'IPCIDRAddress',
        'CIDRv6': 'IP6CIDRAddress',
        'mac-addr': 'MacAddress',
        'dns-name': 'DnsName',
        'address': 'DnsOrIp',
        'email': 'proxmoxMail',
    },

    vtypeFor: function (format) {
        return (format && PVE.meta.Utils.FORMAT_VTYPES[format]) || undefined;
    },

    // The inverse, for a committed row edit: field value -> the JSON value to send.
    parseValue: function (text, kind) {
        if (kind === 'boolean') {
            return text === true || text === 'true' || text === 1 || text === '1';
        } else if (kind === 'number') {
            let n = Number(text);
            if (text === '' || isNaN(n)) {
                throw new Error(gettext('Not a number') + ': ' + text);
            }
            return n;
        } else if (kind === 'array') {
            let t = String(text).trim();
            if (t.charAt(0) === '[') {
                return Ext.decode(t); // throws on malformed JSON, which is what we want
            }
            return t === '' ? [] : t.split(',').map((s) => s.trim());
        }
        return String(text);
    },

    // The value at a dotted path, through maps only -- `undefined` where a segment
    // is missing or the path runs into a list or a scalar (the addressing rule a
    // view has, DESIGN §2).
    valueAt: function (data, path) {
        let cur = data;
        if (!path) {
            return cur;
        }
        let parts = path.split('.');
        for (let i = 0; i < parts.length; i++) {
            if (!cur || typeof cur !== 'object' || Array.isArray(cur)) {
                return undefined;
            }
            cur = Object.prototype.hasOwnProperty.call(cur, parts[i]) ? cur[parts[i]] : undefined;
        }
        return cur;
    },

    // Rolls a set of `path -> message` facts up to every ancestor path, so a
    // collapsed branch still shows a count and the first few messages beneath it.
    rollUp: function (byPath) {
        let out = Object.create(null);
        Object.keys(byPath || {}).forEach(function (path) {
            let segs = path.split('.');
            for (let i = 1; i < segs.length; i++) {
                let ancestor = segs.slice(0, i).join('.');
                let at = out[ancestor] || (out[ancestor] = { count: 0, messages: [] });
                at.count++;
                if (at.messages.length < 3) {
                    at.messages.push(path + ': ' + byPath[path]);
                }
            }
        });
        return out;
    },

    // One element of a list, on one line: presentation only. A map member falls
    // back to its JSON, since there is no shape this recognises beyond a scalar.
    itemSummary: function (v) {
        if (!v || typeof v !== 'object' || Array.isArray(v)) {
            return PVE.meta.Utils.scalarText(v);
        }
        return Ext.encode(v);
    },

    // A schema `type` as the kind `parseValue` speaks: integers and numbers both
    // parse as numbers; a map or a list of one is not a scalar to parse at all.
    schemaValueKind: function (type) {
        if (type === 'integer' || type === 'number') {
            return 'number';
        }
        return type === 'boolean' || type === 'array' ? type : 'string';
    },

    // Which editor a row's *shape* calls for: 'inline', 'multiline' (a textarea) or
    // 'text' (Monaco). A value with structure (a map, an array of maps) is edited as
    // text; a string is inline unless it has a newline or `multiline` says so (DESIGN §8).
    editorKind: function (d) {
        if (!d) {
            return 'none';
        }
        if (d.kind === 'map') {
            return 'text';
        }
        if (d.kind === 'array') {
            let list = Array.isArray(d.rawValue) ? d.rawValue : [];
            return list.some((v) => v !== null && typeof v === 'object') ? 'text' : 'inline';
        }
        if (d.kind === 'string') {
            let raw = typeof d.rawValue === 'string' ? d.rawValue : '';
            return d.multiline || raw.indexOf('\n') !== -1 ? 'multiline' : 'inline';
        }
        return 'inline';
    },

    // What the Value column shows for a value that does not fit on a line. The row
    // keeps its exact `valueText`, which the editor opens on, so truncation here
    // never touches the data.
    previewText: function (text) {
        let s = String(text === undefined || text === null ? '' : text);
        let nl = s.indexOf('\n');
        if (nl === -1) {
            return s;
        }
        let rest = s.slice(nl + 1).replace(/\n+$/, '');
        let more = rest === '' ? 0 : rest.split('\n').length;
        let first = s.slice(0, nl);
        return more ? first + ' ' + Ext.String.format(gettext('(+{0} lines)'), more) : first;
    },

    // The field a row's value is edited with. A declared type wins over the type
    // inferred from the stored value: a stored `1` under a declared `boolean`
    // still gets the checkbox.
    editorFor: function (d) {
        if (d.enumValues) {
            return {
                xtype: 'combobox',
                store: d.enumValues.map((v) => String(v)),
                queryMode: 'local',
                editable: false,
                forceSelection: true,
                // The list renders each member as markup, and a member is schema
                // text. The field itself shows it as a value, which needs nothing.
                listConfig: { getInnerTpl: (field) => '{' + field + ':htmlEncode}' },
            };
        } else if (d.kind === 'boolean') {
            return { xtype: 'proxmoxcheckbox' };
        } else if (d.kind === 'number') {
            let f = { xtype: 'numberfield', allowDecimals: true, hideTrigger: true, keyNavEnabled: false };
            if (d.minimum !== undefined && d.minimum !== null) {
                f.minValue = d.minimum;
            }
            if (d.maximum !== undefined && d.maximum !== null) {
                f.maxValue = d.maximum;
            }
            return f;
        }
        if (PVE.meta.Utils.editorKind(d) === 'multiline') {
            // No `format` here: a PVE format validates one line, none of them a block.
            return { xtype: 'textarea', height: 260, grow: false };
        }
        let f = { xtype: 'textfield', selectOnFocus: true };
        let vtype = PVE.meta.Utils.vtypeFor(d.format);
        if (vtype) {
            f.vtype = vtype;
        }
        return f;
    },

    // A scalar as the string a grammar's `enum` and a hover compare and show.
    scalarText: function (value) {
        return typeof value === 'string' ? value : Ext.encode(value);
    },

    // Runs the vtype a format maps to, and returns that vtype's own message on
    // failure; an unmapped format constrains nothing.
    checkFormat: function (format, value) {
        let vtype = PVE.meta.Utils.vtypeFor(format);
        // Runs before any form field is instantiated, so the VTypes singleton may
        // not exist yet; a validator we cannot reach must constrain nothing.
        let vtypes = Ext.form && Ext.form.field && Ext.form.field.VTypes;
        if (!vtype || !vtypes || typeof vtypes[vtype] !== 'function') {
            return null;
        }
        if (vtypes[vtype](value)) {
            return null;
        }
        return vtypes[vtype + 'Text'] || gettext('invalid value');
    },

    // Human-readable form of a prefix's selector, for the "Applies to" column.
    selectorText: function (selector) {
        let sel = selector || {};
        if (sel.tag) {
            return gettext('tag') + ': ' + sel.tag;
        } else if (sel.all) {
            return gettext('all guests');
        }
        return Ext.encode(sel);
    },

    errText: (err) => String((err && (err.message || err.msg)) || err),

    alertError: (err) => Ext.Msg.alert(gettext('Error'), Ext.htmlEncode(PVE.meta.Utils.errText(err))),
};
