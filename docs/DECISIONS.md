# Architecture Decision Records

> **Numbering note (2026 consolidation):** the ADRs were consolidated in a single pass —
> records that were later reversed, that duplicated another record's topic, or that were
> not about the Lin language itself were removed or folded into the survivor that owns
> their topic (each survivor carries a "Supersedes/absorbs" note). The remaining records
> were then **renumbered contiguously**, so `ADR-001 … ADR-041` is a gapless sequence and
> every number resolves to exactly one record.

## ADR-001: Static typing via lin-check

**Decision**: All Lin programs are type-checked before codegen. The `lin-check` crate performs bidirectional inference, structural typing, union narrowing, and exhaustiveness checking. Runtime type tags still exist in the runtime for `is`/`has` pattern dispatch, but no program should reach codegen with unresolved type errors.

**Rationale**: A full bidirectional type system with generics, variance, and numeric widening allows LLVM to emit unboxed primitives and enables helpful compile-time error messages.

**Consequence**: Type annotations are parsed, checked, and emitted into `TypedModule`. The `lin-check` crate owns all type inference logic.

## ADR-002: Minimal built-ins, stdlib for iteration

**Decision**: Only `lin_for` and `lin_iter` are compiler intrinsics with special codegen. Higher-level functions (`map`, `filter`, `reduce`, `range`, `iterOf`) are implemented in `.lin` stdlib files (`std/array`) and must be explicitly imported by user code.

**Rationale**: `lin_for` and `lin_iter` require special compiler treatment (inline loop emission, iterator struct construction). All other iteration functions can be expressed in Lin itself. Keeping intrinsics minimal means the runtime stays small and the stdlib is readable Lin code.

**Consequence**: `range()` returns a lazy `Iterator`. User code imports `map`, `filter`, `reduce`, etc. from `std/array`. The compiler recognises `lin_for`/`lin_iter` by name and emits them as native loops rather than function calls.

## ADR-003: Objects suppress indentation tracking

**Decision**: When inside `{ }` (brace depth > 0), the lexer suppresses newline tokens and indentation tracking (no INDENT/DEDENT emitted). The same suppression applies inside `( )` and `[ ]`.

**Rationale**: Multi-line JSON object literals must not trigger block parsing.

**Consequence**: You cannot have indentation-significant syntax inside object literals (which is fine — object values are expressions, not statements).

**Absorbs an earlier indentation-flag fix.** The `at_line_start` flag is reset to false unconditionally at the top of `next_token()`, not only when entering `handle_indentation()`. Otherwise a newline inside braces (e.g. a multi-line `import { ... } from "path"`) left the flag stale, and when the closing brace returned depth to 0 it triggered a spurious INDENT on the next token. Always clearing the flag eliminates that whole class of bug with no behavioural change elsewhere.

## ADR-004: String interpolation as compound token

**Decision**: The lexer produces a single `InterpString(Vec<InterpPart>)` token for interpolated strings. Each `InterpPart::Expr` contains its own sub-token-stream that the parser processes independently.

**Rationale**: The initial approach of inlining interpolation tokens into the main token stream caused ordering issues with the pending-token queue. A compound token with embedded sub-streams is self-contained and avoids interaction with indentation tracking.

**Consequence**: Interpolation expressions are parsed in isolation (no access to outer indentation context), which is fine since they're always single expressions.

## ADR-005: Dot-chaining across newlines via lookahead

**Decision**: The parser's postfix expression loop checks for `.` across newline boundaries using a save/restore pattern. If a newline is followed by `.`, parsing continues the dot chain. Otherwise, position is restored.

**Rationale**: The spec requires `x\n  .f()` to chain. But aggressively skipping all indentation tokens breaks block structure. The save/restore pattern is conservative — it only consumes whitespace tokens when followed by a dot.

**Consequence**: Dot-chaining works across lines without breaking function bodies or if-then-else blocks.

**Also covers `&&`/`||` continuation lines.** The same save/restore lookahead is used for `&&`/`||` continuation lines: `parse_and_expr`/`parse_or_expr` use a `skip_continuation_newline` helper that looks past a Newline token for the operator (the lexer suppresses INDENT/DEDENT for lines starting with `&&`/`||` per spec §2.2 but still emits a trailing Newline). Without it, `x >= 5\n  && active` would parse as just `x >= 5`; with it, multi-line boolean expressions and `if` conditions with continuation lines work as specified.

## ADR-006: Bare identifier lambdas

**Decision**: The parser recognizes `name => body` (without parentheses) as a single-parameter lambda when used as a function argument.

**Rationale**: The spec's examples use this form extensively (`x => x * 2`, `n => print(n)`). Without this, every callback would need `(x) => x * 2`.

**Consequence**: `is_bare_lambda()` check applies only in argument position. A standalone `name => ...` at statement level would be ambiguous (could be assignment with `=>`?), but this doesn't arise in practice.

## ADR-007: Module-level environment isolation in the compiler

**Decision**: Each module is type-checked in its own scope. Imports from other modules are resolved before the importing module is checked, with each module's public exports available as a `ModuleSignature`.

**Rationale**: Modules must not pollute each other's namespaces. The compiler uses a module cache keyed by source hash so unchanged modules are not re-checked.

**Consequence**: Circular imports within a single init chain are detected at compile time. Each module's checked `TypedModule` is cached and reused by all importers.

## ADR-008: Stdlib functions as thin Lin wrappers over runtime intrinsics

**Decision**: String, array, object, and IO operations are implemented as C-ABI functions in `lin-runtime` (e.g. `lin_string_trim`, `lin_fs_read_file`) and declared in stdlib `.lin` files via `import foreign "lin-runtime"`. The `.lin` files provide the public API surface.

**Rationale**: String/IO manipulation requires Rust code. The .lin wrapper layer keeps the user-facing API in Lin, making stdlib readable and testable in the same language. The compiler recognises `"lin-runtime"` as a reserved path and always links the runtime archive.

**Consequence**: The stdlib is a mix of pure-Lin logic and thin `lin-runtime` wrappers. Adding a new runtime function requires both a `#[no_mangle] pub unsafe extern "C" fn lin_xxx` in `lin-runtime/src/` and an exported wrapper in the appropriate `stdlib/*.lin` file.

**Extends to all IO.** This pattern extends to all IO: filesystem, HTTP client, and server operations are likewise implemented as `#[no_mangle] pub unsafe extern "C"` functions in `lin-runtime` (`lin_io_read_line`, `lin_fs_read_file`, `lin_http_fetch`, …) and exposed through thin `.lin` modules (`std/io`, `std/fs`, `std/http`, `std/server`) registered via `include_str!`. All IO is synchronous on the calling thread (run in the background via `async`/`threadPool`); the `lin_*` symbols stay implementation details behind the clean wrapper API, and higher-level helpers like `fetchJson`/`postJson`/`parseBody` are written in Lin.

## ADR-009: Multi-line if/then/else syntax

**Decision**: `then` always appears on the condition line (or the last continuation line of the condition). The body follows on an indented block (INDENT … DEDENT). `else` appears at the same indent level as `if`. The parser does not consume any INDENT before `then` — it simply expects `then` after the condition expression.

**Rationale**: Placing `then` at the end of the condition line is clearer and more consistent with how block-opening keywords work in other languages. The old approach of allowing `then` on its own indented line required the parser to tentatively consume an INDENT token before `then`, then emit a corresponding DEDENT, making the grammar more complex with three special-case DEDENT guards. The new rule is simpler: condition, `then`, body block, `else` at original indent, else body.

**Consequence**: All spec-defined if layouts (single-line, multi-line with block body, multi-line with inline body) parse correctly. Condition continuation lines with `&&`/`||` end with `then` on the last continuation line. The `then_indented` tracking variable and its three associated DEDENT guards have been removed from `parse_if_expr`.

## ADR-010: Postfix suppression after DEDENT, and line-leading `[`/`(` as a new statement

**Decision**: The parser's postfix expression loop (`[` and `(`) is suppressed when the immediately preceding consumed token was a DEDENT. Dot-chaining (`.`) is still allowed (it handles cross-line chaining via a separate lookahead mechanism).

**Rationale**: After a block-bodied function expression like `() => \n  42`, the lexer produces `... IntLit(42) Newline Dedent LBracket ...` — the inner block's `skip_newlines` consumes the Newline, so after the Dedent is consumed, no Newline separates the function from the next line's `[`. Without this guard, `[x]` at the outer block level is incorrectly parsed as index access on the function expression.

**Consequence**: Array/object literals at block level after indented function definitions parse correctly as separate expressions. Same-line index access (`f()[0]`) still works because no DEDENT intervenes.

**Supersedes/absorbs the duplicate ADR-028 ("line-leading `[`/`(` is a new statement").** The same intent applies where the boundary is a *suppressed newline* rather than a DEDENT: inside an inline lambda body (`() => ...` with no INDENT, parsed by `parse_inline_block`), `()`/`[]`/`{}` suppress newline tokens (ADR-003), so the postfix loop would otherwise read `expr \n [ ... ]` as `expr[...]`. The lexer records a per-token `newline_before` flag (set by scanning the gap between token spans), and `parse_postfix_expr` suppresses the `LBracket`/`LParen` arms when `at_line_start()` is true. This lets a multi-statement inline body return an array literal on its own line (`val xs = f(); push(xs, y); [ expect(...).toBe(...) ]`) instead of gluing it as an index. The postfix `.` arm is not gated, so same-line indexing and continuation dot-chains are unaffected.

## ADR-011: Inline block parsing for lambda bodies inside parentheses

**Decision**: `parse_function_body` always delegates to `parse_inline_block` when there is no `Indent` token ahead. `parse_inline_block` collects statements until it sees `Newline`, `)`, `]`, `}`, `,`, `Dedent`, or EOF, then returns either the single expression or an `Expr::Block` wrapping all collected statements.

**Rationale**: Inside parentheses, brackets, or braces, the lexer suppresses all INDENT/DEDENT and Newline tokens (ADR-003), so `parse_expr_or_block` cannot detect a multi-statement body. At top level, Newline tokens are present and `parse_inline_block` breaks on them, making it behave identically to `parse_expr` for the single-expression case. The break conditions `]` and `}` prevent over-consuming array and object literal contents. `Comma` ensures argument-list lambdas (e.g. `iter(() => 0, i => i + 1)`) parse correctly.

The earlier version used `val`/`var` as the trigger for multi-statement inline bodies. That was too narrow — bare expression side-effects (calls to `print`, `writeFile`, etc.) were silently dropped, leaving only the first expression evaluated.

**Consequence**: Bare side-effect sequences work in both inline and indented lambda bodies:

```txt
[1, 2, 3].for(x =>
  print("before")    // executed
  print(toString(x)) // executed
)

val myFunc = () =>
  print("first")     // executed
  print("second")    // executed
  42                 // return value
```

## ADR-012: Forward references between top-level functions via mutable cells

**Decision**: Before evaluating a module's statements, a pre-scan registers all `val name = (...) => ...` bindings (function expressions with named pattern) as mutable cells holding `Null`. During evaluation, each function's closure captures the environment containing these cells. When the actual definition is reached, the cell is updated with the real function value.

**Rationale**: The spec (§6.3) expects mutual recursion between top-level functions. Without forward declaration, functions must be defined before use, which prevents mutual recursion and requires careful ordering. The mutable-cell approach solves this without changing evaluation semantics — a function that calls another function reads the cell at call time, by which point the definition has been evaluated.

**Consequence**: Forward references work between functions (e.g., `isEven` calling `isOdd` and vice versa). However, eager top-level evaluation that *immediately* calls a forward-referenced function (before its definition is evaluated) will still fail with "Cannot call value of type Null". This is inherent to sequential evaluation and matches the behavior of languages like JavaScript (`let` before initialization).

**Absorbs the codegen forward-reference + function-identity work.** The same forward-reference mechanism is realised in codegen, where it also establishes *function identity*. (1) Before compiling any top-level function body, `compile_module` pre-scans all `Val` statements and LLVM-declares any function whose value is a named `Function`, storing them in `global_fn_slots`; bodies are compiled in a second pass so sibling functions can call each other. (2) The binding name is propagated onto `TypedExpr::Function { name }` by the checker when a `val` pattern is a plain identifier, so the codegen emits a named LLVM function (`@factorial`, not `@__closure_0`), enables `global_fn_slots` lookup for direct calls, and sets `current_function` for tail-call detection (which feeds the TCO loop transform, ADR-016).

## ADR-013: User module loading from filesystem

**Decision**: When an import path does not match a `std/` prefix, the interpreter resolves it relative to the importing file's directory by appending `.lin` to the path.

**Rationale**: Multi-file programs need to import user-defined modules. The resolution strategy mirrors Node.js-style relative imports without requiring a leading `./` — the `std/` prefix is the only special case, everything else is relative.

**Consequence**: `import { x } from "lib/math"` in `examples/main.lin` loads `examples/lib/math.lin`. Absolute paths and `..` traversal work naturally via the filesystem.

## ADR-014: `Number` as a numerically-bounded generic parameter

`Number` is a **numerically-bounded, monomorphized generic type parameter** with **zero runtime
cost**. (An earlier design modelled it as the union `Int8 | … | Float64` with a runtime tagged
representation in parameter position; that was measured ~3.6× slower than concrete `Int32` and is
superseded by the bounded-generic model below.)

**Decision**: A parameter (or return) annotated `Number` is sugar for an implicit, numerically-constrained
type variable — conceptually `<T: numeric>(x: T)`. Each occurrence mints its OWN fresh bounded
type-var (so `(a: Number, b: Number)` lets `a` and `b` specialize to different families
independently). The function body type-checks because the bound guarantees a numeric family, so
arithmetic on a `Number`-typed operand is permitted; the operator's result type IS the bounded var,
so it flows through. At each call site the concrete family flows in from the argument, and Lin's
existing single-module **monomorphization** (`crates/lin-ir/src/monomorphize.rs`) materializes a
specialized copy (`isEven$Int32`, `isEven$Float64`) compiled to native unboxed ops. A genuinely
non-numeric argument (`String`/`Bool`/`Object`/array) is rejected at the call site with
"expected a numeric type (Number)". A dynamic `AnyVal` value IS accepted (see **§AnyVal** below).

**Rationale**: We measured the boxed-union representation at ~3.6× slower than concrete `Int32`
(every op a tagged `lin_tagged_arith` over heap-boxed operands). The bounded-generic model is the
Rust/Swift approach: the *compile-time guarantee* ("this is a number") is decoupled from the
*runtime representation* (always a concrete family at each specialization). It reuses the
monomorphization machinery that already specializes `<T>` functions, so it adds a constraint table
and a call-site bound check rather than a new type relation or a new runtime kind. We verified the
emitted LLVM: `isEven$Float64` is a bare `frem`+`fcmp`, `isEven$Int32` a bare `srem`+`icmp` — no
`lin_tagged_arith`, no `lin_box_*`/`lin_unbox_*` per op. A 50M-iteration benchmark with a statically
concrete loop variable runs at **1.0×** of the hand-annotated `Int32` version (2.70s vs 2.72s).

**Implementation**: `lin-check` carries a `numeric_tvs: HashSet<u32>` constraint table on the
`Checker`. `resolve_type_with_number[_in]` (in `checker/function.rs`) lowers each `Number`
parameter occurrence — including nested forms like `Number[]` and `(Number) => Number` — to a fresh
quantified generic TypeVar (≥9001) recorded in that table; `forward_declare_functions` and both
`infer_function`/`infer_function_with_hints` use it so the call-driving signature and the body
agree. `infer_binary_op` (`checker/ops.rs`) makes a bounded-var operand drive the result type so it
survives substitution. The call-site numeric bound is enforced in `checker/call.rs` (both the direct
and dot-call paths). `lin-codegen/src/codegen/arith.rs` widens a mixed int/float `Mod` (so a
`Number`→Float64 specialization lowers `x % 2` to `frem`). A bare `Number` RETURN is special-cased:
it can't be pinned from arguments, so the body's (numeric, bound-guaranteed) type is surfaced as the
function's return and the body is checked numeric.

**Nested `Number` (`Number[]`, callbacks).** `resolve_type_with_number_in` recurses into Array,
FixedArray, Union, Function-param/return, and Object-field positions, so every `Number` occurrence
anywhere in an annotation mints its own bounded var — `(xs: Number[])` lowers to
`Array(TypeVar)`. For a `Number[]` argument the element var is pinned from the literal's element
family at the call site (homogeneous array → one shared element family). To make a combinator
callback over such an array specialize, two checker tweaks (`infer_function_with_hints`): (1) a bare
`Number` lambda PARAMETER whose expected type (from the enclosing combinator, e.g. `.map`'s callback
param = the receiver element) is itself a numeric-bounded var REUSES that var instead of minting a
fresh independent one — tying the callback's family to the array element it consumes; (2) a lambda
whose body type is a numeric-bounded var is surfaced as the lambda's RETURN (not erased to the
combinator's free `U` slot), so the call site pins `U` and the outer `(xs: Number[]) => xs.map(…)`
return is inferred. Result: `f([1,2,3])` ⇒ `f$Int32` native `mul i32`, `f([1.5,2.5])` ⇒ Float64.
A `Number` in a **higher-order function-typed parameter** (`(f: (Number) => Number, …)`) still
cannot be inferred at the call site — that is the SAME monomorphization-inference gap an explicit
`<T>` callback param hits (`<T>(f: (T) => T, x: T)` fails identically), not Number-specific; the call
reports the generic "cannot infer a concrete type for the type parameter(s)" error. Use a concrete
numeric family in that position.

**Mixed families in ONE call** of a `Number`-returning function (e.g.
`(a: Number, b: Number) => a + b` at `add(10, 2.5)`) **are supported**. Each `Number` is its own
bounded var, so the call monomorphizes to `add$Int32_Float64` and the arithmetic result is
**re-widened at monomorphization time** to exactly the family the concrete `(a: Int32, b: Float64)`
equivalent produces (`Float64` here, value `12.5`). `add(10, 2)` stays `Int` (both args `Int32`),
`add(1.5, 2.5)` is `Float64` — identical to the concrete-param widening rule. The emitted spec is
native (`sitofp`+`fadd`+`ret double`), no boxing. `infer_binary_op` records an arithmetic op over a
bounded var with the bounded var as its result type (so it survives substitution), but when two
DISTINCT bounded vars feed one op, that single recorded var would freeze to the FIRST family under
plain `subst_type`. So `lin-ir::monomorphize::subst_expr` re-derives an arithmetic op's result type
from its now-concrete operands via `widen_numeric` AFTER substitution, and re-syncs both the
specialized function's `ret_type` and the call's recorded result type to the body's widened tail
type (so the spec signature and the call site agree). Before this fix the result slot stayed `Int32`
while codegen emitted a `double` → the `lin_box_int32(double)` / `ret double`-vs-`i32` ABI crash;
the earlier reject in `checker/call.rs` was a workaround that is now removed.

**§AnyVal: an `AnyVal` value is ACCEPTED at a `Number` parameter** (direct OR projected), consistent with
the existing `AnyVal → Int32` scalar coercion gap (ADR-032). It monomorphizes the bounded var to the
default **`Int32`** family and unboxes **unchecked** — an `AnyVal` holding a non-integer number unboxes
as garbage, exactly the same accepted, documented unsoundness as `val n: Int32 = jsonValue` today.
The safe path for validated extraction is `Int32.fromJson(v)` (ADR-031), which range-checks and
returns `T | Error`. This replaces an earlier inconsistency: a DIRECT `AnyVal` argument
(`val x: AnyVal = 42`, the bare `TypeVar(u32::MAX)` marker) was REJECTED while an `AnyVal` PROJECTION
(`config["count"]`, a fresh inference var) slipped past the call-site bound guard and ran — so the
same value produced a compile error or a result depending only on whether it was indexed. Both now
compile and produce the SAME answer. Mechanically: (1) `arg_satisfies_numeric_bound`
(`checker/call.rs`) accepts ANY `TypeVar`, including the `u32::MAX` AnyVal wildcard (it previously
excluded it); (2) the `Number` var unifies against the AnyVal arg's wildcard type and
`lin-ir::monomorphize` binds it to the wildcard, minting a `$AnyVal`-named spec whose param is a boxed
`ptr` unboxed as `Int32`; (3) for a `Number`-RETURNING body over a AnyVal operand (`x * 3`), the
arithmetic op's result and the spec/return type are re-derived to the concrete unboxed family
(`Int32`) — mirroring the IR's unbox-to-concrete-operand behaviour — so the spec signature
(`define i32 triple$AnyVal`) matches the native scalar it returns instead of a stale boxed `ptr` (the
historical `triple$AnyVal` codegen crash), and the call site re-coerces (boxes) the scalar back to the
`AnyVal` the surrounding context expects.

**Consequence / limitations** (first cut):
- **Validated dynamic numerics use `fromJson`.** A `AnyVal` value passed to a `Number` parameter is
  accepted (§AnyVal above) but decoded as `Int32` with an unchecked unbox; for a range-checked decode
  to a specific family use `Int32.fromJson(v)` etc. We do NOT promote an `AnyVal` `Number` argument to a
  boxed slow path — it specializes to the concrete `Int32` family like every other argument.
- `Number` is no longer part of the structural definition of `AnyVal` (it never resolved to a union).
- **Nested `Number` works** for `Number[]` and combinator callbacks over it (see the implementation
  note above) — `(xs: Number[]) => xs.map((v: Number) => v*2)` specializes and runs natively.
- A `Number` in a **higher-order function-typed parameter** (`(f: (Number) => Number, x: Number)`)
  can't be inferred at the call site — the shared generic-callback inference gap (an explicit `<T>`
  callback param fails identically); use a concrete numeric family there.
