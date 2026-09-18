// ---------------------------------------------------------------------------
// The row model.
// ---------------------------------------------------------------------------

Ext.define('PVE.meta.TreeModel', {
    extend: 'Ext.data.TreeModel',
    fields: [
        { name: 'key', type: 'string' },
        { name: 'docId', type: 'string' }, // which document this row belongs to
        { name: 'path', type: 'string' }, // dotted; this is the `view` of a write
        { name: 'finding', type: 'string' }, // this row does not match its schema
        { name: 'belowCount', type: 'int' }, // findings somewhere beneath this row
        { name: 'belowText', type: 'string' }, // the first few of them, for the tooltip
        { name: 'multiline', type: 'boolean' }, // grammar `multiline` -> a text box
        { name: 'arrayIndex' }, // this row is member N of the list at `path`
        { name: 'addressable', type: 'boolean' }, // false: no view path names this row
        { name: 'rawItem' }, // a list member's real value, whatever it is
        { name: 'valueText', type: 'string' },
        { name: 'description', type: 'string' }, // the comment key `k__`, if present
        { name: 'grammarDescription', type: 'string' }, // the grammar's, shown as tooltip
        { name: 'kind', type: 'string' }, // map | array | string | number | boolean
        { name: 'present', type: 'boolean' },
        { name: 'editable', type: 'boolean' },
        { name: 'expandedCls', type: 'string' }, // iconCls while this map row is open
        { name: 'defaultValue' },
        { name: 'enumValues' },
        { name: 'minimum' }, // grammar `minimum`, honoured by the number editor
        { name: 'maximum' }, // grammar `maximum`, honoured by the number editor
        { name: 'format', type: 'string' }, // grammar `format` -> an ExtJS vtype
        { name: 'rawValue' },
    ],
});

