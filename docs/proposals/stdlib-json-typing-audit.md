# Stdlib `Json` typing audit — tightening the stdlib once `{ String: T }` lands

Status: proposal / audit. Companion to the implemented index-signature `{ String: T }` type
(ADR-082 / spec §5.1.1).

> **Category 1 (under-genericized array/iter collection ops) is DONE** — shipped on master, the
> `std/array` + `std/iter` element-generic ops re-typed from `Json` to `<T>`. It is omitted below; the
> remaining work is Categories 3–5.
>
> **Category 2 (the map-type gap) is DONE** — `std/object`'s map producers (`merge`, `pick`, `omit`,
> `mapValues`) and `std/array`'s `groupBy`/`countBy` are now typed over the index-signature map
> `{ String: T }` (ADR-082); see the summary that replaces the section below. A runtime piece was
> added: `lin_object_get_or_insert_array` is now TAG_MAP-aware (groupBy's result is map-backed).
> Three signatures were intentionally **kept `Json`** for soundness/inference reasons documented in
> the summary: `keys`/`values`/`entries` (tag-aware introspection), `fromEntries`, and
> `HttpResponse.headers` (deferred to the Category 3 http work). The remaining work is Categories 3–5.

The stdlib uses `Json` across 22 modules. Not all of these are lazy — they fall into distinct
categories, and only some are fixed by the new map type. This doc classifies every `Json`-using module
so the implementing agent knows, signature by signature, which to tighten and how. The win is real: a
`Json`-typed value is opaque (no field/element checking, and the runtime boxes it), so every one of
these is both a missed type-safety opportunity and, where a concrete type would let codegen unbox, a
missed performance opportunity.

## The remaining categories

### Category 2 — the map-type gap (DONE)

The map-producing `std/object` ops and `std/array`'s `groupBy`/`countBy` are now typed over the
index-signature map `{ String: T }` (ADR-082). Final shapes:
```
merge      = (a: Json, b: Json): Json   →  <T>(a: { String: T }, b: { String: T }): { String: T }   DONE
pick       = (obj: Json, ks): Json      →  <T>(obj: { String: T }, ks: String[]): { String: T }      DONE
omit       = (obj: Json, ks): Json      →  <T>(obj: { String: T }, ks: String[]): { String: T }      DONE
mapValues  = (obj: Json, f): Json       →  <V,W>(obj: { String: V }, f: (V)=>W): { String: W }        DONE
array.groupBy = (arr: Json, f): Json    →  <T>(arr: T[], f: (T)=>String): { String: T[] }             DONE
array.countBy = (arr: Json, f): Json    →  <T>(arr: T[], f: (T)=>String): { String: Int32 }           DONE
isEmpty    = (x: Json): Boolean         →  kept Json (permissive: object OR array)                     KEPT
```

**Defaulted accessors (the `m[k] ?? default` gap, DONE).** The typed map gave `m[k] : T | Null`, but
a positive `is T` arm on a monomorphized type parameter does not narrow today, so callers either
hand-rolled a per-type coalescer (the RAPTOR `intOr`) or did a double `if m[k] != null then m[k] else d`
read. Closed with two generic accessors that return a bare `T`:
```
object.get = <T>(m: { String: T }, key: String, default: T): T   DONE   (the m[k] ?? default read)
array.atOr = <T>(arr: T[], index: Int32, default: T): T          DONE   (bounds-safe at with a default)
```
Both are written with the `is Null => default; else => <value>` arm order (the working narrowing
form). `object.get` requires a genuine `{ String: T }` receiver (no implicit `Json → { String: T }`
coercion), so a `Json`-typed map or a nested double-index still needs a local coalescer — see the
RAPTOR `intOr`, retained only for its `Json` interchange map and its nested `routeStopIndex` read.
A `default`-arg form of `at` is not expressible: a generic `T` has no spellable default (`null` is
`T | Null`), hence the separate `atOr`. (A monomorphizer fix landed alongside: `Type::Map` was
missing from `collect_subs`/`mentions_generic_tv`/`subst_type`/`erase_nonconcrete_typevars`, so a
generic whose type parameter appeared ONLY inside a `{ String: T }` param — like `get` — failed to
specialize cross-module and emitted an undefined base symbol.)

**Kept `Json` deliberately (each a real constraint, not laziness):**

- `keys` / `values` / `entries` — kept `Json`, tag-aware. The `lin_*_any` runtime bridges dispatch on
  the boxed value's tag, so the SAME function serves a plain `{}`/`Json` record (the dominant use:
  introspecting an arbitrary object, e.g. the `examples/config` schema loader, which calls
  `keys(jsonObject)`) and a typed map. Re-typing the *parameter* to `{ String: T }` is **unsound**:
  with trusted-stdlib `Json` widening, a genuine `Json`/`LinObject` (e.g. a `fromEntries` result, or
  any object literal) is relabelled to the map type at the call boundary WITHOUT converting its
  representation; the body then passes it straight to `lin_keys_any`, which sees a (mis-applied)
  TAG_MAP and reads `LinObject` memory as a `LinMap` → corruption / crash. `§5.1.1` says there is no
  implicit `Json → { String: T }` coercion; the trusted widening must not manufacture one. (A clean
  follow-up would be to make the widening reject `Json → { String: T }`, or insert a real
  object→map conversion, at which point these could become generic.)