- A `Number` nested inside a `<…>` GENERIC type ARGUMENT (`Iterator<Number>`) is still not lowered
  to a bounded var (`resolve_type_with_number_in` recurses into structural forms — Array, Function,
  Object — but defers a `<…>`-application's args to the standard resolver where `Number` is unknown).
- **`Number` is a parameter/return CONSTRAINT, not a value type.** It only lowers to a bounded var in
  a function signature; in a binding position (`val total: Number = 0`) it has no concrete
  representation. `resolve.rs` special-cases this with a guidance error — *"`Number` is a parameter
  constraint, not a value type; … annotate this binding with a concrete numeric family such as
  `Int32` or `Float64`."* — rather than the misleading bare "Unknown type 'Number'". A binding has a
  concrete initializer, so it already has a concrete family; name it.

## ADR-015: Unboxed primitive value representation in LLVM IR

**Decision**: Numeric and boolean types are represented as bare LLVM primitives: `Int32` → `i32`, `Float64` → `double`, `Bool` → `i1`. Strings are represented as `ptr` to a heap-allocated `LinString` struct (refcount + len + bytes). Closures are represented as `ptr` to a `{ fn_ptr, env_ptr }` struct. Union types use a heap-allocated tagged representation.

**Rationale**: The type checker produces `TypedIR` with a concrete `Type` for every expression. This means we know at compile time whether a value is `i32` or `f64`, enabling LLVM to treat them as first-class register-width values rather than tagged `Value` boxes. The performance difference versus a tree-walking interpreter (which boxes everything in a `Value` enum) is typically 50–200×. Strings cannot be unboxed (variable-length), so they remain as pointers.

**Consequence**: No boxing for arithmetic, comparisons, boolean operations, or function calls on primitive types. LLVM's optimizer can treat these as register values and apply standard scalar optimizations. Union types and unknown-typed values (TypeVar) fall back to pointer representation.

## ADR-016: TCO via alloca/loop transform (not trampoline)

**Decision**: Tail-recursive functions are compiled using the "loop transform": parameters are stored in `alloca` slots, the function body is wrapped in a `tco_loop` basic block, and tail self-calls store updated argument values into the alloca slots and branch back to `tco_loop` rather than making a recursive call.

**Rationale**: The alloca/loop approach produces standard LLVM IR that LLVM's optimizer understands — it can apply `mem2reg` to promote the alloca slots to phi nodes, yielding optimal machine code. A trampoline approach (returning a thunk and looping externally) requires a heap allocation per tail call and more complex call-site machinery. The loop transform produces a native loop with no allocation overhead.

**Consequence**: Tail self-calls are identified by `is_tail: bool` in `TypedExpr::Call`, set by the checker when the call is in tail position and the callee is the current function. Non-tail recursive calls and mutual recursion still use normal stack frames. `mem2reg` (run as part of `default<O2>`) eliminates all alloca slots from the final machine code. The spec (§28.3) requires direct self-recursive tail calls to run in constant stack space; mutual tail recursion is not optimised.

**Supersedes an earlier interpreter trampoline.** An earlier design described a `TailResult`/`eval_tail_expr` trampoline in a tree-walking interpreter. That interpreter no longer exists in the codebase — TCO is now realised entirely in codegen by the alloca/loop transform above, which supersedes it.

## ADR-017: Runtime library as a static archive linked into every binary

**Decision**: `lin-runtime` is compiled as a Rust `staticlib` (`crate-type = ["staticlib", "rlib"]`) that provides C-ABI functions (`lin_print`, `lin_string_concat`, `lin_int_to_string`, `lin_array_alloc`, `lin_panic`, etc.). The compile pipeline locates the `.a` file and passes it to the system linker (`cc`) alongside the LLVM-emitted `.o` file.

**Rationale**: LLVM IR cannot express Rust-level operations like `write!` or `alloc::alloc`. The runtime provides these as well-known C symbols that LLVM IR can `declare` and call. A static archive avoids a runtime shared-library dependency on deployed binaries. Using the Rust `staticlib` crate type ensures `rustc` links in all needed Rust stdlib code (allocator, panic handler, etc.).

**Consequence**: Compiled Lin binaries are self-contained: they link against `libc` (via `cc`) plus the runtime `.a`, with no Lin-specific shared libraries required. The runtime is small (~10KB stripped) since it only contains the functions LLVM IR references.

## ADR-018: Closure capture analysis via scope depth tracking

**Decision**: Capture analysis is performed inline during type-checking. When `infer_function` is entered, the current scope depth is pushed onto `function_scope_depths`. During `LocalGet` inference, if the variable's scope depth is less than the innermost function's entry depth, it is recorded as a capture in `capture_stack`. The captures are sorted by `outer_slot` for deterministic codegen.

**Rationale**: A separate capture-analysis pass would need to traverse the typed IR a second time. Doing it inline avoids this while the scope information is naturally available. Scope depth (not slot number) is the right discriminant: variables from the current function's scope are parameters/locals; variables from outer scopes are captures. Stable sorting by slot ensures codegen produces deterministic env struct layouts.

**Consequence**: Closures that capture variables now correctly carry a `captures: Vec<Capture>` list in `TypedExpr::Function`. The codegen heap-allocates environment structs for captured variables and packs `{fn_ptr, env_ptr}` closure values on the heap (not the stack) to support closures that outlive their creating scope.

## ADR-019: Iterator representation as heap-allocated struct; inline for-loop codegen

**Decision**: `range(a, b)` returns a heap-allocated `{i32 start, i32 end}` struct. `for(iterable, body)` is compiled to an inline LLVM loop: for arrays, an i64 index loop with `lin_array_get` element access; for `Iterator<Int32>` (range result), a counted `i32` loop. The `body` closure is inlined — the codegen recognizes `TypedExpr::Function` and `TypedExpr::LocalGet` to avoid creating/calling a closure struct when the body is a literal lambda.

**Rationale**: General iterators need function-pointer dispatch. For the common `range(...).for(i => ...)` pattern, generating a direct counted loop is equivalent to a C `for` loop with no overhead. Array iteration avoids boxing by loading `LinArrayElem.payload` directly. `TypeVar` substitution was added to `infer_call` and `infer_dot_call` to propagate the element type into the body lambda's parameter when the `for` intrinsic's parameter types use `TypeVar`.

**Consequence**: `range(0, n).for(i => ...)` and `arr.for(x => ...)` compile to native loops. The `iter` intrinsic is supported but `map`/`filter`/`reduce` are not yet compiled (runtime panic). Bidirectional type checking was extended (`check_expr` now guides function argument inference using expected parameter types from the call site).

## ADR-020: Concurrency via OS threads

**Decision**: `async(thunk)` spawns a real OS thread. Results are communicated back via `Arc<Mutex<PromiseState>>`. `await` blocks the caller thread until the promise resolves. `ThreadPool` uses `mpsc::channel` with a fixed set of worker threads. `Worker` uses `mpsc::sync_channel` for backpressure.

**Rationale**: OS threads are heavyweight but correct: each thread runs independently with no shared mutable state between concurrent thunks. A true async executor (tokio) would require pervasive `async/await` in the runtime.

**Consequence**: `async` thunks run on true OS threads. `await` blocks the caller thread (not a coroutine yield). Values must be JSON-serializable to cross thread boundaries (spec §24.4).

**Absorbs the cross-thread transfer + thunk-array work.** (1) Cross-thread value transfer uses a JSON bridge: values crossing a thread boundary are serialized to a `JsonValue` enum (`Clone + Send`, no `Rc`/`RefCell`, mirroring Lin's data shapes) and deserialized on the receiving thread, because `Value`'s `Rc<RefCell<…>>` arrays/objects cannot cross threads. `Value::to_json_value()` returns `Err` for non-serializable types, enforcing spec §24.4: functions, iterators, promises, workers, and thread pools cannot cross a boundary. Transfer is O(size) deep copy, the correct cost given the `Rc`-based representation. (2) `async` also accepts an array of thunks `(() => T)[]` — spawning one thread per element and returning an array of promises in input order — so `await(async([t1, t2, …]))` is the natural fork-join idiom.

## ADR-021: FFI via `import foreign` and LLVM `declare`

**Decision**: Foreign function imports use `import foreign "<path>"` followed by an indented block of `val name: Type` declarations. The `foreign` keyword is added to the lexer; the parser reuses the existing indented-block machinery; the AST node is `Stmt::ForeignImport { path, bindings }`. The compiler emits an LLVM `declare` for each binding using the C-ABI type mapping, and library paths collected from `ForeignImport` nodes are passed to the linker step in `lin-compile`. `import foreign "lin-runtime"` is a special reserved path that is always linked and skips normal FFI type validation.

**Rationale**: Reusing `import` as the outer keyword makes foreign imports visually consistent with regular imports; the `foreign` keyword distinguishes them without a separate statement form, and the indented block mirrors function-body parsing (ADR-011). LLVM IR's `declare` is the correct mechanism for external C symbol resolution, and keeping library-path collection in the AST means `lin-compile` can drive the linker without a separate manifest.

**Consequence**: FFI requires `lin build`. The token `foreign` is now a reserved keyword. End-to-end FFI tests compile a C library to `.a` and call it from Lin via `lin build`. The type checker validates that foreign binding types are legal FFI types (numeric, Boolean, Null, or String).

**Absorbs the FFI syntax + call-site validation work.** Two earlier records are folded in here: one recorded the `import foreign "<path>"` syntax + indented-type-block AST shape (now in the Decision above); the other recorded that the checker validates *arity and types* at every foreign call site against the declared `(params) => ret` signature, so `add(1)` against a 2-param `add` is a compile-time arity error rather than a link- or run-time failure.

## ADR-022: `async` var-capture check via global slot tracking

**Decision**: The type checker rejects `async(f)` and `pool.async(f)` calls where the thunk `f` directly references any mutable `var` binding (either captured from a non-global outer scope, or referencing a global `var` from within the thunk body).

Implementation:
- `Checker` gains a `mutable_global_slots: HashMap<usize, String>` field, populated whenever a `Stmt::Var` is processed at global scope (when `function_scope_depths` is empty).
- `first_mutable_capture(expr, mutable_globals)` checks a `TypedExpr::Function` for: (a) any `Capture` where `is_mutable == true`; (b) any `LocalGet` in the body that references a slot in `mutable_global_slots`. Body scanning does not recurse into nested `Function` nodes (inner lambdas have their own capture check when their own `async` call is analysed).
- In `infer_call`, after building `typed_args`, if `func == Ident("async")`, every thunk argument is checked. Same check on the thunk args of `infer_dot_call` when `method == "async"`.
- The check also registers the concurrency builtins (`async`, `await`, `parallel`, `race`, `timeout`, `retry`, `threadPool`, `worker`) as intrinsics in `register_intrinsics()` using `TypeVar`-based signatures, so they resolve instead of producing "Undefined variable" errors.

**Rationale**: Sharing mutable state across OS threads without synchronisation leads to data races. Lin's `var` is captured by `Rc<RefCell<Value>>` in the interpreter and by pointer in the compiler — neither is `Send`. The spec (§24.2) requires a compile-time error. Global vars are not recorded as "captures" (they're accessed directly via `LocalGet` with slot from global env), so a two-pronged check is needed.

**Consequence**: `async(() => counter = counter + 1)` where `counter` is a `var` produces a compile-time error with a help message suggesting snapshot capture. `async(() => message)` where `message` is a `val` is allowed.

## ADR-023: Optional `else` in `if` expressions — implicit `else null`

**Decision**: The `else` branch of an `if` expression is optional. When omitted, the parser synthesizes `Expr::NullLit` at the `if` expression's span as the implicit else branch. The type checker then unions the then-branch type with `Null`, yielding `T | Null` as the expression's type.

**Rationale**: Side-effect-only patterns like `if cond then push(arr, item)` are idiomatic and common in the stdlib. Requiring `else null` is pure noise in these cases — the intent is clear and the result is always discarded. The `else null` pattern also appeared in predicate-style code (`if found == null && f(item) then found = item else null`) where the explicit null was a placeholder with no meaning. Synthesizing `NullLit` at parse time means the AST shape is unchanged — no `Option<Box<Expr>>` needed in `Expr::If` or anywhere downstream.

**Consequence**: The result type widens to `T | Null` when `else` is absent. Code that uses the result of an `else`-less `if` without handling the `Null` case will pass type-checking silently (the union just grows). This is an acceptable tradeoff: the common case (result discarded) gets cleaner syntax, and the footgun (accidentally using a `T | Null` result as `T`) is the same class of error already present whenever any function returns `Null`.

## ADR-024: Memory management — deterministic reference counting, cycles are user responsibility

**Decision**: Lin uses deterministic reference counting (RC) for all heap-allocated values (strings, arrays, objects, closures). RC operations are inserted by the compiler; the runtime provides `lin_string_release`, `lin_array_release`, `lin_object_release`, and `lin_closure_release`. Release functions recurse into heap-typed elements/values so that nested structures are freed correctly. Reference cycles between heap objects are **not** detected and will leak — this is a documented limitation.

**Rationale**: RC is deterministic (no GC pauses), predictable, and systems-friendly. The Perceus approach (Reinking et al., PLDI 2021, used in Koka and Lean 4) shows that compile-time linearity analysis can elide most RC operations, making the overhead negligible for common functional-style code. Cycle detection requires either programmer annotations (`Weak<T>`, as in Swift/Rust) or a runtime trial-deletion pass (as in Nim ORC). Both add complexity. Cycles are uncommon in the data pipeline / request handler patterns Lin targets. The tradeoff is acceptable: correctness for acyclic data (the common case), documentation for the cycle edge case.

**Consequence**: Programs must not create reference cycles between long-lived heap objects if they care about memory usage. The typical fix is to break cycles by setting a field to `Null` before the data becomes unreachable. Future work: `Weak<T>` type (Option B) or ORC-style trial deletion (Option C) can be layered on top without changing the base RC contract.

## ADR-025: Formatter preserves comments

**Decision**: The `lin fmt` formatter (`lin-parse/src/formatter.rs`) preserves `//` line comments (Lin's only comment form). The lexer no longer discards comments: `skip_line_comment` records each one as a `Comment { span, text, own_line }` on a side channel exposed by `Lexer::comments()`, **without** changing the token stream (same kinds, `newline_before` flags, and span lengths — proven by `lin-lex` unit tests). The CLI (`lin-parse::Formatter::with_comments(source, comments)`) reattaches each comment to an AST anchor and re-emits it.

**Rationale**: A `fmt --check` CI gate is unusable if formatting erases the ~1040 comment lines across the stdlib. The chosen design is a span-based reattachment pass, constrained to be idempotent and lossless rather than "heuristic and fragile": comments attach only to a fixed, well-defined set of anchors (every statement in a statement-list slot, every block tail, and each single-expression function body), and the attachment rule is deterministic. The token stream is left untouched (side channel), so no parser/checker/LSP consumer is affected.

**Mechanics**: Own-line comments attach as **leading** lines to the first anchor that starts after them (or dangle at EOF). Trailing comments (code precedes them on the line) attach to the closest anchor lying **entirely on the same source line**; a single-expression function body that is itself control flow (`if`/`match`) is excluded from trailing attachment because its recorded span is a single token and it renders multi-line. A trailing comment with no single-line anchor is demoted to a leading own-line comment of the next anchor (lossless, deterministic). Text is right-trimmed at capture; leading comments sit at the anchor's indentation; trailing comments sit exactly one space after the code.

**Consequence**: `lin fmt` round-trips comments and is idempotent over the whole `stdlib/` + `examples/` corpus with zero comment loss. The one documented imprecision: comments that trail expressions deep inside a multi-line `if`/`match` chain (which have no anchor of their own — only statements, tails, and function bodies are anchors) are hoisted to own-line comments after the enclosing construct rather than staying inline. They are preserved, just repositioned. Example: the per-branch `// q: left +` comments in `examples/raspberry-controller/main.lin` become a block of own-line comments after `applyKey`.

## ADR-026: Default argument values — trailing-comma inversion + per-arity adapters

**Decision**: A parameter may carry a default value (`(a: Int32, b: Int32 = a + 1)`). Optional parameters must be last. Because Lin already gives "supply fewer arguments than declared" a meaning — left-to-right partial application (spec §15.2) — and default values want the *same* call shape to mean "call now, fill the rest from defaults", the two are disambiguated at the call site by an **explicit trailing comma**: `f(x,)` partially applies; `f(x)` is a complete call that fills any omitted trailing defaults (and is an error if an omitted parameter has no default). This inverts the previous rule, where bare under-application curried. `Type::Function` gains a `required: usize` field (count of non-defaulted leading params), excluded from structural compatibility but serialized into module signatures so importers can check arity. Defaults are filled by the **defining** module, not the caller: for a function with optional params, lowering synthesizes one **adapter** per shortfall arity (`f$default{k}`) that binds the omitted parameters to their default expressions and calls the real function. Static calls (direct, dot, imported-by-symbol) route to the adapter by name/id. For the first-class-value path (`val g = f; g(x)`), each default-bearing function gets a static **descriptor** (`{ total, required, entries[] }` of boxed-ABI wrappers) stored at closure offset 32; an indirect under-arity call dispatches through it. The closure struct grew from 32 to 40 bytes (all closures, uniformly, so the runtime frees a single fixed layout); the descriptor is a never-freed static global.

**Rationale**: Synthesizing adapters as `TypedExpr::Function` and lowering them through the normal function path means RC, coercion, and earlier-parameter/chained default references (`(a, b = a + 1, c = b + 1)`) all work for free — defaults are just ordinary expressions evaluated in a scope where the preceding parameters are bound. Filling defaults in the defining module (rather than serializing default *expressions* into `.sig` files for callers to inline) keeps signatures small and makes cross-module defaults work by symbol reference. The trailing-comma marker resolves the currying/default-fill ambiguity at the exact site where intent lives, with zero new tokens. Putting `required` in `Type::Function` but excluding it from compatibility means default-ness never blocks an assignment or argument match — a `(Int32, Int32) => Int32` value is interchangeable whether or not its second parameter had a default.

**Consequence**: Existing code that relied on bare under-application to curry (e.g. `add(10)`) must add a trailing comma (`add(10,)`); within this repo only one example needed migration. The closure ABI change (32→40 bytes) touches every closure allocation site and `lin_closure_release`; all are updated together. A self-recursive *default-fill* tail call cannot use the TCO fast path (it targets a different-arity adapter), so it lowers as an ordinary call. Implementing the indirect path surfaced and fixed a pre-existing bug in the boxed-ABI wrapper: it inferred the Lin return type from the LLVM return kind and treated every pointer return as already-boxed AnyVal, so a function value returning a raw `String`/`Array`/`Object` crashed the indirect caller (which unboxes); the wrapper now takes the real Lin return type and boxes correctly.

## ADR-027: All call paths must coerce arguments to parameter types

**Decision**: Every call-lowering path in `lower_call` (`lin-ir/src/lower.rs`) coerces each argument to the callee's declared parameter type via `lower_call_arg` (which boxes a concrete value to `AnyVal`/`TaggedVal*` when the parameter is union/AnyVal) and retains heap arguments via `retain_call_arg`. This includes the fallback **indirect-call path** — a call through a closure *value* (`val f = ...; f(x)`, a closure passed as a parameter, or any non-statically-resolved callee) — which previously lowered its arguments with a bare `lower_expr` and no coercion.

**Rationale**: Lin's uniform closure ABI passes `AnyVal` parameters as boxed `TaggedVal*`. The named-function and imported-function paths already box concrete arguments (an `Array`, `Object`, or scalar) to match an `AnyVal` parameter; the indirect path is just another way to reach the same ABI and must follow the same rule. The callee's parameter types are read from the callee expression's `Type::Function` signature, identically to the other paths.

**Consequence**: Fixes silent data corruption — before this, an `Array` (or any heap value) passed to an `AnyVal`-typed closure parameter reached the callee as a raw `LinArray*` instead of a boxed `TaggedVal*`. The callee read its tag/payload from garbage, so the value behaved as a different (or empty) object and *mutations through it were lost* (e.g. `push` into an accumulator passed to a stored closure left the original array empty). This is the argument-side analog of the return-side boxing bug noted in ADR-026; together they make the first-class-function/closure path representation-correct for all heap types. Regression: `test_array_passed_to_closure_value_mutates` in `crates/lin/tests/integration.rs`.

## ADR-028: Async concurrency — copy-by-default RC, catchable faults at the thread boundary

**Decision**: Turning the synchronous async stub into real OS-thread concurrency (spec §24) is gated on three model decisions, locked in here (see `docs/ASYNC_DESIGN.md` for the full plan):

1. **RC under threads = Option C (transfer by deep copy) by default, plus two opt-in shared types `Shared<T>` and `Frozen<T>`.** Refcounts stay non-atomic on the single-threaded hot path. Values crossing a thread boundary (a thunk's captured env, and the transferable result returned through a promise) are **deep-copied** so each thread owns a private, disjoint object graph — nothing is shared, so non-atomic RC is sound. The set of boundary-crossing values is exactly the transferable types (JSON-shaped, acyclic, no `Function`/`Iterator`/cycles — already enforced by the checker), so a deep copy is total and bounded. `Shared<T>` (atomic-RC box + `RwLock`, accessor-only, copy in/out) is the escape hatch for shared *mutable* state; `Frozen<T>` (immortal deep-frozen graph, zero-copy lock-free reads via mutation-inference coercion) for shared *read-only* state. Atomic-RC-everywhere (Option A) and dynamic shared-flag RC (Option D) and COW are rejected (§2.3, §2.3.3) — they tax the non-threaded hot path we just optimised.

2. **Catchable faults via a thread-local async-boundary flag.** A runtime fault (`lin_panic`, array OOB, division by zero, non-exhaustive match, null-spread) historically called `std::process::exit(1)` — uncatchable, correct at the top level (spec §20.1). All such sites now route through `crate::fault::runtime_fault(msg)`: inside an async boundary (thread-local depth > 0) it `panic!`s and unwinds to the boundary's `catch_unwind` (becoming an `Error` at `await`, spec §24.2.2); outside, it keeps the `process::exit(1)` behaviour. The spawned thunk runs inside `fault::with_async_boundary`. `lin_exit` (user `exit()`) is unaffected — intentional termination stays a real exit.

3. **`nounwind` is dropped program-wide when the program uses async.** User-emitted Lin functions are marked `nounwind` (sound: value-based errors, frames never unwind) — but a fault inside a thunk now unwinds *through* Lin frames to the boundary, so `nounwind` is unsound for any function reachable from a thunk. We cannot cheaply prove a given function is unreachable from a thunk, so codegen conservatively drops `nounwind` from all user functions whenever the program references any concurrency intrinsic (detected in `lin-compile` by scanning every module's intrinsic map for the `lin_async`/`lin_parallel`/`lin_worker`/… family, which is reachable only through `std/async`). The overwhelmingly common non-async program keeps `nounwind` and its optimisation value (doc §2.4.3 option a).

**Rationale**: The spec's correctness-by-construction guards (`var`-capture ban, transferable-only returns) were designed anticipating threads — they guarantee a thunk shares only immutable, JSON-shaped, acyclic data with its parent, which is exactly what makes Option C's deep copy total and keeps the single-threaded path atomic-free. Catchable faults are the entire point of `async` being Lin's fault-isolation boundary; routing every fault through one helper that branches on a thread-local keeps the top-level `exit` semantics intact while making thunk faults recoverable. The runtime is `panic = "unwind"` (unchanged), so `catch_unwind` works and unwinding crosses the LLVM/Rust boundary; the only requirement is that the Lin frames in between are not `nounwind`, hence decision 3.

**Consequence**: Programs that use async pay a small code-size/optimisation cost (no `nounwind` on user functions) — measured negligible, and zero for non-async programs. Deep-copying large transferable results at a boundary is the cost of Option C; `Shared<T>`/`Frozen<T>` are the escape hatches so we are never forced into all-atomic RC. `Shared<T>` reintroduces deadlock and RC-cycle hazards (documented); `Frozen<T>`'s immortal graphs are never freed (load-once data only). A genuine (non-fault) panic inside a thunk is also caught and surfaced as an `Error` — acceptable, since a runtime bug in a worker should isolate to that worker rather than abort the process. (Implementation note, post-merge with Rust 1.81+: a panic must not unwind out of a plain `extern "C"` runtime fn — the faulting runtime functions and the thunk-call transmutes are `extern "C-unwind"`, and async-reachable Lin frames get `uwtable` so the unwinder can walk through them.)

## ADR-029: `Shared<T>` — opt-in shared mutable state (runtime box; type enforcement deferred)

**Decision**: `Shared<T>` (ADR-028 §2.3.1) is implemented as a runtime box: an **atomic**-refcounted `SharedBox` wrapping an `RwLock` over the inner value (stored as a boxed `TaggedVal*`). Four built-ins, exported by `std/async`: `shared(v)` (deep-copy-in, atomic rc=1), `get(s)` (read lock, deep-copy a snapshot out), `set(s, v)` (write lock, deep-copy in), `withLock(s, f)` (write lock held across `f`, which mutates the inner value in place; `f`'s result is deep-copied out). The box is boxed as `TaggedVal*(TAG_SHARED)`; its retain/release route to atomic `lin_shared_retain_box`/`lin_shared_release_box`, and the thread-transfer copy path **shares** it by an atomic bump rather than copying through (the nesting rule). The inner object graph keeps ordinary non-atomic RC — it is only reachable while a lock is held, so all access is serialized.

**Rationale**: This delivers the load-bearing guarantee — real, race-free shared *mutable* state without taxing the single-threaded hot path (only the box's refcount is atomic; only `Shared` operations take a lock). Copy-in/copy-out at every boundary means no live reference into the inner graph escapes the lock, so the inner non-atomic RC is sound. Validated under ASan and a multi-threaded `#[test]` (8 threads × concurrent get/set) plus a Lin-level concurrent-`withLock`-push test (no lost updates).

**Consequence**: The compile-time **accessor-only enforcement** (rejecting `push(s, 7)`, indexing, auto-unwrap on a `Shared<T>` as a type error) is **now wired** (follow-up landed): a dedicated `Type::Shared(Box<Type>)` variant is threaded through the checker, IR, and codegen. `shared`/`get`/`set`/`withLock` are typed against it (`shared: <T>(T) => Shared<T>`, `get: <T>(Shared<T>) => T`, `set: <T>(Shared<T>, T) => Null`, `withLock: <T,R>(Shared<T>, (T)=>R) => R`); the stdlib wrappers annotate `Shared` (resolvable by name, like `Iterator`); and compat makes `Shared<T>` **invariant** — compatible only with another `Shared<U>` (inner types recursed) and explicitly NOT widening to `AnyVal`/`TypeVar`, so it can't silently flow into an `AnyVal` parameter and lose the guard. Any non-accessor op on a `Shared` value is therefore a type error (`Argument 1 has type Shared<…>, expected …`). At runtime it is still a boxed `TaggedVal*(TAG_SHARED)` (`is_union_type` / `is_union_ty` include it; RC dispatches through the tag-aware path; `capture_kind` → `CAP_TAGGED` so the transfer copy path shares it by atomic bump — the nesting rule). NOTE: `lin check` does not resolve imports, so this enforcement is visible under `lin build`/`lin run` (which do); a bare `lin check` still sees imported names as `AnyVal`. Remaining caveats unchanged: `withLock` mutates in place, so a scalar accumulator (`n => n + 1`) does not persist (use a one-element array or `get`/`set`); `set` collides by name with `std/array`'s `set` when both are imported (alias one); `Shared<T>` makes reference cycles reachable and Lin has no cycle collector (ADR-024); `withLock` reintroduces deadlock potential (no reentrancy, keep critical sections short).

## ADR-030: `Frozen<T>` — opt-in shared read-only state via deep immortal seal (coercion deferred)

**Decision**: `Frozen<T>` (ADR-028 §2.3.2) is implemented as a deep, transitive **immortal seal**. `frozen(v)` (runtime `lin_freeze`, exported by `std/async`) walks the transferable graph rooted at `v` and saturates every heap node's refcount to `IMMORTAL_RC` (string/array/object, recursively). The existing immortal guard on strings is extended to arrays and objects: `lin_array_release`/`lin_object_release` and the array/object arms of `retain_tagged_payload` (and `lin_rc_retain`, already guarded) become **no-ops** when a node's refcount is `>= IMMORTAL_RC`. The thread-transfer copy path shares an immortal array/object by reference (zero-copy), never deep-copies through it. `frozen(v)` returns `v` (now frozen) — the value keeps its plain type, so readers use it transparently.

**Rationale**: The trap with shared read-only data is that a read-only function compiled once against `T` does **non-atomic** `retain`/`release` on its parameter; run on N threads sharing one value, those refcount writes race even though the contents are never written. Making the graph immortal turns retain/release into guarded no-ops that only *read* the sentinel — and a race needs a writer, so concurrent reads of the count are race-free. Therefore the read-only function's existing non-atomic RC runs correctly on a shared frozen value **with no recompilation, no lock, and no atomics**. This is the interned-string immortality trick (already shipped) generalized from one string to a whole graph. Validated by a multi-threaded test (a frozen array read concurrently by N threads) under ASan.

**Consequence**: **Immortal ⇒ never freed.** `frozen` is for load-once, program-lifetime reference data (one O(size) seal at startup); a `frozen()` value created-and-discarded in a loop **leaks** — documented in STDLIB.md. The **mutation-inference read-only coercion** (the §2.3.2 rule that lets a `Frozen<T>` be passed to a `T` parameter *iff the callee doesn't mutate it*, rejecting mutating callees at compile time) is **deferred** — it needs a dedicated `Type::Frozen` variant plus an interprocedural per-parameter mutation-inference pass cached in `ModuleSignature`. Today `frozen(v): T` returns the plain type, so reads "just work", but *mutating* a frozen value is not a compile error — the mutation is silently a no-op on the immortal node (and lost) rather than diagnosed. The runtime immortality/zero-copy-share semantics are fully enforced and safe. A frozen graph is acyclic and immutable, so unlike `Shared<T>` it adds no deadlock and no new cycle hazard.

## ADR-031: Two unary operators — bitwise `~` and logical `!`; unary minus is sugar for `0 - x`

**Decision**: Lin has exactly **two** prefix unary *operators*: bitwise complement `~` (§27.2) and logical not `!` (§8.1). There is **no unary-minus operator** — but a leading `-` on a non-literal is parse-time **sugar for subtraction from zero**: `parse_primary` desugars `-expr` into the binary `0 - expr` (`Expr::BinaryOp { left: IntLit(0), op: Sub, right: expr }`, `lin-parse/src/parser/expr.rs`), so there is no `UnaryOp::Neg` in the AST and no negation typing rule — `-x` *is* `0 - x` and obeys ordinary numeric typing. (A leading `-` directly before digits is instead absorbed into a numeric literal by the lexer, §2.7.) This desugaring predates the `!` work and is relied upon by checked-in code (e.g. `examples/matrix/matrix.lin`'s `-s`). Both genuine unary operators are right-associative and bind tighter than `*` but looser than postfix call/index/dot (§8.2), so `!!x` parses as `!(!x)` and `!a == b` parses as `(!a) == b`. Negated *patterns* (e.g. `is !true`) are explicitly out of scope.

**Rationale**: This supersedes the original v1 design of `~` as the *single* sanctioned unary operator. `!` was added because boolean negation otherwise had to be spelled `x == false` — pervasive boilerplate in stdlib (`std/array`) and user code. It reuses the existing unary pipeline end-to-end: the lexer emits a new `Bang` token, the AST gains `UnaryOp::Not`, IR lowering maps it to the same `crate::ir::UnaryOp::Not` as `~`, and for an `i1` a bitwise-not *is* a logical-not, so codegen's existing `build_not` arm needs no change. When the operand is not statically `Bool` (e.g. a boxed `TypeVar` through a generic lambda), lowering routes it through `lower_cond_as_bool` first to unbox/coerce to a raw `i1`.

**Consequence**: Typing rules differ by operator — `~x` requires an integer and yields that integer type; `!x` requires `Bool` and yields `Bool`; a float operand to `~` (or a non-`Bool` to `!`) is a compile-time error. This supersedes the spec's older "the only unary operator is `~`" / "no unary operators in v1" statements (§2.7, §8.1, §27.2, decision-list #9), now updated to "exactly two unary operators (`~`, `!`); leading `-` is a numeric literal or sugar for `0 - x`". An earlier revision of this ADR (and the matching spec text) overstated the rule as a flat "no unary minus", which read as though `-x` on a computed value was rejected; in fact it has always parsed as `0 - x`, so the wording now distinguishes "no unary-minus *operator*" from "`-x` is sugar".

**Absorbs a duplicate two-unary-operator record**, which restated the same rule and spec-text updates; both are now this one.

## ADR-032: `AnyVal` is a covariant sink — closing the AnyVal→concrete cast hole

**Decision**: `AnyVal` (modelled as `Type::TypeVar(u32::MAX)`, see `types.rs::is_json`) is made a **covariant sink**: anything is assignable *into* `AnyVal` (concrete `T → AnyVal` stays allowed, so `writeJson(value: AnyVal)` and the pervasive "store anything as AnyVal" patterns keep working), but an `AnyVal` *value* is **not** assignable *out* to a fully-concrete **structured object** target — one that (after unfolding `Named` types) is an `Object` with at least one required, non-nullable field. So `val p: Person = readJson(...)` (where `Person = {name: String, age: Int32}`) is now a type error; the value must be decoded via `fromJson` (ADR-033) or narrowed via `is`/`has`. The fix splits the old blanket `(_, TypeVar(_)) | (TypeVar(_), _) => true` arm in `compat.rs` into: (1) `(_, TypeVar(MAX)) => true` (sink), (2) `(TypeVar(MAX), target) => lenient_json || !requires_structured_decode(target)`, (3) the existing permissive arm for all *other* (non-MAX) TypeVars — genuine inference vars, the `9000+` generic slots, intrinsic vars — so inference is unchanged. `requires_structured_decode` deliberately treats only required-field objects as the hazard: `AnyVal` flowing into scalars (`Int32`/`Int64`), buffers (`UInt8[]`), opaque handles, open objects (`{}`), arrays, functions, iterators, or anything still containing a TypeVar stays permissive — those are the language's handle/buffer/polymorphic-return patterns, which have no `fromJson` remedy and predate this change.

**Rationale**: The old rule made `AnyVal` bidirectionally compatible with *everything*, so a value read from an `AnyVal` source could be silently bound to a richly-typed annotation with **zero validation** — the annotation was a lie. Drawing the line at required-field objects catches the real "I claimed this JSON is a `Person`" hazard (the one a decoder can fix) while not breaking the thousands of existing scalar/handle/buffer flows. The leniency is scoped: a per-`Checker` `lenient_json` flag is set **only** for the trusted embedded stdlib (whose wrappers forward `AnyVal` handles into concrete intrinsic/foreign params by design, e.g. `lin_parse_json`, `pathMatch`); user modules and user-defined imported modules always check strictly. After this change the only sound `AnyVal → T` conversions are (a) `fromJson` (validated decode) and (b) `is`/`has` narrowing (a separate `checker/pattern.rs` path that branches on `ty.is_json()` directly and is backed by runtime tag checks, so it stays sound and is unaffected).

**Consequence**: The predicted blast-radius migration sites (`pathMatch`'s `String` params, `lin_parse_json`'s `String` param) did **not** in fact break: their targets are scalars, not required-field objects, so the narrower `requires_structured_decode` rule leaves them permissive — no stdlib widening was needed. The whole stdlib, example suite, and test suite compile unchanged. The scalar/handle escape hatch is a deliberate soundness gap (`AnyVal → Int32` is still unchecked) accepted to avoid a disruptive migration; tightening it later (e.g. a `fromJson` for scalars) is additive. `lin build` of `val p: Person = readJson(...)` (and the direct `val p: Person = jsonReturningCall()` form) now surfaces `Expected type {...}, got ?T4294967295`; the remedy is `Person.fromJson(...)`. (Note: `lin check` of a *single* file leaves imported functions' return types as fresh inference vars rather than `AnyVal`, so the gate cannot fire there for an imported call — the full `lin build` pipeline, which resolves import signatures, is the authority. A bug where a zero-param or all-`AnyVal`-param `AnyVal`-returning function was misclassified as the opaque `Function` annotation — and its `AnyVal` return freshened into a permissive inference var, slipping the gate — was fixed in `infer_call` by requiring the opaque shortcut to have a *non-empty* all-`TypeVar(MAX)` param list.)

**Scope decision — total vs structured (empirically locked)**: a *total* gate (rejecting ANY `AnyVal → concrete T`, including scalars and arrays) was implemented and run against the full suite. It broke (a) the stdlib's pervasive **polymorphic-return idiom** — `slice`/`concat`/`accept`/`wait`/etc. return `AnyVal` and are routinely assigned to concrete `val`s (`val sub: UInt8[] = slice(bytes, 1, 4)`, `val code: Int64 = wait(pid)`, socket `accept(): Int32`) — and (b) **`is`-narrowing into a concrete branch** (`if j is String then j else ""`, whose narrowed value is still statically `AnyVal`). Empirically: `test_is_narrowing_still_works`, `test_slice_preserves_element_type`, `test_net_tcp_loopback_echo`, `test_net_udp_loopback_roundtrip`, `test_proc_spawn_read_wait`, `test_proc_wait_exit_code` all failed under the total gate. These patterns have **no `fromJson` remedy** and forcing one is hostile, so the total scope was rejected and the gate is **scoped to required-field structured objects** — the genuine "unchecked object decode" hazard. This matches the structured-object-only conclusion anticipated in the plan.

## ADR-033: `fromJson` — type-directed JSON decode (descriptor-driven runtime interpreter)

**Decision**: `T.fromJson(json)` (and the equivalent `fromJson(T, json)`) is a **checker special form** that validates an `AnyVal` value against the target type `T` and yields `T | Error`. It is recognised by the surface name `fromJson` at the call site (intercepted in both `infer_call` and `infer_dot_call` *before* arg0/receiver is inferred as a value, since arg0 is a *type*, not a runtime value — so unlike `print`/`for` no `lin_*` wrapper can express it). `std/json` exports a `fromJson` stub purely so the import resolves and `lin check` sees the name; the stub body is never used for real call sites. Validation is implemented as a single generic runtime interpreter `lin_from_json(value, descriptor)` driven by a compact, position-relative byte **descriptor** emitted per call site by codegen (`DescEncoder`), so emitted code is O(1) per site and recursion/cycles are finite back-edges (memoised by named type). The interpreter walks value+descriptor in lockstep, returns the input **cloned** (`lin_tagged_clone`, +1 independently owned) on success or a fresh `Error` on the first structural mismatch, building a JSONPath-ish `path` (e.g. `$.address.city`) during the walk. `Error` is a structural object alias `{ "type": "error", "message": String }` (resolved by `resolve_named_cycle`, not a new `Type` variant — cf. ADR-029), and the runtime error value also carries a `"path"` field, which width subtyping permits.

This ADR records three load-bearing semantic choices and their trade-offs:

- **(a) Union variant selection is FIRST-MATCH-WINS.** A `KIND_UNION` node tries each variant in declaration order and accepts the first that validates. **Trade-off**: for overlapping, non-discriminated object variants the most-permissive / first-listed variant *shadows* more-specific ones — e.g. with `{ "k": String } | { "k": String, "w": Int32 }`, an input that has both `k` and `w` matches the first variant. The *runtime data is fully preserved* (the same value is returned, no fields are dropped), but the *static type* the program reasons about is the matched variant, which may be the wider one. **Recommendation**: give union variants a discriminant field (e.g. a literal `"type"` tag) so exactly one variant matches; list more-specific variants first when overlap is unavoidable. First-match-wins was chosen for v1 because it is predictable, order-explicit, and matches the spec's first-error policy spirit; a "best/most-specific match" rule would be ambiguous and costly.

- **(b) Number policy is target-type-driven.** An **integer** target requires the JSON number to be integral and within the target's width/signedness range (a float like `3.14` is rejected; an integral float like `5000000000.0` against `Int32` is rejected as out of range). A **float** target (`Float32`/`Float64`) accepts any JSON number. An **unconstrained** target (`AnyVal`/a `TypeVar`, encoded as `KIND_JSON`) accepts any number as-is with no narrowing — number-range validation is intentionally skipped there by design. (Note: a bare suffixless integer *literal* in Lin source is typed `Int32` and truncated by the lexer per spec §21 *before* it can reach `fromJson`, so genuine out-of-range integers arriving from real JSON parsing are the cases the range check guards.)

- **(c) `Error` is a structural object alias, but `is Error` IS made to discriminate.** `Error` is `{ "type": String, "message": String }` (open, resolved by `resolve_named_cycle`; the runtime value also carries `"path"`). A *bare* tag check would match any object, so to make the agreed idiom `match result | is Error => .. | is Person => ..` work, **`is Error` is desugared in the checker (`check_pattern`, `Pattern::TypeName == "Error"`) into the value-constrained object pattern `{ "type": "error", "message": _ }`.** This reuses the existing object-pattern lowering (`lower_object_pattern_test`) which checks field presence AND `scrut["type"] == "error"` at runtime — exactly what distinguishes a decode failure (always `"type": "error"`) from a decoded value (any other shape). Standalone `Expr::Is` was routed through the same object-pattern path (its old `IsType` lowering mapped an object pattern to `Type::Never`/tag `0xFF`, which never matched). Exhaustiveness was taught to count this desugared pattern as covering the `Error` union variant. Chosen over adding a dedicated `Type::StrLit` literal-type (which would touch ~20 exhaustive `Type` matches across codegen/boxing/representation — too invasive, cf. ADR-029) and over a new `Type` variant. **Former residual trade-off, now RESOLVED by ADR-036:** when this ADR was written, the standalone expression form `result is Person` compiled to a bare `TAG_OBJECT` check, so it *also* matched the Error object and the `is Error` arm had to come first or a decode failure would route into the `Person` arm and fault on `result["name"]`. ADR-036 makes `is <ObjectType>` check the target's required fields (and types) in **both** the match-arm and expression paths, so `is Person` no longer matches an Error object and the arm order is no longer load-bearing. The `result["type"] == "error"` discriminant still works and remains valid for code that prefers it.

**Rationale**: A descriptor + one generic interpreter (validator strategy C) beats inlining per-site LLVM (code bloat, recursion needs emitted helpers) and per-type generated functions (still heavy IR, forward-decl cycle handling): it keeps generated code tiny, reuses the existing tag/unbox runtime primitives, makes recursion trivial (table indices), and makes the walker ordinary, unit-testable Rust. Returning a **clone** rather than the same retained pointer (a deviation from the original plan note) keeps ownership symmetric: the `fromJson` result is registered `+1 owned` in lin-ir, and the input value temp is independently owned and released by normal liveness — returning the *same* pointer would alias two owners and double-free. Verified under AddressSanitizer: no use-after-free / double-free; the decode-`Error` builder (`make_decode_error`) releases its locally-created key/value strings after `lin_object_set` retains them, so error values are leak-clean (only the program-lifetime interned string-literal cache leaks, as elsewhere).

**Consequence**: The input `AnyVal` is **borrowed** (never consumed); the result is unconditionally a fresh `+1`-owned value (clone on success, fresh `Error` on failure); the descriptor is a static const global (never freed). Array/fixed-array/object/union targets can only be named via a `type` alias or built-in name because the special form requires arg0 to be a bare identifier (`Int32[].fromJson(...)` does not parse; use `type IntArr = Int32[]`). A user who shadows `fromJson` with a *local* binding defers to the normal call path; a global user `fromJson` called with a real type-name arg0 would still be intercepted (an accepted, low-risk corner). `Iterator`/`Function`/`Never`/`TypeVar` target fields encode as `KIND_JSON` (accept-any), since they are not JSON-shaped and have no meaningful structural check.

**Absorbs the discriminant-literal decode fix.** A `Type::StrLit(s)` target field encodes as a dedicated descriptor node `KIND_STRLIT = 10` carrying `{ u16 lit_len, lit_bytes }`, and the runtime interpreter (`lin_from_json`, `lin-runtime/src/decode.rs`) validates the JSON value is a string **and** equals that exact literal (e.g. `expected "alpha" at $.kind, got "beta"`). This replaces the original v1 `KIND_STRING` placeholder, which accepted any string in a discriminant slot and let first-match-wins (a) silently mis-decode a wrong-tagged union (e.g. `Result.fromJson({ "type": "bogus", … })` succeeded). With `KIND_STRLIT`, each union variant's discriminant literal is checked during the probe, so `{ "type": "failure", … }` fails the `"success"` variant and correctly falls through. A plain `Type::Str` field still encodes as `KIND_STRING` (any string), so ordinary string decoding is unchanged; encoder (`codegen/intrinsics.rs`) and decoder are kept in sync.

## ADR-034: Singleton string-literal types

**Decision**: A string literal in **type** position is a singleton type. `Type::StrLit(String)` is a
new `Type` variant (mirrored by `TypeExpr::StringLit` in the surface AST and parsed in
`parser/types.rs` before the `_` fallback). It admits only the one string value. This makes the
spec §19 tagged union discriminate at compile time:

```txt
type Result<T, E> = { "type": "success", "value": T } | { "type": "failure", "error": E }
val r:   Result<Int32, String> = { "type": "success", "value": 1 }   // OK
val bad: Result<Int32, String> = { "type": "nope",    "value": 1 }   // compile error
```

The guiding principle is **"`StrLit` is `Str` at runtime, a singleton at check-time."** A
`StrLit("x")` value is represented at runtime *identically* to a `String` — same `TAG_STR`/6 tag,
same `string_ptr_type` llvm type, same boxing/unboxing, same refcounting, same `toString`. So
nearly every exhaustive `Type` match in `lin-ir`/`lin-codegen` simply grew a new arm grouped with
`Type::Str` (`Type::Str | Type::StrLit(_)`), and a `StrLit` lowers to an owned `Str` temp. The only
genuinely new logic is at check-time.

**This REVERSES the avoidance of a new `Type` variant that ADR-029/049 chose.** ADR-033 explicitly
rejected "adding a dedicated `Type::StrLit` literal-type (which would touch ~20 exhaustive `Type`
matches across codegen/boxing/representation — too invasive)". That cost was paid here, but the
"`StrLit` = `Str` at runtime" mitigation made it mechanical: each of those ~20 sites does *exactly*
what the `Str` arm does, so there is no new representation, no new runtime primitive, and no new RC
class. The three RC classifiers that must stay in lockstep — `lin-ir::lower::is_rc_type`,
`lin-ir::rc_elide::is_rc_type`, and `lin-codegen::types::ty_is_concrete_rc` — all treat `StrLit`
as a refcounted string, and release routes through `string_release` (retain uses the tag-aware
`lin_rc_retain`, identical to `Str`). Validated under AddressSanitizer: a `String`-typed and a
`StrLit`-typed loop (1000 iterations each, build-and-discard) produce an *identical* leak profile
(4140 bytes / 3 allocations — the program-lifetime interned literal cache, leaked by design as
elsewhere), with **no** use-after-free, double-free, or refcount underflow. The §19 divide/Result
example runs and discriminates both branches cleanly under ASan too.

**Compat rules (`compat.rs`, after the `Shared`/`TypeVar(MAX)` arms, before numeric/union/object)**:
1. `(StrLit a, StrLit b) => a == b` — two singletons compatible iff equal.
2. `(StrLit, Str) => true` — a literal widens to the open `String` type.
3. `(Str, StrLit) => false` — load-bearing rejection: an arbitrary string is not statically known
   to equal the singleton, so `val t: Tag = someString` is an error.
4. The `AnyVal`-sink arms are unchanged; an object with a (non-null) `StrLit` field is already treated
   as "structured" by `requires_structured_decode`, so an `AnyVal` value still cannot be silently
   bound to a literal-discriminated object — it must go through `fromJson` or `is`/`has` narrowing.

**Bidirectional refinement (`checker/expr.rs`)**: a bare string-literal *value* still infers to
`String` (`infer_expr` is unchanged — §25). Narrowing happens only in `check_expr` against an
expected type: (a) `Expr::StringLit` against an expected `StrLit("t")` is accepted iff equal and
yields a `StrLit("t")`-typed node (`TypedExpr::StringLit` gained a `Type` field, normally `Str`);
(b) `Expr::Object` against an expected object/union/named type pushes the expected field types down
per-field, and for a union *selects the variant by matching the discriminant literal*, erroring with
the list of valid tags if none match. To make this reach the §19 `divide` body — an
`if/then/else` returning object literals — the expected return type is now pushed into the function
body (and through `if`/block tail positions), but **only when the declared return type mentions a
`StrLit`**, so all other inference and error messages (e.g. "Function body has type …") are
unchanged. This mirrors the existing array-literal refinement pattern.

**`fromJson` validates the exact literal value (ADR-033).** A `StrLit` field encodes as a
`KIND_STRLIT` descriptor node carrying the expected bytes; the runtime interpreter (`lin_from_json`)
checks the JSON value is a string AND equals the singleton, reporting e.g.
`expected "alpha" at $.kind, got "beta"`. This makes `Result.fromJson(...)` reject a wrong
discriminant tag, so the union's first-match-wins probe discriminates variants by their literal tag
(a `{ "type": "failure", ... }` value fails the `"success"` variant's `KIND_STRLIT` check and falls
through to the `"failure"` variant). Superseded the original v1 `KIND_STRING` placeholder.

**Limitations / scope (deliberate, v1)**:
- **`AnyVal → StrLit` stays permissive (unchecked) in user code.** A `AnyVal` value IS assignable to a
  `StrLit` target (`requires_structured_decode(StrLit)` is false — a literal is scalar-like), exactly
  as `AnyVal → Int32` is unchecked (ADR-032 scalar gap). `fromJson` is the validated path. Tightening
  this would diverge from the scalar-gap policy, so it is left consistent by design.
- **Exhaustiveness (Step F) was NOT implemented.** Recognising a literal-discriminated `has`/`is`
  arm as covering a specific union variant is *not* done. This is safe: the existing exhaustiveness
  checker already requires an `else` (or a covering arm) for *any* object-union `has`-match — literal
  or not — and emits a diagnostic when absent; that behaviour is unchanged and consistent. Adding
  partial literal-coverage recognition risked inconsistency for marginal benefit, so it was skipped
  (the §19 examples use `else`, which always satisfies exhaustiveness).
- **Numeric and boolean literal types are out of scope** — only string literals are singletons.

**Consequence**: `lin build`/`lin run` of a wrong-tag object now reports e.g. *"Object does not
match any variant of …; expected a discriminant tag in [\"failure\", \"success\"]"* at the object's
span; a `String → literal` assignment reports *"Expected type \"ok\", got String"*. Literal tags
survive generic substitution (`Result<Int32, String>` and `Result<String, Int32>` both
discriminate), since `substitute` passes `StrLit` through its `_ => ty.clone()` tail. As with
`lin-check` generally, a single-file `lin check` leaves an imported function's return type as a
fresh inference var, so the strictest checking is via the full `lin build` pipeline. Verified: full
suite green (288 integration + 6 + 33 + 7; stdlib 19 files; examples 22 files), plus the new
`examples/result/main.lin` fixture.

## ADR-035: Imported types usable in type position

**Decision**: An `export type Foo = ...` declaration can be imported (`import { Foo } from "m"`,
including `Foo as Bar` aliases) and used in a type annotation in the importing module. Spec §22.3
always promised this ("Types may be imported with the same syntax"), but it was never implemented:
type exports were dropped at the module boundary, so a use-site hit *"Unknown type 'Foo'"*.

**Why it was missing**: a `type` decl produces no runtime code, so the checker resolved it into its
local `TypeEnv::type_decls` and then returned `TypedStmt::Expr(NullLit)` — it left no trace in the
`TypedModule`. `ModuleSignature::from_module` only scanned `TypedStmt::Val`, so the dependent
module's checker (which is seeded from the signature via the `import_types` map) never learned the
name. Value imports worked; type imports silently didn't.

**Mechanism**: mirror the value path one level up, as *module metadata* rather than a statement.
- `TypedModule` gains `exported_types: HashMap<String, (Vec<String>, Type)>` (params + resolved
  body), populated in `check_module` from each `export type` via `env.lookup_type` — alongside the
  existing `intrinsics` metadata map. It is **not** a `TypedStmt`, so `lin-ir` lowering, codegen,
  liveness, and rc_elide are entirely unaffected (they never see it).
- `ModuleSignature` gains `type_exports` (copied from `exported_types`), so dependents that only
  load the `.sig` still get types. Both new fields are `#[serde(default)]`, so stale `.typed`/`.sig`
  caches deserialize as empty and trigger a graceful re-check rather than an error.
- `Checker` gains an `import_type_decls: (module, name) -> (params, body)` input (the type-level
  analogue of `import_types`). `lin-compile` populates it from each import's `type_exports`.
- A new `register_imported_types` pre-pass (run before `forward_declare_types`, since
  forward-declared signatures may annotate with imported types) walks the `Import` stmts and, for
  each binding matching `import_type_decls`, calls `env.define_type(local_name, params, body)`
  honouring `as` aliases.

The body stored is the **fully resolved** type (Named cycle points preserved), so no cross-module
type-env lookup is needed at the use site — it resolves like a local `type` alias. Generic exported
types work because the stored `params` flow through the same `substitute` path as local generics.

**Scope/limits**: registration is scoped to what is imported — referencing `Foo` without importing
it is still *"Unknown type"* (verified by `test_imported_type_unknown_without_import`). Verified:
the web-server example now does `import { HttpRequest as Request, HttpResponse as Response } from
"std/http"` (its local `Request`/`Response` aliases deleted); full suite green; imported types
round-trip through both cold and warm module caches.

## ADR-036: `is <ObjectType>` deep type validation

**Decision**: `x is <Name>` (where `Name` resolves to a non-empty object type, e.g.
`Person = { "name": String, "age": Int32 }`) validates field **types recursively** at runtime, not
merely that the fields are present. The match succeeds only when `x` genuinely conforms to
the target type — so the binding narrowing the matched arm performs is **sound**. The deep walk is
the *same* operation as `fromJson`'s structural validator (ADR-033): it is **reused, not
duplicated**. A new runtime entry point `lin_matches_schema(value, descriptor) -> u8` runs the
existing `validate` walker (`lin-runtime/src/decode.rs`) and returns a bool (`{ let d = Desc { base:
desc }; let mut p = String::new(); validate(value, &d, 0, &mut p).is_ok() as u8 }`); the mismatch
error string is discarded on the cold path. This inherits fromJson's number policy verbatim
(`KIND_INT` integral + width/sign range, `KIND_FLOAT` any number, `KIND_STRLIT` exact value,
`KIND_OBJECT` recursive, `KIND_ARRAY`/`KIND_FIXED`/`KIND_UNION` as in ADR-033) — exactly the
consistent, sound semantics wanted.

**Wiring** (mirrors `Intrinsic::FromJson { target, named_defs }`):
- **Checker** (`checker/pattern.rs`): `check_pattern` for `Pattern::TypeName` whose resolved type is
  a non-empty `Type::Object` now produces a **new typed-pattern variant**
  `TypedPattern::TypeCheckDeep(Type, Vec<(String, Type)>, Span)` carrying the object type plus the
  resolved bodies of every reachable `Named` type (via the existing `collect_named_defs` helper,
  promoted to `pub(crate)`) so IR lowering — which has no type environment — can build the
  (possibly recursive) schema descriptor. Plain primitives, unions, and non-object named types keep
  the bare `TypeCheck` (and its `IsType` tag check); `is Error` keeps its value-constrained object
  pattern (`error_discriminant_pattern`); empty object types `{}` keep `TypeCheck` (bare tag check).
- **A new variant, not a field on `TypeCheck`, was chosen.** `TypeCheck(Type, Span)` is matched in
  ~8 places; adding a field touches all of them invasively. The variant localizes the change: the
  sites that needed updating just treat `TypeCheckDeep` like `TypeCheck` — narrowing
  (`checker/pattern.rs`, narrow to the carried `Type`), zonking (`zonk.rs`, zonk the `Type` and the
  `named_defs` bodies), exhaustiveness (`exhaustiveness.rs`, count it as covering its variant),
  and the IR pattern helpers `pattern_type_check` / `pattern_elem_type` / the no-binding arm in
  `lower.rs`. Only the two `is <ObjectType>` emit sites diverge.
- **IR** (`lin-ir/src/ir.rs`, `lower.rs`, `liveness.rs`): a new instruction `MatchesSchema { dst,
  val, target, named_defs }` (payload mirrors `FromJson`). The two former `is <ObjectType>` sites —
  the standalone `TypedExpr::Is` arm and `lower_match_pattern`'s
  `Is(TypeCheckDeep(..))` — now emit `MatchesSchema` instead of `HasPattern`. `val` is the
  already-boxed-to-AnyVal scrutinee, exactly as the `HasPattern` path boxed it (`box_to_json`);
  liveness treats it as `(uses val, defs dst)`.
- **Codegen** (`codegen/match.rs`, `mod.rs`): `MatchesSchema` reuses `emit_from_json_descriptor`
  (promoted to `pub(crate)`) to emit the same static descriptor global the `FromJson` path builds,
  then calls `lin_matches_schema(val, desc)` and truncates the returned `i8` to a bool. Branchless,
  single basic block — composes inside match-arm test blocks like `compile_ir_has_pattern`.

**RC/memory**: `lin_matches_schema` **borrows** `value` (no clone, no release) and reads a static
const descriptor — no ownership change, low risk. The input value temp's ownership is unchanged from
the `HasPattern` path; the `box_to_json` boxing is still done before `MatchesSchema`.

**Unchanged behavior** (verified): `has { .. }` and inline `is { .. }` object patterns stay
presence + value-constraint (`lower_object_pattern_test`); `is Error` stays the value-constrained
object pattern and still discriminates a decode failure from a decoded value in either arm order;
empty object types `{}` keep the bare tag check; all `fromJson`, async/await `Error`, and
literal-type/union tests stay green.

**Consequence**: `is <ObjectType>` narrowing is now sound — the runtime enforces what the type
narrowing claims. Recursive types (`type Tree = { "value": Int32, "children": Tree[] }`) terminate
because `collect_named_defs` is bounded by a `seen` set and the descriptor encoder memoises Named
nodes as finite back-edges. Number policy is consistent with `fromJson`: `is { "n": Int32 }` rejects
`3.14` (non-integral) and accepts `5.0` (integral). Verified end-to-end via `lin build`: deep
rejection of a wrong (incl. nested) field type, a valid value matching with sound narrowed field
access (`v["age"] + 1` = correct number), `is Error` still discriminating, and recursive `Tree`
validation. Full suite green (302 integration + 6 + 33 + 29 + 7; 42 stdlib/examples test files).

**Absorbs an earlier presence-only check.** An earlier record first made `is <ObjectType>` check required-field
**presence** (closing the unsoundness where `is Person` matched *any* object — including a `fromJson`
decode error or `{ "foo": "bar" }` — and then narrowed the binding to `Person`, so a subsequent
`x["name"]` could null-deref). It was explicitly presence-only: `is Person` on `{ "name": 1, "age":
"x" }` (keys present, wrong types) still matched, deferring field-*type* validation. This ADR closes
that residual gap — a present-but-wrong-typed field now fails too (so `x["age"] + 1` never operates
on a string), while a missing field still fails via the `KIND_OBJECT` walk. Both the match-arm and
standalone-expression forms go through `MatchesSchema`; width subtyping (extra fields) is preserved.

## ADR-037: `std/proc` consolidated into `std/process` (batch + streaming)

**Decision**: There were two documented subprocess modules — a working low-level `std/proc`
(`spawn(argv)` → `Int64`, `readStdout`, `wait` → exit code) and an unimplemented high-level
`std/process` (`exec`/`shell`/`cwd`/`chdir`, `spawn(command,args)`, `wait` → `ExecResult`). They
are merged into a single `std/process` exposing **both** styles:
- **Batch**: `exec(command, args)` / `shell(command)` run to completion and return an
  `ExecResult { status, stdout, stderr }`; `cwd()` / `chdir(path)`.
- **Streaming**: `spawn(command, args)` → opaque `ProcessHandle` (`Int64`); `readStdout(handle, buf)`
  reads the piped stdout incrementally; `kill`; `wait` → exit code (`Int32`).

**Why both, and why `wait` stays exit-code**: streaming and batch don't compose on one handle —
`readStdout` drains the pipe, so a `wait` that returned full stdout (as the doc'd `std/process.wait`
implied) would come back empty. Rather than maintain two registries or silently break streaming, the
batch path (`exec`/`shell`) owns "collect all output", and the streaming `wait` returns just the exit
code. `spawn`/`exec` both take `(command, args)` (the doc'd `std/process` shape); the old argv-array
`spawn(["sh", ...])` form is gone.

**Mechanism**: runtime `crates/lin-runtime/src/proc.rs` → `process.rs`, intrinsics renamed
`lin_proc_*` → `lin_process_*`, adding `lin_process_{exec,shell,cwd,chdir}` (the batch fns build an
`ExecResult` object / run `Command::output()`). Streaming fns keep the monotonic-id `Child` registry
unchanged. `stdlib/proc.lin` → `process.lin`, embedded as `"std/process"` in `lin-compile`. The
stdlib wrappers dogfood ADR-035 (imported types): `ExecResult` is an exported record type and
`ProcessHandle` an exported `Int64` alias, used in the wrapper signatures.

**RC/memory**: `make_exec_result` follows the leak-clean object-build pattern (`fs::make_decode_error`)
— `lin_object_set` retains the key and the value's inner string, so the local `+1` from each
`make_string` is released afterward; the object becomes sole owner and freeing the returned box frees
everything. (The older `make_response_object`/`make_error_obj` skip this; they are program-lifetime
singletons so it never mattered, but `exec` is called repeatedly.) Verified leak-free under
LeakSanitizer (the only residual reports are pre-existing program-lifetime interned string literals).

**Migration**: the lone consumer (`examples/processes`) and the proc tests were updated to the new
`spawn(command, args)` form and `std/process` import. Verified: stdlib + example suites green; three
integration tests (`test_process_spawn_read_wait`, `test_process_wait_exit_code`,
`test_process_exec_and_shell_batch`) pass.

## ADR-038: OS resources are opaque integer fd handles

**Decision**: Operating-system resources exposed by `std/net`, `std/process`, and `std/time`
(timers) are represented to Lin code as **opaque integer handles**, never as runtime object values. A
socket fd is an `Int32` (`udpBind`/`tcpListen`/`tcpAccept`/`tcpConnect` return `Int32 | Error`); a
subprocess handle is an `Int64` (`std/process.spawn` returns a `ProcessHandle`, an exported `Int64`
alias — ADR-037). The integer is meaningful only to the runtime, which keeps the real `fd`/`Child` in
a side table keyed by the integer; user code passes it back to the relevant intrinsic
(`udpRecv(fd, …)`, `wait(handle)`, …). See spec §27.4–§27.6.

**Rationale**: This upholds the §25.1 "no hidden open-handle values" convention already used for
stdin/stdout/filesystem — there is no `Socket` or `Process` object kind to add to the runtime, no new
boxing, and no lifetime/RC story for OS handles. An integer is transferable, comparable, and trivially
representable. Fallible operations return the `T | Error` shape (§25.1); a non-blocking read with no
data yet returns `Null` (not `Error`), so poll loops read naturally (`recv`/`accept` →
`Int32 | Null | Error`).

**Consequence**: No new runtime *kind* is introduced for sockets/processes — they reuse the existing
integer representation (a typed alias like `ProcessHandle` is just `Int64` for readability, not a
distinct kind). The cost is that handles are not type-distinct from ordinary integers; misuse (passing
the wrong integer) is not caught by the type system, consistent with the deliberately-unsafe nature of
the low-level layer. The `Int32`/`Int64` split is pragmatic: fds fit in 32 bits, process handles use 64
to match the platform child representation.

## ADR-039: Share-nothing concurrency — no Mutex/atomics primitive

**Decision**: Lin provides **no** shared-memory concurrency primitives (mutexes, atomics,
cross-thread shared mutable cells). Cross-thread mutable state is modelled exclusively with a
`Worker<Msg, Reply>` (§24.6) that owns the state and serialises all access through its single-threaded
message queue. Spec §27.10 records this as a deliberate absence.

**Rationale**: The concurrency model is share-nothing (§24): `async` thunks and `parallel` may not
capture `var` bindings (compile-time error, ADR-022), and transferred values must be JSON-compatible. A
`Worker` owning its state and processing messages one at a time preserves that invariant — there is no
concurrent access to the worker's closed-over `var`, so no data race is possible without ever
introducing a lock. Adding a `Mutex`/atomic primitive would reintroduce exactly the data-race surface
the model is designed to exclude, and would need a new opaque runtime kind plus a poisoning/lifetime
story.

**Consequence**: Patterns that would use `Arc<Mutex<T>>` in Rust (a shared counter, a connection pool,
a discovered-peer-address cache) are expressed as a `Worker` whose `onMessage` handler closes over the
state (§24.6.4). This is more message-passing boilerplate than a shared cell, accepted as the price of
guaranteed freedom from data races. Single-threaded mutation via `var` is unaffected; the restriction
applies only across thread boundaries.

## ADR-040: Flat unboxed arrays for scalar element types

**Decision**: An array whose element type is a fixed-width scalar — `Int8`/`Int16`/`Int32`/`Int64`,
`UInt8`/`UInt16`/`UInt32`/`UInt64`, `Float32`/`Float64` — is stored as a **packed, unboxed,
contiguous buffer** (one element-width slot per element, no per-element tag), not as an array of boxed
`TaggedVal`s. The runtime provides a flat variant per family
(`lin_flat_array_alloc_{i8,i16,i32,i64,u8,u16,u32,u64,f32,f64}` and matching push/index). A `UInt8[]`
is therefore a literal byte buffer (spec §27.1). Semantically these remain ordinary `T[]` arrays —
every array operation (literals, indexing, in-place write, `length`, `push`, `slice`, `concat`, `==`,
the `std/array` combinators) works identically; the representation is an implementation detail.

**Rationale**: Byte/scalar buffers are the substrate for binary protocols, `std/bytes`, and socket
I/O (`recv` fills a caller-owned `UInt8[]`). Boxing each byte as a tagged value would cost ~16× the
memory and defeat the point. Because the element type is statically known, codegen selects the flat
representation with no runtime tag dispatch. Mixed/`AnyVal`/object arrays keep the boxed tagged
representation; only statically-scalar element types go flat.

**Consequence**: Codegen and the runtime must convert between flat and tagged forms at boundaries where
a flat array meets an `AnyVal`/dynamic context (`lin_flat_to_tagged_*`, used e.g. by `toString` and
dynamic length). The flat/boxed distinction is why `concat`/`slice` are flat-representation-aware:
slicing/concatenating a `UInt8[]` yields a `UInt8[]` whose elements read correctly, not a boxed array
of zeros. `is_flat_scalar` (codegen) is the single predicate deciding the representation and must stay
consistent with the runtime's family set.

## ADR-041: Closures OWN their captures (retain on capture, release on free)

**Decision**: A closure's environment now OWNS one reference per heap/union capture — the same ownership rule arrays and objects already follow for stored elements. At `MakeClosure` the lowerer takes ownership of each capturing value (concrete rc → `Retain` in place; union/`AnyVal` → `CloneBox` so the env holds its own `TaggedVal*`); `lin_closure_release` releases them when the closure is freed. Mutably-captured `var` bindings are unchanged: they store the heap **cell pointer** (shared by reference, ADR-012) and keep their existing borrow-only / `FreeCell` / escaping-cell lifecycle — the env does not own the cell. Scalars need no ownership.

To let the runtime release captures, every closure carries a **capture descriptor** at closure offset 40 (`{ u32 count, u8 kinds[count] }`, a static read-only global): one `CaptureRelease` byte per capture (None / Str / Array / Object / Closure / Tagged). The closure struct grew from 40 to 48 bytes (`CLOSURE_SIZE`); `alloc_closure`/`store_capture_descriptor` centralise the layout, and `lin_closure_release` frees 48 and walks the descriptor (mirroring the recursive element release in `lin_array_release`/`lin_object_release`). Partial-application closures keep borrow semantics with a null capture descriptor. The async thread-transfer path (ADR-027) reads the same descriptor (passed explicitly from the closure, no longer stored at env offset 0) and reuses its codes.

**Rationale**: Captures were **borrow-only** — the env stored a borrowed pointer with no retain, and `lin_closure_release` freed nothing. That is sound only while a closure cannot outlive its captured values' scope. The `safe_callback_depth`/escaping-cell analysis covered that for mutable `var` cells, but immutable value captures had no ownership at all, so a closure that ESCAPES (e.g. returned from a `map`/`filter` callback into the result array) dangled: `map(xs, i => () => i)` then called a thunk returned garbage (`[[object]…]`) because the captured element box had been freed at end-of-iteration and its memory reused. Making the env an owning container, exactly like arrays/objects, fixes this uniformly and reuses the existing store-side discipline (`transfer_into_container` / `own_for_store`).

**Consequence**: `map(xs, i => () => i)` and any escaping capturing closure are now correct. **Performance is unchanged**: the added retain/release pairs are elided by the existing RC-elision pass for the overwhelmingly common non-escaping combinator-callback case (closures created and consumed in one scope) — a before/after benchmark (`benchmarks/closures.lin`, capturing `map`/`filter`/`reduce` over 2M elements) showed no measurable difference, and the full benchmark suite was flat. The cost lands only on closures that genuinely escape, where it is required for correctness. The closure struct is 8 bytes larger (40→48); `CLOSURE_SIZE` in `lin-codegen/src/codegen/call.rs` and the free size in `lin-runtime/src/memory.rs` must stay in lockstep. Verified under ASan (stdlib + every example-project test suite — heavy closure users — show no use-after-free or double-free; the only leak is the pre-existing exit-time top-level-`val` leak, identical for non-capturing `map`).

### `concat` retains copied elements (move-vs-retain split in array element copy)

The same owned-vs-borrowed discipline governs how `concat` copies array elements. When
`lin_array_concat_dyn` copies elements from a **borrowed** source array (the
tagged-element path, `elem_tag == 0xFF`), it **retains** each element's heap payload via
`lin_array_concat_into_retaining`, so the result array and the source array are independent owners.
The non-retaining `lin_array_concat_into` (a raw 16-byte `TaggedVal` move, no retain) is **kept** and
used only where the source is a fresh temp whose ownership is transferred — `concat_dyn`'s
widened-flat path, where `lin_flat_to_tagged_*` boxes raw scalars at `+1` and the temp array is then
`lin_array_free`d (which frees only the struct + buffer, never the element payloads, so the boxes are
correctly *moved* into the result).

The old `concat_dyn` used the move-copy (`lin_array_push_tagged`, raw 16-byte copy
without retain) for **every** path, including borrowed sources. So `concat(a, b)` left `a`/`b` and the
result sharing each element at one refcount; releasing any of them (e.g. `acc = concat(acc, […])` in a
loop, which frees the old `acc`) freed the shared payload out from under the result — a genuine
use-after-free, ASan-confirmed (`heap-use-after-free in lin_string_release`). It was
masked in practice only because string *literals* are interned with immortal refcounts; `concat` of
computed strings/objects corrupted the heap. `lin_array_push_tagged` itself MUST stay non-retaining —
its other callers (`io`/`fs`/`json`/`async_rt`/`frozen` building arrays from freshly-owned values, and
the `map`/`minBy`/`maxBy` element-move convention noted in `lower.rs`) deliberately rely on the move
to transfer ownership; adding a retain there would leak. The fix therefore lives in a *separate
retaining copy primitive* selected per-source-ownership inside `concat_dyn`, not in the shared push.
The move-vs-retain split is the load-bearing invariant: a future change must keep "copy from a
still-live borrowed array → retain; copy from a fresh temp being freed → move". Regression:
`test_concat_fresh_strings_no_use_after_free` (40-iteration growing concat of interpolated strings).
The analogous move-without-retain residual leaks in the `for`/`map` element-shell path (`lower.rs`)
are a distinct, pre-existing issue.

`std/array`'s `append`/`prepend`/`groupBy` follow the same discipline, backed by RC-self-contained runtime intrinsics rather than pure-Lin loops: `lin_array_append_dyn`/`lin_array_prepend_dyn` (`array.rs`) and `lin_object_get_or_insert_array` (`object.rs`), all ordinary `import foreign "lin-runtime"` symbols (ADR-008). `append`/`prepend` **preserve the input's element representation** (ADR-040) — a flat scalar source yields a flat result (coercing the item via `lin_push_dyn` + bulk `concat_into`), a tagged/`AnyVal[]` source yields a tagged result — fixing a latent bug where the old Lin loop allocated a *tagged* array and silently boxed flat bytes. They are hand-rolled (not composed on `concat_dyn`) because `concat_dyn`'s tagged path copies element pointers *without* retaining; the intrinsics instead copy through `lin_push_dyn` (which retains) and retain the item, so the result owns its own +1 per heap element and releases independently. `get_or_insert` does a single hash lookup, retaining the interior group array (or allocating+inserting it) and returning an owned box that `push` mutates in place — making `groupBy` one lookup+push per item.

## ADR-042: Numeric literal suffixes honoured; large bare literals widen, never truncate

**Decision**: Two related fixes to integer-literal typing, both making the implementation match spec §2.6/§21:
1. **Type suffixes are honoured.** `42i8`, `5u64`, `3.14f32` etc. now pin the literal's type, overriding context/default. Previously the lexer recognised the suffix characters but *discarded* them ("we just consume them"), so `1705314600000i64` was an indistinguishable bare `IntLit` that defaulted to `Int32` and **silently truncated** to its low 32 bits (`212583488`).
2. **A bare literal beyond `Int32` widens its default instead of truncating.** With no surrounding context, a suffixless integer literal still defaults to `Int32` when it fits; when it exceeds `Int32`'s range it defaults to the smallest type that *preserves* the value (`Int64`, or `UInt64` for a decimal above `i64::MAX`). It is never silently truncated.

**Why widen rather than error.** An earlier attempt made an out-of-range bare literal a compile error ("annotate or suffix it"). That broke ergonomic, previously-working code: call arguments are inferred context-free first and *then* re-typed to the parameter width (`checker/call.rs`), so `format(1705314600000, …)` (an `Int64` param) legitimately relies on the literal surviving inference. Widening the default preserves the value for that downstream re-typing while still fixing the truncation; a genuinely-too-big-for-any-type case can't arise (the lexer already maps `> i64::MAX` decimals to the `UInt64` bit pattern). A literal assigned where an *incompatible* concrete type is required (e.g. `val x: Int32 = 5i64`, or `[256]: UInt8[]`) is still a hard error — that path was already range-checked and is unchanged.

**Mechanism**: a shared `NumSuffix` enum in `lin-common` is parsed by the lexer and carried on `TokenKind::IntLit`/`FloatLit` → surface `Expr::IntLit`/`FloatLit` (the typed IR already carries a resolved `Type`, so the suffix stops at the checker). `checker/helpers.rs` gains `suffix_to_type`, `default_int_literal_type` (the Int32→Int64→UInt64 widening ladder), and `check_int_literal_fits` (extracted from the old inline range check). `check_expr` keeps context-typing for suffixless literals; a suffixed literal flows through `infer_expr` (typed at its suffix type) and the normal compatibility tail validates it against the expected type. The formatter round-trips suffixes.

**Consequence**: `1705314600000i64` and `val ts: Int64 = 1705314600000` and a bare `1705314600000` all preserve the value; `val x: Int32 = 5i64` is a type error; small suffixed literals (`200u8`) type at their width. Covered by `stdlib/number.test.lin` (suffix preservation + arithmetic round-trip) and integration tests (`test_i64_suffix_preserves_large_literal`, `test_int64_annotation_preserves_large_literal`, `test_bare_literal_overflowing_int32_preserved`, `test_suffix_overrides_expected_context_conflict`). Full suite green; surfaced and fixed during the `std/time` work, where `format(<ms>, …)` first exposed the truncation.

## ADR-043: Cross-module generic instantiation materializes in the IMPORTING module

**Decision**: A generic `val` function (`<T>(x: T): T => x`) defined in an IMPORTED module is monomorphized in the **importing** module's lowering, not the defining one. `lin-compile` threads the already-typed `imported_modules` map into `lower_module_with_imports` → `monomorphize_with_imports`. The pass discovers a call to an imported generic (the importer's `ImportSlot.ty` is a `Function` with a generic TypeVar in its **parameters** — a return-only TypeVar, as on stdlib intrinsic wrappers like `iter: (…) => Iterator<T>`, does NOT count), clones the generic body out of the imported `TypedModule`, substitutes its quantified TypeVars at the concrete call instantiation, **re-homes** the body into the importer, and emits it as a local specialization (`id$Int32`) that the call is rerouted to.

Re-homing (`rehome_imported_body`) rewrites the cloned body's slots: every body-local slot (params, inner `val`/`var`, destructure targets) is remapped to a fresh importer slot (so it can't collide with the importer's own slots or another specialization), and every FREE reference to the origin module's scope is rewritten into an importer-side construct the importer's lowering already resolves — a sibling function/val → a synthesised `TypedStmt::Import` (a `Named` call to `{origin_key}_{name}`), an intrinsic → a merged intrinsic slot, a **thin intrinsic wrapper** (`for = (it,f) => lin_for(it,f)`) → the intrinsic itself (inlined, preserving the polymorphic builtin's concrete-element dispatch), a foreign binding → a `ForeignImport`. An import-of-import resolves to the SOURCE module's symbol, never the intermediate's. Imported modules also monomorphize their OWN sibling generic calls during `lower_import_module` (`monomorphize_import`), keeping ALL generic originals so external importers that don't specialize still resolve the boxed `{module_key}_{name}` symbol.

Two supporting fixes: (1) `subst_expr` now substitutes the declared-type field of statements inside a block (`subst_stmt_types`) — a `var acc: U` in a generic body otherwise kept `ty: TypeVar(U)`, producing a boxed-union cell while the substituted closure that captures it read the concrete type (a misaligned-pointer crash). (2) the checker's `infer_function_with_hints` now surfaces a lambda's CONCRETE body type when the expected return is a quantified generic param (id ≥ 9001), so a higher-order generic call (`mymap(arr, x => x*2)` with `f: (T) => U`) can bind `U` from the lambda body; the bare-TypeVar boxing convention is retained for the AnyVal/`Function` polymorphic-slot case.

**Rationale**: The importer has the full `TypedModule` for every import (not just the signature), so it can see imported generic BODIES — the only place with both the body and the concrete call types. Specializing there avoids touching the imported module's compilation/caching and avoids cross-contamination (each importer derives its own specializations from the cached generic body; the `.lin-cache` stores the TypedModule with the generic body intact, keyed by source hash regardless of how importers instantiate). The no-op invariant is preserved: `module_uses_generic` gates the whole pass, so a module that neither defines nor imports a param-generic function lowers byte-for-byte as before (verified: `benchmarks/array_pipeline.lin` IR is byte-identical to baseline).

**Consequence**: User-defined cross-module generics — including higher-order ones with the `map` shape (`<T,U>(arr: T[], f: (T) => U)`) — specialize to native, unboxed code in the importer (e.g. `id$Int32` is `define i32 @"id$Int32"(i32)`). Verified end-to-end (output + IR proof + ASan, no UAF/leak) and across the cache (two importers using one imported generic at different element types each get correct specializations). **Converting stdlib `map`/`filter`/`reduce` to generic was attempted and DEFERRED at the time**: the specialized bodies are themselves nearly box-free, but (a) the result of a `[]`-plus-`push` build is a *tagged* array while a static `U[]` result type makes consumers read via the *flat* ABI — a representation mismatch — and (b) the per-element boxing at the `lin_for` callback boundary remains (the closure ABI passes a `TaggedVal*`), so the static box count rose and ~20 of the diverse stdlib/example uses regressed. The full flat/zero-box pipeline needed closure-callback ABI specialization — which later landed in ADR-044. stdlib `array.lin` was therefore left `AnyVal`-typed here; the cross-module infrastructure is in place for when that lands.

## ADR-044: Generic `map`/`filter`/`reduce` + a capture-less-lambda inliner — the zero-per-element-box array pipeline (10x at -O2)

**Status.** Accepted. This is the shipped completion of the zero-per-element-box array pipeline.

> **Updated (2026-06-11).** The "a capturing lambda is NOT inlined — it falls through to the
> boxed closure path" restriction below (items 2–3) no longer holds: **capturing literal lambdas
> now also inline at the Layer-1 combinator gate** (`perf/gate-divergence-v2`, re-landed `cbd37826`).
> An earlier admit attempt was reverted as a leak; the real fault was a *stack* overflow (a per-iteration
> `alloca` emitted into the loop body), fixed by hoisting the scratch alloca to the entry block
> (`entry_block_alloca`). A *stored/passed* `Function` value still falls to the boxed closure path; the
> devirtualizable subset of *named* callbacks is attacked separately by Wave C (see ADR-065). The
> hand-rolled per-combinator loop emitters this ADR describes were also unified behind one
> `emit_combinator_loop` scaffold this session (byte-identical; ADR-065) — a refactor under the same
> decision, not a change to it.

**Context (the LINCHPIN goal).** The generics/perf milestone targets ZERO per-element boxing in a
monomorphic array pipeline `range(0,n).map(x=>x*2).filter(x=>x%3==0).reduce(0,(a,x)=>a+x)`. The
blocker is the UNIFORM ALL-PTR BOXED CLOSURE ABI: a closure's stored `fn_ptr` is a `__cls_wrapb_*`
wrapper `ptr(ptr env, ptr boxedArg…) -> ptr boxedRet`, so a combinator calling its callback via
`CallTarget::Indirect` always boxes each element and unboxes the result.

**What shipped.**
1. **Generic signatures** (`stdlib/array.lin`): `map<T,U>(arr:T[], f:(T)=>U): U[]`,
   `filter<T>(arr:T[], f:(T)=>Boolean): T[]`, `reduce<T,U>(arr:T[], init:U, f:(U,T)=>U): U`. At a
   monomorphic scalar call site the checker types the callback param at the concrete element type
   (no input boxing) and the monomorphizer picks the flat (e.g. `$Int32`) specialization.
2. **Call-site combinator-wrapper inline** (`lin-ir/monomorphize.rs`, `try_inline_combinator_wrapper`):
   when a call targets a generic thin intrinsic-combinator wrapper (`map`/`filter`/`reduce`, whose
   body is exactly `lin_map`/… forwarding its params) AND the callback argument is a CAPTURE-LESS
   LITERAL lambda, the call is rewritten in the calling module to a direct `lin_map(arr, <lambda>)`
   (re-homing the intrinsic slot), so the literal lambda is VISIBLE to the intrinsic's IR lowering.
   A capturing lambda or a stored/passed `Function` value is NOT inlined — it falls through to the
   normal closure-call specialization (still correct, just boxed).
3. **Capture-less-lambda inliner in `lower_map`/`lower_filter`/`lower_reduce`** (`lin-ir/lower.rs`,
   `inlinable_lambda` + `inline_lambda_body`): when the callback arg is a capture-less literal
   `Function`, its body is spliced directly into the loop — param bound to the flat element temp,
   body lowered inline — with NO closure alloc and NO per-element box/unbox/indirect call. `reduce`
   additionally carries a CONCRETE-SCALAR accumulator UNBOXED through the loop phi (gated on a scalar
   `result_type`; a union/heap accumulator keeps the boxed AnyVal-phi path). RC: unboxed scalar
   elements/results carry no refcount, so there is no per-iteration box to release.

**The two regressions and how they were solved (correctness-first).**
  - **R1 (`examples/report`):** `validRecords()`/`parseErrors()` ended `…filter(…).map(r=>r["value"])`
    over a `Success | Failure` union; with precise generic `map` the element types as `Record | Null`
    (the union member access is nullable, `filter` does not narrow), not assignable to `Record[]`.
    SOLVED by restructuring the example to be well-typed under precise generics: the pipeline narrows
    the union via an idiomatic `match … has { "type": "success", value } => value; else => …` and
    maps through it. (The narrowing is written inline inside `validRecords`' `.map(…)` callback — a
    multi-arm `match` inside a combinator's parentheses parses fine via offside-rule arm collection,
    commit 286cfd2; no helper indirection is needed. The earlier draft of this note claimed inline
    `match` was impossible under ADR-003/014 indentation suppression — that no longer holds.)
  - **R2 (`sortBy`/`minBy`/`maxBy`/`compact`/`sum`/…):** these stdlib combinators call `map`/`reduce`/
    `filter` internally over `AnyVal[]`; a SIBLING call to the now-generic combinator specialized at
    `$AnyVal` cross-module, where the combinator's owned-array result and the surrounding generic body's
    RC accounting disagreed — a double-release of the intermediate array (capacity-overflow crash).
    SOLVED by keeping those internal callers on non-generic AnyVal helpers (`_mapJ`/`_filterJ`/
    `_reduceJ`, thin wrappers over the same intrinsics). The generic exports still give the zero-box
    fast path at the user's monomorphic call site.

**Two supporting fixes (latent gaps the generic conversion exposed, both general correctness wins):**
  - **Cross-module generic specialization for IMPORTED modules** (`lower_import_module_with_imports`,
    `monomorphize_import_with_imports`): a module that imports a generic AND is itself imported (e.g.
    `examples/report` calling `std/array.reduce`) previously did not specialize its cross-module
    generic calls — they fell to the boxed type-erased original (returns `AnyVal`), crashing a concrete
    scalar use site (`ret i32` vs `ptr`). The import path now monomorphizes with the program's imports
    map, exactly like the top-level importer.
  - **`repoint_call_native` re-coercion**: when a native specialization returns a CONCRETE scalar but
    the checker left the Call's `result_type` as the boxed/erased generic return (the `U` TypeVar
    surfaced as `AnyVal`, e.g. `total = s` with `total: AnyVal`), the Call is wrapped in a `Coerce {
    concrete → original }` so the boxed/unboxed handoff is explicit. Without it the consumer emits
    `store i32`/`ret i32` against a `ptr` slot (a hard codegen type mismatch).

**Result — the HONEST verified release number.** array_pipeline output 1892804906 (unchanged). The hot
map/filter/reduce loops are now FULLY UNBOXED in `main` — flat `lin_flat_array_get_i32`/`push_i32`, a
native `mul`/`srem`/`icmp`/`add`, and the reduce accumulator carried as a native `i32` through the loop
phi — ZERO `lin_box_int32`/`lin_unbox_int32`/closure-call per element. Interleaved RELEASE (`-O2`)
min-of-11, same machine, base vs after: **~328ms → ~33ms = ~10.0x** (verified twice: 10.06x, 9.95x).
This is a real `-O2` speedup, NOT a debug artifact — at `-O2` LLVM cannot elide the boxed closure ABI's
per-element malloc/indirect-call the way it elides cheaper boxing, so eliminating the closure call
itself is what unlocks the win.

**Correctness + safety.** stdlib+examples 59/59; integration 357/0 (isolated). map/filter/reduce
verified over `Int32[]` (flat), `String[]`/`Float64[]` (tagged/flat), `AnyVal[]` (heterogeneous);
capturing lambda → closure path (correct); stored-fn-value callback → closure path (correct); chained
pipeline; non-scalar (array/string) reduce accumulator → boxed AnyVal-phi path (correct). ASan-clean over
the full stdlib+examples leg + flat/tagged/capturing/mixed/sortBy/churn fixtures. No-op invariant: a
non-combinator program's MAIN module IR is BYTE-IDENTICAL to base (only `std/array` differs).

**R2 bug fix — `filter` over an object/heap array double-freed kept elements (the parked segfault).**
The original commit made `filter` produce a result array of the SOURCE's concrete element type. For a
CONCRETE-rc element (object/array/string — e.g. `std/test`'s `Assertion[]`, or any `Object[]`),
`filter`'s keep path pushes the element it READ from the source array via `Push`. For a concrete tagged
element that intrinsic lowers to `lin_array_push_tagged`, which raw-copies the 16-byte `TaggedVal`
WITHOUT bumping the inner refcount (MOVE — correct only for a freshly-owned value). But filter's element
is BORROWED (still owned by the source array), so source and filtered array both referenced the same
object at refcount 1, and releasing both double-freed it → heap-use-after-free (`lin_object_release`),
surfacing as the `examples/*/*.test.lin` segfault via `std/test`'s `results.filter(a =>
a["type"]=="fail")`. Fix: `push_output` takes a `borrowed` flag; `filter` passes `borrowed: true` and
emits a `Retain` first on a tagged concrete-rc push so the result owns its own reference; `map` (pushing
the lambda's freshly-owned result) passes `borrowed: false` and keeps the MOVE. Union elements
(retaining `lin_push_dyn`) and flat scalars (no refcount) need nothing, so the flat-scalar pipeline win
is untouched and the object-array inline win is RETAINED. Verified: stdlib+examples 59/59; integration
357/0; ASan-clean across every example-project test + a `filter`-over-`Object[]` fixture.

**Consequences.** The zero-per-element-box array pipeline ships at a verified ~10x `-O2` speedup.
Generic `map`/`filter`/`reduce` + capture-less-lambda inlining is the mechanism; the internal AnyVal
helpers and the union-narrowing example rewrite keep every heterogeneous/union call site correct; the
import-path monomorphization + native-return re-coercion are general fixes for cross-module generic
calls. Capturing lambdas and stored-fn callbacks keep the (correct, boxed) closure path.

**Absorbs the three staging steps of this saga** — now folded in as the shipped final state:

- **(staging step 1) Flow-typing refinement pins generic combinator element types so alloc-builder bodies emit FLAT arrays.** A generic combinator returns `U[]`, but the only fully-controlled allocation intrinsic, `lin_array_allocate`, infers to the AnyVal-wildcard `Array(TypeVar(MAX))`; monomorphization's `subst` rewrites `U` but never the MAX wildcard, so the allocation stayed TAGGED while a `U=Int32` consumer read it FLAT (garbage). The checker now refines the wildcard element of a fresh `lin_array_allocate` to the function's declared-return element — both for a direct `=> lin_array_allocate(n)` body and for the realistic intermediate-binding shape `val result = lin_array_allocate(n); …; result` — so `subst` pins `Array(Int32)` and `is_flat_scalar` emits a flat allocation matching the flat reader. STRICTLY gated to `lin_array_allocate` (every other `AnyVal[]`-returning call stays tagged); a user annotation on the binding wins. The write uses the representation-aware `lin_array_set`, so producer/writer/reader all agree.
- **(staging step 2) A `AnyVal` argument binds a generic `T[]` param to the AnyVal wildcard, and the import path erases leftover inference TypeVars to AnyVal.** `collect_type_subs` (lin-check) and `collect_subs` (lin-ir) gained `(Array(pt), TypeVar(MAX))`/`(Iterator(pt), TypeVar(MAX))` arms so an `AnyVal` array argument binds `T=AnyVal` (a representation-consistent TAGGED `$AnyVal` monomorph) while an `Int32[]` argument still binds `T=Int32` (FLAT `$Int32`). And `monomorphize_inner` runs every binding through `erase_nonconcrete_typevars`: a leftover/unsolved INFERENCE TypeVar (id `< GENERIC_TV_BASE`) is rewritten to `TypeVar(MAX)` before keying a specialization, so the import path can never emit a `$T<id>` garbage monomorph (which had read/allocated at a bogus element type → capacity overflow / heap corruption). A QUANTIFIED generic id is left untouched so a genuinely-unconstrained param still gets the clean "cannot infer a concrete type" diagnostic.
- **(staging step 3) Route `map`/`filter`/`reduce` through the materializing `lin_*` intrinsics with representation-safe element reads.** Before the inliner landed, the three combinators were first switched from hand-written `for`/`push` loops to thin wrappers over `lin_map`/`lin_filter`/`lin_reduce`, whose lowering allocates a flat output for a flat-scalar result and reads flat only from a PROVABLY-FLAT producer (`is_provably_flat_producer` — a `range`/`map`/`filter`/flat-alloc call or a non-empty scalar array literal); otherwise it reads via the runtime-`elem_tag`-aware `lin_array_get_tagged`, keeping `[]`+push arrays (which allocate TAGGED even when typed `Int32[]`) sound. This step also routes a push into a statically-flat array through `lin_push_dyn`, and fixed three latent bugs the routing exposed: a curried callback at full arity returning a function was wrongly treated as under-application (now disambiguated by arg-count vs declared arity); `emit_index_loop`'s phi back-edge hard-coded the body block (now patched to the block that actually jumps back, needed for filter's keep/skip split); and `collect_type_subs` gained `Array`↔`Iterator` cross-unification so a generic `T[]` param applied to `range(0,n)` binds `T=Int32`. At `-O2` this intermediate step was perf-NEUTRAL on its own (LLVM already elided the cheaper boxing); the real win came only once the closure call itself was eliminated by the inliner above.

**Also absorbs the earlier direct-index-accessor genericization (Phase 6).** A first, conservative pass had already genericized only the **direct-index accessors** — `at: <T>(arr: T[], index: Int32): T`, `set: <T>(arr: T[], idx: Int32, item: T): Null`, `indexOf: <T>(arr: T[], target: T): Int32` — because they read/write a single element through the already-element-type-aware bracket-index path (`arr[i]` / `arr[i] = item`) without allocating or routing the element through an opaque `for`/closure callback, so flat and tagged inputs both stay representation-consistent (no perf delta, pure type-safety). Every *allocating/builder/dyn-fn/numeric/iterator* combinator was deliberately left `AnyVal` at that point precisely because of the boxed-closure-ABI representation mismatch this ADR's inliner later removed — so `map`/`filter`/`reduce` are now generic-and-unboxed, while the numeric reductions (`sum`/`min`/`maxBy`/…) stay `AnyVal` pending a `<T: Numeric>` constraint and the iterable ops live in `std/iter` (ADR-051). The one behavioural note from that pass survives: `at`/`set`/`indexOf` no longer accept a non-array `AnyVal` (e.g. `range(0,10).at(5)` reports `expected Int32[]`); wrap an iterator in an array first.

## ADR-045: `Error` built-in type + `is Error`; `await` enforces Error handling via `T | Error`; first-class `Promise<T>`

> **Update (first-class `Promise<T>`).** The "no nominal Promise type" decision below was later
> reversed: `Type::Promise<T>` is now a first-class opaque handle type, modelled exactly like
> `Shared<T>`/`Stream<T>` (a boxed `TaggedVal*(TAG_PROMISE)` that does not widen to `AnyVal`). The
> change is purely in `lin-check` (variant + compat/zonk/resolve/collect-subs arms), threaded
> through `lin-ir` monomorphize/lower and `lin-codegen` (representation, RC, tagged-array element
> push) the same way `Stream<T>` already is — **no runtime change** (TAG_PROMISE + box/unbox/await
> helpers already existed). The async surface is now precisely typed: `async: <T>(() => T) =>
> Promise<T>`, `await: <T>(Promise<T>) => T | Error`, and `race`/`timeout`/`retry`/`poolAsync`
> carry `Promise<T>` in and out; `std/stream.promise` returns `Promise<Null>`. The codegen
> crash that motivated the original "await, not async" workaround (next paragraph) does **not**
> recur, because `Promise<T>` is its own opaque pointer representation — the union is never applied
> to the promise handle itself, only to the awaited result. The "forgot to `await`" gap noted in
> the **Known limitation** below is now closed: a `Promise<T>` is not assignable to its inner `T`.
> The historical reasoning is preserved unchanged below for context.

**Background — the `Error` type and `is Error`.** `Error` is a built-in type resolving to the
structural shape `{ "type": String, "message": String }` (`resolve.rs::error_type`) — the
conventional error value (spec §20) and the exact object the async runtime builds when a thunk
faults (`{ "type": "error", "message": <msg> }`). `is Error` (and any `is <ObjectShape>`) lowers to
a **field-presence** check (`HasPattern` on the object's keys) rather than a bare tag check, so it
matches error-shaped objects specifically instead of every object — making the spec's §24.2.2
`match await(p) is Error => … else => …` pattern work. `Error` has no special control-flow behaviour
(§20), so a structural object type is the faithful model — it composes in unions and narrows by shape,
with no new runtime support. (The deeper `is <ObjectType>` validation that supersedes the bare
field-presence check is ADR-036; nested-promise auto-flatten — `await` recursing through a
`TAG_PROMISE` result — is implemented in the runtime per §24.2.3.) The other half of §24.2.2 —
forcing callers to handle an uninspected `Error` — was originally deferred (the whole async surface
was `AnyVal`-typed, with no parametric `Promise<T>`), and **shipped at `await`, not `async`**, as
recorded below.

**Decision.** `await` is typed as a generic `<T>(p: T): T | Error` in `stdlib/async.lin`. It is the
single point where a faulted async computation surfaces as an `Error` value (spec §24.2.2), so it is
also the single point where the `Error` member is injected into the type. The result is a union
`T | Error`, and the *existing* union-assignment check — the same machinery `fromJson` relies on
(ADR-033) — rejects assigning it to a bare target type:

```
val r: Int32 = await(p)   // Error: Expected type Int32, got ?T | { "type": String, "message": String }
```

To consume the result you must handle the `Error` case (`match … is Error => … else => …`), exactly
as the spec intends. No new checker, codegen, runtime, or intrinsic code was needed; the feature is a
pure stdlib signature change leaning on generics (ADR-043) plus the pre-existing union-vs-bare check.

**Why `await`, not `async`.** The natural reading of the spec ("async wraps its return in `T|Error`")
is *not* codegen-sound here, and an early attempt to type `async` as `<T>(f: () => T): T | Error`
crashed codegen (`Found PointerValue … but expected the IntValue variant` in `boxing.rs`). The reason:
`async`/`poolAsync`/`race`/`timeout`/`retry` all return a **live `LinPromise*` handle**, not a resolved
value — the value only materialises at `await`. A union type forces a boxed-`TaggedVal*` representation
and scalar boxing; applying it to a promise pointer makes codegen try to box the promise *as* its inner
scalar, which is a representation mismatch. So every promise-*producing* wrapper keeps its opaque `AnyVal`
typing (a `LinPromise*` is just an opaque pointer, same as `AnyVal`), and only the promise-*consuming*
`await` — whose runtime result genuinely is a boxed value — carries the `T | Error` union. This still
satisfies §24.2.2 (you cannot use an awaited result without handling the `Error`), which is the rule the
spec actually cares about; it just attaches the union one call later than the prose suggests.

**Why lightweight (no `Type::Promise<T>`).** Introducing a nominal `Type::Promise<T>` would touch
type compat/resolve/zonk/monomorphize, every async intrinsic signature, and codegen boxing — a large,
risky change for the §24.2.2 guarantee alone. The generic-union approach reuses what already exists.

**Known limitation (honest).** Because there is no nominal `Promise<T>` and a promise handle is erased
to `AnyVal`, this enforces *"you must handle the Error after awaiting"* but does **not** catch *"you forgot
to await"* — i.e. using a promise as if it were the value. A real `Type::Promise<T>` would catch the
latter (a promise wouldn't be assignable to its inner type), but at the cost above. This is not a
regression: the previous all-`AnyVal` typing didn't catch the forgotten-await case either. Deferred.

**Narrowing footnote.** Match narrowing does not strip the structural `Error` member from an
`?T | Error` union whose `T` is an unsolved type variable (the awaited value, since the promise is
`AnyVal`). So `match await(…) is Error => … else => result` leaves `result` typed as the full union in
the `else` arm rather than `?T`. Routing the awaited value through an `AnyVal`-typed boundary (a tiny
`(r: AnyVal): Int32 => match r is Error => … else => r` helper) sidesteps this and coerces cleanly; the
concurrency examples and async tests use that helper for happy-path arithmetic. Improving union
narrowing over unsolved type variables is a separate, pre-existing checker concern.

## ADR-046: Test mocking via `replace` + test-lifecycle story (beforeEach/afterEach/beforeAll/afterAll)

**Status.** Accepted.

**Context.** Restructuring `examples/*` into per-module unit suites surfaced two recurring gaps in
the testing story: (1) no way to isolate a module under test from a real dependency (a unit calling
`std/fs.readFile` or a sibling module always hit the real implementation), and (2) no documented
setup/teardown lifecycle. Lin's compile-time, share-nothing model rules out a JS-style runtime
monkey-patch; the design has to fit how imports actually resolve.

**Key enabling fact.** Every call to an exported function lowers to `CallTarget::Named("<sym>")`,
where `<sym>` is derived from the import's RESOLVED module key (`mangle_module_key(path)` +
export name), NOT the surface path. Codegen resolves a `Named` call by a single
`module.get_function("<sym>")`. So `./some/file`, `./file`, and any transitively-importing module
all emit the SAME symbol, and there is exactly one LLVM definition per symbol. `lin test` compiles
each `.test.lin` as its OWN program, so anything scoped to one program is scoped to one test file.

**Decision — mocking (`replace`).**
- Syntax: `import { someExport } from "./some/file"` (brings in the real symbol + its type), then
  `replace someExport = <expr>` at top level of a `.test.lin`. Vals are replaceable too.
- Semantics (**Option A — replace EVERYWHERE**): the `replace` body becomes the canonical definition
  emitted for that export's mangled symbol; the original module's definition of that symbol is
  skipped in this program. Because resolution is symbol-name based, EVERY reference — the test file,
  the module under test, and any transitive importer, however the path is spelled — sees the mock.
  This is the "replaced no matter how you reference it" guarantee.
- The mock body is type-checked against the real export's signature (drift is a compile error); the
  landed cross-module type-import work supplies the real signature.
- Stdlib is mockable: `std/fs.readFile` etc. are ordinary compiled Lin wrappers, so `replace`
  redirects the Lin-API call sites (the C `lin_fs_*` runtime symbol is never reached). Mocking at the
  Lin-API level is the intent.
- NOT replaceable: the polymorphic INTRINSIC primitives (`print`, `map`, `filter`, `reduce`, `for`,
  `length`, `toString`, the async family) — codegen special-cases these, they are not `Named` calls.
  A `replace` targeting one is a compile error.
- `replace` is only meaningful in a `.test.lin`; using it in a `lin build` program is a hard error
  (it would silently swap stdlib in a shipped binary).
- Spies are an ordinary mock closing over a module-level `var`/`Shared` cell (count calls, capture
  args), asserted after the run — no extra framework.
- Accepted tradeoff of Option A: a mock that references its own replaced symbol self-recurses
  (`replace readFile = (p) => readFile(p)` loops); delegation/pass-through to the real impl is NOT
  possible. Chosen for simplicity over Option B (leave the test file's own binding original).

**Decision — lifecycle.** No dedicated keywords; the eager-evaluation model already supports the
common cases, and DI/combinators cover the rest:
- **beforeAll**: a module-scope `val`/statement above the suite. `test(...)` bodies run EAGERLY as
  the `tests` array is built, so module-scope setup runs once before them.
- **afterAll**: statements after `run(s)`. CAVEAT: `run` calls `exit(1)` on failure, so post-`run`
  cleanup does not execute when a test fails. For guaranteed teardown, pass cleanup to a `run`
  variant or run cleanup before `run`. (Future: `run` could return a status instead of exiting.)
- **beforeEach/afterEach + fixtures**: prefer a functional `around`/`withFixture` COMBINATOR over new
  keywords — a wrapper that builds the fixture, runs the body with it injected, and tears down:
  `val withDb = (name, body) => test(name, () => val db = open(); val r = body(db); close(db); r)`.
  This is per-test setup + teardown + dependency injection using partial application and the existing
  model; assertion FAILURES are values (don't skip teardown). True keyword `beforeEach`/`afterEach`
  would require deferring test bodies (store thunks, run setup/body/teardown in `run`) — deferred as
  a later option if the combinator proves insufficient.

**Consequences.** Mocking is a compile-time, abs-path-keyed, type-checked symbol override scoped to a
single test program — no runtime cost, no cross-test leakage, works for user modules and stdlib
wrappers alike. Lifecycle leans on Lin's functional/DI grain rather than new syntax. Implementation
spans lin-parse (a `Replace` stmt), lin-check (resolve target symbol, type-check body), and
lin-ir/lin-compile (emit the body under the export's symbol; suppress the original; enforce
test-only + intrinsic/unused diagnostics).

**As implemented.** A few details settled during implementation:
- `Stmt::Replace { name, value, span }` (no `TypedStmt::Replace` — the checker collects overrides
  into a side-channel `TypedModule::replacements`, mirroring `intrinsics`/`exported_types`, so no
  exhaustive-match churn across crates). Lowering emits each mock under the export's canonical
  mangled symbol (`{module_key}_{name}`, or `{sym}__val` for vals); the owning module skips emitting
  that symbol and routes its slot through `import_fn_slots`/`import_val_slots`, so internal sibling
  calls also become `Named` calls to the single mock definition.
- **Test-only gating is by FILENAME** (`*.test.lin`), not subcommand. This is what makes it hold for
  every entry point at once: `lin test` AND the ASan CI leg (which runs `lin build <f>.test.lin`)
  accept it, while `lin build`/`lin run` on a normal program reject it. (A subcommand flag would have
  broken the ASan leg.)
- **Type drift** is caught by an explicit `types_compatible` check after `check_expr` — the
  function-hint path treats expected param types as hints, so an annotation could otherwise override
  them silently.
- **`withFixture` is `AnyVal`-typed**, not generic: `(() => AnyVal, (AnyVal) => Null, String, (AnyVal) =>
  Assertion[]) => Test`. A generic stdlib export taking function params + returning a concrete type
  hit a monomorphization edge (resolved as a `__val` wrapper, link error); the `AnyVal` fixture type
  sidesteps it and is sufficient for a test helper.
- **`run` now delegates to `report`**, the non-exiting variant (`(Suite) => Int32`) added for
  guaranteed afterAll teardown — `run(s) = if report(s) > 0 then exit(1) else null`.
- Worked examples: `examples/processes/` (mock `exec`), `examples/dijkstra/` (mock `std/fs`), and
  `examples/web-server/` (mock `render`) — each replaces a side-effecting dependency in its tests; the feature is
  documented in docs/SPECIFICATION.md §22.8–22.9, docs/STDLIB.md (std/test), and the doc-site
  Testing tutorial.

## ADR-047: Circular imports are a compile-time error (DFS visiting-stack)

> **Superseded in part by ADR-052**: cyclic *function* references are now supported via SCC
> type-checking. The stack-overflow protection and the value-init rejection described here remain
> in force (the value cycle is now reported as `a <-> b (… reads an imported VALUE …)`).

**Decision**: A circular import is rejected at compile time with a
`CompileError::ImportCycle` carrying the cycle as a readable chain
(`circular import detected: a -> b -> a`). Import resolution
(`pre_resolve_imports_inner` in `lin-compile`) threads a `visiting: Vec<String>`
DFS stack of module *identities*; before descending into an import it checks
whether that identity is already on the stack and, if so, returns the cycle chain
instead of recursing. A module's identity is its canonicalised absolute file path
(so `../a` and `a` are one module) or, for stdlib, the `std/...` path. The
entry-point module seeds the stack so a cycle that loops back to the entry is also
caught. Each module is popped on the success paths (cache-hit and
checked-and-cached), so a **diamond** — one module reached by two independent
paths — is resolved once and is *not* flagged (the second visit hits the
`cache.contains_key` guard).

**Rationale**: Resolution recurses into each import *before* inserting it into the
cache, so the pre-existing `cache.contains_key` guard could never break a cycle —
`a -> b -> a` recursed forever and **overflowed the stack** (SIGABRT), a crash with
no diagnostic. The spec originally called for *lazy* initialisation with a
*runtime* cycle error, but the implementation resolves eagerly at compile time, so
catching the cycle during resolution (earlier, with a clean message) is both
simpler and stricter than the original design. A visiting-stack DFS is the standard
cycle-detection shape and yields the chain for free.

**Consequence**: A cyclic import fails `lin build`/`lin run` with
`circular import detected: <chain>` and a non-zero exit, never a stack overflow.
Diamonds and ordinary deep import graphs are unaffected. Spec §22.5/§20.1 and
decision-list #29 updated to "circular import is a compile-time error." Regressions:
`test_circular_import_is_diagnosed_not_stack_overflow` and
`test_diamond_imports_are_not_false_cycles` in `crates/lin/tests/integration.rs`.

## ADR-048: `std/template` delegates to minijinja (Jinja syntax; layouts; undefined→empty; render errors→Error)

**Decision**: `std/template` is now backed by the [minijinja](https://crates.io/crates/minijinja)
crate instead of the hand-rolled `${ }` substituter. Template syntax is Jinja-style:
`{{ var }}` substitutions, `{% for %}` / `{% if %}` control flow, and the standard
builtin filter set. This is a **clean break** — the old `${ }` delimiters are gone.
minijinja is added to `lin-runtime` with `default-features = false, features =
["builtins", "serde"]`: `builtins` re-enables control flow + filters (which the
default-off baseline drops); `serde` lets minijinja consume a `serde_json::Value` as
the render context directly. **Undefined / missing variables render as the empty
string** — minijinja's default `Undefined` behaviour; strict/undefined-is-error mode is
deliberately *not* enabled.

**Data bridge**: the runtime already exposes `json::tagged_to_json(tv: *const u8) ->
serde_json::Value`, and minijinja renders against a `serde_json::Value` directly. So
`lin_template_render` is just: resolve the template `LinString` to `&str` →
`tagged_to_json(data)` → fresh `minijinja::Environment` → `render(&ctx)`. The `{}` data
arg may arrive either as a `TaggedVal*(TAG_OBJECT)` or a bare `LinObject*` (the foreign
`{}` param does not force boxing on every path), so the FFI detects the leading tag and
synthesises a stack `TaggedVal(TAG_OBJECT)` for the bare-pointer case before calling
`tagged_to_json` (which only reads tag+payload — it neither frees nor owns the input).

**Error handling (option a)**: the FFI return type changed from `*mut LinString` to a
tagged value `*mut u8` (`AnyVal`), matching the established `readFile`/`lin_fs_read_json`
convention (declared `=> AnyVal` in the `import foreign "lin-runtime"` block, documented
conceptually as `String | Error`). Success boxes the rendered string into a
`TAG_STR` tagged value; a template **syntax error or render failure** returns
`make_error_tagged(...)` → `{ "type": "error", "message": ... }`, discriminated with
`is Error` exactly like other fallible stdlib operations. Typing the wrappers as `AnyVal`
(rather than a literal `String | Error` annotation) is what keeps callers that use the
result directly as a string working unchanged — `out.contains(...)` in the web-server
tests and `writeFile(path, rendered)` in the docs-site builder both rely on the `AnyVal`
wildcard, and a hard `String | Error` annotation would reject those (`Error` has no
`.contains`; `writeFile` wants a `String`). This mirrors how the docs already describe
`readFile` as `String | Error` while its signature is `AnyVal`.

**Ownership**: `lin_string_from_bytes` returns an OWNED (+1) string; that owned
reference transfers into the `TAG_STR` tagged box, and the standard owned-release
lowering contract drops it exactly once. No borrowed-string return path is introduced,
so the recurring owned-vs-borrowed UAF/double-free class does not apply.

**Consequence**: templates gain real loops/conditionals/filters. Migrated `.jinja` files
(`examples/web-server/views/index.jinja`, `docs-site/templates/{page,home}.jinja`) and
all call sites/tests from `${x}` to `{{ x }}`. The "missing key → empty string" change
replaces the old "missing key → `null`". HTML in template variable *values* is passed
through unescaped (the environment uses a non-`.html` template name, so autoescaping is
off) and is not re-parsed as Jinja. Regressions: new `{% for %}` / `{% if %}` /
missing-var / syntax-error cases in `stdlib/template.test.lin`; web-server and docs-site
(44 pages) render verified end-to-end.

### Layout system via minijinja path loader (`render` is file-based)

**Decision**: `std/template` also gains a real layout/inheritance system — `{% extends %}`,
`{% block %}`, `{% include %}` — rather than flat substitution. This is exactly the
capability the old hand-rolled `${ }` engine could not express and was the motivation for
moving to minijinja.

**Why `render` had to become path-based**: template inheritance needs a *loader* — when
a template says `{% extends "base.jinja" %}`, the engine must fetch `base.jinja` by name.
The first cut implemented `render(path, data)` in Lin as `readFile(path)` →
`lin_template_render(string, data)`, i.e. it handed minijinja a single anonymous
in-memory string with no directory context, so `extends`/`include` had nothing to
resolve against. The fix is a second FFI entry point, `lin_template_render_path(path,
data)`, that splits `path` into `(dir, basename)`, sets `env.set_loader(path_loader(dir))`,
and renders the basename. minijinja then lazily loads any referenced template by name
from `dir`. `std/template.render` now calls this directly (no more `readFile` in Lin);
the "file not found" Error is produced by the loader instead of an explicit `readFile`
check, and still surfaces as `{ "type": "error", "message": ... }`.

**`renderWith` stays string-based**: it takes an in-memory template with no source
directory, so it deliberately *cannot* resolve `extends`/`include` — documented as such.
The two entry points are the natural split: inline string vs. file-with-neighbours.

**minijinja features**: this required adding **`multi_template`** (the `{% extends %}` /
`{% block %}` / `{% include %}` statements — gated separately from `builtins`; without it
the parser reports `unknown statement extends`) and **`loader`** (`path_loader`) to the
existing `["builtins", "serde"]` set. Both are pulled in with `default-features = false`.

**Consequence**: the two example projects that use templating now demonstrate proper
layouts instead of duplicated page chrome:
- `docs-site/templates/` — new `base.jinja` holds the shared `<head>`/nav/scripts with
  `{% block body_attrs %}` and `{% block main %}`; `page.jinja` and `home.jinja` shrink to
  `{% extends "base.jinja" %}` + their block overrides (home adds `class="home"` and the
  hero). The builder (`docs-site/builder/main.lin`) switched from `renderWith(readFile…)`
  to `render(path, …)` and now guards the render result as an `Error`.
- `examples/web-server/views/` — `index.jinja` extends a new `base.jinja`, which itself
  `{% include "footer.jinja" %}` to show partials.

Verified end-to-end: `stdlib/template.test.lin` gains an inheritance case (writes
base+child to a temp dir, renders the child); `examples/web-server/template.test.lin`
asserts the base skeleton + included footer appear; the docs-site builder regenerates all
44 pages with the home/regular layouts distinct and no unrendered tags or error objects.

## ADR-049: Affine resource types + move-transfer

**Decision**: A *resource type* — currently just `Stream<T>` (§27.9, ADR-050) — is **affine**: it
may be used **at most once**, and dropping it unused is fine. Resource values cross a thread boundary
by **MOVE** (a pointer handoff, no clone), where transferable JSON-shaped values still cross by deep
**COPY** (ADR-028 Option C, the existing path). Both rules yield **disjoint object graphs** per
thread, so the single-threaded non-atomic refcount (ADR-024) stays sound on both sides.

This completes the transfer model anticipated in ADR-028:

1. **Transferable values cross by deep copy (unchanged).** A JSON-shaped, acyclic, `Function`/
   `Iterator`-free value (the only kind a thunk may capture or return, §24.2) is deep-copied at the
   boundary so each thread owns a private graph. Non-atomic RC is sound because nothing is shared.

2. **Resource values cross by move (new).** A `Stream<T>` cannot be deep-copied — it owns an OS fd and
   a live read position; copying would alias the fd. Instead it is **moved**: the box pointer is handed
   to the worker thread verbatim (no clone, no source-side release), the source binding must never touch
   it again, and the **worker** owns it for the rest of its life and releases it (the RC-drop finalizer,
   ADR-050, runs on the worker). Because the source relinquishes the only reference, the moved graph is
   exactly as disjoint as a copied one — one owner, one thread — so non-atomic RC remains correct without
   atomics. A move is O(1); a copy is O(size). This is the `.promise()` hand-off (§27.9).

**Affine, not strict-linear.** A strict-linear discipline would make *dropping* a resource an error
(every stream must be explicitly consumed). We choose **affine** (use-at-most-once) because the RC-drop
finalizer (ADR-050) already closes the fd deterministically when the last reference goes away, so an
un-consumed stream is not a leak — it is closed at end of scope like any other heap value. Therefore:
- **Double-use is the only hard error.** Using a stream after it has been moved or terminally consumed
  is a compile-time error.
- **Must-use is a WARNING, not an error.** A stream that is built and never drained/awaited is almost
  always a mistake (the pipeline never runs), so the checker warns — but it is sound (the finalizer
  closes the fd), so it does not block compilation.

**The flow-sensitive use-after-move check.** The checker tracks, per local binding of resource type, a
*consumed* flag through the control-flow of a function body. A binding becomes consumed when it is:
passed as an argument (the callee takes ownership), returned, or fed to a terminal/sink op
(`.drain()`/`.promise()`/`.for(...)`/`readText`/`collect`). A later reference to a consumed binding is
the use-after-move error. Branches join conservatively: a binding consumed on *any* path is treated as
consumed afterwards (a use on the other path is still flagged, since the program cannot statically know
which path ran). This is the same shape as the `async` var-capture analysis (ADR-022) — a localized,
flow-sensitive scan over the typed body — not a full borrow checker.

**v1 placement restriction.** A `Stream` value may live **only** in a `val` binding, a function
parameter, or a function return position. It may **not** be stored in an object field, an array element,
or a `var`. This confines the move-checker to *local bindings* with a single static name — it never has
to reason about aliasing through a container or through a mutable cell that two closures share
(ADR-012). The restriction is enforced in the checker (a `Stream`-typed object/array literal element or
`var` initializer is a type error) and is **relaxable later** (container-linearity / region tracking)
without changing the surface API. It mirrors the deliberately-narrow scope choices of ADR-029 (`Shared`
is invariant, never widens to `AnyVal`) and ADR-032 (the structured-decode gate).

**`CAP_MOVE` in the closure-env ABI.** Capture kinds live in two mirrored places that previously stopped
at `CAP_TAGGED=5` (`crates/lin-runtime/src/transfer.rs`'s `CAP_*` family and `lin-ir/src/ir.rs`'s
`enum CaptureRelease`). A resource capture gets a new kind **`CAP_MOVE=6`**. At the thread-transfer
boundary (`transfer_clone_env`, ADR-028/ADR-041) a `CAP_MOVE` capture is **moved, not cloned**: the box
pointer is copied into the worker's env and the source env's slot is **not** released (`release_env_copy`
skips it) — the worker now owns it. `env_is_transferable` accepts a `CAP_MOVE` capture (a resource is
transferable by move even though it is non-copyable). This is the single ABI mechanism that realises the
move at run time; everything else (the affine check) makes it *safe*.

**Rationale**: ADR-028 deliberately left "resource values cross by move" as future work — its Option C
deep-copy is *total* only over transferable types, and a stream is precisely the non-transferable,
single-owner value that copy cannot handle. Move is the natural dual: copy gives each side its own graph
by duplication; move gives each side its own graph by *relinquishment*. The affine check is what makes
relinquishment sound (the source provably never touches the moved value again), and it is the minimum
needed — full linearity (must-use-as-error) buys nothing once the finalizer guarantees cleanup, and a
full borrow checker is far more than local single-owner resources require. The placement restriction
keeps the v1 check tractable by removing aliasing entirely.

**Consequence**: `Stream<T>` is the first affine type; the machinery (consumed-flag flow analysis,
`CAP_MOVE`, the placement restriction) is reusable for any future single-owner OS resource. The
move-transfer path **must** be verified under AddressSanitizer (a `cargo test` pass cannot catch the
UAF/double-free class): a moved stream's fd must close **exactly once**, on the worker, with no
source-side release and no double-free. A program that violates the affine rule fails to compile
(`use of moved value 's'`); a built-but-never-run stream compiles with a `stream is never consumed`
warning. The non-atomic RC hot path (ADR-024) is untouched — `CAP_MOVE` is the only new boundary
behaviour, and it does *less* work than the copy path, not more.

## ADR-050: `Stream<T>` distinct from `Iterator`; `for`/terminal error semantics

**Decision**: A `Stream<T>` is a new **opaque runtime type** — `Type::Stream(Box<Type>)`, sibling to
`Type::Iterator` (`crates/lin-check/src/types.rs`), covariant in `T`, erased to a `TAG_STREAM` box at
runtime — and it is deliberately **NOT** modelled as an `Iterator<T>`. A stream is a lazy pull-graph
over a byte/value source plus an optional push sink; the surface API and semantics live in §27.9. This
ADR records the three semantic choices that make a stream a thing of its own.

**Why a stream is not an iterator.** The iterator protocol (§18.2, ADR-019) is built from four
**pure** state-transition functions — initial-state, continuation predicate, next-state, current-value
— and §18.2.1 even promises restartability by re-running the initial-state thunk. A stream violates
every one of those assumptions:
- It is **effectful**: pulling the next chunk performs a `read(2)` and advances an OS file position;
  there is no pure "current value at this state."
- It is **fallible**: a read can fail mid-traversal (`EIO`, connection reset), which a pure
  `(State) => T` current-value function cannot express.
- It **owns an OS resource** (an fd) with a lifetime, so it is single-owner/affine (ADR-049), whereas
  an iterator is a freely-copyable pure description.

Forcing a stream into the iterator shape would either make the protocol functions impure (breaking
§18.2's contract and the iterator-restartability guarantee) or hide the fd lifetime, so `Stream<T>` is a
distinct opaque kind. It reuses `Iterator`'s *threading* through the compiler (the ~16-file checklist:
types/compat/zonk/resolve, checker, IR lowering/monomorphize, codegen) but never its *protocol*.

**RC-drop auto-close finalizer.** A `TAG_STREAM` box carries the source backend (an fd + read state, or
an adapter node pointing at its upstream). Its **release finalizer closes the fd** when the refcount
reaches 0 if it is not already closed — deterministic cleanup with no GC, exactly the ADR-024 RC contract.
An explicit `close(s)` is **idempotent** (closing an already-closed stream is a no-op) and exists for
callers who want determinism rather than scope-end timing. This is what makes the affine *drop-is-fine*
rule of ADR-049 sound: an un-consumed stream still closes its fd. The finalizer is in the UAF/double-free
class and **must** be ASan-verified (fd closes exactly once).

**`for` over a stream returns `Null | Error`.** A stream is consumed with `.for(fn)` dot-application —
**not** `for…in`, which does not exist in Lin (iteration is always `.for(fn)`/combinators, §18). Driving
a stream to completion can end two ways, so `for` over a stream is typed **`Null | Error`** (unlike `for`
over an array/iterator, which is `Null`, §18.1):
- **EOF ends the loop normally** → the `for`-expression evaluates to `Null`.
- **A read `Error` mid-traversal becomes the `for`-expression's value** → the loop stops and yields the
  `Error`.

So a stream `for` reads like any other fallible op (§25.1):

```txt
val outcome = readStream("in.log").lines().for(line =>
  print(line)
)
match outcome
  is Error => print("read failed: ${outcome["message"]}")
  else     => null
```

The exact ending is **source-kind-dependent**: a file stream that reaches EOF ends with `Null`; a socket
stream whose peer resets ends with `Error`. `lin_for`'s lowering (`lin-ir/src/lower.rs`, `lower_for`)
gains a stream branch that drives the pull-graph and produces the `Null | Error` result.

**In-band error threading through the lazy graph.** The pipeline is a lazy pull-graph; rather than make
every adapter re-check `is Error`, a **poisoned upstream makes every downstream adapter a passthrough**.
The first read error propagates *as data* straight to the terminal op, which surfaces it as the
`Null | Error` (`for`/`drain`) or `Promise<Null | Error>` (`promise`) result. This keeps the chain
fluent — `readStream(p).lines().map(f).filter(g).writeLines(q).drain()` has no error handling at each
step, only at the terminal — and it short-circuits: once poisoned, no further reads or user callbacks
run. It is the stream analog of the §20 value-based error convention: errors are ordinary values flowing
through the graph, not exceptions.

**Rationale**: Keeping `Stream` distinct from `Iterator` preserves the iterator's purity/restartability
contract (which the optimizer and the §18 combinators rely on) while giving effectful, fallible,
fd-owning sources an honest type. The RC-drop finalizer reuses the existing deterministic-RC cleanup
(ADR-024) instead of inventing a lifetime story. Typing `for` as `Null | Error` makes a stream loop obey
the same `T | Error`-handling discipline as every other fallible stdlib op (§25.1, ADR-045), and
in-band threading keeps the fluent API the brief requires without an `is Error` at every adapter.

**Consequence**: `Stream<T>` adds one opaque runtime kind (`TAG_STREAM=19`), one finalizer, and a
stream branch in `lower_for`; it does not touch the iterator path. A stream `for`/terminal that ignores
its `Null | Error` result trips the same union-vs-bare-target check as `await` (ADR-045) when assigned
to a bare type. Distinct from `Shared<T>` (ADR-029, shared mutable, atomic-RC, copy-in/out) and
`Frozen<T>` (ADR-030, immortal read-only): a stream is single-owner, moved not shared (ADR-049). Full
surface spec in §27.9; stdlib API in `std/stream` (STDLIB.md).

## ADR-051: Unified iterable combinators via receiver dispatch (`std/iter`)

**Decision**: The iterable combinators — `map`/`filter`/`reduce`/`for`/`while`/`take`/`drop`/`flatMap`/
`takeWhile`/`dropWhile`/`flatten`/`concat`/`find`/`some`/`every` — plus the iterator constructors
`range`/`rangeStep`/`iter`/`iterOf` live in **one** module, `std/iter`, and dispatch on the **static
type of the receiver** (arg0): **eager** (a materialised `U[]`) for an `Array`/`Iterator` receiver,
**lazy** (a `Stream<U>` adapter node) for a `Stream` receiver. Terminals over a stream gain an `| Error`
arm (a stream read is fallible). One name, one import, one fluent chain over any iterable source — the
receiver's type picks eager-vs-lazy. This is Lin's first-argument-dispatch philosophy (§4.4) applied to
the combinator set.

**Why this is not a new mechanism.** `for` ALREADY dispatched this way: `lower_for` branched on
`Type::Stream(_)` to emit a stream driver (`Null | Error`) versus the array index-loop (`Null`).
Arrays and iterators were ALREADY unified under one name via a `T[] | Iterator` union param. ADR-051
folds the third source — `Stream` — into a unification that already covered two of three, rather than
inventing function overloading. The combinator name set is **closed** (the fixed builtin list above), so
this is a restricted special-case, not general overloading.

**Mechanism (one dispatch fact, three readers).** The receiver type at a concrete call site drives all
three of:
1. **Return typing** — `streamish_combinator_ret` (`lin-check/src/checker/call.rs`) re-types the
   call-site result when arg0 is *definitely* a stream: adapters return `Stream<T>`/`Stream<U>`
   (keeping the chain lazy for the next link); terminals return `U | Error` (`reduce`),
   `T | Null | Error` (`find`), `Boolean | Error` (`some`/`every`), `Null | Error` (`for`/`while`).
   For an `Array`/`Iterator` receiver the eager array-shaped type flows through unchanged.
2. **IR redirect** — `stream_combinator_intrinsic_name` (`lin-ir/src/lower.rs`) re-routes a
   `std_iter_*` call whose arg0 is `Type::Stream(_)` to the lazy `lin_stream_*` backend, bypassing the
   eager pure-Lin/`lin_map` body entirely.
3. **Affine consumption** — `callee_routes_to_stream_op` keys the use-after-move check on the SAME
   `(module, export)` dispatch fact.

The std/iter wrappers spell their iterable param as the union `T[] | Iterator | Stream` (so a `Stream`
is accepted without the opaque type leaking into a bare `AnyVal` param) and **drop their return
annotations** so the checker-computed receiver-dependent type flows through. The "definitely a stream"
predicate (`is_definitely_stream`) accepts a bare `Stream<…>` or a `Stream<…> | Error` (a source
intrinsic's result), but NOT a mixed `Array | Iterator | Stream` union — a union with a live array
branch keeps its eager return.

**Module boundary — combinators in `std/iter`, array-shaped ops in `std/array`.** The combinators work
over *any* iterable, so they belong to the iterable module. The genuinely array-shaped ops — those that
need a materialised, indexable, ordered array (`push`/`slice`/`set`/`at`/`length`/`reverse`/`sort`/
`sortBy`/`zip`/`unique`/`chunk`/`compact`/`indexOf`/`partition`/`sum`/`product`/`min`/`max`/`minBy`/
`maxBy`/`append`/`prepend`/`scan`/`groupBy`/`countBy`/`arrayAllocate*`) — stay in `std/array`.
`std/iter` is the **lower** module in the dependency graph: it must NOT import from `std/array` (that
would form an `array → iter → array` cycle, a compile-time error, ADR-047), so the two array primitives
its bodies need internally (`length`/`push`) are duplicated there as private thin wrappers; `std/array`'s
own combinator-using helpers import the combinators back from `std/iter`.

**Combinators are NOT dual-exported.** Exporting the same combinator from both `std/iter` and (say)
`std/array`/`std/stream` was **rejected** as confusing — there must be exactly one source of each name.
`std/stream` therefore **stopped exporting** `map`/`filter`/`take`/`for`/etc.; it keeps only
stream-specific ops (`readStream`/`writeStream`/`writeLines`/`drain`/`collect`/`readText`/`promise`/`close`/`lines`/
`linesMax`/`chunks`). A stream pipeline imports its combinators from `std/iter` and its sources/sinks
from `std/stream`.

**Affine subsumption — the prior soundness hole is closed.** The old affine check consumed a stream off
a fragile NAME allowlist (`is_stream_consuming_op`) while the IR moved a stream on ANY streamish-typed
arg; the two lists drifted (`linesMax`/`promise`/`close` slipped through, and `promise` is a cross-thread
UAF locus). Receiver dispatch makes consumption read the SAME fact the dispatch computes: any
definitely-stream value flowing into any stream-dispatched combinator or stream-specific op is MOVED, per
argument (so `concat`'s two stream args are both consumed). The checker, the IR redirect, and the affine
consume-set now read one fact, not three lists, so they cannot diverge. The five adversarial
use-after-move attacks became regression tests (all reject at check time). Cross-ref ADR-049 (affine
resource types + move-transfer).

**v1 limitation — dispatch is at the concrete call site only.** Receiver dispatch fires when a combinator
is called with a *concrete* `Stream` receiver. A stream passed THROUGH a user-defined generic `Iterable`
parameter and combined inside that function stays **array-shaped** (the param is the union, not a definite
stream), so it does not become lazy. This is the safe resolution: the eager array path is always correct;
only the lazy optimisation is forgone. Relaxable later without changing the surface API.

**Rationale**: One combinator vocabulary over arrays, iterators, and streams is the user-facing payoff —
`readStream(p).lines().drop(1).take(4).map(f).reduce(0, g)` reads identically to the array form yet runs
lazily with bounded memory. Reusing the `for` dispatch precedent (a closed name set, type-directed
return) avoids general overloading. Keying affine consumption off the dispatch fact eliminates the
allowlist-vs-IR divergence class outright rather than patching it.

**Consequence**: `std/iter` is a new stdlib module; `std/array` and `std/stream` shed the moved
combinators (existing imports migrate to `std/iter`). New lazy stream backends
(`lin_stream_map`/`filter`/`take`/`drop`/`take_while`/`drop_while`/`flat_map`/`flatten`/`concat`/`while`)
and stream terminals (`lin_stream_reduce`/`find`/`some`/`every`, alongside the existing `lin_stream_for`)
back the lazy arm; each drives-and-consumes the stream on the calling thread, fault-isolated. The
flat-producer recognition (the unboxed-scalar fast path, ADR-044) follows the moved names. Verified on
`feat/streams`: full suite green, ASan clean, all five affine attacks reject. Full stream semantics in
§27.9 (ADR-050); combinator surface in §18; stdlib API in `std/iter` (STDLIB.md).

## ADR-052: Cyclic function imports compile via SCC type-checking (no userland change)

**Decision**: A true import cycle whose cross-module edges are *function* references now compiles
**as written**, with no annotations and no syntax changes. Import resolution in `lin-compile`
(`pre_resolve_imports_from_ast`) no longer rejects on a back-edge. Instead it: (1) loads the whole
import graph up front (`build_import_graph`, keyed by stable module identity, recording every
path-string spelling each module is reached by); (2) decomposes it into strongly-connected
components (`tarjan_sccs`, emitted in reverse-topological order); (3) resolves a singleton SCC with
no self-edge through exactly the old per-module path (`resolve_singleton`, incl. the `.lin-cache`
fast path); and (4) type-checks a true multi-module (or self-importing) SCC together via
`check_scc`. A genuine **value-init** cycle is still rejected.

`check_scc` is a two-pass seed-and-recheck fixed point:
- **Phase 1** checks each member against the already-resolved `cache` — a peer not yet checked
  falls back to a fresh TypeVar at the existing import-binding seam (`checker/stmt.rs` Import arm),
  yielding a provisional `ModuleSignature` for every member.
- **Value-init guard** (between the phases, using the Phase 1 signatures to distinguish function
  from eager-value peer exports): a top-level non-function `val`/`var` whose initialiser reads a
  peer export that is itself a *non-function value* is unbreakable module-init recursion (spec
  §7.3) → `CompileError::ImportCycle`. Binding/calling a peer *function* is fine (resolved by
  symbol, never recomputed at init).
- **Phase 2** re-checks each member with all provisional peer signatures seeded into `import_types`
  (`check_module_with_seeded_imports`), so a cross-module call resolves to a peer's
  *Phase-1* provisional type. This grounds a member whose return type is locally determinable
  (a literal/annotation in its own body); it does **not** ground one whose return type flows
  *through* a peer call — see the boundary-soundness gap under **Known limitation**.
The Phase 2 modules are registered; cyclic members are never read from `.lin-cache` (their type
depends on peers), though their `.sig` is persisted for out-of-SCC dependents.

**Rationale**: The codegen/IR layer was already cycle-ready — imported functions are called by
mangled `Named` symbol resolved at link time, and codegen pre-declares all symbols, so a back-edge
needs no special lowering. The only barrier was the front-end's hard reject plus per-module checker
isolation (ADR-007). The fresh-TypeVar fallback at the import-binding seam is the hook that makes
seed-and-recheck work without sharing a checker (no ADR-007 break). Per-path-string registration is
preserved (each spelling gets its own compiled copy + mangled symbols, matching long-standing
absolute-vs-relative behaviour).

**Consequence**: Mutually-recursive functions across modules — including chains of 3+ modules —
compile and run correctly with zero userland annotations (the runtime calls the real symbol, so
results are always right). The entry module may itself participate in a cycle (its body is then also
emitted under the import-mangled symbol a peer calls; a duplicated definition, code-size only).
Regressions in `crates/lin/tests/integration.rs`:
`test_cyclic_imports_mutual_recursion_unannotated`,
`test_cyclic_imports_value_init_cycle_still_errors`,
`test_circular_import_function_reference_compiles_not_stack_overflow` (the former
reject-expectation, repurposed), `test_diamond_imports_are_not_false_cycles`.

**Known limitation (boundary-soundness gap)**: the SCC fixed point is a single re-check, not
iterated to convergence, and the trigger is *whether a return type flows through a peer call* — not
hop-count. A cyclic export whose body returns a locally-determinable value (literal/annotation, even
inside a cyclic module) infers correctly. But a cyclic export whose return type depends on a peer
call ends up with a **permissive/unsolved type at the module boundary**: its consumers get no return
type, so `val k: Int32 = peerRecursiveFn(x)` type-*checks* even when the function returns `String`.
Verified: in an `a→b→c→a` cycle where `fromC = (n) => if n==0 then "done" else fromA(n-1)`, binding
`fromA`/`fromB`/`fromC`'s result to `Int32` *or* `Bool` is wrongly accepted; the same `fromC` outside
any cycle correctly infers `String` and rejects `Int32`. This is a **missed-error soundness gap, not a
miscompile** — runtime behaviour is correct because codegen calls the real symbol. The fix is to
iterate Phase 2 to a fixed point (re-seed with each round's signatures until they stop changing), or
to fail closed by *requiring* an explicit return annotation on a peer-call-dependent cyclic export.
Not yet done; tracked as follow-up.

## ADR-053: Optional 0-based index parameter on iterable combinators

**Decision.** The iterable combinators expose an OPTIONAL 0-based `Int32` **source** index as a
trailing callback parameter (`(item, i) => …`; `reduce`: `(acc, item, i) => …`), the JS
`forEach((item, idx) => …)` model. A 1-arg (reduce: 2-arg) callback stays valid and unchanged — the
index is opt-in by arity, fully backward-compatible. Scope: `for`/`map`/`filter`/`reduce`/`while` and
the derived `find`/`some`/`every`/`flatMap`/`takeWhile`/`dropWhile` plus `std/array`'s `partition`.
The index is always the **source** position (even for `filter`/`takeWhile`/`dropWhile`, whose output
position differs). EXCLUDED: the key-extractor/aggregator combinators `sortBy`/`minBy`/`maxBy`/
`groupBy`/`countBy` (element position is meaningless to a key function); and **streams** — the runtime
`Stream` combinators keep 1-arg callbacks (a pull-driven stream has no materialised source position).

**Type system.** The enabling rule is **arity-width subtyping** on callbacks (spec §5.8): a function
value declaring FEWER parameters is assignable where MORE are expected, provided the extra expected
trailing parameters are all `Int32`. This is deliberately tight — it does not open arbitrary arity
subtyping; a value with more parameters than expected, or extra non-`Int32` expected parameters, still
rejects. The intrinsic signatures (`lin_for`/`lin_map`/`lin_filter`/`lin_while`/`lin_reduce`) declare
the index param `Int32`; an unannotated index param infers `Int32` via the existing bidirectional
hinting, an explicit `Int32` annotation is allowed, and any other annotation is a clear compile error.

**Lowering — two tiers.**
- **Tier A — intrinsic combinators** (`for`/`map`/`filter`/`reduce`/`while`) are codegen-inlined LLVM
  loops (`emit_index_loop`, `lin-ir/src/lower.rs`). The loop counter already exists; it is narrowed
  Int64→Int32 (a `Coerce`/`trunc`) and passed as the trailing callback argument. The inline fast path
  (ADR-044) is **preserved** — no closure, no thunk; a 1-param inline lambda simply ignores the
  surplus index temp (`inline_lambda_body` binds by the lambda's own param count, and the dead
  narrowing temp is DCE'd).
- **Tier B — derived combinators** are pure-Lin wrappers that call `f(item, idx)` with a threaded
  counter. They re-type their callback param to the indexed form and operate on **`AnyVal` elements**
  (matching their original `arr: AnyVal` shape — keeping the boxed monomorphization unchanged).

**In-place adapter (not a thunk).** When a user passes a SHORTER callback, the checker PADS the typed
lambda with synthetic unused trailing `Int32` parameters (`infer_function_with_hints`), so the compiled
closure actually has the arity the caller invokes it with. This is the "adapter" the design called for,
done in place: no wrapper-closure allocation (which would defeat the Tier-A inline), and it makes the
Tier-B `f(item, idx)` call always match the closure's true arity. As defence-in-depth, the boxed
closure-call helpers (`call_body_closure`/`call_body_closure_with_elem_boxes`) TRUNCATE their argument
list to the closure's declared arity, so a 1-param closure value reaching there is never over-called
(the one ABI hazard — a 1-param closure called with 2 args is UB).

**No runtime/ABI change.** `lin-runtime` is untouched; the stream combinators keep their 1-arg ABI.

**Inference note.** A `for`-style callback whose expected element param is the `AnyVal` wildcard
(`TypeVar(MAX)`) binds the param DIRECTLY to `AnyVal` rather than to a fresh inference var. Minting a
fresh var there left an ambiguous element (a `[]`+push array) unsolved and defaulted to the wrong
scalar (surfaced as an `i8` element — a real regression caught by the pool stress test). This matches
the prior opaque-`Function` behaviour; a non-MAX generic `T` (map/filter's element) still mints a fresh
var so the receiver can pin it.

## ADR-054: Richer FFI — `Ptr`(=Int64) + `$ORIGIN` rpath + `std/ffi` raw-memory island

**Decision**: Pure Lin can drive a vendored C shared library through three cooperating pieces, none
of which extend the type system or codegen with a new pointer kind:

1. **`Ptr` is a zero-cost alias to `Int64`** (`lin-check/src/resolve.rs`: `"Ptr" => Ok(Type::Int64)`).
   A pointer/handle crossing the FFI boundary is just a 64-bit scalar — never refcounted, ABI-identical
   to a C `void*`, and freely passed back into another foreign function. It is purely a *documentation*
   alias at the source level.
2. **`std/ffi`** (`stdlib/ffi.lin` over `crates/lin-runtime/src/ffi.rs`) is a small "island" of unsafe
   raw-memory primitives — `cstr`/`withCstr`/`alloc`/`free`/`peek*`/`poke*` — for marshalling `String`
   arguments into NUL-terminated C strings and reading/writing fixed-layout structs returned through a
   `void*` out-param. `withCstr(s, body)` is the recommended leak-free idiom: allocate, run the
   callback, free, return its result. Bare `cstr` does not free (the caller owns the buffer) for the
   case where the C API retains the pointer.
3. **Auto `$ORIGIN`-relative rpath** (`lin-compile` `fn link`): for each foreign `.so`/`.dylib` the
   linker is given `-Wl,-rpath,$ORIGIN/<relpath-from-binary-dir-to-so-dir>`, computed with a small
   pure-Rust `relative_path` helper (no `pathdiff` dependency). The link runs via `std::process::Command`
   (no shell), so `$ORIGIN` reaches the ELF dynamic loader literally; `readelf -d` confirms RUNPATH
   carries a literal `$ORIGIN`. The produced binary + co-located `.so` are therefore **relocatable** —
   move both together and the library still resolves at runtime with no `LD_LIBRARY_PATH`.

**Rationale**: The goal was a keystone proving Lin can talk to real C libraries (SDL-shaped) without
adding a foreign-function bindgen, a GC-visible pointer type, or system-path linking. Keeping `Ptr` an
`Int64` alias means zero churn across the ~20 exhaustive `Type` matches in check/IR/codegen, and the raw
primitives live entirely behind a stdlib wrapper over existing `lin_*` runtime symbols.

**Rejected alternatives**:
- **A distinct `Ptr` newtype `Type` variant** — would let the checker forbid arithmetic on raw handles,
  but buys no runtime benefit and fans a new case across every exhaustive `Type` match in the
  compiler. Rejected; the alias is sufficient for the prototype. (A future opaque newtype remains a
  possible follow-up purely for checker ergonomics.)
- **System-path linking (ldconfig / `LD_LIBRARY_PATH` / absolute rpath)** — rejected in favour of a
  *vendored* `.so` reached by a `$ORIGIN`-relative rpath, so a built artifact is self-contained and
  relocatable. An absolute canonicalized rpath remains only as a **fallback** when a relative path
  can't be expressed (different mount / no common ancestor): robust but not relocatable.

**Consequence**: `examples/sdl/` is a two-demo "SDL game" project (`bounce.lin`, `ai_worker.lin`)
that drives the **real SDL3 C ABI** (`SDL_Init`/`SDL_CreateWindow`/`SDL_RenderFillRect`/
`SDL_PollEvent`/`SDL_RenderReadPixels`/…) from pure Lin via `Ptr` handles, a `withCstr` `char*` title,
and an `SDL_FRect` built with four `pokeF32`. Each demo runs a FIXED frame count and self-terminates
(headless SDL3 emits no scripted quit event), then PROVES rendering actually happened by reading a
pixel back with `SDL_RenderReadPixels` — peeking the returned `SDL_Surface`'s `pixels`/`pitch` fields
and asserting the drawn pixel's exact color (XRGB8888 channel order decoded from the surface `format`).
A **real, vendored `libSDL3.so` 3.4.10** (built headless with `SDL_UNIX_CONSOLE_BUILD`, ~2.9 MB,
committed with its `libSDL3.so → .so.0 → .so.0.4.10` soname symlink chain) is linked by relative path;
the binary records `NEEDED libSDL3.so.0` and finds it via the `$ORIGIN` rpath. `ai_worker.lin` adds an
`async` PURE worker (a `World` snapshot deep-copied in, a planned point deep-copied back) to show that
SDL handles stay on the main thread while only values cross the share-nothing boundary. The integration
tests `test_sdl_bounce_headless` / `test_sdl_ai_worker_headless` build each demo and run the binary from
another cwd with `LD_LIBRARY_PATH` cleared and `SDL_VIDEODRIVER=dummy`, proving the rpath chain
end-to-end against the real library. `stdlib/ffi.test.lin` covers the raw-memory helpers and `withCstr`
without needing `lin build`.

**macOS relocatability**: now handled. The rpath token is platform-selected via `cfg!(target_os = "macos")`
— `@loader_path` on macOS (`$ORIGIN` is meaningless to dyld), `$ORIGIN` on Linux. Because an
executable records a dylib's own `install_name` (LC_ID_DYLIB) rather than the linked path, the rpath
is only consulted when that reference is `@rpath/<leaf>`; after a successful link on macOS we do a
best-effort `install_name_tool -change <recorded> @rpath/<leaf> <exe>` (recorded path found by
parsing `otool -L`, matching by leaf incl. soname-versioned variants), and skip silently if the
tools are missing or the entry is already `@rpath/...`. Validated by
`test_ffi_vendored_shared_lib_relocatable` (builds its own dylib with `-install_name @rpath/...`) on
the macos-latest CI leg. The committed SDL3 lib remains a Linux x86-64 ELF, so the SDL demo tests are
Linux-gated.

**Known limitations**: The
vendored SDL3 is Linux x86-64, software-rendered via the dummy driver (proves the ABI + rasterisation,
not GPU output). `withCstr` leaks its buffer only if the callback faults (Lin has no try/finally). A
real bindgen / struct-layout DSL is future work — struct offsets are hand-computed by the programmer
today.

## ADR-055: Typed index-signature object type `{ String: T }` backed by a hashed `LinMap`

**Decision**: Add a typed *index-signature* object type `{ String: T }` (Option A of the
now-retired `typed-map-index-signature` proposal; this ADR is the surviving record) — an object used
as a dictionary with arbitrary String keys all mapping to value type `T`. It is a **new `Type` variant** (`Type::Map(Box<Type>)`,
surface `TypeExpr::IndexSig`) distinct from the fixed-field `Type::Object` record, and is backed at
runtime by a **distinct hashed container `LinMap`** (open-addressing, FNV-1a, linear probing) giving
**O(1) average** lookup/insert — *not* the O(n) association-list `LinObject`.

Surface and checker rules:
- `m[k]` yields `T | Null` (the §6.1 safe-bracket missing-key rule); `m[k] = v` requires `v : T` and
  `k : String`.
- An empty `{}` literal infers `{ String: T }` from its annotated context (binding target / return
  type), else stays a fixed record. A non-empty string-keyed literal checked against a `{ String: T }`
  context produces a `LinMap` whose values are each checked against `T`.
- No implicit `AnyVal → { String: T }` coercion: a `Map` target is a *structured decode* in user code
  (`compat::requires_structured_decode`), so a raw `AnyVal` must go through `fromJson`/narrowing,
  exactly like a structured record (parity with §6.3 / ADR-045). The trusted stdlib stays lenient.
- A `Map` is its OWN type — not structurally compatible with a fixed `Object` in either direction
  (`compat.rs`), covariant in `T` (`Map<U>` → `Map<T>` when `U` compat `T`).
- `is`/`has` against an index-signature type is **disallowed** (the cheaper v1, as the proposal
  permits) — and it falls out for free: an `is`/`has` pattern parses only a `Pattern::TypeName`
  (a bare identifier), so `{ String: T }` is simply not spellable in pattern position.

**Alias keys (follow-up).** The key position originally required the *literal* identifier `String`,
hardcoded in the parser. This was relaxed so the key may be **any type alias that resolves to
`String`** (`type StopID = String` ⇒ `{ StopID: T }`). The parser now recognises the index-signature
form on *any* bare `Ident` followed by `:` (records use quoted-string keys, so the two forms stay
disjoint) and parses the key as a full type-expr; `TypeExpr::IndexSig` carries that key type-expr
(it was value-only before). The "key must be `String`" check **moved from parse time to resolution
time** (`resolve.rs`), where aliases are expanded with the same cycle-visiting set — a key resolving
to anything but `Type::Str` is `Map key type must be String, but it resolves to <T>`. The key
type-expr is preserved (not collapsed to `String`) so the formatter round-trips the alias the user
wrote. Underlying key type is still `String`-only; this is purely a spelling/aliasing convenience.

**Literal-union key ⇒ fixed-record sugar (follow-up).** The resolution-time key dispatch was
extended with a second accepted shape: a key that resolves to a **closed string-literal union**
(or a single `StrLit`). `{ DayOfWeek: Boolean }` where `type DayOfWeek = "Monday" | … | "Sunday"`
is **sugar** that expands to the fixed record `{ "Monday": Boolean, …, "Sunday": Boolean }` — one
field per literal, all of the value type. This is *not* a `Map`/`LinMap`: it resolves to an
ordinary `Type::Object` (unsealed, exactly like an inline object-literal type; the named-type
unfold path seals it when it is a `type T = …` body, so `named ⇒ sealed` is inherited) and is
structurally identical to the hand-written record — assignable both ways. The three key shapes are
disjoint and dispatched in `resolve.rs`'s `IndexSig` arm purely on what the (alias-expanded) key
resolves to: `String` → dynamic `Map`; a closed `StrLit`-union → fixed record; anything else →
`Index-signature key type must be String or a union of string literals, but it resolves to <T>`.
The expansion composes with the total-literal-key index rule (ADR-035 / the safe-bracket §6.1
exception): indexing the expanded record by a key of the *same* literal union covers every field,
so the read is provably total — `calendar[dow] : Boolean`, no `| Null`. **Caveat (deliberate):** the
meaning of `{ K: V }` therefore depends on `K`'s definition (a `String` alias → map; a literal-union
alias → record); these have different runtime representations, so refactoring a key alias between
the two silently flips the type. The overload was chosen over a distinct mapped-type syntax
(`{ [K in U]: V }`) because it is a single additive resolver branch that turns a former *error* into
a record (no existing valid program changes meaning); the explicit syntax is reserved for if/when
key-dependent value types are wanted.

**Backing-representation choice — a distinct `LinMap`, values boxed-but-hashed.** A separate
container (rather than retrofitting a hash side-index onto `LinObject`, the #4b
`hashed-json-object.md` route) **sidesteps the inline `MakeObject` codegen ABI constraint** entirely
(the inline literal path GEPs `LinObject` at `entries@16`, 24-byte stride; `LinMap` is opaque to
codegen — every access goes through `lin_map_*` FFI). Each `LinMap` slot stores
`(key: *mut LinString, value: TaggedVal)` — values **boxed inside the 16-byte TaggedVal exactly like
`LinObject` entries**, so the refcount discipline (retain on store, release on overwrite/free) is
the byte-for-byte proven `object.rs` discipline; only the *lookup* changes from a linear scan to a
hash probe. This was chosen deliberately for **correctness margin**: the recurring UAF/double-free
bug class lives in exactly these value RC paths, and reusing the proven discipline keeps the risk in
the (well-tested) hashing logic, not in novel value ownership. The map's `refcount` sits at offset 0
(u32), so the generic `lin_rc_retain` works for it unchanged.

**Flat-scalar value unboxing — IMPLEMENTED (follow-up, was deferred from v1).** When the value type
`T` is a *flat scalar* (the `is_flat_scalar` set the codebase already unboxes for flat scalar arrays:
`Int8/16/32/64`, `UInt8/16/32/64`, `Float32/64` — Bool excluded, as for flat arrays), the value is
stored **unboxed**: the raw scalar lives **inline in the slot's existing 16-byte `TaggedVal`**
(`tag` = `T`'s boxed-scalar convention, `payload` = the raw scalar bits), with **NO per-value heap
box, NO refcount, and NO box-shell to free**. No `LinMap`/`Slot` layout change was needed and the
runtime container is untouched — the change is entirely in **codegen**:
- *Store* (`emit_map_set`, and the `MakeObject` map-literal path): marshal the scalar through a
  **stack** `TaggedVal` (`build_tagged_val_alloca`) instead of a heap `box_value` + `tagged_release`,
  exactly as a flat scalar **array** slot is written. `lin_map_set` copies the 16 bytes inline and
  `retain_tagged_payload` is already a no-op for a scalar tag (`_ => {}` arm), so the proven set/
  overwrite/free/grow/keys/values/entries RC discipline stays byte-identical — there is simply
  nothing to retain or release for a scalar payload.
- *Width-normalisation*: the value is first coerced (`compile_ir_coerce`) to `T`'s representation, so
  a narrower source (an `Int32` variable stored into a `{ String: Int64 }` map) reads back
  `T`-correct (signed-extended to Int64, `is Int64` matches), **fixing** the v1 width limitation
  noted below.
- *Missing-key / `T | Null` representation*: presence is tracked **solely by the slot's `key`**
  (`key.is_null()` = empty slot), entirely independent of the value bytes — so unboxing introduces no
  sentinel ambiguity. `m[k]` is typed `T | Null` (a union), so codegen returns the **borrowed
  interior `&slot.value`** (a `TaggedVal*`) verbatim — null pointer for a missing key (→ language
  `Null`), or the inline scalar `TaggedVal` for a present key. Because the union result of a
  projection is *not* `is_rc_type`, the IR treats it as a borrowed interior pointer (identical to the
  established `lin_object_get` projection contract) and never retains/releases it — which is trivially
  sound for a scalar (no inner heap payload). `match m[k] is Int64 => …` unboxes by reading those
  bytes via the normal tag-dispatch. **Verified under AddressSanitizer**: the flat-scalar store/
  lookup/keys/values/entries/free path is corruption-clean (no UAF/double-free/overflow), and a
  String-valued map exercising the unchanged boxed path leaks identically — i.e. the only ASan
  finding (a `var`-local map not released at recursive-function scope exit) is **pre-existing and
  representation-independent**, not introduced by unboxing.

**Relationship to `hashed-json-object.md` / ADR-056 (#4b).** #4b *did* land independently as
**ADR-056** (a lazy O(1) hash side-index on large generic `AnyVal` objects). The two are
**complementary, not redundant**, and operate at different layers: ADR-056 makes the *untyped* `{}`
/ `AnyVal` dictionary O(1) at runtime with zero surface-language change, while this ADR adds a *typed*
`{ String: T }` surface form (fidelity + a value type that flows through the stdlib) backed by its
own `LinMap`. Code that wants a typed dictionary uses `{ String: T }` and gets O(1) by construction
and an unboxed scalar payload; code that stays on dynamic `AnyVal` still gets O(1) lookup from ADR-056.
The earlier draft of this ADR (written before ADR-056 merged) claimed to *supersede* #4b on the
assumption it would be left unimplemented — that is no longer accurate: both shipped and coexist.

**Stdlib.** `std/object`'s `keys`/`values`/`entries` are made **tag-aware** (new runtime bridges
`lin_keys_any`/`lin_values_any`/`lin_entries_any` dispatch on the boxed value's tag — TAG_OBJECT →
`LinObject`, TAG_MAP → `LinMap`), so the SAME functions work over both an `AnyVal`/`{}` record and a
typed map. `length`/`isEmpty` likewise handle TAG_MAP (`lin_map_length`, `lin_length_dyn`). The
constructor cluster (`fromEntries`/`merge`/`pick`/`omit`/`mapValues`) stays on the `AnyVal`/`LinObject`
representation (a map and a record are different runtime containers; silently re-routing them would
change behaviour) — they remain `AnyVal`-typed and unchanged.

**Performance (microbenchmark `benchmarks/map_index_signature.lin`, insert+lookup of N distinct
keys, debug-built compiler / O2 output):**

| N | `{ String: T }` (LinMap) | `AnyVal` object (assoc-list) | speedup |
|------:|------:|------:|------:|
| 10000 | 18 ms | 1824 ms | ~101x |
| 20000 | 35 ms | 5664 ms | ~162x |
| 40000 | 76 ms | 26920 ms | ~354x |
| 80000 | 308 ms | 151969 ms | ~493x |

The map roughly doubles as N doubles (linear); the `AnyVal` object roughly quadruples (quadratic). At
N=200000 the `AnyVal` version exceeds 120 s while the map finishes in ~0.4 s.

**Flat-scalar unboxing — measured delta.** A value-churn microbenchmark
(`benchmarks/map_flat_scalar.lin`: an `{ String: Int64 }` map, 64-key set × 100k rounds of overwrite
+ read-back, debug-built compiler / O2 output, median of 7) goes ~5575 ms (boxed) → ~5314 ms
(unboxed), about **5% faster** — the unboxed store does NO heap box allocation, NO box-shell free, and
NO value refcount, vs a `lin_box_int64` + `lin_tagged_release` per store on the boxed path
(confirmed in the emitted IR). The win is bounded because the dominant per-store cost is the
non-inlined `lin_map_set` (hash + probe) and the small-integer box cache already makes scalar boxing
cheap; the structural payoff is **zero value heap traffic / zero value RC** for scalar maps plus the
width-correctness fix above (the boxed path mis-tagged an `Int32`→`{String:Int64}` store, reading
back as `Int32` and yielding wrong results under an `is Int64` match; the unboxed path widens to `T`
and reads back correctly).

**Rejected alternative — a nominal `Map<K, V>` container (Option B).** More powerful (non-String
keys, cleanly separates dictionaries from records) but a larger surface (new literal/constructor
syntax, `for`/destructuring/equality interactions) and a discoverability footgun (users reach for
`{}` first). The index-signature form is the smaller change, tightens the already-discoverable `{}`
type, and is exactly the String-keyed shape the RAPTOR maps need. Non-String-keyed maps can be
revisited later as an addition.

**Known limitations / follow-ups**: a flat-scalar value `T` is now stored **unboxed** and reads back
`T`-width-correct (see "Flat-scalar value unboxing" above — both the old "values are boxed" and the
old "an `Int32` stored into a `{ String: Int64 }` reads back tagged Int32" limitations are now
resolved for flat-scalar maps). A **non-scalar** `T` (String/Array/Object/nested-Map/union) is still
stored boxed (the proven `object.rs` value RC discipline). `fromJson<{String:T}>` is not a v1 decode
target (the decoder produces a `LinObject`, not a `LinMap` — the descriptor writer treats `Map` as
accept-any only for match exhaustiveness); `keys`/`values`/`entries` over a map are hash-order, not
insertion-order. A `var`-local map that goes out of scope inside a recursive function is not released
at scope exit (a **pre-existing**, representation-independent IR-lowering gap — reproduces on the
boxed String-valued path too; unrelated to unboxing).

## ADR-056: Large `AnyVal` objects get a lazy O(1) hash side-index (RAPTOR #4b)

**Decision**: `AnyVal` objects keep their association-list `entries` buffer as the source of truth, but
gain an **optional, lazily-built open-addressing hash side-index** for O(1)-average key lookup once an
object grows past a threshold (`HASH_INDEX_THRESHOLD = 16` entries). The change lives entirely in
`crates/lin-runtime/src/object.rs`; no codegen change was required.

- **Layout**: three fields are *appended* to `LinObject` at byte offsets ≥ 24 — `index: *mut u32`
  (@24, open-addressing table of `entry_slot + 1`; `0` = empty cell, `null` = no table yet),
  `index_cap: u32` (@32, power-of-two table size or 0), `index_dirty: u32` (@36). The existing header
  fields are **untouched**: `refcount`@0, `len`@4, `cap`@8, `flags`@12, `entries`@16, and the 24-byte
  `LinObjectEntry` stride. This preserves the ABI contract codegen's `MakeObject` inline-literal path
  depends on (it does direct GEP at those hardcoded offsets and reads only `len`@4 / `entries`@16 / the
  24-byte entries — audited on HEAD, never touches ≥ 24).
- **Lazy build + probe**: `lin_object_get` / `lin_object_has` build the table on first access when
  `len >= THRESHOLD` and (`index.is_null() || index_dirty != 0`), then probe in O(1) average. Below the
  threshold the linear scan is kept — faster for tiny N and allocation-free, and small objects stay
  byte-for-byte unchanged. The lazy trigger **must** key off `index == null || index_dirty` because the
  codegen inline-literal path builds large literals **without** calling `lin_object_set`, so no
  constructor can be assumed to have maintained the index.
- **Maintenance**: `lin_object_set`'s append branch inserts the new slot in O(1) when a table exists
  (entry *slot indices* are stable across an `entries` realloc — only the buffer base moves — so the
  table survives a grow); the overwrite branch is a no-op for the index (same key, same slot). When an
  append would push the load factor past ~0.7, the table is marked `index_dirty` for a larger rebuild on
  next lookup. `lin_object_merge` / `lin_object_copy_except` route through `lin_object_set`, so they are
  maintained automatically. `lin_object_release` frees the table before the header.
- **Hash**: FNV-1a over the key bytes; linear probing on a power-of-two table.

**Why a side-index over the alternatives** (full write-up was in the now-deleted
`docs/proposals/hashed-json-object.md`): it removes the O(n²) wall for the language's *existing,
discoverable* `{}` type with zero surface-language change, no small-object perf change, and no codegen
ABI change. A dedicated `Map<K,V>` runtime/stdlib type (proposal option b) was rejected as the *first*
move because users reach for `{}` first and would keep hitting the wall; it remains a possible additive
follow-up for non-string keys. (See ADR-055 for the typed-map `{ String: T }` *surface syntax*
direction, which is orthogonal to this runtime representation.)

**Correctness/safety**: the index stores **only `u32` slot indices — never refcounted pointers** — so a
bug here is structurally outside the UAF/double-free class that `object.rs` is otherwise prone to; the
worst a stale index can do is point at the wrong slot, which is defended against by reconfirming **every
probe hit** with `lin_string_key_eq`. Proven by a Rust fuzz/oracle test
(`crates/lin-runtime/tests/object_index_fuzz.rs`): for N = 0,1,15,16,17,64,1000 it interleaves
set/overwrite/merge/copy_except/release and asserts `get`/`has`/`keys` agree with a linear-scan oracle on
every key including absent ones, validated under ASan for leaks/UAF.

**Consequence**: building an N-key dictionary by repeated `set`/`get` drops from O(n²) to O(n)
(microbench: 16k-key build 142ms → 0.7ms). The motivating RAPTOR port's index-build (`PREP`) phase fell
from ~145s to ~27s with the cross-language correctness digest unchanged
(`group=26203913 range=773022892 journeys=139`). The RAPTOR loader's sorted-array `bsearch` /
contiguous-run grouping workarounds (adopted to avoid big object maps) are now unnecessary and could be
simplified back to plain `{}` maps — a follow-up, not part of this change.

## ADR-057: Sealed records — unboxed struct layout for named record types

**Decision**: A **named** record type (`type T = { … }`) is *sealed*: its runtime
values are laid out as an **unboxed, constant-offset struct** instead of the
uniform boxed, refcounted, string-keyed `LinObject`. Crucially, this changes
**representation only** — type **compatibility stays structural**. A wider or
`AnyVal` value is still assignable where `T` is expected (§5.9 width subtyping is
unchanged); at that boundary it is **projected** by a *non-mutating copy* into a
fresh sealed value holding exactly `T`'s fields. The source is untouched and
keeps any extra fields in its own scope.

This is what makes a fixed-offset layout *sound* under width subtyping: a value
of a named type is **never reinterpreted in place** as a different layout — it is
always copied into the canonical layout at the boundary — so a given named type
always has exactly one physical layout, and field offsets are unambiguous.

Layout (all in `crates/lin-runtime/src/sealed.rs`): `[ u32 rc | u32 size | u64
desc_ptr | fields… ]`. Scalar fields are stored inline at natural-aligned byte
offsets (declaration order); heap fields (String/Array/nested-sealed) are 8-byte
owned pointer slots. `desc_ptr` points at a static, codegen-emitted **field
descriptor** `{ count, {offset, kind}* }` listing the heap fields — it reaches
every drop site (scope exit, closure-capture release, thread-transfer, var
reassign) without the static type, driving per-field retain (on construct /
projection-copy) and release (on drop, nested recursing). `rc` at offset 0 means
the existing `lin_rc_retain`/RC machinery works unchanged. Field read is a
constant-offset load; construction stores fields by offset with no string keys.
Operations that need the universal JSON shape (`==` cross-representation,
`toString`, `keys`, spread, dynamic `obj[k]`, `AnyVal` boundary, thread-transfer)
**materialize** the struct into a boxed `LinObject` at that edge — correct, and
the slow path is only the rare ops, not field access.

Arrays of **all-scalar** sealed records (`T[]`) are stored as a `LinArray` of
**contiguous, header-less, packed element payloads** (new `elem_tag = 0xFE`;
element stride + descriptor in trailing `LinArray` fields, leaving the
flat/tagged element offsets untouched). The array owns its elements, so there is
no per-element refcount; `arr[i].field` is a constant-stride GEP + load.

**Rationale**: Object/record-heavy code paid a `lin_object_get` hash lookup +
boxed-pointer chase + unbox on *every* field access, regardless of how precisely
typed (`crates/lin-codegen/src/codegen/types.rs` previously discarded the known
`Type::Object` shape). A sequence of measured experiments (box pool, Perceus
reuse, box elimination — all in `docs/`/memory) established the cost is the
representation, not the allocator. Sealed layout removes the call, the lookup,
and the box in one move. Measured: **~83×** faster field access on an
access-bound scalar-record loop, **~5.9×** on a mixed String-field record,
**~87×** on a scalar-record array versus a boxed `AnyVal[]`, and **~5.7×** on the
`records` cross-language benchmark versus the same code typed `:AnyVal`.

A `Type::Object` carries a `sealed: bool` (set when a named record type resolves;
`false` for anonymous literals, inferred shapes, and the `Error` alias). The flag
is **representation-only**: the manual `impl PartialEq for Type` ignores it, and
`compat.rs` ignores it, so inference, narrowing, exhaustiveness, and structural
compatibility are exactly as before.

**Rejected / deferred alternatives**:
- **Sealing by shape (any all-scalar object, named or anonymous)** — rejected: it
  would seal pervasive anonymous `{x,y}` literals and force conversions at most
  boundaries. Gating on the *named* `sealed` marker keeps the fast layout where a
  programmer named a precise shape, and keeps anonymous/`AnyVal` flow boxed.
- **Opt-in `sealed`/`exact` keyword** — rejected in favour of "all named record
  types are sealed", which needs no new syntax and matches the intuition that
  naming a type names an exact shape.
- **NaN-boxing / a non-copying reinterpretation of a wider value as a narrower
  layout** — unsound under width subtyping + unordered objects; the non-mutating
  boundary projection is what restores soundness.
- **Stack allocation of non-escaping sealed records (Stage 4)** — *initially*
  rejected: the first prototype kept the lowering owning-model's per-read
  `lin_rc_retain`/`lin_sealed_release` *calls* on the stack value (made no-ops by an
  immortal-RC guard, but the guarded calls across the non-inlinable runtime boundary
  cost more than the cheap heap allocation they replaced — **~12% slower** on
  `records`). **Now SHIPPED** once the stated prerequisite was implemented:
  `lin_ir::escape` is a sound escape analysis (carry-class union-find over
  representation-preserving aliasing edges + self-`TailCall` arg↔param unification;
  fail-safe to heap on any Return / container-store / closure-capture / repr-changing
  coerce / unknown-retaining-call use) that marks each non-escaping all-scalar sealed
  `MakeObject` with `stack = true` AND **deletes every `Retain`/`Release` instruction
  on the proven-stack-resident carry class** (so the no-op RC calls vanish from the IR
  entirely and the entry-block `alloca` SROA-promotes to registers). The immortal-RC
  header sentinel is kept as defense-in-depth. Result: the `records` `@step` hot loop
  went from 25 retain / 25 release / heap-alloc per iteration to **0 / 0 / a reused
  stack alloca**; measured **3532 ms → 583 ms (~6.1×)** on the `records` benchmark
  (median of 11 interleaved runs vs current master), turning the old regression into
  a win that **beats Go** (≈717 ms) and closes toward Rust (≈240 ms). ASan-clean over
  the full `stdlib`+`examples` corpus plus dedicated escaping fixtures (returned /
  array-stored / closure-captured stay heap; high-N TCO loop has no stack growth and
  no use-after-return). Heap-field sealed records remain heap (Stage-4 scope =
  all-scalar only).

**Consequence**: One stdlib type required migration, and it is the canonical
illustration of the one user-visible semantic change. `std/test`'s
`Assertion = { "type": String }` deliberately used a named type as an **open
carrier** — a failing assertion stuffed extra keys (`message`/`expected`/
`actual`) that the reporter reads by name. Under sealed semantics a named-typed
value drops extra fields on the projection copy, so the assertion helpers/matchers
now return `AnyVal` (values keep their extra keys; the array-of-assertions test-body
guard is preserved at the array level). **The idiom that sealed records break:** a
named record type used as a deliberately-extensible bag of extra fields. Code that
needs open extra-field carry-through must type the value `AnyVal`, not a named
record. This is the §5.9.1 lossy-projection rule (see SPECIFICATION). Implemented
across Stages 0.5 (inert `sealed` marker through resolution), 1 (scalar records),
2 (heap-field records), and 3 (scalar-record arrays); arrays of heap-field records
remain boxed (a follow-up). The whole change is ASan-verified (the per-field RC at
every drop site is the UAF/double-free-prone surface) and run-equivalence-checked
over `stdlib/`+`examples/`.

## ADR-058: An evidence-free empty collection literal requires a type annotation

**Status**: Accepted.

**Context**: A context-free empty literal inferred a degenerate element type — `[]` →
`Array(Never)`, `{}` → an empty record `{ }`. This silently misbehaved: the type carried no
usable element information, so it forced array-building primitives (`std/array.push`/`append`/
`prepend`/`compact`) to keep an opaque `(AnyVal, …)` signature, which in turn meant the element
type was never checked — `push(intArr, "a string")` type-checked and would corrupt at runtime.

**Decision**: An empty collection literal (`[]` or `{}`) with **neither contextual type evidence
nor contents** is a compile error that asks for an annotation. "Contextual evidence" is any of: an
annotation on the binding, a typed function parameter (argument position), a declared return type,
or a typed array element. The check is deliberately narrow and surgical: it fires only in the
genuinely contextless position — an un-annotated `val`/`var` binding whose value is a *bare* `[]`
or `{}` (`checker/stmt.rs`, gated on the `expected.is_none()` branch via `empty_literal_kind` in
`checker/helpers.rs`). Every contextual empty still flows through `check_expr` with the expected
type pushed down and is **unaffected** — `val a: Int32[] = []`, `f([])` into a typed param, a
`(): Int32[] => []` return, `val m: { String: Int32 } = {}`, and `val o: {} = {}` for a dynamic
record all keep working exactly as before.

Error messages:
- array: `cannot infer the element type of an empty array literal; add a type annotation, e.g.
  `val xs: Int32[] = []``
- map/object: `cannot infer the value type of an empty map/object literal; add a type annotation,
  e.g. `val m: { String: Int32 } = {}``

**Phase 2 (the array-builder generic payoff) is DEFERRED, not delivered.** With an annotated
accumulator the natural follow-up is `push = <T>(arr: T[], item: T): Null` (and likewise
`append`/`prepend`), which would finally CHECK the element type and close the `push(intArr,
"str")` hole. That generic form does NOT compile correctly today: the monomorphized body
`push$<T>` calls the `lin_push` intrinsic with its `item: T` param, and the intrinsic mishandles a
BOXED element arriving across that param boundary for several element representations —
- a borrowed `AnyVal` element (e.g. `push(acc, m["k"])` over a stream `.for`) **double-frees** (the
  codegen `Array(AnyVal)` path routes to `lin_array_push_tagged`, which CONSUMES the box without an
  inner retain, contradicting the IR's `transfer_into_container` retain-semantics assumption); and
- a concrete heap-object element (e.g. `push(pqEntries, e)` over a `{ … }[]`) is stored as a raw
  object pointer though it is actually a `TaggedVal*` box → an `index_probe` crash on read.

These are pre-existing monomorphized-body/intrinsic representation gaps that the empty-literal
requirement does not remove. Making `push`/`append`/`prepend` generic is gated on first fixing that
representation contract (ASan-verified RC work). `append`/`prepend` carry an additional independent
blocker: a literal item (`b.append(3)` on a `UInt8[]`) defaults to `Int32` and splits the static
`T` from the flat runtime representation (a call-site literal-width inference gap). `compact` keeps
`AnyVal` because its natural `<T>((T | Null)[]): T[]` is still unparseable (postfix `[]` on a
parenthesized union). `arrayAllocate` stays `(Int32): AnyVal` (a return-only `T` would force an
annotation at every call site). So Phase 2 remains future work; `push`/`append`/`prepend` keep
their `(AnyVal, …)` signatures for now.

**Consequence**: The newly-erroring evidence-free empty literals across the repo were annotated
(`stdlib/string.lin`, `stdlib/array.lin`, `stdlib/iter.lin`, `stdlib/object.lin`, plus
`*.test.lin` and several `examples/`/`benchmarks/`/`docs-site/builder/` accumulators). The
`std/test`-style dynamic record accumulator pattern (`var o = {}; o[k] = v`) annotates as `: {}`
(the empty-record type, preserving the growable `LinObject` backing) — NOT `AnyVal`, which has a
pre-existing escaping-object bug, and NOT `{ String: T }`, which switches to the hashed `LinMap`
backing and changes `toString`/`keys` behavior.

## ADR-059: Generic `push`/`append`/`prepend` (`<T>(arr: T[], item: T)`) — Phase 2 delivered

**Status**: Accepted. (Delivers Phase 2 deferred by ADR-058; depends on ADR-058's empty-literal
annotation requirement.)

**Context**: ADR-058 deferred making `push`/`append`/`prepend` generic because the generic body
exposed codegen/RC gaps. On current master several relevant fixes had landed (null-complement
narrowing, empty-array-arg flat-scalar element adoption, is-pattern TypeVar substitution in
monomorphization). Re-establishing the gaps on master under AddressSanitizer found that the two RC
gaps ADR-058 named (borrowed-`AnyVal` double-free; concrete-object `index_probe` crash) **no longer
reproduce** — the borrowed-`AnyVal` push is RC-balanced, and the concrete-object push reads back
correctly — and the `append(UInt8[], 3)` literal-width gap is closed at the *empty-array-arg* level.
But making the signatures generic surfaced four NEW representation gaps, now fixed:

1. **Literal-width clobber across args** (`append(uint8Arr, 221)` → `Int32[]`): the bare integer
   literal's `Int32` default overwrote the `T = UInt8` binding the array arg established. Fixed in
   `lin-check` (`checker/call.rs`): a bare `IntLit` item DEFERS its substitution and only binds an
   otherwise-unbound `T`; the canonical binding is taken from the first (container) arg, and a later
   compatible arg never clobbers it (`collect_and_save_subs_no_clobber`). Applied to both
   `infer_call` and `infer_dot_call`.
2. **Concrete-object element corruption** (`push(out: Field[], {…})` crashed at `object.rs:195` /
   misaligned scalar deref): the element was projected into a SEALED struct but stored raw into the
   TAGGED array under `TAG_OBJECT`. Fixed in `lin-codegen` (`codegen/data.rs`,
   `tagged_array_push_value`): a sealed-repr Object element is MATERIALIZED to a boxed `LinObject`
   before the tagged store. The matching RC: the source sealed struct STAYS OWNED (the
   materialization retains its heap fields; scope-exit's `lin_sealed_release` balances them), so
   the IR's per-element transfer-retain is skipped for this case (`lin-ir` `lower.rs`,
   `push_sealed_elem_into_tagged`, mirroring `push_into_sealed_array`).
3. **Nested generic push in a cross-module generic** (`mymap<T,U>`'s `push(result: U[], …)`
   monomorphized to `mymap$Int32_Int32` heap-buffer-overflowed): the re-homed `push` (an
   import-of-import thin intrinsic wrapper) was re-homed to the boxed `std_array_push` `$AnyVal`
   specialization instead of inlined to the `lin_push` intrinsic. Fixed in `lin-ir`
   (`monomorphize.rs`, `classify_origin_slot`'s `Import` arm): an import-of-import whose source
   defines a thin intrinsic wrapper is INLINED to the intrinsic, so `lin_push`'s runtime
   element-tag dispatch keeps the flat representation.
4. **Dynamic (`AnyVal`) element into a concrete-scalar array** (`push(buf: UInt8[], src[i]: AnyVal)` →
   `zext ptr` codegen error): a genuinely-`AnyVal` (or leftover inference-var) item flowing into a
   bare-`TypeVar` param a container arg pinned to a concrete type is REBOUND to the `AnyVal` wildcard,
   so the call monomorphizes at `$AnyVal` (→ `lin_push_dyn`, which converts the boxed element into the
   array's runtime slot — the non-generic `push` behaviour). Fixed in both `lin-check`
   (`infer_call`/`infer_dot_call`) and `lin-ir` (`monomorphize.rs`), since the monomorphizer
   re-derives subs independently.

**Decision**: `push = <T>(arr: T[], item: T): Null`, `append`/`prepend` = `<T>(arr: T[], item: T):
T[]`. The element type is now CHECKED — `push(intArr, "str")` / `append(intArr, "s")` are compile
errors, closing the soundness hole. `compact` stays `(AnyVal): AnyVal` (its natural
`<T>((T | Null)[]): T[]` is still unparseable — postfix `[]` on a parenthesized union). The
empty-literal annotation requirement (ADR-058) is a HARD PREREQUISITE: without it an unannotated
`val acc = []` is `Never[]` and generic push mis-stores elements as `null`; with it the accumulator
pins `T`. This branch therefore carries ADR-058's checker change and annotations (cherry-picked) and
**must land at or after Phase 1**.

**Verification**: RC balance verified under AddressSanitizer (runtime built with
`-Zsanitizer=address`, linked via clang `-fsanitize=address`). The borrowed-`AnyVal`, closure-`AnyVal`,
concrete-object, and cross-module-generic push churn loops (5000 iters) are clean of
use-after-free / double-free / heap-overflow. A pre-existing, change-independent leak in the
sealed-record-array push path remains on master (same magnitude with the old `AnyVal` `push`); it is
out of scope here.

**Consequence**: No repo-wide call-site sweep was needed beyond ADR-058's annotations — the four
fixes make every existing `push`/`append`/`prepend` call site (incl. `examples/codec`,
`examples/raspberry-controller`, the `decode`/`appendBytes` `AnyVal`-element patterns, and the
cross-module `mymap`) compile and run correctly. Regression tests in
`crates/lin/tests/integration.rs` and `stdlib/array.test.lin`.

## ADR-060: Enforce `lin_*` intrinsics are stdlib-only

**Status**: Accepted. (Enforces the long-standing rule from ADR-002/ADR-008.)

**Context**: Compiler builtins use `lin_*` names (`lin_print`, `lin_object_set`, `lin_array_allocate`,
`lin_map`, …). Per ADR-002/ADR-008 and CLAUDE.md these are *stdlib-internal only* — user code must
use the clean stdlib re-exports (`print` from `std/io`, `arrayAllocate` from `std/array`,
index-assignment `obj[k] = v` instead of `lin_object_set`). The rule was documented but **not
enforced**: `register_intrinsics()` defines all 72 `lin_*` intrinsics into *every* module's type env,
so user code could call them directly (`examples/dijkstra` was found doing exactly this).

**Decision**: Gate intrinsic name resolution on a per-module `Checker.allow_intrinsics: bool`. The
flag is the OR of two things, set by the compile pipeline: `is_stdlib` (the trusted-stdlib signal
already threaded per module, the same source as `lenient_json`) and the `LIN_ALLOW_INTRINSICS`
test-only escape-hatch env var. A dedicated flag rather than reusing `lenient_json` keeps the two
meanings distinct (`lenient_json` is specifically about AnyVal→concrete coercion).

**Choke point**: every bare-name reference — call targets `lin_foo(...)` (via `infer_call` →
`infer_expr`) and intrinsic-as-value uses — flows through `Checker::infer_ident`. After the binding
resolves to a `slot`, if `!allow_intrinsics` and `self.intrinsic_slots.contains_key(&slot)`, emit a
hard error ("`<name>` is a compiler-internal intrinsic and cannot be used in user code") with a help
pointing at the stdlib equivalent. The check sits before capture-tracking / stream-affine logic.

**Escape hatch**: `LIN_ALLOW_INTRINSICS` re-enables intrinsics for the compiler's own
monomorphization/codegen fixtures, which legitimately drive `lin_*` directly from user-level `.lin`
sources (e.g. the IR-proof tests asserting flat-array allocation, the object-grow tests). The
integration-test helpers (`run`, `run_expect_err`, `check_source`, `lin_check_ok*`, and the inline
IR-proof `Command`s) set it via the shared `lin_cmd()` builder; the negative regression test
(`test_intrinsic_rejected_in_user_code`) uses a bare `Command` *without* it.

**Unaffected**: ffi-declared `lin_*` foreign symbols in the stdlib (`lin_signal_wait`,
`lin_io_read_line`, `lin_string_trim`, …) are NOT in `intrinsic_slots` — they are declared via
`import foreign "lin-runtime"` — so gating on `intrinsic_slots` membership leaves them alone. Stdlib
modules check with `allow_intrinsics = true`, so their own intrinsic re-exports keep working.

## ADR-061: Record intersection types (`&`)

**Status**: Accepted.

**Context**: Lin had union (`|`) but no way to say "this record plus these extra fields". The
language author wanted `type OldPerson = Person & { "wisdom": Boolean }` to produce the record with
all of `Person`'s fields plus `wisdom`, without re-typing the base.

**Decision**: Add a **record-only** intersection operator `&` at the type level. `A & B` resolves to
a plain `Type::Object` whose fields are the UNION of both operands' fields. There is NO new runtime
or codegen representation — the result is an ordinary record type, so sealed-records (ADR-057) and
all width-subtyping machinery apply to it unchanged.

- **Grammar/precedence**: `&` binds tighter than `|` (TypeScript convention), so `A & B | C` parses
  as `(A & B) | C`. It is left-associative; `A & B & C` merges all three. Implemented as a new
  parser level `parse_type_intersection` sitting between `parse_type_expr` (`|`) and
  `parse_type_primary` (the leaves), in `crates/lin-parse/src/parser/types.rs`. New AST node
  `TypeExpr::Intersection(Vec<TypeExpr>, Span)` (`crates/lin-parse/src/ast.rs`). A single operand
  passes straight through, so non-intersection types are unaffected.
- **Resolution** (`crates/lin-check/src/resolve.rs`): resolve each operand; each must be a
  `Type::Object` or it is an error. Merge the field IndexMaps left-to-right; a key seen twice must
  have the SAME field type (de-dup) or it is an error.
- **Field conflict** (same key, different types) → `intersection type has conflicting field "k": T1 vs T2`.
- **Non-record operand** → `intersection \`&\` is only valid between record types; operand \`T\` is not a record`.
- **`sealed` flag**: the merged object is produced UNSEALED, exactly like an inline object-literal
  annotation. When the intersection is the body of `type T = A & B`, the named-annotation path
  (`expand_named_body`) seals the unfolded `Type::Object`, so **named = sealed** is inherited
  automatically — no special-casing needed.

**Consequences**: No codegen/IR change — the result is a `Type::Object` like any other (verified:
`cargo build --workspace` is clean, no exhaustiveness arm needed in codegen/IR, which never matched
on `TypeExpr` directly). The formatter gained an `Intersection` arm (`A & B`) so `&` round-trips.
Restriction is documented: intersection is record-only in this first cut (no field-type
intersection, no `&` with unions/scalars). A richer "intersect the field types" semantics is left
for later; the clean sound rule for records is "fields must agree or error".

## ADR-062: Representation-inference pass (packed-vs-boxed as a dataflow fact, not a Type attribute)

**Status**: **SUPERSEDED by ADR-069** (the representation reset, 2026-06-14). The flow-sensitive
packed-vs-boxed inference + `verify()` lattice + boxed-shadow this ADR introduced were the origin of
the "path-9" dead ends; the reset makes representation **type-determined** (a record is *always* a flat
packed struct; the dynamic case is the runtime-tagged `TAG_RECORD`/`AnyVal`), deletes the flow inference
and the boxed shadow, and collapses `repr.rs` to a pure layout calculator. Retained below for history.

> Original status: Accepted (single-owner direction landed incrementally; `verify()` now covers every
repr-consuming opcode; the producer/consumer literal-drift prerequisite is fixed; heap-field array
packing characterized as a sound partial with one whole-program blocker — see Consequences).

> **Updated (2026-06-11).** Two corrections after the path-9 close-out and a dead-code sweep:
> 1. **The `BoxKeepPacked`/`UnboxKeepPacked` *IR opcodes* described below were deleted** (`22a769b0`):
>    they had **zero construction sites** workspace-wide (the Stage-4 keep-packed-through-containers
>    machinery was never emitted on the live path). The *codegen helpers* `compile_ir_box_keep_packed`/
>    `compile_ir_unbox_keep_packed` survive and are still called directly from `emit_map_set` /
>    `compile_ir_index` for the `{String: Sealed[]}` map-value keep-packed store/read — so the zero-copy
>    box/unbox-by-pointer behaviour the lattice relies on is intact; only the never-fired IR-instruction
>    wrappers are gone. Concurrently `Inner::WrapsPacked(Layout)` (its only consumer treated it as
>    `Opaque`, and it was producible only via the dead seed) and the unread `PackedSealedArray.on_heap`
>    field were removed. `Inner` is kept as an enum (just `Opaque`) for cheap re-land.
> 2. **Heap-field record-array packing is now CLOSED-NEGATIVE, not "a sound partial pending one
>    blocker."** The "remaining whole-program blocker" framing in Consequences was the *capability*
>    question; the *value* question was answered by path-9 (see `docs/PERFORMANCE.md` §5 and the ADR-063
>    update): fully packing heap-field record graphs end-to-end through generic boundaries measured
>    **~1.8–3.5× SLOWER** (RAPTOR), because the cost is representation-boundary **materialization**, not
>    the field read. The gate therefore stays scalar-only **by decision, not by missing engineering**.
>    The all-scalar sealed-record path (the part that *did* pay — ADR-057, the `records` win) is
>    unaffected. This ADR's machinery (the lattice, the single-owner direction, the verify/oracle gates)
>    remains the live representation pass; only the heap-field *extension* is abandoned.

**Context**: Sealed records (ADR-057) and sealed-record arrays are laid out as a *packed* physical
representation — a header-less `[rc|size|desc|fields…]` struct, and a contiguous `elem_tag == 0xFE`
`LinArray` of such payloads — that the dynamic `LinObject`/`TaggedVal` machinery cannot read. Whether
a given value is in the packed form or the boxed form is a *physical* fact, NOT something the static
`Type` can express: the same static type (e.g. `Neighbor[]`) is *packed* in a just-constructed temp
but *boxed-wrapping-a-still-packed-buffer* in a temp read back from a `{String: Neighbor[]}` map slot
(map values are always `TaggedVal`). Historically the packed-vs-boxed decision was REPLICATED across
three type-driven predicate families — `Codegen::sealed_array_elem`/`sealed_fields`/`is_flat_scalar`,
`lin_ir::lower::is_sealed_scalar_array` (+ the `lower_coerce_arg` coercion triggers), and
`lin_ir::monomorphize::field_packed_scalar`/`mentions_sealed` — that had to be kept byte-for-byte in
lockstep. Any drift was a silent boxed-vs-packed mismatch: a UAF or a wrong-shaped release that
`cargo test` does not catch (only ASan does).

**Decision**: Introduce a per-function representation-inference pass (`crates/lin-ir/src/repr.rs`,
run immediately before `rc_elide`) that computes a per-temp representation lattice and stores it on
`LinFunction.repr`, indexed by `Temp.0`. Codegen reads `func.repr[t]` at the decide/assume sites
instead of re-deriving from `Type`.

- **Lattice** (per-temp, flow-sensitive): `Repr ::= Unknown(TOP) | Packed(Layout) | Boxed(Inner) |
  FlatScalar(ScalarTy) | Bottom`, where `Layout` is `PackedStruct{fields}` or
  `PackedSealedArray{elem_layout, on_heap}`, and `Inner` is `Opaque` or **`WrapsPacked(Layout)`** —
  the key refinement: a boxed `TaggedVal`/`LinObject` slot whose payload pointer is a STILL-PACKED
  buffer (keep-packed-by-pointer, zero-copy). `WrapsPacked` is **unspeakable in the type system**, which
  is precisely why representation lives on a side table, not on `Type`. FAIL-SAFE: anything not proven
  is `Boxed(Opaque)`; a `Packed` label is only ever assigned by proof from a definite packed producer
  carried along representation-preserving edges (shared union-find carry classes, `carry.rs`).
- **Single-owner principle**: representation is decided in ONE place. The layout computers
  (`sealed_fields`/`sealed_array_elem`) survive only as repr.rs's seed-time oracle — the LAST place a
  `Type` predicate runs — and the bridge helpers (`sealed_array_to_tagged`/`sealed_project_from`/
  `sealed_array_materialize_elem`/`emit_sealed_release`) survive as the *lowered form* of pass-decided
  coercions, no longer called from type-guessing arms.
- **Keep-packed-by-pointer** (Stage 4): `BoxKeepPacked`/`UnboxKeepPacked` IR ops wrap/unwrap a packed
  pointer into/out of a `TaggedVal` in O(1) without materializing. A `{String: Sealed[]}` map store is
  one 16-byte tagged write over the existing `0xFE` buffer; the read-back is a tag-checked pointer load
  feeding a packed reader directly — no per-access O(n) materialize (the dijkstra map hot-loop fix).
- **Boundary catalogue**: an *island* (a carry class with no boxed seed and no conflict edge) stays
  byte-for-byte the current packed codegen — so the constant-offset typed loads / contiguous pushes /
  packed `sealed_eq` of a static loop kernel (the ~87x speedups) are preserved by construction. The
  pass only acts at boundaries: container stores (keep-packed), genuinely-dynamic consumers
  (toString/keys/spread/AnyVal/FFI/equality → materialize once), union membership (box), and
  cross-representation call args.
- **Soundness gates**: a debug-only `verify(func)` walks every instruction and asserts the repr each
  opcode REQUIRES of each operand equals `func.repr[operand]` — a silent mismatch becomes a
  compile-time panic, the formal statement of "representation mismatch is inexpressible". `verify`
  covers EVERY repr-consuming site: the READ assume sites `FieldGet`/`SealedArrayFieldGet`/`Index`
  (packed constant-offset load) AND the WRITE/CONSUME sites `Push` (array operand + the standalone
  Packed-struct element from `push$T`) and the sealed-array `IndexSet` (array operand). The
  RHS-value of an IndexSet/store and a map/object store are NOT asserted — they decide storage from
  the container and COERCE the value at the slot (`sealed_project_from` projects a boxed
  `arr[i] = { … }` literal in), so legitimately carry a Boxed repr. A Stage-2 `oracle_check` asserts
  the new analysis agrees with the old type predicate at every decide site, so each swap is provably
  a conservative no-op. Both run as `debug_assert!` in `repr::run`, exercised by the full `cargo
  test` corpus — so the producer/consumer drift class (a boxed array reaching a packed Push/Index)
  is now a debug-build compile panic, not an ASan-only-catchable runtime UAF.

**Consequences / current boundary**:
- Two representation-DECIDE sites are read from `func.repr` (single-owner): the sealed-array IndexSet
  RHS project-vs-verbatim decision (`val_repr.packed_struct_fields()`), and the `Release` instruction's
  release SHAPE (`emit_release_repr` — the wrong-release-after-divergence fix). Both verified
  byte-identical on the corpus and ASan-clean (incl. dijkstra).
- Codegen's `sealed_repr_differs` is no longer a representation-decide predicate (only an internal
  `sealed_construct` field-coercion helper); equality (`emit_eq`) materializes both operands by design
  (the boundary catalogue's materialize-both dynamic consumer).
- **Producer/consumer LITERAL drift — FIXED (prerequisite for any heap-field ungate).** An inferred
  empty array literal `[]` infers bottom-up to `Array(Never)` and lowers to a BOXED buffer; a concrete
  packed/flat-scalar `T[]` param's callee does packed stride-N push/get → a representation DRIFT
  (the calc-lexer `scan(.., [])` / `[].fill()` shape — a latent packed-array UAF, ASan-only). The
  prefix `infer_call` already routed an array-literal ARGUMENT through expected-type checking against a
  concrete array param; `infer_dot_call` did NOT — neither for its arguments NOR for the empty-literal
  RECEIVER. Both now adopt the concrete param's RESOLVED element representation (a `Named` record alias
  and its `Object{sealed}` body resolve identically — the alias is expanded at annotation time by
  `resolve_named_cycle`/`expand_named_body` — so producer and consumer agree, no silent boundary). The
  extended `verify()` makes any residual drift a debug panic. This fix stands alone (it corrects the
  latent SCALAR packed-array UAF) and is the precondition the heap-field ungate needed.
- **Heap-field record arrays stay BOXED (fail-safe), one remaining blocker.** The per-element
  heap-field RC machinery and the dynamic-consumer boundaries (materializer, whole-element read,
  out-of-shape→Null) are heap-field-complete and ASan-clean on single-module `Person[]`/`Line[]`
  lifecycle fixtures. The two historically-cited blockers are now CLOSED: (a) FIELD OMISSION is a
  compile error (`omits_required_field`), so a packed element can never store a NULL heap pointer; (b)
  the LITERAL drift above is fixed. The ONE remaining blocker is **whole-program record
  representation consistency for a record reachable as a `{String: T[]}` MAP-VALUE element** (the
  dijkstra `{String: Neighbor[]}` shape). A `{String: T[]}` map is pervasively read into a `T[]|Null`
  UNION (`match adj[u] is Null => [] else => …`) and then BOTH mutated in place (`push(it, x)`) AND
  iterated by the generic boxed `for`. In-place mutation REQUIRES keep-packed-by-pointer (a shared
  `0xFE` buffer); the boxed `for`/`lin_array_get_tagged` reader REQUIRES a boxed `Object[]` (it reads a
  `0xFE` buffer's packed structs as `TaggedVal`s → garbage: the `0x07` heap-field deref crash, or — for
  SCALARS — a silent misread that is LATENT on master since no scalar `{String: P[]}`-iterated test
  exists). The two are irreconcilable at one map-value representation UNLESS either (i)
  `lin_array_get_tagged` materializes a packed element to a keyed `LinObject` via a NAMED full-field
  descriptor (a runtime LinArray-layout change — today's heap-only `elem_desc` lacks field names), OR
  (ii) the record is boxed CONSISTENTLY everywhere it is reachable from the map (a CROSS-MODULE
  record-taint pass — a record type is packed-everywhere or boxed-everywhere, never per-occurrence,
  else the read-back drifts). Either is larger than a local gate. Until one lands, heap-field element
  arrays stay scalar-only — sound, not maximal. (`Codegen::sealed_array_elem_field_packable` + its
  lower.rs/monomorphize/repr mirrors; the gate note there has the re-enable recipe.)
- The lower.rs/monomorphize coercion-insertion triggers (`lower_coerce_arg`, `type_repr_differs`,
  `mentions_sealed`, `combinator_unsound_over_sealed`) are NOT yet deleted: they remain the emitters of
  the boundary coercions until a repr.rs STEP-4 coercion-insertion pass relocates them. That relocation
  is the remaining single-owner work; the lattice, side table, keep-packed ops, and the two swapped
  decide sites are the parts that landed.

## ADR-063: Stage 3b — whole-program record-representation consistency (the heap-field-array packing unlock)

**Status**: ~~Proposed~~ **ABANDONED — CLOSED-NEGATIVE (2026-06-11).** Stage 3b's premise — that
packing heap-field record graphs end-to-end would move RAPTOR's query phase toward Go/Node — was
**built and measured, and is false.** Three independent agents produced digest-correct end-to-end
typed RAPTOR; it ran **~1.8–3.5× SLOWER** (PREP 7.7 s→27.2 s, GROUP 19.9 s→36.2 s, RANGE 59.4 s→105.3 s).
The dominant cost is **representation-boundary materialization**, not the field read this ADR set out
to make constant-offset: functional code threads records through many generic boundaries (worker
boundary → nested-record gate → TCO param leak → `Trip|Null` union boxing → map-value
materialize-per-access), and each is a materialize-or-leak seam — "fix-for-a-fix all the way down."
The full record + mechanism is in `docs/PERFORMANCE.md` §5 (path-9). **Do not re-attempt heap-field
end-to-end packing for perf.** The orthogonal win that *did* pay (typing RAPTOR's dictionaries off
`AnyVal` → O(1) `LinMap`, ~5.6× PREP) shipped separately (ADR-055). The all-scalar sealed-record packing
(ADR-057) is unaffected and remains Lin's headline strength. The design below is retained as the
record of what was tried and why it was closed, **not as a roadmap.**

**End goal (do not lose sight of this).** The point of Stage 3b is NOT "pack heap-field record
arrays" for its own sake. It is to let real typed-record-heavy programs — the RAPTOR benchmark being
the yardstick — keep their hot-path values (`Trip`, `StopTime`, `tripsByRoute: {String: Trip[]}`,
`trip["stopTimes"][pi]["arrivalTime"]`) in the *packed* representation end-to-end, so field access is
a constant-offset load and LLVM can optimise across it. Profiling (ADR-062 / [[project_raptor_perf_frontier]])
measured the gap: a packed typed-record field read is ~70x an `AnyVal` `lin_object_get`, AND the packed
value is transparent to LLVM (hoist/fold/SROA/dead-elim) whereas `AnyVal` is a total optimisation
barrier. **Success metric: RAPTOR GROUP/RANGE wall-clock moves materially toward Go/Node (~16s query
vs Lin's ~390s), with the cross-language digest gate (`group=26203913 range=773022892 journeys=139`)
byte-identical.** Stage 3b is the enabling representational work; the conversion of the RAPTOR port to
typed trips + the rebench is the deliverable that proves it.

**Context — this is NOT a from-scratch design; it is the final materialisation of ADR-062.** The
packed-vs-boxed machinery is already built and coherent: the `Repr` lattice (`crates/lin-ir/src/repr.rs`),
the per-element-per-field RC primitives and descriptor-driven release (`lin-runtime/src/sealed.rs`,
`release_sealed_array_elems`, `lin_sealed_array_set`/`_push_struct_retaining`, `sealed_array_materialize_elem`),
the `BoxKeepPacked`/`UnboxKeepPacked` IR ops (zero-copy box/unbox of a still-packed pointer through a
`TaggedVal`, already in `ir.rs` + codegen), and the debug-only `verify`/`oracle_check` soundness gates.
The per-element heap-field RC is ALREADY heap-field-complete and ASan-clean on single-module
`Person[]`/`Line[]` fixtures (construct/push/field-read/index-set/drop/transfer/`==`/toString/filter/
map/sortBy). The two historically-cited blockers (field omission; producer/consumer literal drift) are
CLOSED. So the gate `sealed_array_elem_field_packable` stays scalar-only for exactly ONE reason:

**The remaining blocker (precisely).** WHOLE-PROGRAM record-representation consistency for a record
reachable as a `{String: T[]}` MAP-VALUE element (the dijkstra `{String: Neighbor[]}` shape, and
RAPTOR's `tripsByRoute: {String: Trip[]}`). Such a map value is pervasively read into a `T[]|Null`
union (`match adj[u] is Null => [] else => …`) and then BOTH (a) mutated in place (`push(it, x)` —
REQUIRES keep-packed-by-pointer, a shared `0xFE` buffer) AND (b) iterated by the generic boxed `for` /
`lin_array_get_tagged` (REQUIRES a boxed `Object[]`; it reads a `0xFE` buffer's packed structs as
`TaggedVal`s → heap-field deref crash, or for scalars a silent misread — latent on master). These are
irreconcilable at ONE map-value representation. The session of 2026-06-07 confirmed the per-op RC is
sound (8 surrounding RC/correctness bugs found+fixed by exhaustive ASan probing) — the blocker is
genuinely this representation-consistency question, not RC discipline.

**Decision.**
1. **Resolve the blocker via mechanism (i): named-full-field-descriptor materialisation in the boxed
   reader, NOT (ii) a cross-module record-taint pass.** When `lin_array_get_tagged` (and the generic
   boxed `for`) reads an element out of a `0xFE` packed buffer whose element layout carries a NAMED
   full-field descriptor, it materialises a keyed `LinObject` view on demand (the descriptor gains
   field names; today's `elem_desc` is heap-only/nameless). This keeps the map value in ONE packed
   representation that BOTH in-place mutation (keep-packed) and the boxed reader (materialise-on-read)
   can consume. Rationale for (i) over (ii): (i) is LOCAL (a runtime LinArray-layout extension +
   the boxed-reader materialise path — no new whole-program pass, no cross-module type-taint
   inference, which would be a large new analysis with its own soundness surface and would pessimise
   any record that touches a map anywhere); (i) composes with the existing `WrapsPacked`/`BoxKeepPacked`
   machinery the lattice already has; (i) degrades gracefully (a boxed read just pays one materialise,
   exactly as an `AnyVal` read does today — never a crash). (ii) is reserved as a fallback only if (i)
   proves to have an unfixable hot-path cost.
2. **The gate stays a SINGLE source of truth, and Stage 3b lands by DIALING THE GATE, not by adding
   implementation special-cases.** Consolidate the 4 lockstep mirrors
   (`Codegen::sealed_array_elem_field_packable` + `lower::is_sealed_array_elem_field_packable` +
   `monomorphize::field_packed_scalar` + `repr::sealed_array_elem_field_packable`) so three derive from
   one (with a unit test asserting agreement). The per-shape staging (scalar → +String → +nested
   record-array) is expressed PURELY as widening that one predicate as the harness proves each shape
   clean — the descriptor-driven primitives handle every heap-field kind uniformly; there are NO
   per-shape branches in the RC implementation. This is the line between "incremental landing of a
   uniform design" and "a million patches".
3. **The verification harness (below) is built and green BEFORE any representation change, and is the
   merge gate for every gate-widening step.**

**The verification harness (Phase 1 — build first, independent value).** A generated, exhaustive
differential + ASan matrix over the cross-product that the hand-written 48 sealed tests only sampled:
{operation} × {value position} × {field shape}.
- OPERATIONS: build-literal, factory-return, field-read (scalar + heap), whole-element read (`arr[i]`),
  index-set, push, array-drop, sort/sortBy/map/filter/reduce/find, pass-by-value arg, **tail-call
  thread** (the scanRouteAt shape), store-as-map-value, read-from-map-value-then-iterate, nest.
- POSITIONS: val-binding, call-arg, array-literal element, return expr, push-arg, map-value, union
  member (`T|Null`), tail-recursive param.
- FIELD SHAPES: all-scalar, +String, +scalar-array, +record-array (the `Trip{stopTimes:StopTime[]}`
  shape), +nested-scalar-record, +nested-record-array.
Each generated program runs in a build/drop LOOP and is checked under ASan **both** `detect_leaks=1`
(a per-iteration leak SCALES with the loop count; a constant residual is the string-intern cache and
is fine) **AND** `detect_leaks=0` (no UAF/double-free), PLUS a **run-equivalence** check that the
packed result equals the boxed-fallback result (force-boxed via a sibling build with the gate off).
This harness would have caught all 8 bugs fixed on 2026-06-07; it converts "probe and hope" into
mechanical, terminating coverage and is the standing regression net for the ownership contract below.

**The ownership contract (the invariant the whole representation maintains).** An element slot of a
packed sealed-record array owns exactly +1 of each heap field. EVERY operation maintains it via ~5
descriptor-driven primitives — element-construct (retain each heap field), element-read-into-owned
(retain on materialise/escape; a pure in-place scalar/heap field READ takes none), element-overwrite
(release-old-fields then retain-new), element-drop / array-drop (`release_sealed_array_elems` walks
the descriptor), and the keep-packed box/unbox (`BoxKeepPacked`/`UnboxKeepPacked`, RC-neutral pointer
wrap). No operation hand-codes per-field RC; they all route through these. The remaining
scanRouteAt-projection TCO-release leak ([[project_raptor_perf_frontier]]) is subsumed here: it is the
element-read-into-owned + tail-call-thread cell of the matrix, fixed by the same primitive, not a
separate patch.

**Consequences.**
- Builds strictly on ADR-062 (the lattice, keep-packed ops, single-owner direction); does not
  re-decide any of it. The new work is: the named-descriptor runtime extension (i), the boxed-reader
  materialise-on-read path, the gate consolidation, and the harness.
- Incremental + safe: the gate is widened one field-shape at a time, each gated on the harness being
  green for that shape under full ASan + run-equivalence + the corpus staying ≥ its current count
  (the 72→55 regression of the naive full ungate is structurally prevented — you cannot widen a shape
  the harness hasn't cleared).
- If (i)'s on-read materialise proves too costly on a measured hot path, fall back to (ii)
  (cross-module record-taint) for the specific record, OR keep that record boxed (today's behaviour) —
  never a correctness compromise.

## ADR-064: Unboxed tagged sum types (`SumNode`) + keep-packed-through-containers via a runtime tag

**Status**: Accepted (Stages 0–4 implemented + measured; merged to master). Stage 5 (arena/FBIP node
reuse) deferred as low-value for this workload.

**Problem.** A recursive tagged union typed as `AnyVal` (the interpreter/parser/compiler workload class —
`type Ast = Num | BinOp`) compiles to boxed string-keyed `LinObject`s reached by non-inlined
`lin_object_get` per field, with `lin_matches_schema` dispatch. Measured ~77× Rust on the `interp`
benchmark. Neither sealed records (monomorphic, one field-map) nor the shipped union-discrimination
(still boxed) close it, because the value is intrinsically a *union* AND *recursive*. The only
representation that does is an unboxed tagged sum type — an inline discriminant + max-variant packed
payload, recursive children by `*SumNode` pointer, O(1) tag-switch dispatch, const-offset fields.

**Decision.** A union of ≥2 sealed records sharing a distinct `StrLit` discriminant, every other field a
scalar or a recursive self-reference, packs as a heap `SumNode` `[u32 rc | u32 size | u64 desc | u32 tag
| u32 pad | payload]` (mirrors the sealed-record header so `lin_rc_retain` works verbatim). It is another
`Repr::Packed(Layout::SumNode)` in the ADR-062 pass — same single-owner dataflow, same
materialize-at-dynamic-edges discipline. Self-recursion is detected env-free (the unique `Named` in the
variant fields). Recursive children are `KIND_SUMNODE` owned pointer slots with a recursive RC drop walk.

**The hard part — keep-packed through boxed containers, and how the store/read asymmetry was solved.**
For a `SumNode` to stay unboxed inside a boxed record field or `{String:_}` map value (essential: the
`interp` parser cursor `{node, pos}` else round-trips boxed↔packed every parse step and the unboxed-eval
win is *swamped* — measured 0.768 s, a regression vs the 0.526 s `AnyVal` baseline), the store and the read
must agree on representation. They CAN'T agree statically: the store sees the value's sum-union type, the
read sees the field's declared type which the checker leaves as a partially-expanded `Named`/`Union`. The
resolution is a **distinct runtime tag `TAG_SUMNODE`** that makes the slot self-describing — the read
dispatches on the tag (`TAG_SUMNODE` → unwrap the still-packed `*SumNode` zero-copy + retain; `TAG_OBJECT`
→ project), so no static agreement is needed and the general repr STEP-4 coercion pass was NOT required.
The tag also routes the slot's RC to `lin_sumnode_release_self` (never `lin_object_release`, which would
misread the SumNode's offset-4 size as a `LinObject` length — the type-confusion class). The
genuinely-dynamic consumers (`toString`/`==`/json/spread/worker-transfer) MATERIALIZE to a real
`LinObject` via a per-type materializer fn-ptr stored at the `SumDesc` head, so a kept-packed pointer
never escapes its representation domain (a cross-thread one would be a UAF). **Result: `interp` 0.437 s,
1.20× faster than the `AnyVal` baseline and 1.76× faster than the materializing port; `evalNode` fully
unboxed.** With the AST unboxed the next floor is tokenizer strings / `Token[]` alloc / closure ABI, not
the AST — so Stage 5 (FBIP node reuse) is deferred as low-value until profiling says node alloc/RC
dominates.

**Soundness.** Every stage is ASan-gated (the recurring RC/UAF class; `cargo test` does not catch it).
The repr `oracle_check`/`verify` debug-asserts gained `SumNode` arms making a packed/boxed mismatch a
debug-build compile panic. Two classes of bug were found ONLY by behavioral + ASan testing of the
canonical *build-in-a-function-and-return* and *store-in-a-container-and-read-back* shapes (NOT by
`cargo test`, and NOT by the agents' inline-construct tests): a tail-return nested-child pushdown gap, an
untyped-object store CloneBox-on-raw-SumNode overflow, and a map-round-trip double-release (four arg
classifiers each claiming the one projected node). **Lesson: for representation changes, ASan-green ≠
correct — a wrong-repr read is often an ASan-invisible logic error; verify the real workload shape
behaviorally.**

## ADR-065: Ownership as an IR fact, one combinator loop emitter, push-model flatMap fusion, and lambda-set devirtualization

**Status**: Accepted (landed incrementally on master, 2026-06-11; the ownership-fact migration is
ongoing — 3 of ~14 per-site heuristics consumed, the rest staged behind the same verifier).

**Context**: A four-subsystem coherence audit found one recurring failure mode across the IR/codegen
boundary: **the same fact was decided in N independent places that had to be kept byte-for-byte in
lockstep, with any drift an ASan-only-catchable UAF.** Three instances drove this ADR's structural work,
plus one perf opportunity the audit surfaced:
- **Refcount ownership** was re-derived ad hoc at every site that retains/releases (`own_for_read`,
  `own_for_store`, the Index-result lifetime, the borrowed-container-base gate, the intrinsic
  retain/release table). No single source said what a function/intrinsic does to its arguments'
  ownership, so each site guessed and the guesses could disagree (the leg1 leak class).
- **The counted-loop scaffold** was hand-copied across six combinator emitters (`for`/`map`/`filter`/
  the fusion engine/`emit_index_loop`/`emit_packed_index_loop`) and again in `lower_while` — ~95%
  identical CFG with subtly different early-exit/phi-back-edge handling (`calls.md §3`).
- **flatMap** was treated as a fusion *barrier* (it splits a chain), forcing a materialized intermediate
  array where the rest of the chain fused to a zero-allocation loop.
- **The non-devirtualizable call boundary** (ADR-044, `docs/PERFORMANCE.md` §4): a combinator calling
  its callback through the uniform boxed-closure ABI boxes each element and unboxes the result across an
  opaque indirect call. The path-8 finding said named-call devirt is a dead end (named calls are already
  direct); the real lever is *lambda-set*-shaped — the callback site *inside* a stdlib combinator body.

**Decision**: Replace each "decided in N places" pattern with a single authority, and take the one
devirtualization the profile justified.

1. **Ownership is a first-class IR fact, verified, then consumed.** `LinFunction` carries
   `param_conventions: Vec<Convention>` and a `ret_convention`, where `Convention ::= Borrow | Own |
   Inout`, inferred during lowering and seeded from a hand-audited intrinsic ownership table
   (`crates/lin-ir/src/ownership_verify.rs`). A report-only `LIN_OWNERSHIP_SHADOW` pass walks every
   call edge and checks RC balance against the declared conventions — it ships **inert** (zero behaviour
   change) and is the standing oracle. Per-site RC heuristics are then **migrated to read the fact**
   instead of re-deriving it: consumed so far are the **Index-result lifetime** (`609a4f10` →
   `index_result_convention`), the **owning-read / owning-store strategy** trichotomy (`ea7e59dc` →
   `owning_strategy`, the single authority `own_for_read`/`own_for_store` mirror), and the
   **borrowed-container-base gate** (`9dcd8945`). Each migration is proven byte-identical (sorted-IR
   diff = 0, ASan A/B identical, RAPTOR digest exact). The remaining ~11 sites (notably `tco_owns` —
   not byte-identical — and the full intrinsic table, taken last) are staged behind the same verifier.
2. **One combinator loop emitter.** `emit_combinator_loop` (`lin-ir/src/lower.rs`) is the single
   counted-loop scaffold, parameterized by element access (`Materialize` vs `Packed` view) and a
   `LoopFlow` return (`Fallthrough` vs `ContinueIf` for early-exit), with a dedicated latch block so the
   header phi back-edge is always latch-relative (no `patch_phi_incoming`). `emit_index_loop` /
   `emit_packed_index_loop` become thin wrappers, and `lower_while` is re-expressed through it. Output
   is byte-identical on the run-equivalence corpus for `for`/`map`/`filter`/fusion/`while`.
3. **flatMap fuses as a push-model loop-nest stage**, not a barrier. `FuseStage::FlatMap` lowers a
   flatMap-bearing chain via a recursive CPS engine (`fm_process`/`emit_flatmap_fused_loop`) that wraps
   the downstream pipeline in an inner loop over each `f(elem, idx)` inner array; flatMap-free chains
   keep the original linear lowering byte-identically. Output-position indices thread via per-stage
   counter cells; the inner array is released after its inner loop and inner elements reclaim through the
   existing `free_combinator_*` discipline. A barrier mid-chain *splits* the chain — it does not kill
   fusion. Empty-inner (`x => []`), lone, string-inner, and barrier-split cases are all covered.
4. **Lambda-set devirtualization for `find`/`some`/`every` with a named no-capture callback.** The
   monomorphizer gains a per-callback specialization axis: a call to `find`/`some`/`every` with a bare
   reference to a top-level no-capture function `L` mints a specialization keyed on `(type args,
   callback identity)`, then **substitutes the callback parameter with `L`** inside the combinator body,
   turning the per-element boxed indirect call into a direct `@L(i32)` call. A capturing lambda or a
   stored/passed `Function` value correctly stays on the indirect path. This is the realized, narrow
   form of the lambda-set thesis (the path-11 direction); the general whole-surface case is unsolved.

**Consequences**:
- **Coherence**: ownership, the loop scaffold, and the fusion stage each now have one authority. Future
  RC work edits the convention table / `ownership_verify`, not N call sites; future loop work edits
  `emit_combinator_loop`, not seven copies. The `LIN_OWNERSHIP_SHADOW` verifier makes a convention
  drift a reported violation rather than a silent UAF.
- **Perf** (all measured; see `docs/PERFORMANCE.md` §5): Wave C devirt measured **2.54×** on a
  `find`+`some` microbench (2 M `Int32[]`, 200 iters: 32.6 s→12.85 s), RAPTOR IR byte-identical (it has
  no inline-named-callback `find`/`some`/`every` site). The capturing-lambda inline re-land (ADR-044
  update) measured **~3.9×** on a local-capture map/reduce microbench. The loop-emitter unification and
  the ownership migrations are **not** perf wins themselves — they are byte-identical refactors that buy
  coherence; flatMap fusion removes the materialized intermediate where the rest of the chain already
  fused.
- **Soundness / verification discipline**: every ownership migration and the loop unification are gated
  on **byte-identical IR** (sorted-IR diff = 0) plus ASan A/B and the RAPTOR digest, not just a green
  `cargo test` — consistent with the repr-work lesson that ASan-green ≠ correct for representation/RC
  changes (ADR-062/064). The capturing-lambda inline's earlier revert (a per-iteration loop-body
  `alloca` → stack overflow at scale, invisible to the single-cell harness) is why the re-land hoists
  the scratch alloca to the entry block and is verified on a 100 k-element scaling fixture.
- **Staging**: the ownership-fact migration is deliberately incremental — the fact ships inert first,
  each consumer is a separate byte-identical step, and the not-byte-identical consumers (`tco_owns`) and
  the broad intrinsic table are sequenced last so the high-confidence wins land without waiting on the
  hard cases.

## ADR-066: Null-coalescing operator `??`

**Status**: Accepted (implemented; lexer/parser/checker/formatter, lowering by desugar).

**Decision.** Add a built-in binary operator `a ?? b` with the semantics `if a != null then a else b`:
`a` is evaluated exactly once, and `b` only when `a` is `Null` (short-circuit). It is the lowest
binary rung (rung 13, below `||`; SPECIFICATION.md §8.2/§8.3), left-associative, so `x ?? y ?? z`
chains and `a ?? b == c` parses as `a ?? (b == c)`.

**Why an operator, not a stdlib `or(a, b)`.** Operators in Lin are built-ins, not functions (§8.1) —
a function form cannot short-circuit (both args are evaluated before the call), and the defaulted-read
idiom is pervasive enough (`m[k] ?? default`, `cfg["timeout"] ?? 30`) to earn surface syntax. The
keyed convenience `object.get(m, k, default)` already existed and stays; `??` generalises it to any
nullable expression and is what `get` is now documented in terms of.

**Coalesces `Null` only — never `Error`.** Lin's value-based error convention (§4, §20) requires
failures to stay explicit. If the left is `T | Null | Error` and holds an `Error`, that `Error` flows
**through** `??` unchanged — it is NOT replaced by the default (which would silently swallow real
failures). The result type is `(T | Error) | D`. This is the single most important semantic choice and
is regression-tested (`test_coalesce_error_passes_through`).

**Never-null left is a compile error.** The left type must be able to be `Null` (bare `Null`, a union
containing `Null`, or `AnyVal`). A left that is never null (`5 ?? 1`) makes the default dead code, so it
is a spanned diagnostic rather than silently accepted. The result type strips `Null` from the left and
unions the right's type `D`, collapsing to the bare stripped type when `D` is assignable to it —
identical to `if x != null then x else d` and to `object.get`'s documented collapse (reuses the
existing `without_null`/`flatten_union`/`types_compatible` helpers, no new typing machinery).

**JS-style no-unparenthesised-mixing rule.** `a || b ?? c` and `a ?? b || c` are parse errors telling
the user to parenthesise. This avoids the genuinely-ambiguous reading and matches JavaScript/TypeScript
(and Swift/C#'s spirit). It is the conservative choice: the restriction can be *relaxed* later (pick a
precedence and allow the mix) without breaking any program that compiled under it; the reverse — adding
a restriction later — would be a breaking change. Detected in the parser at the `??` rung via a
transient "did this operand consume a top-level `&&`/`||`?" flag that nested parenthesised groups reset.

**Lowering: desugar, do not hand-roll RC.** `??` is NOT desugared in the parser (a dedicated
`Expr::Coalesce` AST node keeps the formatter able to round-trip `??` exactly as written, parens and
all). It is desugared in the **checker** to `{ val tmp = left; if tmp != null then tmp else right }`
over a fresh anonymous slot, producing ordinary `TypedExpr::Block`/`If`/`LocalGet`/`BinaryOp(NotEq)`
nodes. This inherits the proven `if`/`else` + `!= null`-narrowing lowering, ownership, and
RC-reconciliation paths verbatim — instead of writing new retain/release logic over a union temp, which
is historically the #1 source of leaks/UAF in this codebase. Codegen needed **zero** changes.

**Continuation lines.** A line beginning with `??` is a continuation of the previous logical line
(suppressed INDENT/DEDENT in the lexer, exactly like the existing `&&`/`||` rule — ADR-005), so a long
chain can wrap.

**Non-goals.** No optional-chaining `?.` (Lin's safe-bracket access already null-propagates through
chains — §6.1 — making `?.` redundant); no `??=` compound assignment (low value, and `var` reassignment
covers it); no bare `?` token (a lone `?` stays an unknown-character lex error as today).

## ADR-067: Heap-field discriminated SumNodes; and the `T | Null` repr frontier

**Status**: Accepted/partial (heap-field SumNodes landed 2026-06-12; `T | Null` packing is an
identified, scoped frontier — see below). Extends ADR-064.

**Context**: ADR-064 introduced the `SumNode` repr — a discriminated union of records (≥2 `Object`
variants sharing a `StrLit` discriminant) compiled as a **tagged, packed** value with const-offset
field reads, where the runtime's ~17 dynamic consumers (`toString`/`eq`/`toJson`/`keys`/release/…)
dispatch on `TAG_SUMNODE`. The gate originally admitted only **scalar/Bool** non-discriminant fields:
a variant carrying a `String`/`Array`/nested-sealed field fell back to the boxed `LinObject` path,
where every match-narrow field read costs a `lin_object_get` key-lookup **plus** a `sealed_alloc`
re-materialization. This is the same boxed/AnyVal decay the PERFORMANCE.md §2 RAPTOR penalty traces to:
a typed record that *looks* like JSON should not be *represented* as JSON.

**Decision (landed)**: Widen the SumNode gate to admit **heap-field** variants. A discriminated union
whose variants carry `String`/`Array`/nested-sealed fields now uses the packed SumNode repr; the
runtime already supported heap-field SumNodes (per-variant heap-field drop table in `sumnode.rs`), so
the change was codegen-side: the gate mirror (`repr.rs` + `codegen/types.rs`, which **must** agree or
repr/codegen disagree → miscompile), heap-field slot sizing, the SumDesc drop table, `sumnode_construct`
retain, the materializer's boxing at dynamic boundaries, and the match-narrow direct-read. Measured: a
2-variant heap union's match-narrow reads go from 20 `lin_object_get` + `sealed_alloc` to **0** —
replaced by const-offset GEP loads — with the digest unchanged, ASan-clean RC balance, and byte-identical
output for existing scalar SumNodes. Two non-obvious traps were found and fixed: (1) `has`-pattern
matches must materialize a SumNode scrutinee (its tag is `TAG_SUMNODE`, not `TAG_OBJECT`); (2) a closure
whose declared return type is SumNode-eligible returns a boxed `TaggedVal*`, so the indirect-call result
must be coerced back via `sumnode_project_from_boxed` rather than treated as a raw SumNode (else a
`CloneBox` reads the tag byte as a refcount → silent corruption).

**The `T | Null` frontier (NOT yet landed — documented to prevent re-treading the dead end)**: RAPTOR's
dominant residual cost is `Trip | Null` — a **single** sealed-record variant plus `Null`, which is not a
discriminated SumNode (no second record variant, no `StrLit` discriminant). A `NullableSealed` repr that
made `T | Null` a **raw packed pointer** (null = `Null`, non-null = `*T`) was built and *measurably
works* for the hot path (RAPTOR `scanRouteAt` `lin_object_get` 62 → 0, `scanBack` 13 → 0, digest exact,
RAPTOR ASan-clean) — but it is **unsound**: a raw, *untagged* pointer that escapes to a dynamic consumer
(`toString`/`eq`/…) is misread as a boxed value, producing intermittent heap corruption (the test suite
went from a reliable 72/72 to an intermittent 71/72 with a random victim). Enumerating escape sites to
materialise at each (map-store, then `toString`, then …) is a fix-for-a-fix chain that never closes. The
**sound** direction, consistent with ADR-064, is to give `T | Null` a **distinct tag** (a nullable
SumNode: tag distinguishes `Null` from the packed `T`) so it is self-describing and the existing dynamic
consumers dispatch on it correctly — soundness by construction, no escape analysis. The tag check adds a
small per-read cost over the raw pointer, but eliminates the corruption. This is the recommended path to
round off the typed-RAPTOR cliff; the raw-pointer approach is retired as CLOSED-NEGATIVE-UNSOUND.

## ADR-068: Generation-stamped `TarEntry` opaque handles for composable tar streaming

**Status**: Accepted (implemented; runtime + checker + IR + codegen + stdlib).

**Context.** The existing `untar(s, callback)` terminal and `manifest`/`files` adapters do not compose
with `std/iter`. A user wanting to `filter` archive entries before extracting must write a manual state
machine inside the `untar` callback. The Go `tar.Reader` / Rust `tar::Entries` precedent shows that
composable entry handles are both practical and idiomatic.

**Decision.** Add three exports to `std/archive`:
- `entries(s: Stream<UInt8[]>): Stream<TarEntry>` — splits the byte stream into entry handles.
- `header(e: TarEntry): TarHeader` — always-valid metadata accessor (no lock needed; fields are copied at parse time).
- `body(e: TarEntry): Stream<UInt8[]>` — one-shot body stream; failures (expired / double-taken) are in-band on first read.

**`TarEntry` design axioms:**
1. **Refcounted, not affine.** Unlike `Stream`, a `TarEntry` is refcounted and can be stored in
   variables or arrays. It is NOT single-use — holding a reference past the stream step is the
   basis of the `find`-style early-stop pattern.
2. **Generation counter.** A shared `TarEntriesState` holds a `u64` generation. Each entry is
   stamped at mint time; advancing the archive bumps the generation. `TarBodySource` re-checks
   on every read; a mismatch → in-band Error.
3. **Body is one-shot.** An `AtomicBool body_taken` flag prevents calling `body()` twice on
   the same entry. The second call returns a stream that yields Error on the first read.
4. **Header survives expiry.** Name, size, and typeflag are copied into `TarEntryBox` at parse
   time. `lin_tar_header` reads from the copy — never from the shared state — so it is
   lock-free and valid even after the body expires.
5. **Parent closed when shared state drops.** `TarEntriesState::drop` closes the parent upstream.
   The entries `StreamBox` holds an `Arc` clone; each live `TarEntry` holds another. The
   parent closes when the last `Arc` drops — which is the last `TarEntry` handle going out of
   scope, not when the entries stream closes. This enables the find-style pattern:
   closing the entries stream early keeps the current entry's body alive until the handle is dropped.
6. **Non-transferable.** `TarEntry` is listed in `is_definitely_non_transferable` in
   `lin-check/src/checker/helpers.rs`. An async thunk that returns a `TarEntry` is a
   compile-time error. Cross-thread capture enforcement is at the checker's return-type gate
   (same as `Stream`); a future capture-type check could tighten this.

**`lin_tar_header` calling convention.** The stdlib wrapper `header(e: TarEntry): TarHeader`
generates code that calls `lin_object_get` directly on the return value of `lin_tar_header` (the
codegen treats the return as an unboxed `LinObject*`). `lin_tar_header` therefore returns a raw
`LinObject*` (NOT a `TaggedVal*` from `alloc_tagged`). The wrapper then projects fields from that
object into a sealed `TarHeader` record. This differs from stream item objects, which ARE
returned as `TaggedVal*` via `alloc_tagged(TAG_OBJECT, ...)` and go through `lin_unbox_ptr`
in the `pull_tagged` path.

**`body().promise()` semantics.** A body sub-stream MAY be moved to a worker via `.promise()`;
the compiler does not refuse it (body returns a `Stream<UInt8[]>`, and streams are legally
CAP_MOVE'd). Cross-thread reads are mutex-serialized against the shared cursor state and will
almost always observe generation mismatch — the archive will have advanced before the worker
thread reads — yielding an in-band Error on the first read. This is memory-safe: all cursor state
is behind `Arc<Mutex>` and TarEntry refcounts are atomic. A dedicated non-transferable refusal
for body sub-streams was considered and deferred; the current guidance is to drain the body on
the calling thread (documented in the `body` stdlib doc comment).

**Rejected alternatives.**
- **Lent (affine) body streams.** Making `body()` return an affine `Stream` that the type checker
  bounds to the entry's scope would catch misuse at compile time, but requires an "owned-lifetime"
  type system feature Lin does not have. The in-band Error on first read is pragmatic and explicit.
- **Temp-file spilling.** Buffering the body to a temp file makes the body freely readable after
  expiry but adds latency, disk I/O, and complexity. The use cases that need full body replay are
  better served by `files()` (which already buffers to `UInt8[]`).
- **Making `TarEntry` generic.** A `TarEntry<T>` that carries the parsed body as a `T` would
  require the parser to be a user-supplied function, turning `entries` into a combinator. This
  is a viable design for a typed CSV reader but wrong for a raw tar adapter where body parsing
  is always the caller's responsibility.

## ADR-069: The representation reset — records are flat packed value structs; representation is type-determined (supersedes ADR-062)

**Status**: Accepted (landed incrementally on `master` over the 2026-06-12…14 reset; Stages 0–4 + 6a +
the Stage-6b `Json`→`AnyVal` rename + the genuine-dynamic-object → `LinMap` conversion are merged).
Full as-built status, the staged design, and the honest re-measure are distilled in
`docs/PERFORMANCE.md` §5.6 (the representation reset — path-9 resolution). Supersedes **ADR-062** and
folds in **ADR-063/064/067** (SumNode, the `T|Null` repr frontier) as the now-unified representation
model.

**Context.** ADR-062 made a record's representation a *flow-sensitive dataflow fact* (packed where
provably safe, boxed `LinObject` otherwise), reconciled occurrence-by-occurrence by a `repr.rs`
inference + `verify()` lattice with a "boxed shadow" join. Every hard seam (record unions, map-value
packing, cross-module worker transfer) was a packed-vs-boxed reconciliation, and each fix spawned
another — the "path-9" dead ends. The cost it was clawing back was real (field access on a record was a
string-keyed `lin_object_get`, an LLVM optimisation barrier) but the mechanism never converged.

**Decision.** Representation is **type-determined, not inferred**:

1. **A record is always a flat packed sealed struct** (ADR-057 layout: header then inline scalar fields
   + 8-byte owned pointer slots for heap fields). Field access is **always** a constant-offset load.
   There is no boxed shadow and no flow-sensitive packed-vs-boxed choice. `repr.rs` is a pure layout
   calculator. Records keep **reference semantics** (`val b = a` shares the pointer; mutation through a
   parameter is visible) — so Perceus/`rc_elide` is a pure optimization, never a correctness obligation.
2. **The dynamic case is runtime-tagged, not a compile-time repr.** When a record flows into a slot whose
   static type is dynamic (`AnyVal`) or a union, it is wrapped as **`TAG_RECORD`** — a tagged sealed
   pointer (modelled on `SumNode`, ADR-064) — and dispatched on the runtime tag. No materialization to a
   string-keyed object on the boundary. `match … is T` narrows a union value to the **typed sealed
   pointer**, so member reads stay constant-offset.
3. **Unions of records have a physical repr.** `T | Null` over a sealed record is `Layout::NullableRecord`
   (a nullable sealed pointer — no tag word, null is the discriminant); `A | B` record unions are a
   synthesized tag + sealed payload pointer. This dissolved the union-materialization cost that three
   prior boxed-shadow attempts could not (it required deleting the shadow *first*).
4. **`Json` is renamed `AnyVal`** — the JSON-shaped dynamic top type. It deliberately **cannot smuggle a
   handle**: `Stream`/`Promise`/`Shared`/`TarEntry` are rejected from widening into it (compat guards);
   code that must carry an opaque/parametric value uses generics `<T>` (a non-wildcard TypeVar *does*
   carry handles) or a union for a closed set. `Json` is retained as a deprecated resolver alias.
   (Open: `Function`/`Iterator` currently still widen into `AnyVal` — a coherence wrinkle, undecided.)
5. **The conflated "JSON object" is being dissolved into a real hashmap.** Genuinely-dynamic
   string-keyed objects (HTTP request/URL/process/regex/env) now build a **`LinMap` (`TAG_MAP`)**, not a
   `LinObject` — consumers already dual-dispatch `TAG_MAP` (`obj[k]`, keys/values/entries, eq, toString).
   This drops insertion-order for those objects (hash order); the few order-sensitive cases use an
   explicit ordered map or a pair list (§2.6 — one of the five userland-visible changes, see the project
   doc §7.2).

**Consequences.**
- The "path-9" problem space is **deleted**, not patched: no flow inference, no boxed shadow, no
  per-occurrence reconciliation. The compiler is *smaller* (e.g. a −135-line Stage-6a dead-code sweep).
- **Performance is at ~parity, not faster — and that is the honest, measured outcome.** A cycle profile
  (project doc §0.1) shows ~85% of the reference workload (typed RAPTOR) is the **call/value axis**
  (closure/loop dispatch, control flow); representation touches ≤~4%, and `tagged_arith` is 0.0% (typed
  arithmetic fully inlines). So the reset is an **architecture + simplification** win that removes a
  whole class of dead ends and makes Go-style packed value layouts the model — *not* a RAPTOR speedup.
  The real perf lever (the call/value axis) is a separate project. §5.6 inline-array layout is retired
  (pointer-chase measured at 0.2%).
- **Not yet finished:** `LinObject` is not fully deleted. The genuine-dynamic producers are converted,
  but ~10 *known-shape* runtime result objects (`FileStat`, `MemInfo`, `Datagram`, `TimeComponents`,
  the `Error` `{type,message}` shape, …) are still built as `LinObject` because they are accessed via
  concrete record types after `is` narrowing (direct `lin_object_get`); making them `TAG_RECORD` requires
  refactoring each runtime intrinsic to return primitives and construct the typed record Lin-side (the
  named descriptor is a compile-time codegen global). That work is simplification-only (no perf gain) and
  is tracked but not committed to.

**Rejected alternatives.**
- **Keep ADR-062's flow inference and harden it.** Three union attempts and the map-value seam proved the
  packed-vs-boxed reconciliation does not converge; the boxed shadow is the root, and it had to be deleted
  rather than reconciled.
- **A true `Any` top type that holds handles.** Smuggling a live handle through a dynamic value type is
  unsound (RC/identity/serialization); generics + unions cover the real cases. See §2.5 / the compat guards.
- **A tracing GC to absorb the alloc traffic.** Measured CLOSED-NEGATIVE earlier (no workload is
  alloc-bound; the cost is work-per-alloc on the call/value axis), independent of this ADR.

**As-built (2026-06).** The reset landed its core goals (TAG_OBJECT deleted, records always packed,
no materialization on union boundaries), but two pieces of the stated end-state have not yet been
reached and should not be treated as already done:

1. **The repr lattice still runs.** `repr::analyze` (`lin-ir/src/repr.rs`) is still a full union-find
   carry-class + seed + lattice-`join` dataflow pass with a fixpoint fold and a debug oracle check.
   The stated "repr.rs is a pure layout calculator" is aspirational: SumNode, NullableRecord, and
   TailCall-coerce cases still require the flow-sensitive join to resolve their representation.
   Collapsing this to a pure `type → Layout` function requires either resolving those edge cases or
   explicitly accepting them as permanent flow-sensitive exceptions.

2. **The sealed-eligibility gate is duplicated 4–5 times.** The predicate that decides whether a
   record is packed (`sealed_fields` / `is_sealed_scalar_repr` / `field_packed_scalar`) exists
   hand-copied in `codegen/types.rs`, `repr.rs`, `lower.rs`, `monomorphize.rs`, and `escape.rs`,
   maintained by `// Kept byte-identical` comments. This is the "decided in N places" fragility the
   reset was meant to end.

Both are tracked as consolidation debt in `docs/TODO.md` Wave B. Do not trust the "single owner /
no flow-sensitive choice" prose above as a description of the current code — read `repr.rs` and the
gate predicates directly.

## ADR-070: Object-literal arguments are checked against concrete record parameters

**Status**: Accepted.

**Context**: Surfaced while writing `std/datetime` (whose records carry non-default integer-typed
fields). In `infer_call` (`crates/lin-check/src/checker/call.rs`), an object literal passed *directly*
as a call argument was routed through expected-type-directed `check_expr` only when the parameter was
a `Type::Map` — a structural `Type::Object` record parameter fell through to bottom-up `infer_expr`.
So a field-value literal never adopted its expected field type: `readY({ "y": -44 })` against
`(r: { "y": Int64 })` typed `-44` as the `Int32` default (spec §21), which codegen then
**zero-extended** into the i64 field slot — `-44` read back as `2^32 − 44`. Binding the literal to a
typed `val` first was correct (it went through the directed path), so only the direct-argument case
was affected.

**Decision**: Extend the call-argument routing condition to include concrete (TypeVar-free)
`Type::Object` parameters: `matches!(param_ty, Type::Map { .. } | Type::Object { .. })`. The
`check_object_against` Object arm already self-gates — it directs only when it changes the outcome
(discriminant literals, sealed records, field widening) and otherwise returns `None` to fall back to
inference — so this is safe for plain structural records and only fixes the missing
expected-field-type push-down.

**Consequences**: Negative (and any non-Int32-default) integer literals in direct object-literal
arguments now adopt their declared field width. Regression guard:
`test_fmt_negative_int64_field_in_direct_object_arg` in `crates/lin/tests/integration.rs`.

## ADR-071: The formatter parenthesises greedy-tailed binary operands

**Status**: Accepted.

**Context**: Surfaced while writing `std/datetime`, whose civil-date math uses `(if … else …) / N`.
`fmt_binop_operand` (`crates/lin-parse/src/formatter.rs`) only wrapped `BinaryOp` and `Coalesce`
operands. An `if`/`match`/bare-lambda/block operand fell through unwrapped, but these parse their
trailing branch by consuming a full expression, so as a binary operand they bind looser than the
operator. `(if c then y else z) / 400` re-emitted as `if c then y else z / 400`, which reparses
`z / 400` into the `else` — a silently different value. The formatter's contract is to never change
program meaning, so this is a soundness bug, not a style nit.

**Decision**: In `fmt_binop_operand`, unconditionally parenthesise an
`Expr::If`/`Match`/`Function`/`Block` operand. Always-wrapping is the only locally-sound rule: the
operand carries no "is there anything to my right" context at that point, and both sides are unsafe
(left as above; right when the whole binary expression is itself followed by more, e.g.
`(x * if c then a else b) + 1` would swallow `b + 1`). It is idempotent.

**Consequences**: One committed benchmark (`object_equality.lin`) re-canonicalised to add the
now-required parens around `matches + (if … else …)` — value-identical, just explicit. The formatter
corpus run-equivalence gate (`crates/lin/tests/integration.rs`) stays green; regression guard:
`test_fmt_parenthesizes_if_as_binary_operand`.

## ADR-072: Mixed-integer-width arithmetic widens to the result type

**Status**: Accepted.

**Context**: Surfaced while writing `std/datetime`. In `compile_binary_op_values`
(`crates/lin-codegen/src/codegen/arith.rs`), when two integer operands had different widths the
narrower was extended to the *wider operand's* width. But the checker's mixed-signedness widening rule
(`widen.rs`) takes the RESULT past both operands — `Int32 + UInt8`, and even `Int32 + UInt32` (both
32-bit), yield `Int64`, because neither operand's type can represent every value of the other. The op
then ran at the operand width and produced an `add i32` feeding an `i64` box/return slot — an LLVM
type mismatch that failed the build (and would miscompile if it slipped through). `lin check` passed
(the checker's result type was correct); only codegen was wrong.

**Decision**: In the integer-width reconciliation, compute the target width as
`max(lhs_width, rhs_width, result_width)` — folding the result width in **only for arithmetic**
(`Add`/`Sub`/`Mul`/`Div`/`Mod`); comparisons (Bool result), bitwise, and shift keep the operand width
as before. Build the target LLVM int type from the *width* (`8/16/32/64 → iN_type()`), NOT from a
`Type` — an operand's LLVM value width can transiently exceed its static type's width, and deriving
the target type from a `Type` made the recursive dispatch fail to converge (an observed stack overflow
compiling the chained shift/or in `std/bytes` `u32FromBe`). Building from the width guarantees both
extended operands land at exactly the target width, so the recursion terminates. Sign- vs zero-extend
still follows each *source* operand's signedness (an unsigned UInt32 4e9 widens to 4e9, not negative).

**Consequences**: Mixed-width integer arithmetic compiles and computes at the checker's result width.
Regression guard: `test_mixed_integer_width_arithmetic_widens_to_result` in
`crates/lin/tests/integration.rs`. Full workspace + stdlib/examples suites stay green.

**Record-field types — resolved as: keep `Int64`** (see ADR-073). `weekday` returns the
numeric-literal-union `Weekday = 0 | … | 6` (the `match` narrows the wide intermediate to the small
type, which lowers to i32). The `Date`/`Time` record *fields* stay `Int64` rather than `UInt8`/`UInt16`,
because reading a narrow field back into the civil-date math silently overflows (§21: a suffixless
literal adopts the narrow operand's width, so `153 * month` runs at `UInt8` width). The explicit
`narrowTo*` family (ADR-073) is the boundary tool for storing a wide result into a narrow field, not a
licence to make hot-path numeric fields narrow.

## ADR-073: `narrowTo*` — explicit `Int64 → fixed-width` narrowing casts

**Status**: Accepted.

**Context**: `std/number`'s `to*` integer-narrowing family (`toUInt8`/`toInt8`/…/`toUInt64`) takes
`UInt64`, so only an already-*unsigned* (or masked) value reaches it (§21). A value **computed in
`Int64`** — the result of arithmetic, or anything signed — cannot: `Int64 → UInt64` is not an implicit
coercion (it could wrap a negative; the rule in `compat.rs::is_numeric_compatible` is deliberate). And
`toInt32` takes a `Float64`, so there was no integer→`Int32` narrowing at all. Surfaced by `std/datetime`,
whose civil-date math is all `Int64` but whose natural field types are small.

**Decision**: Add a parallel `narrowTo*` family taking a signed `Int64`:
`narrowToUInt8`/`narrowToInt8`/`narrowToUInt16`/`narrowToInt16`/`narrowToUInt32`/`narrowToInt32`/
`narrowToUInt64`. Each truncates to the named width with two's-complement (`as`-cast) semantics —
identical low-bits behaviour to the `to*` family, the only difference being the accepted input type.
Backed by seven trivial `lin_narrow_*` runtime symbols (`crates/lin-runtime/src/number.rs`) declared
as FFI in `stdlib/number.lin` — no codegen/RuntimeFns change, exactly like the `to*` family. The
implicit-coercion rule is left untouched (signed→unsigned stays non-implicit); this is purely an
*explicit* escape hatch.

**Decision (datetime fields): keep `Int64`.** With the narrowing path available, `std/datetime` could
have moved its record fields to `UInt8`/`UInt16`/`Int32`. It deliberately does NOT: reading a narrow
field back into the civil-date arithmetic silently overflows, because per §21 a suffixless integer
literal adopts the *narrow operand's* width — `153 * month` with `month: UInt8` computes at `UInt8`
width (`153 * 2 = 306 → 50`). Right-sizing the fields would force an explicit `val m: Int64 = …` widen
at every field read in `toEpochDay`/`fromEpochDay`/`addMonths`/…, where a single missed widen is a
silent wrong answer. The hot-path numeric fields therefore stay `Int64`; `narrowTo*` is used only where
a value genuinely crosses into a narrow type (and `weekday`'s `Weekday` return is narrowed via `match`,
which sidesteps the issue entirely). A future change to §21's literal-in-narrow-arithmetic rule could
revisit this.

**Consequences**: A computed `Int64` can now be stored into any fixed-width integer. Regression guards
in `stdlib/number.test.lin` (in-range, out-of-range-truncation, negative-`Int32`, signed→unsigned
reinterpret). Spec §21 and STDLIB.md document the family and the read-back-overflow caution.

## ADR-074: Function overloading — statically-resolved, type-directed, all-arguments dispatch

**Status**: Accepted.

**Context**: Lin binds a function name to exactly one value. Defining a second function with the same
name silently overwrote the first (`TypeEnv` is a flat `IndexMap<String, VarInfo>`; `define` is a plain
`insert`), and a call resolved its callee by **name alone** — argument types were checked *against* the
resolved type, never used to *select* it. This precludes the common idiom of one conceptual operation
with several concrete shapes (`area(c: Circle)` / `area(r: Rect)`, `encode(s: String)` /
`encode(n: Int32)`), forcing artificial name suffixes (`areaCircle`, `areaRect`). The backend, by
contrast, was already overload-ready: monomorphization mangles specialized generics into per-type
symbols (`identity$Int32`), and codegen looks functions up by string name (`module.get_function`). The
only true blockers were in `lin-check`: the one-name-per-scope environment, and the resolve-callee-then-
check-arguments inference order.

**Decision**: Add **function overloading** — multiple top-level functions (or function-typed `val`s in the
same scope) may share a name, distinguished by their **parameter types**. Resolution is:

1. **Type-directed and over *all* arguments.** The selected overload is a function of the full tuple of
   argument types, not just the receiver/first argument. `combine(1, 2)` and `combine(1, "x")` pick
   different overloads. (We considered first-argument-only single dispatch — cheaper, fits dot-syntax —
   but chose all-argument dispatch for expressiveness; the cost is the inference-order change below.)
2. **Static only.** The overload is chosen at compile time from the static types of the arguments and
   baked into a fixed call target. There is **no** runtime tag dispatch to select an overload.
3. **Union ambiguity is a compile error.** An argument is matched by its static type *as a whole*. A
   union argument `A | B` is applicable to a parameter only if the *entire* union is assignable to it; a
   union that would select different overloads for different members matches none and is rejected. This
   is what keeps (2) honest — the compiler never silently inserts a runtime branch.

**Registration (`lin-check`)**: A name may bind an **overload set** of functions instead of a single
`VarInfo` (`VarInfo.overloads: Vec<OverloadAlt>`, each alternate carrying its own slot + function type).
Rules:
- Only **functions** overload. A name cannot be both a non-function `val` and an overload set, and a
  non-function binding still cannot be redefined (unchanged shadowing rules for values).
- Two overloads with **identical parameter-type signatures** are a duplicate-definition error — the
  return type is never consulted during dispatch, so it cannot disambiguate.
- Overloading is **scope-local**: an overload set lives in one scope; an inner binding of the same name
  shadows the whole set as today.
- The function pre-scan registers each definition via `define_fn_overload`; the per-val bind
  (`bind_pattern`) re-binds each definition to its OWN forward-declared slot, matched by parameter type
  (tolerant `types_compatible`, first still-unbound candidate wins) so each overload keeps a distinct
  slot — hence a distinct FuncId and LLVM symbol.

**Call-site resolution (`lin-check`, `checker/call.rs`)**: For `f(a₁…aₙ)`:
1. If `f` resolves to a **single** binding, behave exactly as before (fast path; no inference reorder,
   no behaviour change for existing programs).
2. If `f` resolves to an **overload set**:
   a. Infer all argument types first via a SPECULATIVE pass that is rolled back (scopes truncated,
      stream-consumption restored) so the real arg-checking runs exactly once with no double side
      effects. Lambda (function-literal) args are left untyped and treated as wildcards for selection,
      then checked for real against the chosen signature.
   b. **Applicability filter**: keep candidates whose arity fits (a complete call supplies between
      `required` and `params` arguments after default-filling, §15.6; a partial application supplies a
      prefix) and each of whose parameters the corresponding argument type is assignable to (whole-union
      rule above; the callback arity-width rule §5.5 applies inside an argument).
   c. **Most-specific selection**: candidate `A` dominates `B` iff every supplied-prefix parameter of `A`
      is at least as specific as `B`'s, where a concrete type is more specific than a generic `TypeVar`
      wildcard. Choose the unique dominator (so `f(3)` prefers `f(Int32)` over `f<T>(T)`).
   d. **Zero applicable** → `no matching overload for f(...)`, listing candidate signatures and the
      argument types.
   e. **≥2 applicable with no unique dominator** → `ambiguous call to f`, listing the tied candidates.

**Lowering & codegen**: Each overload is already a separate `TypedStmt::Val` with its own slot, so it
lowers to its own FuncId; a `Direct` call resolves through `global_fn_slots[slot]` independent of the
symbol name. The only requirement is a unique LLVM *symbol*: the checker mangles the overloaded
function's `TypedExpr::Function.name` to `base$<param-tokens>_<slot>` (reusing the monomorphizer's
naming style; the trailing slot guarantees uniqueness). The `TypedStmt::Val.name` stays the source name,
so exports and DWARF are unaffected. Functions that are *not* overloaded keep their plain symbol — no
ABI change, no mangling churn for the overwhelming majority of code. Codegen is otherwise unchanged.

**Interactions**:
- **Partial application** (§15.2): `f(x,)` over an overload set selects on the supplied prefix by the
  same applicability+specificity rules; if the prefix doesn't pin a single overload it is an ambiguity
  error. (Partial application of a non-overloaded function is unchanged.)
- **Default parameters** (§15.6): defaults are accounted for in the arity check, so an overload is a
  candidate at every arity its defaults permit. If two overloads tie only after default-filling, that is
  the ordinary ambiguity error.
- **Generics/monomorphization**: a generic overload still specializes per call as today; overloading only
  changes *which* candidate is selected before specialization runs.
- **Cross-module**: supported. `ModuleSignature` carries an `overloads` map (name → each member's
  function type + the exact mangled symbol the exporting module emitted). A dependent seeds these into
  `Checker.import_overloads`; the `import` handler registers the imported name as an overload set in the
  env (one slot per member, each `ImportSlot` carrying its symbol), so ordinary call-site resolution
  applies and each member lowers to its own `{module_key}_{symbol}` `Named` target. The one remaining gap
  is an overload set defined *within an import cycle* and used across that cycle: SCC Phase-1 seeding
  carries a single provisional type per name, so such a name falls back to single-signature resolution
  inside the cycle (acyclic imports — the overwhelming majority, including all stdlib use — are fully
  handled).

**Consequences**: Existing single-binding programs are byte-for-byte unaffected (single-binding fast
path, plain symbols). New surface area is concentrated in `lin-check` (overload-set environment +
selection algorithm) where the risk lives; IR/codegen changes are additive name-mangling. Two new
diagnostic classes (`no matching overload`, `ambiguous call`) plus duplicate-signature detection.
Spec §14.6 documents the user-facing rules; integration tests cover resolution by each argument position,
no-match, union-spanning rejection, ambiguity, concrete-over-generic preference, and the default/partial
interactions. `examples/overloading/` is a runnable demo.

## ADR-075: Numeric-conversion tie-break for overload resolution

**Status**: Accepted. Refines ADR-074.

**Context**: ADR-074 selects the unique *most-specific* applicable overload, where specificity is the
subtype order. That order is total for numeric types of the **same signedness** (`UInt8 <: UInt16 <:
UInt32 <: UInt64`), so an exact-width argument already resolves correctly — `toBe(uint32Value)` picks the
`UInt32` overload over the also-applicable `UInt64` one. But two numerics of **different signedness** are
mutually incomparable: a narrower value widens into *both* (`UInt16` is assignable to `UInt64` *and* to
`Int64`), and neither parameter is a subtype of the other, so subtype specificity finds no winner and the
call is rejected as ambiguous. This blocked the natural stdlib cleanup of folding the signed-input
`narrowTo*` family into the unsigned-input `to*` family as overloads (the `UInt64` vs `Int64` pair is
exactly the incomparable case). Empirically confirmed: `f(UInt16)` over `f(UInt64)`/`f(Int64)` → ambiguous.

**Decision**: Add a **numeric-conversion tie-break** that runs only *after* subtype specificity fails to
produce a unique winner (so it never changes a case ADR-074 already resolved, and a former hard ambiguity
can only become a resolution, never a regression). Among the applicable candidates, rank by how cheaply
each argument converts to its parameter and apply the "better function member" rule: candidate `A` beats
`B` when, over the supplied arguments, every conversion into `A` is no more expensive than into `B` and at
least one is strictly cheaper. A unique maximal candidate wins; otherwise the call stays an ambiguity
error.

Per-argument conversion cost (`numeric_conv_cost`, `checker/call.rs`):
- exact type match → `0`;
- same sign-class widening (signed→signed, unsigned→unsigned, float→float) → `1 + width-gap` (cheapest
  non-exact);
- cross-signedness int widening → `10 + gap`; int→float → `20 + gap`; float→int → `30 + gap`;
- any non-numeric or unknown conversion → a single neutral constant, so non-numeric argument positions
  never drive the ranking — a record-vs-record tie (ADR-074's ambiguity case) stays ambiguous because
  both candidates share that neutral cost at every position.

**Consequence for the stdlib (ADR-073 reversal)**: the `narrowTo*` family is folded into `to*` as
`Int64`-input overloads (`toUInt8(v: UInt64)` + `toUInt8(v: Int64)`, etc.). Resolution now reads
intuitively: an **unsigned** argument prefers the unsigned (`UInt64`) overload — the old `to*` truncation
of an already-unsigned value; a **signed/computed `Int64`** argument prefers the signed overload — the old
`narrowTo*` two's-complement truncation; an `Int32`/narrower-signed argument is applicable only to the
`Int64` overload (signed→unsigned is not implicit), so it resolves there unambiguously. `toInt32` gains an
`Int64` overload alongside its `Float64` one (disjoint, no tie-break needed). The narrowing semantics are
unchanged — only the spelling collapses from two name families to one overloaded family.

**Consequences**: One new, self-contained ranking pass in `select_overload`; no IR/codegen impact. The
tie-break is deliberately conservative — it differentiates *only* numeric widenings, leaving every other
ambiguity (records, unions, generics already handled at the specificity tier) exactly as ADR-074 left it.
Spec §14.6 notes the numeric preference. Regression tests cover same-signedness selection, signed/unsigned
selection of the incomparable pair, and the unchanged record-ambiguity error.

## ADR-081: Condition-only `while(() => Boolean)` loop overload

**Status**: Accepted.

**Context**: Lin's `while` was a single iterable combinator (`<T>(src: T[] | Iterator | Stream, f: (T, Int32) => Boolean) => Null`), requiring a collection to iterate over. The imperative pattern `while cond { body }` was not expressible directly — users had to reach for `range(0, n).for(...)` with a sentinel or write a manual tail-recursive function.

**Decision**: Add a second `while` overload to `std/iter` using ADR-074 overload resolution:

```lin
export val while = (f: () => Boolean): Null =>
  whileLoop(f)

val whileLoop = (f: () => Boolean): Null =>
  if f() then whileLoop(f) else null
```

The 1-arg form takes a zero-argument closure and loops until it returns `false`. It is a pure-Lin tail-recursive function: the `whileLoop` helper's self-call is in tail position, so the TCO alloca/loop transform (ADR-016) applies — the stack is constant regardless of iteration count.

**Why a private helper name**: the exported `while` calls `whileLoop`, whose name does NOT end in `_while`. The IR lowering's `combinator_callee_name` function identifies intrinsic combinators by the trailing `_while` symbol component; using a distinct helper name ensures the recursive body is lowered as a plain function call, never routed through `lower_while` (which expects ≥2 args and would panic on a 1-arg call).

**IR arity gate**: as a defensive measure, `combinator_callee_name` now also gates on `args.len() >= 2` before returning `"while"` for an import or intrinsic slot. This prevents any future stdlib refactor that accidentally routes a 1-arg call through the combinator path from reaching `lower_while` and panicking at `args[1]`.

**Monomorphizer overload-body fix**: when two overloads share the same source name (`"while"`), the monomorphizer's `find_exported_fn` call previously returned the first same-named export — giving the 1-arg import slot the 2-arg body (a thin `lin_while` intrinsic wrapper). The monomorphizer then re-homed the 1-arg slot to `lin_while` and triggered the panic. The fix: pass the import binding's `symbol` field (the exact mangled LLVM name, `Some("while$..._<slot>")` for overload members) to `find_exported_fn`, which now pins the lookup to the body whose `TypedExpr::Function.name` matches, preventing cross-overload body confusion. A parallel fix applies to the rehome-import-of-import path in `classify_origin_slot`.

**Consequences**: `while(() => cond)` is the idiomatic imperative loop. The existing `xs.while(pred)` (2-arg) form is byte-for-byte unchanged. A single `import { while } from "std/iter"` gives both forms.
