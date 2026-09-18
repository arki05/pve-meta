// ---------------------------------------------------------------------------
// The core: pve-meta-core, built for the browser (crates/pve-meta-wasm).
//
// The server's own code answers the questions this editor needs: how a document
// reads and dumps, which key names are legal, which prefix governs a path, what a
// schema makes of a value, and what a set of staged edits does to a document.
// Below it: `Codec`, a stateless face over the core's functions; `Shape` and
// `EditSet`, objects that own their inputs (a Shape its listing and tags, an
// EditSet its list) and whose methods are the core's -- so a call site reads as
// the concept and not as a string passed to `call`, and what a Shape derives is
// computed once.
//
// The ABI is four exports and a JSON document each way (see the crate's own
// doc comment): `pm_alloc`/`pm_free` for the request, `pm_call` to run it, and
// `pm_output` for the response. No wasm-bindgen, no generated glue, no build
// step beyond `cargo build --target wasm32-unknown-unknown`.
//
// Loaded lazily on first use like Monaco, and `attach`ed directly by the offline
// test harness, which instantiates the same `.wasm` the package ships.
// ---------------------------------------------------------------------------

PVE.meta.CoreError = function (err) {
    this.name = 'CoreError';
    this.message = err.message;
    // Where the parser stopped, 1-based, when it knows -- the text editor's marker.
    this.line = err.line;
    this.column = err.column;
};
PVE.meta.CoreError.prototype = Object.create(Error.prototype);
PVE.meta.CoreError.prototype.constructor = PVE.meta.CoreError;

PVE.meta.Core = {
    SRC: '/pve2/js/pve-meta-extjs/pve-meta-core.wasm',
    ABI: 2,
    exports: null,
    promise: null,

    load: function () {
        let me = PVE.meta.Core;
        if (!me.promise) {
            // `instantiateStreaming` compiles as the bytes arrive but insists on
            // `Content-Type: application/wasm`, which pveproxy's static file
            // table may or may not know; the buffered path takes whatever type
            // it was given. Try the fast one, fall back to the sure one.
            let buffered = () =>
                fetch(me.SRC)
                    .then((r) => (r.ok ? r.arrayBuffer() : Promise.reject(new Error(r.status + ' ' + me.SRC))))
                    .then((bytes) => WebAssembly.instantiate(bytes, {}));
            let streaming = () =>
                typeof WebAssembly.instantiateStreaming === 'function'
                    ? WebAssembly.instantiateStreaming(fetch(me.SRC), {}).catch(buffered)
                    : buffered();
            me.promise = me.exports
                ? Promise.resolve(me)
                : streaming().then(function (result) {
                      me.attach(result.instance);
                      return me;
                  });
        }
        return me.promise;
    },

    // Hand over an instantiated module. The harness does this with the file from
    // the build tree; the browser does it through `load`.
    attach: function (instance) {
        let me = PVE.meta.Core;
        let ex = instance.exports;
        if (typeof ex.pm_abi !== 'function' || ex.pm_abi() !== me.ABI) {
            throw new Error('pve-meta core: ABI mismatch (wanted ' + me.ABI + ')');
        }
        me.exports = ex;
        me.encoder = new TextEncoder();
        me.decoder = new TextDecoder();
    },

    loaded: function () {
        return !!PVE.meta.Core.exports;
    },

    // One request: `{fn, args}` in, `ok` out, or a CoreError from `err`. Every
    // argument is a JSON value already -- a document, a path string, a listing --
    // which is why this can be a dozen lines and needs no generated bindings.
    call: function (name, ...args) {
        let me = PVE.meta.Core;
        let ex = me.exports;
        if (!ex) {
            throw new Error(gettext('The pve-meta core is not loaded'));
        }
        let bytes = me.encoder.encode(JSON.stringify({ fn: name, args: args }));
        let ptr = ex.pm_alloc(bytes.length);
        new Uint8Array(ex.memory.buffer, ptr, bytes.length).set(bytes);
        let len;
        try {
            len = ex.pm_call(ptr, bytes.length);
        } finally {
            ex.pm_free(ptr, bytes.length);
        }
        // `memory.buffer` afresh: the call may have grown the memory, which
        // detaches any earlier view of it.
        let text = me.decoder.decode(new Uint8Array(ex.memory.buffer, ex.pm_output(), len));
        let res = JSON.parse(text);
        if (res.err !== undefined) {
            throw new PVE.meta.CoreError(res.err);
        }
        return res.ok;
    },
};

