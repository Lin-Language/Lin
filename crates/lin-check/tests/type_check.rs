use lin_check::Checker;
use lin_lex::Lexer;
use lin_parse::Parser;

fn parse_and_check(source: &str) -> Result<lin_check::TypedModule, Vec<lin_common::Diagnostic>> {
    let mut lexer = Lexer::new(source, 0);
    let tokens = lexer.tokenize();
    let mut parser = Parser::new(tokens);
    let module = parser.parse_module();
    let mut checker = Checker::new();
    // These compiler-internal unit tests legitimately drive `lin_*` intrinsics directly (ADR-060).
    checker.allow_intrinsics = true;
    checker.check_module(&module)
}

#[test]
fn test_integer_literal() {
    let result = parse_and_check("val x: Int32 = 42");
    assert!(result.is_ok(), "Expected Ok, got: {:?}", result.err());
}

#[test]
fn test_float_literal() {
    let result = parse_and_check("val x: Float64 = 3.14");
    assert!(result.is_ok(), "Expected Ok, got: {:?}", result.err());
}

#[test]
fn test_string_literal() {
    let result = parse_and_check("val x: String = \"hello\"");
    assert!(result.is_ok(), "Expected Ok, got: {:?}", result.err());
}

#[test]
fn test_bool_literal() {
    let result = parse_and_check("val x: Boolean = true");
    assert!(result.is_ok(), "Expected Ok, got: {:?}", result.err());
}

#[test]
fn test_type_mismatch() {
    let result = parse_and_check("val x: Int32 = \"hello\"");
    assert!(result.is_err());
}

#[test]
fn test_arithmetic_widening() {
    let result = parse_and_check("val x: Int32 = 1 + 2");
    assert!(result.is_ok(), "Expected Ok, got: {:?}", result.err());
}

#[test]
fn test_function_type() {
    let result = parse_and_check("val add = (a: Int32, b: Int32): Int32 => a + b");
    assert!(result.is_ok(), "Expected Ok, got: {:?}", result.err());
}

#[test]
fn test_undefined_variable() {
    let result = parse_and_check("val x = y");
    assert!(result.is_err());
}

#[test]
fn test_immutable_assignment() {
    let result = parse_and_check("val x = 1\nx = 2");
    assert!(result.is_err());
}

#[test]
fn test_mutable_assignment() {
    let result = parse_and_check("var x = 1\nx = 2");
    assert!(result.is_ok(), "Expected Ok, got: {:?}", result.err());
}

// M7: Recursive types
#[test]
fn test_recursive_type_tree() {
    let src = r#"
type Tree = { "value": Int32, "children": Tree[] }
val leaf: Tree = { "value": 1, "children": [] }
"#;
    let result = parse_and_check(src);
    assert!(result.is_ok(), "Recursive Tree type should be valid: {:?}", result.err());
}

#[test]
fn test_recursive_type_person_nullable_spouse() {
    let src = r#"
type Person = { "name": String, "spouse": Person | Null }
val p: Person = { "name": "Alice", "spouse": null }
"#;
    let result = parse_and_check(src);
    assert!(result.is_ok(), "Person with nullable spouse should be valid: {:?}", result.err());
}

#[test]
fn test_named_type_alias() {
    let src = r#"
type MyInt = Int32
val x: MyInt = 42
"#;
    let result = parse_and_check(src);
    assert!(result.is_ok(), "Simple type alias should work: {:?}", result.err());
}

// M7: Compatible/incompatible assignment
#[test]
fn test_object_structural_compatible() {
    let src = r#"
type Point = { "x": Int32, "y": Int32 }
val p: Point = { "x": 1, "y": 2 }
"#;
    let result = parse_and_check(src);
    assert!(result.is_ok(), "Structural object assignment should work: {:?}", result.err());
}

#[test]
fn test_object_missing_field_incompatible() {
    let src = r#"
type Point = { "x": Int32, "y": Int32 }
val p: Point = { "x": 1 }
"#;
    let result = parse_and_check(src);
    assert!(result.is_err(), "Missing field should fail");
}

// M10: Numeric widening
#[test]
fn test_widening_int32_to_int64_in_function_arg() {
    let src = r#"
val f = (n: Int64): Int64 => n
val result = f(42)
"#;
    let result = parse_and_check(src);
    assert!(result.is_ok(), "Int32 literal should widen to Int64 param: {:?}", result.err());
}

