# Columnar Record Arrays — Design Doc

**Status**: Design + feasibility study (not yet implemented). See companion spike in `crates/lin-runtime/src/columnar.rs`.

## 1. Motivation

Lin's two current record-array layouts:

| tag | name | data layout | alias semantics |
|-----|------|-------------|-----------------|
| `0xFD` | pointer-backed | spine of 8-byte struct pointers | shared ownership; mutations via original ref are visible |
| `0xFE` | inline-stride | contiguous header-less payloads, stride = record byte width | value-copy; no per-element header; best cache line efficiency for random access |

Both are **array-of-structs (AoS)**. For a `Trip = { dep: Int64, arr: Int64, stop: String }` array of N elements, `0xFE` stores them as:

```
[ dep0|arr0|stop0 | dep1|arr1|stop1 | dep2|arr2|stop2 | … ]
        ↑ stride = 24 bytes
```

The RAPTOR hot loop scans `trip.dep` across all trips of a route — a single field of every element. On `0xFE`, each cache-line holds `64/stride ≈ 2–3` elements (stride 24 bytes), loading the unused `arr` and `stop` fields alongside the needed `dep` field. For a scan over N=100k+ trips this is ≈3× wasted bandwidth.

**Columnar (struct-of-arrays, SoA)** places each field in its own contiguous buffer:

```
dep column:  [ dep0, dep1, dep2, … ]   (Int64 × N)
arr column:  [ arr0, arr1, arr2, … ]   (Int64 × N)
stop column: [ *str0, *str1, *str2, … ] (ptr × N)
```

`arr[i].dep` becomes `dep_col[i]` — a stride-8 load from a fully-packed Int64 column. Cache density: 8 `dep` values per 64-byte line vs ≈2–3 for `0xFE`. The RAPTOR departure-time scan reads only the `dep` column and never touches `arr` or `stop`.

---

## 2. Runtime layout (proposed tag `0xFC`)

### 2.1 The `LinColumnarArray` header (tag `0xFC`)

Reuse the existing `LinArray` struct but repurpose `elem_stride`, `elem_desc`, and `elem_named_desc`:

```
LinArray header (repr(C)):
  u32  refcount           @ 0
  u8   elem_tag = 0xFC   @ 4   ← new COLUMNAR_ARRAY_TAG
  u8   _pad3[3]           @ 5
  u64  len                @ 8   ← number of records
  u64  cap                @ 16  ← capacity (same for all columns)
  ptr  col_ptrs           @ 24  ← *mut *mut u8, owned array of N_fields column buffers
  u64  n_fields           @ 32  ← (reuses elem_stride slot) number of columns
  ptr  col_meta           @ 40  ← (reuses elem_desc slot) ColMeta[], static codegen global
  ptr  named_desc         @ 48  ← (reuses elem_named_desc slot) same NamedDesc as 0xFE
```

`col_ptrs` is a heap-allocated `*mut *mut u8` holding one pointer per field, in **declaration order**:

```
col_ptrs[0] → dep column: contiguous Int64 buffer of `cap` elements
col_ptrs[1] → arr column: contiguous Int64 buffer of `cap` elements
col_ptrs[2] → stop column: contiguous ptr (*LinString) buffer of `cap` elements
```

### 2.2 Column metadata (`ColMeta`)

A static codegen-emitted global per columnar type, analogous to `SealedDesc`:

```
ColMeta layout (repr(C)):
  u32  n_fields
  ColFieldMeta fields[n_fields]:
    u32  kind        ← same KIND_* constants as SealedDesc (SCALAR/STRING/ARRAY/SEALED)
    u32  elem_size   ← byte width of one element in this column (4/8/1/2)
```

`kind == KIND_SCALAR` → raw element (i32, i64, f32, f64, bool); width = `elem_size`.
`kind == KIND_STRING` → `*LinString` elements; width = 8.
`kind == KIND_ARRAY`  → `*LinArray` elements; width = 8.
`kind == KIND_SEALED` → `*sealed_struct` elements; width = 8.

### 2.3 Element read: `arr[i].field`

For a static field read at a known column index `col_idx` with element width `elem_size`:

```
col_ptr  = col_ptrs[col_idx]          // pointer load from col_ptrs array
elem_ptr = col_ptr + i * elem_size    // GEP: no multiply for power-of-2 widths
value    = *elem_ptr (as T)           // scalar load (or pointer load for heap fields)
```

For **scalar** fields (Int64, Int32, Float64) the full codegen path is:

```llvm
%col_ptrs = load ptr, ptr addrspace(0) (arr + 24)
%col_ptr  = load ptr, ptr addrspace(0) (%col_ptrs + col_idx*8)
%elem_ptr = getelementptr i64, ptr %col_ptr, i64 %i
%value    = load i64, ptr %elem_ptr
```

This is two dependent pointer loads + one GEP + one value load — the same depth as a flat-scalar array read from `0xFE` (which is also two pointer loads + GEP + load). The critical difference is **cache line utilisation**: on the dep-column scan `%col_ptr` stays hot in L1 for the full scan; no arr/stop bytes are fetched.

For **String/Array/nested-sealed** field columns the load yields a pointer, which must be retained if the result escapes (same contract as the 0xFE field-read retain).

### 2.4 Whole-element read: `arr[i]` (materialization)

When a whole-element record is needed (e.g. `val t = trips[i]; use(t.dep, t.arr, t.stop)`), the runtime materializes a 0xFE-style standalone sealed struct:

```
lin_columnar_materialize_elem(arr, i) → *sealed_T (+1 owned)
```

This allocates a fresh sealed struct of the same layout as a 0xFE element, then copies field-by-field from the column buffers. Heap field pointers are retained. The result is indistinguishable from a 0xFE element. Codegen can either:
1. Emit `SealedArrayFieldGet` for known field reads and avoid materialization entirely.
2. Fall back to materialization when the element is used as a whole record value.

For RAPTOR-style loops that only access one field per iteration, path (1) is the common case and materialization never fires.

### 2.5 Push: `push(arr, elem)`

For an inline-construction loop (`range(0, N).for(i => push(arr, { dep: ..., arr: ..., stop: ... }))`):

```
lin_columnar_push(arr, sealed_struct_ptr)
```

walks the fields of the source sealed struct and appends each field value to the corresponding column buffer. Grows all columns together (double capacity, realloc each col_ptr buffer). If the source is a literal-constructed temp, codegen can scatter the individual field values directly to the column buffers without constructing the sealed struct at all (the "push-scatter" optimisation — see §7.2).

---

## 3. Alias and mutation semantics

Columnar is **read-mostly** by design, like 0xFE:

- `push` appends a NEW element; it does NOT retain the source struct (values are copied column-wise, heap fields are retained only in the column buffer).
- There is no `arr[i].dep = newVal` field mutation path today; the spec does not allow `arr[i].field = v` on sealed arrays.
- Whole-record mutation via `arr[i] = newElem` can be supported but requires updating all column buffers for element `i` — same cost as a 0xFE `IndexSet`.
- Thread transfer: `clone_columnar` deep-copies each column buffer and retains/copies heap field elements, analogous to `lin_array_clone_flat`.

Consequence: **columnar is value-semantics**, equivalent to 0xFE. No aliasing between the array and any separately-constructed element struct.

---

## 4. Eligibility gate (when is columnar chosen?)

Columnar is strictly an extension of the 0xFE gate. A `T[]` is eligible for columnar iff:

1. `T` is a named sealed record (same gate as 0xFE).
2. The escape-analysis pass proves the array is **non-aliasing** at construction — same precondition as `inline == true` on 0xFE.
3. **New**: at least one column contains only scalars AND the array is expected to be scanned field-at-a-time (see §4.1).

Without (3), 0xFE is equally good or better (it keeps fields co-located, which is better for random-access whole-element reads).

### 4.1 Heuristic: when to prefer columnar over 0xFE

Columnar wins when the dominant access pattern is **single-field sequential scan** over a large array. The compiler cannot know this statically, but an annotation-driven opt-in is sufficient for the initial design:

```lin
@columnar
type Trip = { "dep": Int64, "arr": Int64, "stop": String }
```

Or, as a lower-level trigger, the repr-inference pass could choose columnar when a loop body accesses only a single field of a sealed array (the `SealedArrayFieldGet` instruction is the only array read instruction in that function). **Initial design: annotation-only opt-in.**

---

## 5. Relationship to 0xFE and shared infrastructure

Columnar is a **superset specialisation** of 0xFE. The two share:

| Component | 0xFE shares with Columnar |
|-----------|--------------------------|
| `SealedDesc` / `ColMeta` | Nearly identical; ColMeta adds `elem_size`; ColMeta drops per-field byte_offset (not needed for columnar) |
| Named descriptor | Same `NamedDesc` format (field name → field index mapping) |
| `lin_sealed_alloc` | Used to allocate materialized elements from columnar reads |
| `release_sealed_array_elems` | Columnar needs a parallel `release_columnar_array_cols` that walks col buffers |
| Repr lattice | New `Layout::ColumnarArray { elem_layout }` variant in `repr.rs`; same pipeline integration |
| `sealed_array_elem` gate | Columnar gate piggybacks on the same `Type::sealed_array_elem` predicate |
| `sealed_named_descriptor` codegen helper | Shared; columnar also needs a named desc for dynamic field reads |
| `lin_array_release` | Needs a new `0xFC` branch freeing each column buffer via `col_meta` then the `col_ptrs` array itself |

The two diverge at:

- **Alloc**: `lin_sealed_array_alloc(cap, stride, desc, named_desc)` → columnar needs `lin_columnar_array_alloc(cap, col_meta, named_desc)` allocating `n_fields` separate column buffers.
- **Push**: `push_struct_retaining` copies one contiguous payload → columnar scatters field-by-field.
- **Field read**: 0xFE: `data + i*stride + field_offset` → columnar: `col_ptrs[col_idx] + i*elem_size`.
- **Release**: 0xFE: walk one contiguous buffer → columnar: iterate `n_fields` column buffers.
- **Materialize**: 0xFE: `memcpy(sealed_header + payload)` → columnar: gather field-by-field into fresh sealed struct.

---

## 6. Paths that need to change

### 6.1 `crates/lin-runtime/src/array.rs`
- Add `COLUMNAR_ARRAY_TAG: u8 = 0xFC`.
- Extend `LinArray` comment (the physical layout is unchanged — `n_fields` fits in `elem_stride`, `col_meta` in `elem_desc`, `col_ptrs` in `data`).
- Add `0xFC` branches in `lin_array_free`, `lin_array_release`, `lin_array_clone_flat` (actually a new `lin_columnar_clone`).
- New functions: `lin_columnar_array_alloc`, `lin_columnar_push`, `lin_columnar_field_get_i64` (and variants per type), `lin_columnar_materialize_elem`, `lin_columnar_array_set_elem`.

### 6.2 `crates/lin-runtime/src/sealed.rs` (or new `crates/lin-runtime/src/columnar.rs`)
- `ColMeta` struct and `release_columnar_array_cols`.
- `lin_columnar_materialize_elem`.
- `clone_columnar` (deep copy for thread transfer).

### 6.3 `crates/lin-ir/src/repr.rs`
- New `Layout::ColumnarArray { elem_layout: IndexMap<String, Type> }` variant.
- Extend `join` to handle `ColumnarArray` (same as `PackedSealedArray`: identical layouts merge, mismatch demotes to Boxed).
- New `Repr::packed_columnar_array_layout()` accessor.

### 6.4 `crates/lin-ir/src/lower/mod.rs`
- `make_array_repr`: add columnar gate (annotation check OR heuristic).
- `lower_coerce_arg`: columnar array at a non-columnar boundary → materialize or box (same as 0xFE).

### 6.5 `crates/lin-codegen/src/codegen/mod.rs` (MakeArray)
- New branch: `if arr_repr == ColumnarArray { ... }` calls `lin_columnar_array_alloc` and scatters elements.

### 6.6 `crates/lin-codegen/src/codegen/data/array.rs`
- `compile_ir_sealed_array_field_get`: add `0xFC` dispatch path (two-pointer-load + GEP + load).
- `sealed_array_materialize_elem`: add `0xFC` branch calling `lin_columnar_materialize_elem`.
- `sealed_array_to_tagged`: add `0xFC` branch.
- `sealed_array_project_from`, `sealed_array_project_owned`: add `0xFC` in the tag-check.

### 6.7 Push path (`lin_array_push` tagged and `push_into_sealed_array`)
- `lin_array_push_tagged`: add `0xFC` branch calling `lin_columnar_push`.
- `lin_sealed_array_push_struct_retaining`: columnar variant (`lin_columnar_push_struct_retaining`).

---

## 7. Estimated RAPTOR win and cost

### 7.1 RAPTOR bottleneck recap

RAPTOR's hot loop (the backward trip scan in `routeScanner.lin`) iterates over `routeTrips[i]`, loads `trip.stopTimes[stopIndex].departureTime`, and compares it against a threshold. Today `routeTrips` is `AnyVal[]` — fully boxed, JSON-typed, with full dynamic dispatch at every field read. The field reads are the LLVM optimisation barrier.