- `fromEntries` — kept `Json`. The target `<T>(pairs: [String, T][]): { String: T }` fails
  monomorphization at every call site: the inference engine does not decompose a tuple-in-array
  parameter `[String, T][]` to bind `T` (verified minimal repro: `<T>(p: [String, T][]): T` won't
  infer even with the argument annotated). Re-type once nested-tuple inference lands.
- `HttpResponse.headers` (and `HttpRequest`/`HttpOptions` `headers`) — left `Json`. Tightening to
  `{ String: String }` cascades into the http server/client constructors (`json`/`text`/`redirect`
  build `headers` as object literals) and the web-server example, and hits the same Json→map
  unsoundness. Deferred to the Category 3 http work (which re-types the http module anyway).

**Runtime change:** `lin_object_get_or_insert_array` (used by `groupBy`) is now TAG_MAP-aware — it
detects a `LinMap`-backed map argument and routes through `lin_map_get`/`lin_map_set` instead of the
`LinObject` path (covered by `get_or_insert_array_groupby_over_map` in `crates/lin-runtime`).

**Behaviour notes for callers:** a `{ String: T }` map is hash-backed, so `keys`/`values`/`entries`
over a map are in **hash order**, not insertion order (plain `{}` records still preserve insertion
order). `groupBy`/`countBy` now return maps; `toString` of a typed map currently renders `[object]`
(TAG_MAP has no structural `toString` yet — an open ADR-082 follow-up, surfaced by the changed
`groupBy` return type).

### Category 3 — result/error unions returned as `Json` (fix with `T | Error` / concrete records)

