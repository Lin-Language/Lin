# Implementation Plan: `fromJson` + Json→concrete cast-hole fix

Worktree: `/tmp/lin-fromjson` (branch `fromjson-and-json-cast`).
All file:line citations verified against the source on this branch.

---

## 1. Summary and locked design decisions

### Feature 1 — `fromJson`

- **Surface**: `T.fromJson(json)` (idiomatic) and `fromJson(T, json)` are equivalent.
  `x.f(y)` parses to `Expr::DotCall{receiver,method,args}` (lin-parse/src/ast.rs) and the
  checker desugars dot-calls to `f(x, y)` (lin-check/src/checker/call.rs `infer_dot_call`,
  ~line 277, comment "Desugar: receiver.method(args) -> method(receiver, args)"). So no
  parser change is needed — but the desugar in `infer_dot_call` types the *receiver* as a
  value first (`self.infer_expr(receiver)`, call.rs:295), and `Person` is not a value. So
  `fromJson` must be intercepted as a **checker special form** in BOTH `infer_call` and
  `infer_dot_call` BEFORE arg0/receiver is inferred as a value.
- **Signature semantics**: `fromJson(T, value: Json) -> T | Error`.
- **First-error-wins**: validation stops at the first structural mismatch and returns a single
  `Error` value. On full success it returns the **same pointer** (Json and typed values share
  the tagged representation — zero copy).
- **Number policy** (driven by the *target field* type):
  - Target `Int32`/any integer family → require the JSON number to be integral and within the
    target's range; reject `3.14`, reject overflow.
  - Target `Float64`/`Float32` → accept any JSON number, coerce via `tagged_as_f64`
    (crates/lin-runtime/src/tagged.rs:373).
  - Target unconstrained (`Json`/`Number`) → accept any number as-is (no narrowing).
- **Special-form recognition**: recognised by **surface name `fromJson` at the call site**,
  NOT by an imported `lin_*` alias. Rationale below (§2, Step 1) — unlike `print`/`for`, the
  arg cannot be expressed as a value, so a `lin_from_json` stdlib wrapper cannot exist. The
  stdlib `std/json` module exports a clean `fromJson` *type alias of the special form*: users
  still write `import { fromJson } from "std/json"`, but the checker matches the name and the
  type-namespace receiver, regardless of import (see Step 1 for the precise recognition rule
  and the name-collision guard).
- **Validator strategy (RECOMMENDED): a single generic runtime interpreter driven by a
  compile-time-emitted schema descriptor.** Three options were weighed (§5). There is NO
  runtime type info, so the descriptor (a compact byte/word array describing the target type
  tree) is emitted by codegen as a static global and passed as data to one runtime function
  `lin_from_json(value, descriptor_ptr) -> TaggedVal*` that walks value+descriptor in lockstep.
  This keeps recursion/cycles trivial (descriptor is a flat table with indices; named-type
  back-edges are just indices into the table), keeps generated code O(1) per `fromJson` site,
  and reuses the existing tag/unbox runtime primitives.
- **Error value shape**: reuse the existing runtime error representation —
  `TaggedVal*(TAG_OBJECT)` wrapping `{ "type": "error", "message": String }`
  (crates/lin-runtime/src/fs.rs:23 `make_error_obj`). The validator extends it with a
  `"path"` field (a JSONPath-ish string, e.g. `$.address.city`) built up during the walk.
  New runtime helper `make_decode_error(msg, path)`.

### Feature 2 — close the Json→concrete cast hole

- compat.rs:57 currently is `(_, Type::TypeVar(_)) | (Type::TypeVar(_), _) => true`, making
  `Json` (which is `Type::TypeVar(u32::MAX)`, see types.rs:82 `is_json`) bidirectionally
  compatible with everything → `val p: Person = readJson(...)` typechecks with no validation.
- **Surgical directional fix**: split the arm so that `Type::TypeVar(u32::MAX)` (Json) is a
  **covariant sink only**:
  - `anything → Json` stays allowed (target is Json): keeps `writeJson(value: Json)` etc.
  - `Json → concrete non-Json T` is **rejected** (value is Json, target concrete): forces
    `fromJson`, `is`/`has` narrowing, or an annotation change.
  - Genuine inference/intrinsic TypeVars (any `TypeVar(n)` with `n != u32::MAX`, including the
    9000+ generic slots and fresh unification vars) stay **bidirectionally permissive** as
    today — do not break inference.
