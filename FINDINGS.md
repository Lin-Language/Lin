# Explore E — Findings: Post-reset cleanup sweep

Branch: `explore/review`
Gates: cargo 803/0 · lin 72/72 · RAPTOR digest 26203913/773022892/139 (all verified per commit)

---

## What was cleaned (6 commits, net −5 lines)

### fix(fs): unreachable match arm — `crates/lin-runtime/src/fs.rs:499`

**Bug** (not just cosmetic): `TAG_INT32` was not in scope in `lin_fs_write_file_bytes`'s match
statement, so the compiler treated it as a fresh binding pattern (matches everything), making the
wildcard `_` arm unreachable. Both arms did `payload as i32` anyway, so the fix was to collapse
to a plain `let v = payload as i32`. Three compiler warnings eliminated.

### docs: stale ADR-062 → ADR-069 cross-references

- `crates/lin-ir/src/repr.rs` module header: said "the single owner … (ADR-062)" — updated to
  "… (ADR-069, which supersedes ADR-062)"
- `crates/lin-ir/src/ir.rs:668` `LinFunction.repr` doc: same
- `crates/lin-common/src/tags.rs:50` `TAG_BIGNUM` comment: said "aliases `BigInt = Json`" —
  stdlib had already been updated to `export type BigInt = AnyVal`; comment lagged

### refactor(check): `is_json` / `is_json_dynamic` → `is_any_val`

- `Type::is_json()` → `Type::is_any_val()` in `lin-check/src/types.rs`
- `is_json_dynamic()` → `is_any_val()` in `checker/expr.rs` (fn rename + 4 call sites)
- `checker/pattern.rs`: 2 call sites
- `monomorphize.rs`: local closure replaced with `.is_any_val()` method call
- `lin-lsp/src/main.rs`: 1 comment reference updated

### refactor(check): `json_type()` → `any_val_type()`

Constructor for the AnyVal type in `lin-check/src/resolve.rs` and all call sites
(`pattern.rs`, `call.rs`). All callers are within `lin-check`; no external crates affected.

### docs(check): `Json` → `AnyVal` in checker inline comments

`intrinsics.rs`, `helpers.rs`, `stmt.rs` — comments that described `TypeVar(u32::MAX)` as
the "Json wildcard" or said "Json-in/Json-out" now use `AnyVal`.

---

## What was found but NOT touched

### Category: technically correct aliases that remain as `Json`

Many comments in `codegen/match.rs`, `codegen/arith.rs`, and `codegen/debug_info.rs` still
say "Json" when describing the AnyVal/dynamic type. These are not wrong — `Json` is still a
valid language alias — but they're inconsistent with the new canonical name. They weren't
changed because:
- They appear inside long, load-bearing technical explanations where a transcription error
  could mislead a reader more than the old name does
- The value per change is low (correct comment → slightly more correct comment)

**Locations** (low priority, safe to batch later):
- `crates/lin-codegen/src/codegen/match.rs` — ~15 occurrences: "Json wildcard", "sum→Json",
  "Json/union wildcard", "Json-erased member", etc.
- `crates/lin-codegen/src/codegen/arith.rs` — 5 occurrences
- `crates/lin-codegen/src/codegen/debug_info.rs:210` — "union / Json / Null"
- `crates/lin-runtime/src/tagged.rs:340` — comment uses "null TaggedVal* is the Json null value"

### Category: `Inner::Opaque` single-inhabitant enum (deliberate)

`crates/lin-ir/src/repr.rs:69-82` — `Boxed(Inner)` wraps a single-inhabitant `Inner::Opaque`.
The comment explicitly explains this was kept (instead of collapsing to a bare `Boxed`) so that
re-adding a `WrapsPacked` variant if needed is a one-line change. This is a deliberate
forward-declaration, not dead code.

### Category: `left_is_json`, `item_is_json`, `ret_is_json_wildcard` local variable names

In `checker/expr.rs:1313` and `monomorphize.rs:2189,2798,3145`. These are local variables,
not exported API. Renaming them is extra churn for zero readability gain (the names describe
what the predicate _checks_, and the checks are still about the Json/AnyVal dynamic type).

### Category: codegen comments still saying "Json" for the language type

`match.rs:393` — `// This is the ONE flow the BRIEF targets: val j: Json = p`
`match.rs:439` — "Json → Item[]"
These are describing user-level Lin code constructs where `Json` is still the accepted alias.
Fine to leave.

---

## Prioritized tidy-up list (for future sessions)

**P1 (merge-ready, small, safe):**
None — everything at P1 was done in this session.

**P2 (low-risk, worth doing in a batch):**
1. Update ~20 `Json` occurrences in `codegen/match.rs` and `codegen/arith.rs` comments to
   `AnyVal`. Pure doc-only change, trivially byte-identical.
2. Rename remaining local variables `left_is_json` / `item_is_json` / `ret_is_json_wildcard`
   in `checker/expr.rs` and `monomorphize.rs`.

**P3 (investigate before touching):**
3. `crates/lin-runtime/src/tagged.rs` — several "Json" usages in comments. These are in the
   core RC dispatch path; verify each description is accurate before rewording.
4. `crates/lin-check/src/checker/expr.rs:1227` — multi-paragraph comment about unconstrained
   inference vars still says "treat it as `Json` for the merge decision". Technically correct
   (Json is still a valid alias) but could say "treat it as `AnyVal`" for consistency.

---

## Coherence verdict

The codebase is in **good shape** post-reset. The sweep found:
- **1 real bug** (unreachable pattern / shadow-binding in fs.rs — fixed)
- **3 stale ADR cross-references** (fixed)
- **1 stale type-alias comment** in tags.rs (fixed)
- **A consistent naming lag** across the checker: 5 public symbols still using `json` in their
  names after the `Json→AnyVal` rename (all fixed)
- **No dead code** from the old ADR-062 flow-sensitive repr-inference machinery — it was
  already fully removed before this session
- **No TODO/FIXME** left in crates/ — the codebase is genuinely debt-free on that axis
- **All `#[allow(dead_code)]` uses are clippy-only** (too-many-arguments) or platform-cfg;
  no suppressed warnings hiding dead logic
