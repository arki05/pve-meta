// Monaco glue for the pve-meta editor page.
//
// Nothing in the Proxmox pwt/PDM stack embeds a third-party JS widget (the one place
// PDM needs one - xterm.js - it iframes the legacy ExtJS console instead), so this file
// is a deliberate, clearly-bounded exception to "no .js files"; see
// docs/design/PDM-DESIGN-LANGUAGE.md sections 9 and 11. Everything else on the page is
// pwt widgets.
//
// Monaco is shipped in the package (dist/vs, copied from node_modules by Trunk) and
// loaded with its own AMD loader - never from a CDN. Monaco does not inherit CSS, so
// its theme is built from the --pwt-* custom properties resolved on the mount element
// (which is inside whatever .pwt-scheme-* subtree the page put it in, so the values are
// already the right pair).

(function () {
    'use strict';

    var editors = {}; // id -> { el, editor, diff, model, pending, onChange, readOnly }
    var nextId = 0;
    var monacoPromise = null;

    function log(msg, err) {
        if (window.console) {
            window.console.error('pve-meta-monaco: ' + msg, err || '');
        }
    }

    // Absolute URL of the shipped Monaco tree, derived from this script's own location
    // so it keeps working under pveproxy's /pve2/js/pve-meta-ui/ mount point.
    function vsPath() {
        var script = document.querySelector('script[src*="pve-meta-monaco.js"]');
        var base = script ? script.src : document.baseURI;
        return new URL('../vs', base).href.replace(/\/$/, '');
    }

    function loadMonaco() {
        if (monacoPromise) {
            return monacoPromise;
        }
        monacoPromise = new Promise(function (resolve, reject) {
            var vs = vsPath();
            var loader = document.createElement('script');
            loader.src = vs + '/loader.js';
            loader.onload = function () {
                window.require.config({ paths: { vs: vs } });
                window.require(['vs/editor/editor.main'], function () {
                    resolve(window.monaco);
                }, reject);
            };
            loader.onerror = function () {
                reject(new Error('failed to load ' + loader.src));
            };
            document.head.appendChild(loader);
        });
        return monacoPromise;
    }

    // --- theme -------------------------------------------------------------

    function cssVar(style, name) {
        return (style.getPropertyValue(name) || '').trim();
    }

    // Monaco only accepts #rgb/#rrggbb(aa); CSS custom properties may resolve to
    // rgb()/rgba(). Anything else is dropped rather than breaking defineTheme().
    function hex(value) {
        if (!value) {
            return null;
        }
        if (value.charAt(0) === '#') {
            return value.length === 4 || value.length === 7 || value.length === 9 ? value : null;
        }
        var m = value.match(/^rgba?\(\s*([\d.]+)[\s,]+([\d.]+)[\s,]+([\d.]+)/i);
        if (!m) {
            return null;
        }
        var out = '#';
        for (var i = 1; i <= 3; i++) {
            var c = Math.max(0, Math.min(255, Math.round(parseFloat(m[i]))));
            out += (c < 16 ? '0' : '') + c.toString(16);
        }
        return out;
    }

    // The mapping is the one documented in PDM-DESIGN-LANGUAGE.md section 11.4.
    var COLOR_MAP = {
        'editor.background': '--pwt-color-background',
        'editor.foreground': '--pwt-color',
        'editorWidget.background': '--pwt-color-surface',
        'editorWidget.border': '--pwt-color-border',
        'editorGroup.border': '--pwt-color-border',
        // Deviation from the doc's table: --pwt-color-neutral-alt is a *surface* token
        // (a near-white in light mode, a near-black in dark), so as a foreground it is
        // invisible in both. The dimmed-text role token is its `on-` partner.
        'editorLineNumber.foreground': '--pwt-color-on-neutral-alt',
        'editorLineNumber.activeForeground': '--pwt-color-on-neutral',
        'editorCursor.foreground': '--pwt-color-primary',
        'editor.selectionBackground': '--pwt-color-primary-container',
        'editorError.foreground': '--pwt-color-error',
        'editorWarning.foreground': '--pwt-color-warning',
        'editorInfo.foreground': '--pwt-color-primary',
        focusBorder: '--pwt-color-focus',
    };

    function themeName(dark) {
        return dark ? 'pve-meta-dark' : 'pve-meta-light';
    }

    // Light/dark is a class on <html> (pwt-dark-mode); "auto" in light adds no class at
    // all, so test for dark and default to light.
    function isDark(name) {
        if (name === 'dark' || name === 'light') {
            return name === 'dark';
        }
        return document.documentElement.classList.contains('pwt-dark-mode');
    }

    function defineTheme(monaco, el, dark) {
        var style = window.getComputedStyle(el);
        var colors = {};
        Object.keys(COLOR_MAP).forEach(function (key) {
            var value = hex(cssVar(style, COLOR_MAP[key]));
            if (value) {
                colors[key] = value;
            }
        });
        monaco.editor.defineTheme(themeName(dark), {
            base: dark ? 'vs-dark' : 'vs',
            inherit: true,
            rules: [],
            colors: colors,
        });
        return themeName(dark);
    }

    function anyElement() {
        var id = Object.keys(editors)[0];
        return id ? editors[id].el : document.body;
    }

    // --- public API --------------------------------------------------------

    function mount(el, opts) {
        opts = opts || {};
        var id = opts.id || 'pve-meta-monaco-' + nextId++;
        var entry = (editors[id] = {
            el: el,
            editor: null,
            diff: null,
            onChange: null,
            value: opts.value || '',
            readOnly: !!opts.readOnly,
        });

        loadMonaco()
            .then(function (monaco) {
                if (editors[id] !== entry || entry.disposed) {
                    return;
                }
                var style = window.getComputedStyle(el);
                var dark = isDark(opts.theme);
                entry.editor = monaco.editor.create(el, {
                    value: entry.value,
                    language: opts.language || 'yaml',
                    theme: defineTheme(monaco, el, dark),
                    readOnly: entry.readOnly,
                    automaticLayout: true,
                    minimap: { enabled: false },
                    scrollBeyondLastLine: false,
                    renderLineHighlight: 'line',
                    tabSize: 2,
                    insertSpaces: true,
                    // A code editor is monospace, whatever the page's sans stack is;
                    // 'monospace' is pwt's own $pwt-font-monospace (_theme_common.scss).
                    // The size follows the page so the editor reads at pwt's body scale.
                    fontFamily: 'monospace',
                    fontSize: parseFloat(style.fontSize) || 13,
                });
                entry.editor.onDidChangeModelContent(function () {
                    entry.value = entry.editor.getValue();
                    if (entry.onChange) {
                        entry.onChange(entry.value);
                    }
                });
            })
            .catch(function (err) {
                log('mount failed', err);
            });

        return id;
    }

    function mountDiff(el, original, modified) {
        var id = 'pve-meta-monaco-diff-' + nextId++;
        var entry = (editors[id] = { el: el, editor: null, diff: null });

        loadMonaco()
            .then(function (monaco) {
                if (editors[id] !== entry || entry.disposed) {
                    return;
                }
                var dark = isDark();
                entry.diff = monaco.editor.createDiffEditor(el, {
                    theme: defineTheme(monaco, el, dark),
                    readOnly: true,
                    renderSideBySide: true,
                    automaticLayout: true,
                    minimap: { enabled: false },
                    scrollBeyondLastLine: false,
                    fontFamily: 'monospace',
                });
                entry.diff.setModel({
                    original: monaco.editor.createModel(original || '', 'yaml'),
                    modified: monaco.editor.createModel(modified || '', 'yaml'),
                });
            })
            .catch(function (err) {
                log('diff mount failed', err);
            });

        return id;
    }

    function setValue(id, text) {
        var entry = editors[id];
        if (!entry) {
            return;
        }
        entry.value = text;
        if (entry.editor && entry.editor.getValue() !== text) {
            // setValue() resets the undo stack but keeps the viewport; that is what we
            // want for a Reload / view switch.
            entry.editor.setValue(text);
        }
    }

    function setReadOnly(id, readOnly) {
        var entry = editors[id];
        if (!entry) {
            return;
        }
        entry.readOnly = !!readOnly;
        if (entry.editor) {
            entry.editor.updateOptions({ readOnly: entry.readOnly });
        }
    }

    function onChange(id, cb) {
        var entry = editors[id];
        if (entry) {
            entry.onChange = cb;
        }
    }

    // monaco.editor.setTheme() is global: it repaints every editor on the page, the diff
    // editor in the dialog included. The colors themselves are read from the --pwt-*
    // custom properties, which only carry their new values once pwt's ThemeLoader has
    // swapped the stylesheet - and it does that asynchronously, after the
    // 'pwt-theme-changed' event that brought us here. So apply the theme now (instant
    // feedback for a pure prefers-color-scheme flip, where no stylesheet moves) and again
    // once the next frames and a short settle window have passed; a token makes sure only
    // the newest switch keeps repainting.
    var themeToken = 0;

    function setTheme(name) {
        if (!monacoPromise) {
            return;
        }
        var token = ++themeToken;
        var apply = function () {
            if (token !== themeToken) {
                return;
            }
            monacoPromise
                .then(function (monaco) {
                    if (token !== themeToken) {
                        return;
                    }
                    monaco.editor.setTheme(defineTheme(monaco, anyElement(), isDark(name)));
                })
                .catch(function (err) {
                    log('theme update failed', err);
                });
        };

        apply();
        window.requestAnimationFrame(function () {
            window.requestAnimationFrame(apply);
        });
        window.setTimeout(apply, 250);
    }

    function dispose(id) {
        var entry = editors[id];
        if (!entry) {
            return;
        }
        entry.disposed = true;
        delete editors[id];
        if (entry.diff) {
            var model = entry.diff.getModel();
            // Detach first: monaco's standalone diff editor keeps its {original,
            // modified} pair (and stays in monaco.editor.getDiffEditors()) after
            // dispose(), so a disposed widget would otherwise still hold two disposed
            // models alive.
            entry.diff.setModel(null);
            entry.diff.dispose();
            if (model) {
                model.original.dispose();
                model.modified.dispose();
            }
        }
        if (entry.editor) {
            var m = entry.editor.getModel();
            entry.editor.dispose();
            if (m) {
                m.dispose();
            }
        }
    }

    window.pveMetaMonaco = {
        mount: mount,
        mountDiff: mountDiff,
        setValue: setValue,
        setReadOnly: setReadOnly,
        setTheme: setTheme,
        onChange: onChange,
        dispose: dispose,
    };
})();