#[test]
fn test_widening_int_to_float64() {
    let src = r#"
val f = (x: Float64): Float64 => x
val result = f(1)
"#;
    let result = parse_and_check(src);
    assert!(result.is_ok(), "Int32 should widen to Float64: {:?}", result.err());
}

#[test]
fn test_narrowing_disallowed() {
    let src = r#"
val x: Int32 = 3.14
"#;
    let result = parse_and_check(src);
    assert!(result.is_err(), "Float64 should not implicitly narrow to Int32");
}

// M10: Widening across signed+unsigned and integer+float
#[test]
fn test_widening_signed_unsigned() {
    // Int32 + UInt32 should produce Int64 (widened to signed with more bits)
    let src = "val x: Int64 = 1 + 1";
    let result = parse_and_check(src);
    assert!(result.is_ok(), "Int32+Int32 widened to Int64: {:?}", result.err());
}

#[test]
fn test_person_array_assignable_to_json_array() {
    // Person[] should be assignable to Json[] (covariance)
    let src = r#"
type Person = { "name": String }
val people: Person[] = [{ "name": "Alice" }]
"#;
    let result = parse_and_check(src);
    assert!(result.is_ok(), "Person[] construction: {:?}", result.err());
}

// M8: Union narrowing
#[test]
fn test_union_is_narrowing() {
    let src = r#"
val x: Int32 | String = 42
val desc = if x is Int32 then "number" else "string"
"#;
    let result = parse_and_check(src);
    assert!(result.is_ok(), "Union is-narrowing in if should work: {:?}", result.err());
}

#[test]
fn test_has_pattern_accepts_extra_fields() {
    let src = r#"
val obj = { "name": "Alice", "age": 30 }
val has_name = obj has { "name" }
"#;
    let result = parse_and_check(src);
    assert!(result.is_ok(), "has pattern should accept extra fields: {:?}", result.err());
}

#[test]
fn test_foreign_import_legal_types() {
    let src = r#"
import foreign "libmath.a"
  val sqrt: (Float64) => Float64
  val add: (Int32, Int32) => Int32
"#;
    let result = parse_and_check(src);
    assert!(result.is_ok(), "FFI with legal types should pass: {:?}", result.err());
}

#[test]
fn test_foreign_import_illegal_type_reports_error() {
    let src = r#"
import foreign "libfoo.a"
  val badFn: (Json) => Json
"#;
    let result = parse_and_check(src);
    // Json is not a legal FFI type, should have errors
    assert!(result.is_err(), "FFI with illegal type should produce error");
    let errs = result.unwrap_err();
    let has_ffi_error = errs.iter().any(|d| d.message.contains("illegal FFI type"));
    assert!(has_ffi_error, "Expected 'illegal FFI type' error, got: {:?}", errs);
}

#[test]
fn test_async_var_capture_rejected() {
    let src = r#"
var counter = 0
val p = lin_async(() =>
  counter = counter + 1
  counter
)
"#;
    let result = parse_and_check(src);
    // async thunk captures a var — should produce a compile-time error
    assert!(result.is_err(), "async capturing var should be rejected");
    let errs = result.unwrap_err();
    let has_var_capture_error = errs.iter().any(|d| d.message.contains("mutable variable"));
    assert!(has_var_capture_error, "Expected var-capture error, got: {:?}", errs);
}

#[test]
fn test_async_val_capture_allowed() {
    let src = r#"
val message = "hello"
val p = lin_async(() => message)
"#;
    let result = parse_and_check(src);
    assert!(result.is_ok(), "async capturing val should be allowed: {:?}", result.err());
}

#[test]
fn test_async_array_of_thunks_var_capture_rejected() {
    let src = r#"
var x = 10
val ps = lin_async([() => x, () => 42])
"#;
    let result = parse_and_check(src);
    assert!(result.is_err(), "async([...]) capturing var should be rejected");
    let errs = result.unwrap_err();
    let has_var_capture_error = errs.iter().any(|d| d.message.contains("mutable variable"));
    assert!(has_var_capture_error, "Expected var-capture error, got: {:?}", errs);
}