**With typed RAPTOR** (the `Trip`-typed work described in prior docs), the trip array becomes `Trip[]` in the `0xFE` repr: dep/arr as packed Int64 fields, stop/service as packed pointer fields. The hot loop reads `dep` from `0xFE` at offset `dep_field_offset` within stride-N payloads. Cache line efficiency: ≈ 64 / stride values per line (stride ≈ 40 bytes for a 4-field record → ≈1.6 values/line).

**With columnar**, the `dep` scan reads from a dedicated `dep` column at stride-8: 8 Int64 values per cache line → ≈5× improvement in cache line utilisation for the scan. The `arr` and `stop` columns are never touched during the pure dep scan.

**Estimated win**: 2–4× reduction in memory bandwidth for the RAPTOR dep-scan inner loop. RAPTOR's measured 1.8–2.0× materialization penalty vs the typed baseline suggests that ~half the cost is field reads; halving that read cost plausibly yields a 1.3–1.6× overall improvement on RAPTOR. The theoretical bandwidth win over 0xFE is bounded by the fraction of total time spent in sequential dep-field scans — RAPTOR is a plausible case where this fraction is large (the route scan is the algorithm's inner loop over millions of trips).

### 7.2 Cost

- **Construction**: scatter N×F column pushes instead of one contiguous memcpy. For small arrays the overhead is visible. For RAPTOR-scale arrays (10k–100k trips) the construction is amortised by scan iterations.
- **Random access** (`arr[i]` whole-element read): requires two pointer loads instead of one GEP from the data base — a constant factor slower than 0xFE. Not a regression for single-field access.
- **Code size**: new runtime functions, new codegen branches. Estimated ~600 lines of Rust (runtime) + ~300 lines of codegen (inkwell paths).
- **Aliasing / mutation**: same constraints as 0xFE (read-only per element). No new constraints.
- **Interop**: a columnar array passed to a generic function or stored in a `{String: T[]}` map will be materialized to a 0xFD or 0xFE array at the boundary (same rules as current 0xFE keep-packed logic). This is correct but adds a one-time conversion cost at the boundary.

### 7.3 Break-even

Columnar wins when: `(scan_fraction × 5×) - (construction_overhead × N) > 1.0`. For RAPTOR-scale trip arrays (N = 50k, iterated 10×), construction overhead is O(N×F) pushes ≈ 50k × 4 × 1 scatter ops; scan wins are O(N × iters × scan_fraction). RAPTOR is well past the break-even point.

---

## 8. Recommended first slice

**Phase 0 — runtime-only POC** (this doc includes a spike in `crates/lin-runtime/src/columnar.rs`):
- `LinColumnarArray` header re-using `LinArray`.
- `lin_columnar_array_alloc` + `lin_columnar_push_i64_i64_ptr` (2 scalar + 1 pointer field).
- `lin_columnar_field_get_i64` + scan loop verification.
- No codegen integration — proves layout math and RC contract.

**Phase 1 — repr + codegen wiring (estimated 2–3 days)**:
1. Add `COLUMNAR_ARRAY_TAG = 0xFC` to runtime and extend `lin_array_free`/`lin_array_release`.
2. Add `Layout::ColumnarArray` to `repr.rs` + gate on `@columnar` annotation in `make_array_repr`.
3. Wire `MakeArray { inline: Columnar }` in codegen/mod.rs: call `lin_columnar_array_alloc` + scatter.
4. Wire `SealedArrayFieldGet` for `0xFC`: two pointer loads + GEP + load.
5. Wire materialization for whole-element reads.

**Phase 2 — push-scatter optimisation**:
For `push(arr, { dep: expr1, arr: expr2, stop: expr3 })` where the RHS is a literal, codegen can skip constructing the sealed struct and scatter the field values directly to the column buffers — eliminating the alloc/dealloc of the intermediate struct. Requires a new `MakeObject`-to-scatter fusion pass or inline detection in the push lowering.

**Phase 3 — RAPTOR integration**:
Add `@columnar` to the `Trip` type in the typed RAPTOR benchmark; measure; report.

---

## Appendix: spike summary

The runtime POC in `crates/lin-runtime/src/columnar.rs` demonstrates:
- A `LinColumnarArray` aliased onto `LinArray`'s header (same struct, `0xFC` tag in `elem_tag`).
- A 2-Int64-field + 1-pointer-field record array of 1 million elements.
- `lin_columnar_field_get_i64` doing the two-pointer-load + GEP + load.
- A scan over all `dep` values, verifying the checksum and the layout arithmetic.
- The spike compiles under `cargo test -p lin-runtime` and passes without unsafe warnings.
