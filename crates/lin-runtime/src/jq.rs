//! `std/jq` runtime support: run a jq filter program against a Json value using the
//! pure-Rust `jaq` engine. Outputs are collected into a Json array.

use crate::fs::{make_error_tagged, resolve_lin_str};
use crate::json::{json_to_tagged, tagged_to_json};

use jaq_core::load::{Arena, File, Loader};
use jaq_core::{data, unwrap_valr, Compiler, Ctx, Vars};
use jaq_json::Val;

/// Run a jq filter (`filter`) over a Json input (`input`).
///
/// Returns a tagged Json *array* of all output values produced by the filter, or the canonical
/// `{ "type": "error", "message": ... }` value on a compile/parse/runtime error.
#[no_mangle]
pub unsafe extern "C" fn lin_jq_query(input: *const u8, filter: *const u8) -> *mut u8 {
    let filter_src = match resolve_lin_str(filter) {
        Some(s) => s,
        None => return make_error_tagged("invalid jq filter string"),
    };

    // Convert the input tagged value to serde_json, then to a jaq `Val` via its JSON reader.
    let input_json = tagged_to_json(input);
    let input_bytes = match serde_json::to_vec(&input_json) {
        Ok(b) => b,
        Err(e) => return make_error_tagged(&format!("jq: cannot serialise input: {e}")),
    };
    let input_val = match jaq_json::read::parse_single(&input_bytes) {
        Ok(v) => v,
        Err(e) => return make_error_tagged(&format!("jq: cannot read input: {e}")),
    };

    // Assemble the jq standard library (core + std + json builtins).
    let defs = jaq_core::defs()
        .chain(jaq_std::defs())
        .chain(jaq_json::defs());
    let funs = jaq_core::funs()
        .chain(jaq_std::funs())
        .chain(jaq_json::funs());

    let arena = Arena::default();
    let loader = Loader::new(defs);
    let program = File {
        code: filter_src.as_str(),
        path: (),
    };

    let modules = match loader.load(&arena, program) {
        Ok(m) => m,
        Err(errs) => return make_error_tagged(&format!("jq: parse error: {errs:?}")),
    };

    let filter = match Compiler::default().with_funs(funs).compile(modules) {
        Ok(f) => f,
        Err(errs) => return make_error_tagged(&format!("jq: compile error: {errs:?}")),
    };

    let ctx = Ctx::<data::JustLut<Val>>::new(&filter.lut, Vars::new([]));
    let mut outputs: Vec<serde_json::Value> = Vec::new();
    for res in filter.id.run((ctx, input_val)).map(unwrap_valr) {
        match res {
            Ok(v) => {
                // jaq `Val`'s Display is compact JSON; round-trip it back through serde_json.
                let s = v.to_string();
                match serde_json::from_str::<serde_json::Value>(&s) {
                    Ok(j) => outputs.push(j),
                    Err(e) => return make_error_tagged(&format!("jq: cannot read output: {e}")),
                }
            }
            Err(e) => return make_error_tagged(&format!("jq: {e}")),
        }
    }

    json_to_tagged(&serde_json::Value::Array(outputs))
}