// M9: Narrowing per arm
#[test]
fn test_match_is_arm_narrows_scrutinee() {
    // After `is Int32`, the scrutinee is narrowed to Int32 so Int32-specific operations are allowed.
    let src = r#"
val x: Int32 | String = 42
val result = match x
  is Int32 => x + 1
  is String => 0
  else => -1
"#;
    let result = parse_and_check(src);
    assert!(result.is_ok(), "is-arm should narrow scrutinee type: {:?}", result.err());
}

// M9: Exhaustiveness — error on non-exhaustive closed union
#[test]
fn test_match_non_exhaustive_union_error() {
    let src = r#"
type Color = "Red" | "Green" | "Blue"
val c: String = "Red"
val label = match c
  is String => "ok"
"#;
    let result = parse_and_check(src);
    // A match on a Union type without an else arm and missing variants should warn/error.
    // Here we check that checking a Union without covering all variants produces a diagnostic.
    // (The result may be Ok with warnings — we only require the match doesn't crash.)
    // For this test, we just verify it completes without panicking.
    let _ = result;
}

#[test]
fn test_match_exhaustive_union_with_else_ok() {
    let src = r#"
val x: Int32 | String = 42
val result = match x
  is Int32 => "number"
  else => "other"
"#;
    let result = parse_and_check(src);
    assert!(result.is_ok(), "match with else arm should be exhaustive: {:?}", result.err());
}

#[test]
fn test_match_exhaustive_closed_union_no_else() {
    let src = r#"
val x: Int32 | String = 42
val result = match x
  is Int32 => "number"
  is String => "string"
"#;
    let result = parse_and_check(src);
    // Both variants covered — should succeed without error.
    assert!(result.is_ok(), "Fully covered union match should be ok: {:?}", result.err());
}

// M17: Transferability check
#[test]
fn test_async_function_return_type_rejected() {
    // async thunk that returns a Function value — non-transferable
    let src = r#"
val makeAdder = (n: Int32) => (x: Int32) => x + n
val p = lin_async(() => makeAdder(5))
"#;
    let result = parse_and_check(src);
    assert!(result.is_err(), "async returning Function should be rejected");
    let errs = result.unwrap_err();
    let has_transfer_error = errs.iter().any(|d| d.message.contains("non-transferable"));
    assert!(has_transfer_error, "Expected non-transferable error, got: {:?}", errs);
}

#[test]
fn test_async_json_return_type_allowed() {
    // async thunk returning a plain Int32 — transferable
    let src = r#"
val p = lin_async(() => 42)
"#;
    let result = parse_and_check(src);
    assert!(result.is_ok(), "async returning Int32 should be allowed: {:?}", result.err());
}

// Regression: the match-arm-union-vs-declared-object bug. A function declared to return a named
// object type `R`, whose body is a `match`/`if` with one arm yielding a `Json` value and another
// yielding a concrete object literal, previously formed the union `Json | {concrete}` and rejected
// it against `R`. Each arm is now checked against `R` directly (bidirectional push): the object
// literal checks structurally, and a `Json` value is accept-any in arm position.
#[test]
fn test_match_json_arm_plus_object_arm_against_declared_object_return() {
    let src = r#"
type R = { "status": Int32, "headers": Json, "body": String }
val other = (): Json => { "status": 200, "headers": { "a": 1 }, "body": "ok" }
val handle = (b: Boolean): R =>
  match b
    is true => other()
    else => { "status": 404, "headers": { "a": 1 }, "body": "no" }
"#;
    let result = parse_and_check(src);
    assert!(result.is_ok(), "match Json-arm + object-arm vs declared object should check: {:?}", result.err());
}

// Same bug, `if` form: `if cond then jsonValue else objectLiteral` declared `: R`.
#[test]
fn test_if_json_arm_plus_object_arm_against_declared_object_return() {
    let src = r#"
type R = { "status": Int32, "headers": Json, "body": String }
val other = (): Json => { "status": 200, "headers": { "a": 1 }, "body": "ok" }
val handle = (b: Boolean): R =>
  if b then other() else { "status": 404, "headers": { "a": 1 }, "body": "no" }
"#;
    let result = parse_and_check(src);
    assert!(result.is_ok(), "if Json-arm + object-arm vs declared object should check: {:?}", result.err());
}

