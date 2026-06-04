# Stdlib `Json` typing audit — tightening the stdlib once `{ String: T }` lands

Status: proposal / audit. Companion to `docs/proposals/typed-map-index-signature.md` (the accepted
index-signature `{ String: T }` type).

> **Category 1 (under-genericized array/iter collection ops) is DONE** — shipped on master, the
> `std/array` + `std/iter` element-generic ops re-typed from `Json` to `<T>`. It is omitted below; the
> remaining work is Categories 2–5.

The stdlib uses `Json` across 22 modules. Not all of these are lazy — they fall into distinct
categories, and only some are fixed by the new map type. This doc classifies every `Json`-using module
so the implementing agent knows, signature by signature, which to tighten and how. The win is real: a
`Json`-typed value is opaque (no field/element checking, and the runtime boxes it), so every one of
these is both a missed type-safety opportunity and, where a concrete type would let codegen unbox, a
missed performance opportunity.

## The remaining categories

### Category 2 — the map-type gap (THE reason for `{ String: T }`; fix with the new type)

`std/object` operations over dynamic-key objects. These are `Json` precisely because there was no type
for "object with arbitrary string keys → `T`". Once `{ String: T }` exists:
```
keys       = (obj: Json): String[]                 →  <T>(obj: { String: T }): String[]
values     = (obj: Json): Json                     →  <T>(obj: { String: T }): T[]
entries    = (obj: Json): Json                     →  <T>(obj: { String: T }): [String, T][]
fromEntries = (pairs: Json): Json                  →  <T>(pairs: [String, T][]): { String: T }
merge      = (a: Json, b: Json): Json              →  <T>(a: { String: T }, b: { String: T }): { String: T }
pick       = (obj: Json, ks: String[]): Json       →  <T>(obj: { String: T }, ks: String[]): { String: T }
omit       = (obj: Json, ks: String[]): Json       →  <T>(obj: { String: T }, ks: String[]): { String: T }
mapValues  = (obj: Json, f): Json                  →  <V,W>(obj: { String: V }, f: (V)=>W): { String: W }
isEmpty    = (x: Json): Boolean                     →  stays permissive (accepts object OR array — keep Json)
```
This is the category that also unlocks the **performance** payoff (hashed backing + unboxed `T`), and
it's the one the RAPTOR port needs for `kConnections`/`kArrivals`/`routeStopIndex`/`routesAtStop`.
NOTE the value type must be uniform per object — `groupBy`/`countBy` (in `std/array`) naturally produce
`{ String: T[] }` / `{ String: Int32 }` and should be re-typed to those:
```
array.groupBy = (arr: Json, keyFn): Json           →  <T>(arr: T[], keyFn: (T)=>String): { String: T[] }
array.countBy = (arr: Json, keyFn): Json           →  <T>(arr: T[], keyFn: (T)=>String): { String: Int32 }
```
Existing record types with a `Json` field that is really a string-keyed map should be tightened too —
e.g. `http.lin` already declares `type HttpResponse = { "status": Int32, "headers": Json, "body":
String }`; `headers` should become `{ String: String }`.

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

(Category 1 — array/iter generics — is already shipped.)

1. **Category 2** (object/map) — gated on `{ String: T }` landing; this is the headline fix and the
   performance unlock. Re-type `std/object` + `groupBy`/`countBy`.
2. **Category 3** (result/error unions) — define the success records (`Stat`, `DirEntry`,
   process-result), then re-type fs/process/net/http/env/time to `… | Error`. Each module is
   independent; do them one at a time with their tests.
3. Leave Categories 4 and 5 (and async handles) as `Json` — those are correct or constrained.

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

- `typed-map-index-signature.md` — the prerequisite for Category 2 (and provides the unboxed value
  storage on top of the already-shipped O(1) lookup). Build that first.
- The hashed-`Json`-object change (#4b, **ADR-081 — shipped**) already gives plain `{}` objects O(1)
  lookup. Category 2 here is about the remaining *typing* / *unboxing* gap the map type closes, not the
  lookup complexity, which is already fixed.
- Unions, generics, `Error`, and `is`/`has` all already exist — Categories 1 and 3 need **no new
  language feature**, only applying what's there. Only Category 2 waits on `{ String: T }`.
