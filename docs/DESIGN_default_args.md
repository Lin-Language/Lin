# Design: Default argument values

Status: in progress (branch `feat/default-args`)

## Surface syntax

```
val f = (a: Int32, b: Json = { "k": 1 }) => ...
```

- A parameter may carry `= <expr>` after its (optional) type annotation.
- **Optional params must be last.** Once a parameter has a default, every following
  parameter must also have a default. Enforced in the checker; a violation is a
  compile-time error.
- A default expression may reference parameters declared *before* it and any
  outer/captured binding: `(a: Int32, b: Int32 = a + 1)`.

## Currying vs default-fill — the disambiguation rule

Lin already treats under-application (fewer args than declared) as **partial
application / currying** (spec §10.2). Default values want the same call shape to
mean "call now, fill the rest from defaults". These cannot both be the default.

**Decision (full inversion):** currying becomes *explicit* via a trailing comma.

| Call            | Meaning                                                        |
|-----------------|---------------------------------------------------------------|
| `f(x)`          | Call now. Trailing params filled from their defaults. Error if any omitted param has **no** default (i.e. `argc < required`). |
| `f(x,)`         | Partial application — return a function awaiting the rest (today's behaviour). |
| `f(x, y)`       | Full call.                                                    |
| `f(x, y,)`      | Partial application of a *saturated* arg list = the value `f` applied to all of them but still "open"; in practice equals a full call when argc==total. Trailing comma only changes behaviour when `argc < total`. |

Spec §10.2 is amended; a new ADR records the inversion. The blast radius in existing
`.lin` code is exactly one site (`examples/functions.lin:11`, `val addTen = add(10)`
→ `add(10,)`); no dot-partial-application uses exist in stdlib/examples.

`x.f` with no arg list (dot partial application, §11.1) is unaffected — it stays
partial application (there is no arg list to default-fill).

## Compilation strategy: per-arity adapters + closure descriptor

The LLVM calling convention is fixed-arity, and a call site to an *imported* function
sees only its type (from the `.sig`), never its default expressions. So defaults are
filled by the **defining** module, not the caller.

For a function `f` with `total` params of which the first `required` have no default,
the defining module emits, for each `k` in `required ..= total`:

- `k == total`: the real function `f`.
- `k <  total`: an **adapter** `f@k(a_0..a_{k-1})` whose body is:
  ```
  let a_k     = <default expr for param k>      // earlier params in scope
  let a_{k+1} = <default expr for param k+1>
  ...
  return f(a_0, ..., a_{total-1})               // tail call
  ```
  The default expressions lower exactly once, in their home module, reusing the
  existing function-body lowering. Earlier-param references work because `a_0..a_{k-1}`
  are the adapter's own parameters.

### Direct / named / dot calls (statically resolved)
Call site with `k` non-partial args where `required <= k < total` → emit a call to the
symbol `f@k` (or `f` when `k == total`). Fully static, works cross-module because `f@k`
is an ordinary exported symbol.

### Indirect calls through a function value (`val g = f; g(x)`)
Chosen to behave uniformly (fill defaults), so a function *value* must expose its
adapters. A function with defaults gets a static, immutable **descriptor**:

```
struct FnDesc { i32 total; i32 required; ptr entries[total - required + 1]; }
// entries[k - required] = pointer to the arity-k entry (adapter or real fn)
```

The closure struct gains a `desc` pointer (null for functions without defaults):
`{ i32 rc, i32 pad, ptr fn_ptr, ptr env_ptr, ptr desc }`.

At an indirect call site the arg count `k` and the value's `total` are both statically
known from the callee value's type, so the path is chosen at **compile time** (no
runtime branch):
- `k == total` → call `fn_ptr` directly (unchanged from today).
- `k <  total` (non-partial) → load `desc`, load `entries[k - required]`, call it with
  `env + k` args.
- partial (trailing comma) → existing partial-application machinery, unchanged.

## Type representation

`Type::Function { params, ret }` gains `required: usize` (count of leading params with
no default; `required == params.len()` for functions without defaults).

- `required` is **excluded** from structural compatibility (`types_compatible` compares
  params/ret only) so default-ness never blocks an assignment/argument match.
- Serialized in the `.sig` so importers know an imported function's `required` and can
  type-check `f(x)` (error when `k < required`).

`TypedParam` gains `default: Option<Box<TypedExpr>>`. The checker type-checks each
default against its param type and enforces the trailing-optional rule.

## Implementation stages (each ends green: `cargo build --workspace && cargo test --workspace`)

1. **Docs** — spec §10 amendment + ADR (this file is the working draft).
2. **Parser** — `Param.default`; `parse_param` reads `= expr`; `parse_call_args`
   reports trailing comma; `Expr::Call`/`DotCall` gain `partial: bool`. Update all
   constructors/match arms across crates.
3. **Checker** — `Type::Function.required`; `TypedParam.default`; type-check defaults;
   enforce optional-last; arity logic in `infer_call`/`infer_dot_call` split on
   `partial` (curry) vs default-fill; signature serialization carries `required`.
4. **IR + adapters (static calls)** — lower adapters `f@k`; lower default exprs; route
   direct/named/dot calls with `k < total` to the adapter symbol. Covers
   `examples/functions.lin`-style usage and the motivating example.
5. **Closure descriptor (indirect calls)** — emit `FnDesc`, extend closure layout,
   route under-arity indirect calls through the descriptor.
6. **Tests/examples** — integration test + `examples/default_args.lin`; migrate
   `examples/functions.lin:11` to `add(10,)`; STDLIB/spec doc updates.