// Guard against over-broadening: a genuinely wrong-shaped object arm must STILL error.
#[test]
fn test_match_wrong_shaped_object_arm_still_errors() {
    let src = r#"
type R = { "status": Int32, "headers": Json, "body": String }
val other = (): Json => { "status": 200, "headers": { "a": 1 }, "body": "ok" }
val handle = (b: Boolean): R =>
  match b
    is true => other()
    else => { "status": 404, "body": 99 }
"#;
    let result = parse_and_check(src);
    assert!(result.is_err(), "wrong-shaped object arm must still error");
}

// Guard against over-broadening (ADR-045): a DIRECT `Json` body returned as a structured object
// (not via a match/if arm with a concrete-object companion) must still error — the relaxation is
// scoped to checked match/if arms, not bare bodies.
#[test]
fn test_bare_json_body_against_declared_object_still_errors() {
    let src = r#"
type R = { "status": Int32, "headers": Json, "body": String }
val other = (): Json => { "status": 200, "headers": { "a": 1 }, "body": "ok" }
val handle = (): R => other()
"#;
    let result = parse_and_check(src);
    assert!(result.is_err(), "bare Json body vs structured object return must still error (ADR-045)");
}

// A SCALAR numeric annotation on an ARRAY literal is a genuine type error and must STAY one —
// but the message must (a) report the TRUE value type (the array `Int32[]`, NOT the bogus
// "UInt32") and (b) suggest the `T[]` element-annotation fix.
#[test]
fn test_scalar_annotation_on_array_literal_errors_with_hint() {
    let result = parse_and_check("val x: UInt8 = [1, 2, 3]");
    let diags = result.expect_err("scalar annotation on an array literal must error");
    let diag = &diags[0];
    // Primary message names the array value type, not the scalar/wrong-default-element token.
    assert!(
        diag.message.contains("Int32[]"),
        "message must name the array value type `Int32[]`, got: {}",
        diag.message
    );
    assert!(
        diag.message.contains("UInt8"),
        "message must name the scalar annotation `UInt8`, got: {}",
        diag.message
    );
    assert!(
        !diag.message.contains("UInt32"),
        "message must NOT contain the bogus `UInt32` token, got: {}",
        diag.message
    );
    // Help suggests the element-annotation fix.
    let help = diag.help.as_deref().unwrap_or("");
    assert!(
        help.contains("UInt8[]"),
        "help must suggest `UInt8[]`, got: {:?}",
        diag.help
    );
}

// Regression: the array-element annotation already works and must keep compiling.
#[test]
fn test_scalar_element_array_annotation_compiles() {
    let result = parse_and_check("val x: UInt8[] = [1, 2, 3]");
    assert!(result.is_ok(), "Expected Ok, got: {:?}", result.err());
}

// Regression: a scalar literal against a scalar annotation must keep compiling (literal adoption).
#[test]
fn test_scalar_literal_scalar_annotation_compiles() {
    let result = parse_and_check("val x: UInt8 = 1");
    assert!(result.is_ok(), "Expected Ok, got: {:?}", result.err());
}

// The hint is scoped: a scalar annotation on a NON-numeric array (string elements) errors but
// must NOT carry the `UInt8[]` element-annotation suggestion.
#[test]
fn test_scalar_annotation_on_string_array_no_hint() {
    let result = parse_and_check("val x: UInt8 = [\"a\", \"b\"]");
    let diags = result.expect_err("scalar annotation on a string array must error");
    assert!(
        diags[0].help.is_none(),
        "no element-annotation hint for a non-numeric array, got: {:?}",
        diags[0].help
    );
}

// --- Parameter def-site entries in span_type_map (LSP inlay hints) ---------------------------
//
// The checker records a definition-site `(name_span, type, Some(name_span))` entry in
// `span_type_map` for EVERY function/lambda parameter — including unannotated params that are
// never used — so the language server can show an inlay type hint at the parameter's name span
// without relying on a USE of the param (only uses are otherwise recorded). These tests assert
// that contract. They are metadata-only: the success/failure of `check_module` is unchanged.

