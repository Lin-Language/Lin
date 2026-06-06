use crate::string::{LinString, lin_string_from_bytes};
use crate::tagged::{TaggedVal, TAG_OBJECT, TAG_STR, TAG_ARRAY, TAG_NULL, alloc_tagged};
use crate::fs::{make_error_tagged, resolve_lin_str};

/// Bridge a Lin data value to a `serde_json::Value` for use as a minijinja render
/// context. `tagged_to_json` expects a `TaggedVal*`; the foreign `{}` arg may instead
/// arrive as a bare `LinObject*` (the param does not force boxing in every code path),
/// so detect the tag and synthesise a stack `TaggedVal(TAG_OBJECT)` for a raw object
/// pointer. `tagged_to_json` only reads tag+payload — it neither frees nor takes
/// ownership, so the synthesised stack value is safe.
unsafe fn data_to_context(data: *const u8) -> serde_json::Value {
    if data.is_null() {
        return serde_json::Value::Object(serde_json::Map::new());
    }
    let tag = *data;
    if tag == TAG_OBJECT || tag == TAG_STR || tag == TAG_ARRAY || tag == TAG_NULL {
        // Already a TaggedVal*.
        crate::json::tagged_to_json(data)
    } else {
        // Bare LinObject*: wrap it so tagged_to_json sees a TAG_OBJECT.
        let tv = TaggedVal { tag: TAG_OBJECT, _pad: [0; 7], payload: data as u64 };
        crate::json::tagged_to_json(&tv as *const TaggedVal as *const u8)
    }
}

/// Box a successfully-rendered string into a `TAG_STR` tagged value.
///
/// `lin_string_from_bytes` returns an OWNED (+1) string; that reference transfers
/// into the tagged box (the lowering contract releases it once when the value is
/// dropped). No borrowed-return path is introduced.
unsafe fn ok_string(out: &str) -> *mut u8 {
    let ls = lin_string_from_bytes(out.as_ptr(), out.len() as u32);
    alloc_tagged(TAG_STR, ls as u64)
}

/// Render a Jinja-style template **string** against a data object.
///
/// Templating is delegated to the `minijinja` crate (ADR-048). Syntax is Jinja:
/// `{{ var }}` substitutions, `{% for %}` / `{% if %}` control flow, and the
/// standard filter set. Undefined / missing variables render as the empty string
/// (minijinja's default `Undefined` behaviour — strict mode is NOT enabled).
///
/// This entry point takes the template as an in-memory string and registers a single
/// anonymous template, so `{% extends %}` / `{% include %}` have nothing to resolve
/// against. For layout/inheritance, use `lin_template_render_path` (ADR-048), which
/// loads from a directory.
///
/// Signature (C-ABI): `(template: *const LinString, data: *const u8) -> *mut u8`
/// where the result is a Lin tagged value (`Json`): a `TAG_STR` wrapping the
/// rendered string on success, or an `Error` object `{type:error,message}` on a
/// render/parse failure (faithful to Lin's `is Error` convention — see ADR-048).
///
/// `data` may be a raw `LinObject*` or a `TaggedVal*(TAG_OBJECT)`; both are
/// accepted and bridged to a `serde_json::Value::Object` via `tagged_to_json`.
#[no_mangle]
pub unsafe extern "C" fn lin_template_render(
    template: *const LinString,
    data: *const u8,
) -> *mut u8 {
    let src = (*template).as_str();
    let ctx = data_to_context(data);

    // Render with a fresh environment. Undefined variables stay empty (default).
    let mut env = minijinja::Environment::new();
    if let Err(e) = env.add_template("template", src) {
        return make_error_tagged(&format!("template syntax error: {}", e));
    }
    let tmpl = match env.get_template("template") {
        Ok(t) => t,
        Err(e) => return make_error_tagged(&format!("template error: {}", e)),
    };
    match tmpl.render(&ctx) {
        Ok(out) => ok_string(&out),
        Err(e) => make_error_tagged(&format!("template render error: {}", e)),
    }
}

/// Render a Jinja-style template **file** against a data object, with full layout
/// support (ADR-048).
///
/// Unlike `lin_template_render`, this resolves the template through minijinja's
/// `path_loader` rooted at the template file's own directory. That makes template
/// inheritance work: a page template can `{% extends "base.jinja" %}` and fill
/// `{% block %}`s, or pull in partials with `{% include "_nav.jinja" %}`, and the
/// referenced files are loaded by name from the same directory.
///
/// The loaded template name is the file's basename (e.g. `page.jinja`); referenced
/// templates use their basename too (they live alongside it). Loading is lazy — only
/// templates actually referenced are read from disk.
///
/// Signature (C-ABI): `(path: *const u8, data: *const u8) -> *mut u8`. `path` may be a
/// bare `LinString*` or a `TaggedVal*(Str)`. Result is the same `Json` convention as
/// `lin_template_render`: `TAG_STR` on success, `{type:error,message}` on a missing
/// file or a syntax/render error.
#[no_mangle]
pub unsafe extern "C" fn lin_template_render_path(
    path: *const u8,
    data: *const u8,
) -> *mut u8 {
    let path_str = match resolve_lin_str(path) {
        Some(s) => s,
        None => return make_error_tagged("invalid UTF-8 template path"),
    };

    // Split into the loader root (directory) and the template name (basename). The
    // path_loader resolves `{% extends %}`/`{% include %}` names against this root, so
    // base layouts and partials are found next to the entry template.
    let (dir, name) = match path_str.rfind('/') {
        Some(i) => (&path_str[..i], &path_str[i + 1..]),
        None => (".", path_str.as_str()),
    };

    let ctx = data_to_context(data);

    let mut env = minijinja::Environment::new();
    env.set_loader(minijinja::path_loader(dir));
    let tmpl = match env.get_template(name) {
        Ok(t) => t,
        // A get_template error here is either "template not found" (missing file) or a
        // syntax error in the entry/parent template; both map onto the Error convention.
        Err(e) => return make_error_tagged(&format!("template error: {}", e)),
    };
    match tmpl.render(&ctx) {
        Ok(out) => ok_string(&out),
        Err(e) => make_error_tagged(&format!("template render error: {}", e)),
    }
}