The I/O modules return a success value OR an `Error`, but type it `Json`. `Error` is already a
first-class composable union member (`std/async`'s `await` is `<T>(p: T): T | Error`, and `is Error`
discriminates it — §20/§11). These should name their real union:
```
fs.readFile   = (path): Json                       →  (path): String | Error
fs.readLines  = (path): Json                       →  (path): String[] | Error
fs.writeFile/appendFile/mkdir/rm/cp/mv = …: Json   →  …: Null | Error
fs.stat       = (path): Json                       →  (path): Stat | Error   (define a Stat record)
fs.ls         = (path, opts): Json                 →  (path, opts): String[] | Error  (or DirEntry[]|Error)
process.exec/shell = …: Json                       →  …: { stdout: String, stderr: String, code: Int32 } | Error
process.wait  = (handle): Json                     →  (handle): Int32 | Error
net.tcpConnect/tcpAccept/udpBind = …: Json         →  …: Int32 | Error   (fd, or Error)
net.tcpRecv/tcpSend/udpRecv = …: Json              →  …: Int32 | Error   (byte count, or Error)
http.fetch/fetchWith = (url[,opts]): Json          →  …: HttpResponse | Error   (HttpResponse already exists in http.lin)
http.fetchJson = (url): Json                       →  (url): Json | Error   (body is genuinely dynamic → Json payload, but the failure arm should be named)
env.getEnv    = (name): Json                       →  (name): String | Null
time.fromIso/parse = …: Json                       →  …: { … } | Error   (define a Time/Parsed record)
```
These need concrete records defined for the success payloads (`Stat`, `DirEntry`, a process-result
record, `HttpResponse` already exists). Independent of the map type EXCEPT where the success payload is
itself a dynamic-key object (e.g. response headers → `{ String: String }`). Higher-value for safety
(callers currently can't be forced to handle the `Error` arm); medium effort (defining the records).

### Category 4 — genuinely dynamic `Json` (LEAVE AS `Json` — this is correct)

Parsing/serialization of arbitrary external data, where `Json` is the honest type:
```
json.fromJson (the decode target is dynamic until narrowed), json.toJson, json.readJson
yaml.* , jq.* , fs.readJson , http.fetchJson's body payload, io.readLine/prompt (line of unknown shape)
```
Do **not** touch these. `fromJson`/`is`/`has` are the sanctioned bridge from `Json` to concrete types
(§6.3, §19); their whole job is to start from `Json`.

### Category 5 — `Json` as "any value" sinks and numeric-polymorphism (case-by-case)

```
io.print/printErr = (x: Json): Null                — intentional "any printable"; KEEP (or a Display/Show
                                                      bound if Lin grows one — out of scope)
array.sum/min/max/sumBy = (arr: Json[]): Json      — numeric polymorphism; the `Number` story (§ ADR-018)
                                                      or a numeric bound, not the map type. Tighten only
                                                      if a clean numeric constraint exists; else leave.
hash.hash = <T>(x: T): String                      — already generic; fine.
```

### Special note — async/concurrency handles (LEAVE; documented constraint)

`std/async` types promises/workers/pools/thunks as `Json`. The file's own header explains why: a live
promise handle must NOT be typed `T | Error` at rest because codegen would try to box the promise
pointer as a `T` — the `Error` injection happens only at the `await` site (which IS correctly
`<T>(p: T): T | Error`). So most async `Json`s are a deliberate handle-typing constraint, not laxity.
`shared`/`worker` return opaque handle types (`Shared`) already. Leave async alone unless the promise
representation changes; flag any change against that header comment.

## Suggested order of work

(Categories 1 and 2 are shipped.)

1. **Category 3** (result/error unions) — define the success records (`Stat`, `DirEntry`,
   process-result), then re-type fs/process/net/http/env/time to `… | Error`. Each module is
   independent; do them one at a time with their tests. Pick up `HttpResponse.headers →
   { String: String }` here (deferred from Category 2 because it cascades into the http module).
2. Leave Categories 4 and 5 (and async handles) as `Json` — those are correct or constrained.

## Validation per change

- `cargo build --workspace && cargo test --workspace` green.
- `cargo run -p lin -- test stdlib/ examples/` green (the colocated `*.test.lin` suites exercise these).
- `lin fmt --check stdlib/` clean.
- For Category 2/3, confirm callers that previously leaned on `Json`'s permissiveness still compile or
  are updated — tightening a return type from `Json` to `String | Error` is a **breaking change** for
  any caller that used the result without narrowing, so sweep `stdlib/`, `examples/`, `benchmarks/`, and
  the docs-site for affected call sites (this is the same repo-wide-ripple risk that stdlib export moves
  have — grep before/after).
- Update `docs/STDLIB.md` for every re-typed signature.

## Relationship to the other proposals

- The index-signature `{ String: T }` type (**ADR-082 — shipped**, spec §5.1.1) — the prerequisite
  for Category 2 (and provides the unboxed value storage on top of the already-shipped O(1) lookup).
- The hashed-`Json`-object change (#4b, **ADR-081 — shipped**) already gives plain `{}` objects O(1)
  lookup. Category 2 here is about the remaining *typing* / *unboxing* gap the map type closes, not the
  lookup complexity, which is already fixed.
- Unions, generics, `Error`, and `is`/`has` all already exist — Categories 1 and 3 need **no new
  language feature**, only applying what's there. Only Category 2 waits on `{ String: T }`.
