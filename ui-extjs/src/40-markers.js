// ---------------------------------------------------------------------------
// Markers for the text editor: which line a finding goes on, and what a hover
// says. The findings themselves come from the Shape (the core); this only places
// them, by scanning the YAML the editor holds. The pairing holds while the buffer
// still is what the server sent, so the caller clears both once it is dirty.
// ---------------------------------------------------------------------------

PVE.meta.Markers = {
    // A scan, not a parser: the store dumps canonically (block style, two-space indent,
    // one mapping key per line), so an indent stack resolves every key's path. Sequence
    // items are not indexed (a view addresses through maps only) and a block scalar's
    // body is skipped, so prose that reads `foo: bar` is never taken for a key.
    lineIndex: function (yaml) {
        let out = Object.create(null);
        let stack = []; // [indent, key]
        let blockAt = null;
        String(yaml || '')
            .split('\n')
            .forEach(function (raw, i) {
                let line = raw.trim();
                let indent = raw.length - raw.replace(/^\s+/, '').length;
                if (blockAt !== null) {
                    if (line === '' || indent > blockAt) {
                        return;
                    }
                    blockAt = null;
                }
                if (line === '' || line.charAt(0) === '#' || line === '---' || line === '...') {
                    return;
                }
                if (line === '-' || line.slice(0, 2) === '- ') {
                    return;
                }
                let split = PVE.meta.Markers.splitKey(line);
                if (!split) {
                    return;
                }
                while (stack.length && stack[stack.length - 1][0] >= indent) {
                    stack.pop();
                }
                stack.push([indent, split.key]);
                out[stack.map((e) => e[1]).join('.')] = i + 1;
                let value = split.rest.trim();
                if (value.charAt(0) === '|' || value.charAt(0) === '>') {
                    blockAt = indent;
                }
            });
        return out;
    },

    splitKey: function (line) {
        if (line.charAt(0) === '"') {
            let key = '';
            for (let i = 1; i < line.length; i++) {
                let c = line.charAt(i);
                if (c === '\\') {
                    key += line.charAt(++i);
                } else if (c === '"') {
                    let after = line.slice(i + 1);
                    return after.charAt(0) === ':' ? { key: key, rest: after.slice(1) } : null;
                } else {
                    key += c;
                }
            }
            return null;
        }
        let at = line.indexOf(':');
        if (at <= 0) {
            return null;
        }
        let key = line.slice(0, at).replace(/\s+$/, '');
        return key ? { key: key, rest: line.slice(at + 1) } : null;
    },

    // Findings paired with the line to underline; one whose path the text does not
    // carry is dropped, because a marker on the wrong line is worse than none.
    placed: function (findings, index) {
        let out = [];
        (findings || []).forEach(function (f) {
            if (index[f.path] !== undefined) {
                out.push({ line: index[f.path], message: f.msg });
            }
        });
        return out;
    },

    hoverText: function (schema) {
        if (!schema) {
            return null;
        }
        let parts = [];
        if (schema.type) {
            parts.push(schema.format ? schema.type + ' (' + schema.format + ')' : schema.type);
        }
        if (schema.enum) {
            parts.push(gettext('one of') + ': ' + schema.enum.map((v) => String(v)).join(', '));
        }
        if (schema.minimum !== undefined && schema.maximum !== undefined) {
            parts.push(schema.minimum + '..' + schema.maximum);
        } else if (schema.minimum !== undefined) {
            parts.push(gettext('at least') + ' ' + schema.minimum);
        } else if (schema.maximum !== undefined) {
            parts.push(gettext('at most') + ' ' + schema.maximum);
        }
        if (schema.default !== undefined) {
            parts.push(gettext('default') + ': ' + PVE.meta.Utils.scalarText(schema.default));
        }
        if (schema.description) {
            parts.push(schema.description);
        }
        return parts.length ? parts.join(' · ') : null;
    },
};