- **Safe Json→T paths after this change**: (a) `fromJson`, (b) `is`/`has` narrowing (a
  different code path — checker/pattern.rs — left unchanged, still sound via runtime tag
  checks). This must be documented.

---

## 2. Step-by-step implementation (reading order across crates)

> Tokens: **no change**. AST: **no change** (DotCall already exists; type-namespace receiver
> `Person` parses as `Expr::Ident("Person")`).

### Step 1 — Checker: `fromJson` special form + type-namespace resolution
Files: `crates/lin-check/src/checker/call.rs`, `crates/lin-check/src/checker/expr.rs`,
`crates/lin-check/src/typed_ir.rs`, `crates/lin-check/src/types.rs`,
`crates/lin-check/src/resolve.rs`.

1a. **Add the `Error` type.** `Error` does not currently resolve — `resolve_named_cycle`
   (resolve.rs:65-110) has no `"Error"` arm, and there is no `Type::Error`. (Verified: `Error`
   appears only in *comments* in stdlib, never in a real annotation; fs/proc/tty all return
   `Json`.) Add `"Error" => Ok(error_type())` to `resolve_named_cycle`, where `error_type()`
   returns `Type::Object({"type": Str, "message": Str})` (an open object — width subtyping
   means the runtime's `{type,message,path}` still satisfies it). Do NOT add a new `Type`
   variant; an object alias avoids touching the ~20 exhaustive `Type` matches (same reasoning
   ADR-044 used to defer `Type::Shared`). This makes `T | Error` expressible as
   `Type::Union(vec![T, error_type()])`.

1b. **Add a `TypedExpr::FromJson` variant** in typed_ir.rs (near `Call`, line 119):
   `FromJson { target: Type, value: Box<TypedExpr>, result_type: Type /* = T | Error */, span }`.
   Carrying the resolved `target: Type` explicitly is what lets lower/codegen emit the
   descriptor; `result_type` (= `T | Error`) is what flows to assignment/return checks.
   Add the `FromJson` arm to `TypedExpr::ty()` and `TypedExpr::span()` (and any exhaustive
   match over TypedExpr in lower.rs / liveness.rs / rc_elide.rs — grep for `TypedExpr::Call`
   to find them all).

1c. **Recognition helper.** Add `Checker::try_from_json_special_form(name, receiver_or_arg0,
   value_arg, span) -> Option<Result<TypedExpr, Diagnostic>>`. It fires when the call's
   *callee name* is `fromJson` (surface name — match the `Expr::Ident("fromJson")` callee in
   `infer_call`, and `method == "fromJson"` in `infer_dot_call`). It:
   - Resolves arg0/receiver in the TYPE namespace: the receiver/arg0 must be
     `Expr::Ident(type_name)` (or a `TypeExpr`-spellable name). Resolve via
     `self.env.lookup_type(type_name)` + `crate::resolve::resolve_type` to a `Type` T.
     If arg0 is not a known type name, emit a diagnostic ("fromJson's first argument must be a
     type, e.g. `Person.fromJson(json)`").
   - Infers the value arg as a value; require it compatible with `Json`
     (`self.types_compatible(value_ty, &json_type())`).
   - Returns `TypedExpr::FromJson { target: T, value, result_type: Union([T, error_type()]),
     span }`.

   **Name-collision guard / why surface name not `lin_*`:** Existing intrinsics (`print`,
   `for`, `async`) are recognised by their `lin_*` name *after* the stdlib wrapper forwards to
   `lin_print(x)` (the wrapper is ordinary Lin; recognition is in call.rs:221 `name ==
   "lin_async"`, lower.rs:1804, codegen). That model REQUIRES the argument to be expressible as
   a runtime value. `fromJson`'s arg0 is a *type*, so no `lin_from_json(T, json)` wrapper can
   exist. Therefore recognition must be at the surface `fromJson` call. To keep hygiene
   (ADR-002/009): the special form only fires when arg0 resolves to a known type name AND
   `fromJson` is in scope as the std/json export (guard against a user shadowing `fromJson`
   with their own value binding — if `self.env.effective_type("fromJson")` is a user value,
   defer to the normal call path). The `std/json` export is a sentinel type/marker so
   `import { fromJson } from "std/json"` is required for discoverability and for `lin check` to
   not flag `fromJson` as undefined.

1d. **Wire into `infer_call`** (call.rs:10): at the very top, before
   `self.infer_expr(func)`, if `func` is `Expr::Ident("fromJson")` and args.len()==2, call the
   helper. **Wire into `infer_dot_call`** (call.rs:265): before `self.infer_expr(receiver)`
   (call.rs:295), if `method == "fromJson"`, call the helper with `receiver` as arg0 and
   `args[0]` as the value. This covers `Person.fromJson(json)` (DotCall) and
   `fromJson(Person, json)` (Call).

### Step 2 — IR: `Intrinsic::FromJson` + lowering
Files: `crates/lin-ir/src/ir.rs`, `crates/lin-ir/src/lower.rs`.

2a. Add `Intrinsic::FromJson` to the `Intrinsic` enum (ir.rs:38). Add a companion IR concept
   for the **schema descriptor**: emit it as a const data blob. Simplest: add a new
   `Instruction::CallIntrinsic`-adjacent path that carries the descriptor bytes, OR (preferred,
   less invasive) emit the descriptor entirely in codegen from the `target: Type` — pass the
   target type through to codegen. To pass the target type: add a `lower_from_json` that lowers
   the value arg, allocates a temp of `result_type`, and emits
   `Instruction::CallIntrinsic { dst, intrinsic: Intrinsic::FromJson, args: [value_temp],
   ret_ty: result_type }`. Crucially, codegen reads `arg_tys`/`ret_ty` — but it needs the
   *target* T, which is `ret_ty` with the `Error` object variant stripped. Provide a helper
   `strip_error(ret_ty) -> T` shared by lower/codegen, OR carry T in a dedicated
   `Intrinsic::FromJson` payload. **Recommendation:** make `Intrinsic::FromJson(Box<Type>)`
   carry the target `Type` directly (the enum already carries data for `FlatArrayAlloc(kind)`),
   avoiding any fragile strip-Error inference.

2b. In `lower.rs`, add `TypedExpr::FromJson { target, value, result_type, .. }` arm to
   `lower_expr` (the big match dispatching to `lower_call` etc.). Lower `value` via
   `lower_expr`; box it to Json if it is concrete (reuse `lower_call_arg`/`box` helpers, since
   the runtime expects a `TaggedVal*`). Register the result as owned (it is +1: either the
   same pointer retained, or a fresh Error). Mark `result_type` as a union (`T | Error`) so
   rc_elide/liveness treat the result as a boxed union — confirm against the union-result
   ownership rule at lower.rs:1771-1779.

### Step 3 — Codegen: emit descriptor + call runtime validator
Files: `crates/lin-codegen/src/codegen/intrinsics.rs`,
`crates/lin-codegen/src/codegen/runtime.rs`, `crates/lin-codegen/src/codegen/mod.rs`.

3a. In `compile_ir_intrinsic` (intrinsics.rs:137) add an `Intrinsic::FromJson(target)` arm.
   Pattern after `Intrinsic::Length` (intrinsics.rs:158), which already dispatches on a static
   type — but here we *emit a descriptor* from `target` rather than branch inline.
3b. **Descriptor encoding** (compile-time → static global `[i8]` / `[i32]`): a flat table of
   nodes. Each node = `{ kind: u8, ... }`:
   - `KIND_STRING/BOOL/NULL` — tag check only.
   - `KIND_INT(width, signed)` — integral + range check; `KIND_FLOAT(width)` — accept number,
     coerce.
   - `KIND_JSON` — accept anything (no check; for `Json`/`Number` fields).
   - `KIND_ARRAY(elem_node_idx)` — TAG_ARRAY check, recurse each element to `elem_node_idx`.
   - `KIND_FIXEDARRAY(len, [node_idx...])` — TAG_ARRAY + length==len + positional recurse.
   - `KIND_OBJECT(nfields, [ (key_string_global, value_node_idx, nullable_flag) ... ])` —
     TAG_OBJECT; for each field look up key; missing allowed iff `nullable_flag` (target field
     type includes Null) — mirrors compat.rs:91-98 object rule; extra fields ignored (width
     subtyping).
   - `KIND_UNION(nvariants, [node_idx...])` — try each variant node in order; first that
     validates (structurally) wins; if none, error.
   - `KIND_NAMED(node_idx)` — back-edge index into the table → handles recursion/cycles with
     **no infinite loop** because the descriptor is finite and named types resolve to a stable
     table slot (build the table with a `HashMap<Type, idx>` memo so each distinct Named type
     is emitted once and recursive references point back).
   Build this table in a codegen helper `emit_from_json_descriptor(&Type) -> GlobalValue`,
   memoised, walking the (already-resolved) `Type`. Strings (object keys) emitted as interned
   `LinString` globals (reuse existing string-literal emission).
3c. Declare `lin_from_json` in `RuntimeFns` (runtime.rs): signature
   `(value: ptr, descriptor: ptr) -> ptr`. Emit a call passing the (boxed) value temp and the
   descriptor global. The result is the value pointer (success, retained) or an Error
   TaggedVal*.

### Step 4 — Runtime: generic validator + decode-error
Files: `crates/lin-runtime/src/json.rs` (or a new `crates/lin-runtime/src/decode.rs`),
`crates/lin-runtime/src/fs.rs` (error helper), `crates/lin-runtime/src/lib.rs` (module decl).

4a. Add `#[no_mangle] pub unsafe extern "C" fn lin_from_json(value: *const u8,
   desc: *const u8) -> *mut u8`. It reads the descriptor table and walks `value` (a
   `TaggedVal*`) recursively. Uses existing primitives: `lin_get_tag` (tagged.rs:237),
   `lin_unbox_*` (tagged.rs:247-281), `tagged_as_f64` (tagged.rs:373), object get/has, array
   length/get. Returns the SAME `value` pointer on success (retained +1 via existing
   retain helpers), or `make_decode_error(msg, path)` on first mismatch.
4b. Number policy in the validator:
   - integer target: read the numeric tag; accept TAG_INT32/INT64/UINT64; for TAG_FLOAT64
     accept only if `f.fract()==0` and within target range; reject otherwise. Range from
     descriptor width/signedness.
   - float target: accept any numeric tag, value passes through (no mutation; the box already
     holds the number; readers coerce via `tagged_as_f64`).
   - Json/Number target (`KIND_JSON`): accept any tag.
4c. `make_decode_error(msg, path)` in fs.rs: extend `make_error_obj` to also set `"path"`.
   Keep `{"type":"error"}` so existing `response["type"] == "error"` checks (http.lin:35)
   keep working, and so the value is compatible with the `Error` object alias from Step 1a.
4d. Path tracking: pass a small growable `String` (or fixed buffer) down the recursive walk;
   append `.field` / `[i]` as it descends; emit it into the error on failure.

### Step 5 — stdlib `std/json`
Files: new `stdlib/json.lin`, `stdlib/json.test.lin`; register in
`crates/lin-compile/src/lib.rs` `stdlib_source` (lib.rs:281-304, add
`"std/json" => Some(include_str!("../../../stdlib/json.lin"))`).

5a. `stdlib/json.lin` exports:
   - `fromJson` — the special-form sentinel. Because the checker recognises the surface name,
     this export exists so `import { fromJson } from "std/json"` resolves and `lin check` does
     not report `fromJson` undefined. Model it like the concurrency intrinsics that are
     *registered* (intrinsics.rs) so they resolve: register a `fromJson` binding with a
     placeholder type `(Json, Json) => Json` in `register_intrinsics` and re-export from
     std/json, but have the checker special-form intercept before that signature is used.
     (Alternative: a thin `export val fromJson` wrapper is impossible because arg0 is a type —
     so the registered-intrinsic-name route is the chosen one.)
   - Optionally re-export `parseJson`/`readJson` adjacency helpers (out of scope; leave to
     std/fs and std/http which already expose `lin_parse_json`).
5b. Document in `docs/STDLIB.md`: add a `std/json` section with
   `fromJson: (Type, Json) -> T | Error` and the `Person.fromJson(json)` idiom, number policy,
   first-error semantics, and the Error shape `{type,message,path}`.

### Step 6 — Cast-hole fix
File: `crates/lin-check/src/compat.rs` (line 57).

Replace:
```
(_, Type::TypeVar(_)) | (Type::TypeVar(_), _) => true,
```
with:
```
// Anything is assignable INTO Json (covariant sink): concrete T -> Json.
(_, Type::TypeVar(n)) if *n == u32::MAX => true,
// Json is NOT assignable to a concrete non-Json target: forces fromJson / narrowing.
// (But Json -> Json, and Json -> a permissive inference var, stay allowed.)
(Type::TypeVar(s), Type::TypeVar(_)) if *s == u32::MAX => true,
(Type::TypeVar(s), _) if *s == u32::MAX => false,
// Non-MAX inference / generic / intrinsic TypeVars stay bidirectionally permissive.
(_, Type::TypeVar(_)) | (Type::TypeVar(_), _) => true,
```
Note ordering: the `n == u32::MAX` target arm must precede the generic permissive arm.
Confirm `Type::Union` arms (compat.rs:66-73) still let `T | Error` (Step 1a) work: a `Json`
value is NOT compatible with a concrete union variant after this change (intended), but
`fromJson`'s `T | Error` result *as a value* assigned to a `T | Error` annotation is an exact
match, and to a wider position is fine.

### Step 7 — Migrate broken sites (see §3)
Files: stdlib + examples per the blast-radius list.

### Step 8 — Docs / ADR
Files: `docs/DECISIONS.md` (new ADR, §5), `docs/SPECIFICATION.md` (§6.2 note that Json→concrete
needs an explicit decode), `docs/STDLIB.md` (Step 5b).

### Step 9 — Tests (see §4).

---

## 3. Cast-hole blast-radius

This is the highest-risk part: `Json` flows into a concrete-typed position any time a value
read from a `Json` (index access, `readJson`, `lin_parse_json`, http body, a `Json`-typed
param) is assigned to / passed to / returned as a concrete (non-Json, non-TypeVar) type.
Bracket access on `Json` yields `Json` (expr.rs:214 `Type::TypeVar(_) => fresh_type_var()` and
the `is_json` index arm), so `json["k"]` is Json → Json-typed positions are FINE.

**Method used:** grepped stdlib/*.lin and examples/*.lin for concrete-typed params/returns and
for `["..."]` reads flowing into them. The definitively-identified sites:

1. **examples/web-server/handlers.lin:14** — `pathMatch("/users/:id", req["path"])`.
   `req: Json` so `req["path"]: Json`; `pathMatch(pattern: String, path: String)`
   (http.lin:58) — second arg is concrete `String`. **Now errors.**
   Migration: `req["path"]` reads a Json string; either change `pathMatch`'s `path` param to
   `Json` (cheapest, semantics unchanged — the runtime already handles boxed strings), or
   narrow at the call site (`match`/`is String`). **Recommended: change `pathMatch` param to
   `Json`** (it forwards to `lin_server_path_match` which takes Json-ish anyway).

2. **examples/web-server/router.lin:8** — `pathMatch("/users/:id", path)` where `path` is the
   `match req["path"]` binding `is path when ...`. `path` is bound at the scrutinee type
   (`Json`). Same fix as #1 (Json param on `pathMatch`).

3. **examples/web-server/handlers.lin:16 / main.lin / *.test.lin** — `m["id"]` (Json) used
   only inside object literals (`{ "id": m["id"], ... }`) and string interpolation
   (`"User ${m["id"]}"`). Object-literal field values target Json (FINE); interpolation calls
   `toString` (TypeVar param, FINE). **No change.**

4. **stdlib/http.lin:55-56 `parseBody(req: Json)`** → `lin_parse_json(req["body"])`.
   `req["body"]: Json` passed to `lin_parse_json: (String) => Json` (http.lin:4) — concrete
   `String` param. **Now errors.** Migration: change the foreign decl `lin_parse_json` param
   to `Json` (the runtime parses a boxed string fine; `resolve_lin_str` in fs.rs:52 already
   accepts boxed-or-raw strings), OR narrow. **Recommended: `lin_parse_json: (Json) => Json`.**

5. **stdlib/http.lin:36 `fetchJson`** — `lin_parse_json(response["body"])`, `response: Json`
   from `lin_http_fetch`. Same as #4 — fixed by the `lin_parse_json` param widening.

6. **General stdlib audit (must re-confirm by building):** every other stdlib function I read
   (string.lin, array.lin, object.lin, test.lin, fs.lin, net.lin) takes `Json` or concrete
   params fed by *concrete* values, not by Json reads:
   - string.lin `_length(s: String)`, `_substring(...)` etc. are called with `String` args
     (the public `length`/`substring` params are `String`, supplied by callers' concrete
     strings) — FINE.
   - test.lin internal `Assertion[]` flows are built from `Assertion` values, not Json — FINE.
   - net.lin / fs.lin params are concrete (`Int32`, `String`, `UInt8[]`) supplied by concrete
     callers; their RETURNS are `Json` flowing to `Json` positions — FINE.
   - array/object functions take `Json`/`Json[]` params — Json→Json, FINE.

**Feasibility:** the confirmed breaks are few (the `pathMatch` String params and the
`lin_parse_json` String param) and all migrate by **widening a stdlib param from `String` to
`Json`** — a one-pass change, no staging needed. **However**, the static grep cannot prove
completeness; Step 0 of execution MUST be: apply the compat.rs fix, run
`cargo build --workspace && cargo test --workspace` and `lin test stdlib/` + compile every
`examples/*.lin`, and treat each new "Argument N has type Json, expected <concrete>" /
"Expected type <concrete>, got Json" diagnostic as a migration site. The grep above predicts
the set; the build is the authority.

**Interplay with narrowing (confirmed safe):** `is`/`has` narrowing is a different path
(checker/pattern.rs:185-239, which branches on `ty.is_json()` directly and binds Json field/
element types) and does NOT consult `is_compatible`. Match-arm narrowing (ADR-035) shadows the
binding with the narrowed type. String/numeric *literal* match arms on a Json scrutinee
(`match req["path"] is "/" => ...`) compare values at runtime via tag checks — also unaffected.
So closing the compat hole does not break `is`-narrowing. After this change the only sound
Json→T conversions are: (a) `fromJson`, (b) `is`/`has` narrowing. Document this in
SPECIFICATION §6.2 and the new ADR.

---

## 4. Test plan

### 4.1 Integration tests — `crates/lin/tests/integration.rs`
Use the existing harness `run(source) -> Vec<String>` (line 28) for success cases and
`run_expect_err(source) -> String` (line 75) for compile-error cases.

`fromJson` success/behaviour (compile + run, assert stdout):
- `test_from_json_object_success` — `type Person = {"name":String,"age":Int32}`;
  `Person.fromJson({"name":"Bob","age":30})` then print a field; assert decoded value.
- `test_from_json_direct_call_form` — `fromJson(Person, j)` equals dot form.
- `test_from_json_missing_required_field` — missing `age` (non-nullable) → result is Error;
  assert `is Error`/`["type"]=="error"` branch taken.
- `test_from_json_missing_nullable_field_ok` — field typed `T | Null` absent → success.
- `test_from_json_extra_field_ignored` — extra key present → success (width subtyping).
- `test_from_json_wrong_type` — `"age": "x"` (string where Int32 expected) → Error, with path
  `$.age` in the message.
- `test_from_json_int_range_reject` — `"age": 3.14` and `"age": 4294967296` for Int32 → Error.
- `test_from_json_float_accepts_int` — target Float64, JSON `5` → success.
- `test_from_json_nested_object` — `{"address":{"city":String}}`; wrong nested type → Error
  with path `$.address.city`.
- `test_from_json_array` — `Int32[]` with a non-int element → Error at `$[2]`.
- `test_from_json_fixed_array` — `[String,Int32]` length/positional checks.
- `test_from_json_union_variant` — `type Shape = {"k":"a",...} | {"k":"b",...}`; first
  structurally-matching variant wins; non-matching → Error.
- `test_from_json_recursive_type` — `type Tree = {"value":Int32,"children":Tree[]}` decodes a
  nested tree (exercises the descriptor back-edge; no infinite loop).
- `test_from_json_error_value_shape` — assert decoded Error has `type/message/path`.

Cast-hole (compile errors via `run_expect_err`):
- `test_json_to_concrete_now_errors` — `val p: Person = readJson(path)` → expect a type error
  mentioning Json/Person (was silently OK before).
- `test_json_arg_to_concrete_param_errors` — passing `json["x"]` to a `String` param errors.
- `test_concrete_to_json_still_ok` — `writeJson(path, person, {})` still compiles (sink).
- `test_is_narrowing_still_works` — `if j is String then j else ""` compiles (narrowing path
  intact).

### 4.2 Example fixture — `examples/`
- New `examples/from-json.lin`: read/define a Json literal, `Person.fromJson(...)`, handle the
  `T | Error` with a `match is Error`/`else` and print. Add to CI's example sweep (non-network).
- Update `examples/web-server/*` and `examples/dijkstra/main.lin` if the build surfaces any
  Json→concrete breaks (per §3); keep them compiling.

### 4.3 stdlib tests — `stdlib/json.test.lin`
Mirror 4.1 in Lin using `std/test` (`test`/`suite`/`expect().toBe()`):
- success, missing required field, missing nullable ok, extra field ignored, wrong type,
  int-range reject (3.14 and overflow), float-accepts-int, nested object, array element error,
  fixed-array length, union variant selection, recursive type, Error shape.
- A `Json→concrete now-errors` case cannot be a runtime test (it is a compile error) — assert
  it in integration.rs (4.1) instead.

Run all via `cargo test --workspace` and `cargo run -p lin -- test stdlib/`.

---

## 5. Validator-strategy comparison, open questions, risks

### Validator strategy — three options weighed
- **A. Fully inline LLVM per site.** Codegen emits the recursive checks directly. Pro: no
  runtime fn, best for tiny types. Con: code blows up for large/recursive types; recursion
  needs emitted helper functions anyway; duplicates logic the runtime already has (tag/unbox).
- **B. Generated per-type runtime-style function.** Emit one LLVM function per target type,
  recursive calls between them. Pro: shares per-type code across call sites. Con: still emits a
  lot of IR; cycle handling needs forward decls; more codegen machinery.
- **C. (RECOMMENDED) Single generic runtime interpreter + compile-time descriptor.** Codegen
  emits only a small static descriptor table; one runtime fn `lin_from_json(value, desc)` walks
  both. Pro: O(1) emitted code per site; recursion/cycles are just table indices; reuses
  runtime tag primitives; easiest to test (the walker is ordinary Rust). Con: descriptor
  encoding is a new (small) data format; interpreter has a tiny dispatch overhead (irrelevant
  for I/O-bound decode). Given "no runtime type info," C is the natural fit and the least code.

### Open questions / risks
1. **Recursion / cycles** — solved by the descriptor memo (each Named type → one table slot;
   recursive refs are back-edges). Risk: mutually-recursive types must share one table; ensure
   `emit_from_json_descriptor` memoises by `Type` (or by Named name) before emitting children.
2. **Union variant selection ambiguity** — "first structurally-matching variant wins" is
   order-dependent. For overlapping object variants (same discriminant absent) the first listed
   wins, which may surprise users. Mitigation: document; recommend a discriminant field
   (`"type"`). Keep first-match-wins for v1 (matches spec §30.8 first-error policy spirit).
3. **Number widening edge cases** — `Int32` target with JSON `5.0` (serde may parse as float):
   policy accepts integral floats in range; `5.5` rejected; values like `2^53+1` lose
   precision as f64 — for an `Int64` target whose JSON arrived as a float, reject if not
   exactly representable. UInt64 > i64::MAX handling must read TAG_UINT64 unsigned
   (tagged.rs:36 note). Risk: ensure the runtime reads the right signedness per the descriptor.
4. **`Error` as an object alias** — width subtyping means a *successful* decode of a type that
   happens to look like `{type,message}` could be mistaken for an Error by user code that does
   `is Error`. Low risk (users decode known types), but document that `Error` is structural.
5. **Special-form vs user shadowing of `fromJson`** — guard (Step 1c) defers to the normal
   call path if `fromJson` is bound to a user value; verify with a test.
6. **ADR required for the cast-hole fix — YES.** Draft:
   - **Title:** *ADR-046: `Json` is a covariant sink — closing the Json→concrete cast hole.*
   - **One-line rationale:** `Json` (TypeVar(u32::MAX)) is made assignable INTO any type but
     not OUT to a concrete type, so silent unchecked `val p: Person = readJson(...)` becomes a
     type error and the only sound Json→T paths are `fromJson` (validated decode) and `is`/`has`
     narrowing (runtime tag checks); genuine inference/generic TypeVars stay permissive.
   Also note a one-line follow-up in ADR-018/Json discussion that `Number`/`Json` field targets
   in `fromJson` skip number-range validation by design.

### Critical Files for Implementation
- /tmp/lin-fromjson/crates/lin-check/src/compat.rs
- /tmp/lin-fromjson/crates/lin-check/src/checker/call.rs
- /tmp/lin-fromjson/crates/lin-codegen/src/codegen/intrinsics.rs
- /tmp/lin-fromjson/crates/lin-runtime/src/json.rs
- /tmp/lin-fromjson/stdlib/json.lin