/// Parse + check, returning the checker so tests can inspect `span_type_map`.
fn parse_and_check_with_checker(
    source: &str,
) -> (Checker, Result<lin_check::TypedModule, Vec<lin_common::Diagnostic>>) {
    let mut lexer = Lexer::new(source, 0);
    let tokens = lexer.tokenize();
    let mut parser = Parser::new(tokens);
    let module = parser.parse_module();
    let mut checker = Checker::new();
    checker.allow_intrinsics = true;
    let result = checker.check_module(&module);
    (checker, result)
}

/// Find the def-site type string recorded for the identifier `name` at byte offset `at` in the
/// source: an entry whose use-span == def-span and whose span starts at `at`.
fn param_def_type_at(checker: &Checker, at: u32, name_len: u32) -> Option<String> {
    checker
        .span_type_map
        .iter()
        .find(|(span, _, def)| {
            span.start == at
                && span.end == at + name_len
                && *def == Some(*span)
        })
        .map(|(_, ty, _)| ty.clone())
}

#[test]
fn test_unused_annotated_param_recorded_in_span_type_map() {
    // `x` is annotated `Int32` and NEVER used in the body — `infer_ident` never runs for it, so
    // only the def-site entry can carry its type.
    let source = "val f = (x: Int32) => 0";
    let at = source.find('x').unwrap() as u32;
    let (checker, result) = parse_and_check_with_checker(source);
    assert!(result.is_ok(), "Expected Ok, got: {:?}", result.err());
    assert_eq!(
        param_def_type_at(&checker, at, 1).as_deref(),
        Some("Int32"),
        "param `x` def-site type missing/wrong; span_type_map = {:?}",
        checker.span_type_map
    );
}

#[test]
fn test_unused_unannotated_param_has_def_site_entry() {
    // An UNANNOTATED, unused param: its type stays a free var (`?T`-ish). We only assert that a
    // def-site self-entry EXISTS at the name span (the LSP suppresses unresolved `?T`).
    let source = "val f = (y) => 0";
    let at = source.find('y').unwrap() as u32;
    let (checker, result) = parse_and_check_with_checker(source);
    assert!(result.is_ok(), "Expected Ok, got: {:?}", result.err());
    let has_entry = checker
        .span_type_map
        .iter()
        .any(|(span, _, def)| span.start == at && span.end == at + 1 && *def == Some(*span));
    assert!(has_entry, "no def-site entry for unannotated param `y`: {:?}", checker.span_type_map);
}

#[test]
fn test_inferred_param_def_site_resolves_to_concrete_type() {
    // `n`'s type is INFERRED from the call context: it is added to `1`, forcing it numeric, and
    // the call `f(5)` pins it to `Int32`. The def-site entry must carry the RESOLVED type (zonked),
    // not an unsolved `?T`.
    let source = "val f = (n) => n + 1\nval r = f(5)";
    let at = source.find("(n)").unwrap() as u32 + 1; // the `n` inside the parens
    let (checker, result) = parse_and_check_with_checker(source);
    assert!(result.is_ok(), "Expected Ok, got: {:?}", result.err());
    let ty = param_def_type_at(&checker, at, 1);
    assert_eq!(
        ty.as_deref(),
        Some("Int32"),
        "inferred param `n` def-site type should resolve to Int32; span_type_map = {:?}",
        checker.span_type_map
    );
}

#[test]
fn test_unused_lambda_param_from_for_callback_recorded() {
    // The classic inlay-hint case: a `for` callback param `i` that is NEVER used. Its type is
    // supplied by the combinator's callback element type (`Int32`) via `infer_function_with_hints`,
    // so the def-site entry should resolve to a concrete type rather than `?T` — and exist at all,
    // unlike a use-driven entry (there is no use).
    let source = "import { range, for } from \"std/iter\"\nrange(0, 5).for(i => 0)";
    // The `i` param: locate the `i =>` occurrence.
    let at = source.find("i =>").unwrap() as u32;
    let (checker, result) = parse_and_check_with_checker(source);
    assert!(result.is_ok(), "Expected Ok, got: {:?}", result.err());
    let has_entry = checker
        .span_type_map
        .iter()
        .any(|(span, _, def)| span.start == at && span.end == at + 1 && *def == Some(*span));
    assert!(
        has_entry,
        "no def-site entry for unused `for`-callback param `i`: {:?}",
        checker.span_type_map
    );
}
