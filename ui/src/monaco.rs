//! Rust bindings for the Monaco glue in `js/pve-meta-monaco.js`.
//!
//! `window.pveMetaMonaco` is a plain global defined by a `<script>` in `index.html`, so
//! the externs below name it as a JS namespace rather than importing a module — fewer
//! build moving parts, and the same shape the design doc's option 2 describes
//! (`docs/design/PDM-DESIGN-LANGUAGE.md` §11.3).
//!
//! Mounting happens from `Component::rendered`, the pattern pwt itself uses for raw DOM
//! work (§11.2). Every instance is disposed the moment the page stops showing what it was
//! mounted for — the component is dropped, the view or document switches, the diff dialog
//! closes; Monaco leaks a `ResizeObserver` and a model otherwise, and a buffer left over
//! from another view is a buffer that can be applied to the wrong path.

use js_sys::{Object, Reflect};
use wasm_bindgen::prelude::*;
use web_sys::Element;

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = pveMetaMonaco, js_name = mount)]
    fn js_mount(el: &Element, opts: &JsValue) -> String;

    #[wasm_bindgen(js_namespace = pveMetaMonaco, js_name = mountDiff)]
    fn js_mount_diff(el: &Element, original: &str, modified: &str, language: &str) -> String;

    #[wasm_bindgen(js_namespace = pveMetaMonaco, js_name = setValue)]
    fn js_set_value(id: &str, text: &str);

    #[wasm_bindgen(js_namespace = pveMetaMonaco, js_name = setLanguage)]
    fn js_set_language(id: &str, language: &str);

    #[wasm_bindgen(js_namespace = pveMetaMonaco, js_name = setReadOnly)]
    fn js_set_read_only(id: &str, read_only: bool);

    #[wasm_bindgen(js_namespace = pveMetaMonaco, js_name = setTheme)]
    fn js_set_theme(name: &str);

    #[wasm_bindgen(js_namespace = pveMetaMonaco, js_name = onChange)]
    fn js_on_change(id: &str, callback: &JsValue);

    #[wasm_bindgen(js_namespace = pveMetaMonaco, js_name = dispose)]
    fn js_dispose(id: &str);
}

/// Options for [`mount`].
pub struct MountOptions<'a> {
    /// Initial editor content.
    pub value: &'a str,
    /// Monaco language id (`yaml` here — from Monaco's basic-languages, no schemas).
    pub language: &'a str,
    /// Whether the caller may edit this view.
    pub read_only: bool,
    /// `dark` or `light`, from pwt's theme state.
    pub theme: &'a str,
}

/// Create an editor inside `el` and return its id.
pub fn mount(el: &Element, options: &MountOptions) -> String {
    let opts = Object::new();
    set(&opts, "value", &JsValue::from_str(options.value));
    set(&opts, "language", &JsValue::from_str(options.language));
    set(&opts, "readOnly", &JsValue::from_bool(options.read_only));
    set(&opts, "theme", &JsValue::from_str(options.theme));
    js_mount(el, &opts)
}

/// Create a read-only side-by-side diff editor inside `el` and return its id.
pub fn mount_diff(el: &Element, original: &str, modified: &str, language: &str) -> String {
    js_mount_diff(el, original, modified, language)
}

/// Replace the editor's text (a no-op when it already matches).
pub fn set_value(id: &str, text: &str) {
    js_set_value(id, text);
}

/// Re-tag the editor's model, so the YAML/JSON toggle highlights what it shows.
pub fn set_language(id: &str, language: &str) {
    js_set_language(id, language);
}

/// Enable or disable editing.
pub fn set_read_only(id: &str, read_only: bool) {
    js_set_read_only(id, read_only);
}

/// Re-derive Monaco's theme from the `--pwt-*` variables for `dark` or `light`.
pub fn set_theme(dark: bool) {
    js_set_theme(if dark { "dark" } else { "light" });
}

/// Install the change listener. The returned closure must be kept alive for as long as
/// the editor exists.
pub fn on_change(id: &str, callback: impl Fn(String) + 'static) -> Closure<dyn Fn(String)> {
    let closure = Closure::wrap(Box::new(callback) as Box<dyn Fn(String)>);
    js_on_change(id, closure.as_ref());
    closure
}

/// Destroy the editor and its model.
pub fn dispose(id: &str) {
    js_dispose(id);
}

fn set(obj: &Object, key: &str, value: &JsValue) {
    let _ = Reflect::set(obj, &JsValue::from_str(key), value);
}
