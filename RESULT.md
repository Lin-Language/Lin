# perf/gettrip-rc — RESULT

## Branch: `perf/gettrip-rc`
## Base: `de704e0e` (dense-array representation for integer-keyed maps)

---

## Profile findings

Inspected the LLVM IR (`LIN_EMIT_IR=1 LIN_NO_OPT=1`) of `@._route_scanner_getTrip` and
`@.._gtfs_service_runsOn` in the bench module.

### Bottleneck A: `service["days"][dow]` map-materialise — CONFIRMED, FIXED

`runsOn` accesses `service["days"][dow]` where:
- `service["days"]` is `ServiceDays` — a sealed record with 7 boolean fields ("0".."6")
- `dow` is `DayOfWeek = 0|1|2|3|4|5|6` (integer-literal union → boxed `TaggedVal*`)

The existing "sealed record indexed by non-literal key" codegen path materialised the **entire
sealed struct into a `LinMap`** per call (7× alloc+set, int_to_string, map_get, clone, 3× free).
~15+ allocations per `runsOn` invocation, called once per trip per backward-scan iteration.

IR before fix (runsOn if_then1 block):
```
sealed_mat = call ptr @lin_map_alloc(7, 0)
...  (7× lin_box_bool + lin_map_set)
ui64 = call i64 @lin_unbox_int64(ptr %2)
sealed_dynk_kstr = call ptr @lin_int_to_string(i64 %ui64)
sealed_dynk_get  = call ptr @lin_map_get(ptr %sealed_mat, ptr %sealed_dynk_kstr)
sealed_dynk_clone = call ptr @lin_tagged_clone(ptr %sealed_dynk_get)
call void @lin_string_release(...)
call void @lin_map_release(...)
```

IR after fix (direct GEP):
```
sseq_boff = mul i64 %dow, 1
sseq_toff = add i64 24, %sseq_boff    ; 24 = SEALED_HEADER
sseq_fp   = getelementptr i8, ptr %days_ptr, i64 %sseq_toff
sseq_v    = load i1, ptr %sseq_fp, align 1
```

### Bottleneck B: Duplicate `service["dates"][date]` map_get — MINOR, NOT FIXED

Two `lin_map_get_int` calls on the same (map,key) pair in `runsOn`: one for the `!= null` check,
one for the value in the if-body. Cross-block CSE would eliminate one. Deferred — LLVM at O2 may
inline and CSE these after the serial call overhead is removed.

### Bottleneck C: Trip materialization per backward-scan iteration — NOT FIXED

`val trip = timetable["trips"][route["tripsBase"] + i]` allocs a 56-byte sealed Trip per
iteration (lin_sealed_alloc + memcpy). Not fixable without changing ownership semantics: `trip` is
stored as `lastFound: Trip | Null`, requiring a standalone owned copy.

---

## Fix

`crates/lin-codegen/src/codegen/data/index.rs`: added `sealed_seq_int_key_layout` fast path in
`compile_ir_index` (the `packed_struct_fields` branch).

When a sealed record has fields `"0"`, `"1"`, …, `"N-1"` with uniform scalar type and uniform
slot size, AND the runtime key is an integer (or boxed integer-literal-union), emit a direct
bounds-checked GEP + load instead of materializing to a LinMap.

---

## Benchmark: interleaved GROUP medians (11+ rounds)

BASE rounds: 2411, 2477, 2476, 2394, 2481, 2411 → **sorted median: 2444 ms**
NEW  rounds: 1981, 1899, 1948, 1948, 1962, 1952 → **sorted median: 1950 ms**

Win-rate: **6/6 (100%)**
GROUP improvement: **−20%** (2444 → 1950 ms)
RANGE improvement: **−21%** (7290 → 5736 ms)

---

## Digest gate

```
DIGEST group=26203913 range=773022892 journeys=139  ✓
```

---

## RC counts (before/after)

The materialization path performed ~15+ allocations per `runsOn` call.
After: 0 allocations for the `days[dow]` access (scalar GEP load).

---

## Test results

`cargo test --workspace` (excl. `test_net_udp_loopback_roundtrip` which is a pre-existing slow
network test):
- **977 passed; 0 failed** (integration suite)
- All other crate unit tests: all pass

`./target/release/lin test stdlib/ examples/`:
- **73 passed, 1 failed** (the 1 fail = `examples/report/parse.test.lin` — pre-existing)

New regression test added: `test_sealed_seq_int_key_direct_load` — verifies correct values for
all valid indices.
