## Status: proposal

# std/url

Structured URL parsing and building — the partner to `std/http`. Today `std/http` takes URLs as opaque strings (`fetch(url: String)`), so any code that needs to inspect a host, swap a path, add a query parameter, or follow a relative `Location:` redirect has to do ad-hoc string surgery and gets RFC 3986 resolution subtly wrong. `std/url` gives a single parser that turns a string into a typed `Url` record, a `build` that round-trips it back, and a `join` that performs correct reference resolution. It is modelled on `std/path` (pure string manipulation, no I/O) but for the URL grammar instead of filesystem paths, and it deliberately leaves percent-encoding and query-string parsing to `std/encoding` so there is exactly one implementation of each of those concerns.

This module parses the **RFC 3986** generic-URI syntax (`scheme://userinfo@host:port/path?query#fragment`), with the WHATWG reference-resolution algorithm for `join`. It is non-normalising and non-decoding by default: a parsed component is the raw substring as it appeared, so `build(parse(s))` reproduces `s` for any well-formed input.

Import:

```txt
import { parse, build, join, withQuery, withPath } from "std/url"
```

### Types

```txt
type Url = {
  "scheme":   String,         // "https"; lowercased; "" if the input was relative
  "userinfo": String | Null,  // raw "user:pass" between "//" and "@", undecoded
  "host":     String,         // "example.com", "[::1]", "192.0.2.1"; "" if no authority
  "port":     Int32 | Null,   // 443; Null if absent (the scheme's default is NOT filled in)
  "path":     String,         // "/a/b"; raw, percent-encoding preserved
  "query":    String | Null,  // raw, WITHOUT the leading "?"; Null if absent
  "fragment": String | Null   // raw, WITHOUT the leading "#"; Null if absent
}
```

Design decisions baked into the record:

- **`query` is a raw `String`, not a parsed map.** Query syntax (`a=1&b=2`, repeated keys, `;` separators, `+`-as-space) is a per-application concern, and parsing it lives in `std/encoding` as `parseQuery`. Keeping the raw string here means `Url` has one obvious representation and `build` round-trips losslessly. Callers who want structure write `parseQuery(u["query"])`.
- **No auto-decoding.** Every textual component (`userinfo`, `host`, `path`, `query`, `fragment`) is the exact substring from the input — percent-escapes are left intact. See *Percent-encoding* below for the justification.
- **`port` is `Int32 | Null`**, never a default. `parse("https://x")["port"]` is `Null`, not `443`. Filling in scheme defaults is policy that belongs to `std/http`, not to a faithful parser.
- **A missing component is distinguished from an empty one** via `Null` vs `""`. `http://h/?` has `query == ""` (present, empty); `http://h/` has `query == Null` (absent). This is what makes `build` a true inverse of `parse`.
- **The record is the public surface.** There is no opaque handle: `u["host"]` is just a field read, and record-update syntax already gives immutable updates (see `withQuery`/`withPath` for the common shorthands).

---

### parse

```txt
val parse: (s: String) -> Url | Error
```

Parses `s` as an RFC 3986 URI (or relative reference) into a `Url`. Returns an `Error` (`{ "type":"error", "message": String }`, matched with `is Error`) if `s` violates the generic syntax — e.g. a non-ASCII or control byte outside a percent-escape, a port that is not all digits, or an unterminated IPv6 `[...]` host.

Components are split, **not** decoded. An absolute URL fills in `scheme` and (if `//` is present) the authority fields; a relative reference leaves `scheme == ""` and `host == ""`.

```txt
val u = parse("https://user@example.com:8443/api/v1?tag=a%20b#top")
match u
  is Error => print("bad url: ${u["message"]}")
  else =>
    print(u["scheme"])    // "https"
    print(u["userinfo"])  // "user"
    print(u["host"])      // "example.com"
    print(u["port"])      // 8443
    print(u["path"])      // "/api/v1"
    print(u["query"])     // "tag=a%20b"   (NOT decoded)
    print(u["fragment"])  // "top"

parse("/just/a/path?x=1")["scheme"]   // ""        (relative reference)
parse("mailto:nin@example.com")["host"] // ""       (no authority)
parse("http://h:notaport/")            // Error
```

---

### build

```txt
val build: (u: Url) -> String
```

Serialises a `Url` back to a string. It is the inverse of `parse`: for any `s` that parses successfully, `build(parse(s)) == s`. Components are emitted verbatim (already-encoded), with separators inserted only for the components that are present:

- `scheme` (if non-empty) followed by `":"`
- `"//"` + `userinfo` + `"@"` + `host` + `":"` + `port` — the authority block is emitted whenever `host` is non-empty (or `userinfo`/`port` is set); `userinfo`/`port` segments are omitted when `Null`
- `path`
- `"?"` + `query` (only when `query` is not `Null`)
- `"#"` + `fragment` (only when `fragment` is not `Null`)

```txt
build({
  "scheme": "https", "userinfo": Null, "host": "example.com",
  "port": Null, "path": "/search", "query": "q=lin", "fragment": Null
})
// "https://example.com/search?q=lin"
```

Because `build` does not encode, callers are responsible for ensuring component strings are already percent-encoded where the grammar requires it (use `std/encoding.urlEncode`). `build` of a hand-built record with a raw space in `path` produces a string that would fail to re-`parse` — this is intentional: `build` trusts its input the same way `std/path.join` trusts its segments.

---

### join

```txt
val join: (base: String, ref: String) -> String | Error
```

Resolves `ref` against `base` using the RFC 3986 §5 (WHATWG-equivalent) reference-resolution algorithm and returns the resulting absolute URL string. This is the operation an HTTP client needs to follow a `Location:` header or an `<a href>` that may be absolute, scheme-relative, absolute-path, or relative-path. Returns `Error` if `base` is not an absolute URL or either side fails to parse.

The dot-call form reads as "resolve this link against where I am": `base.join(href)`.

```txt
"https://example.com/a/b/c".join("../d")        // "https://example.com/a/d"
"https://example.com/a/b".join("/x")            // "https://example.com/x"
"https://example.com/a/b".join("//cdn.example.com/x")
                                                // "https://cdn.example.com/x"
"https://example.com/a/b?q=1".join("?q=2")      // "https://example.com/a/b?q=2"
"https://example.com/a/b".join("#frag")         // "https://example.com/a/b#frag"
"https://example.com/a/b".join("https://other/")// "https://other/"
"/relative/base".join("x")                      // Error  (base not absolute)
```

`join` performs the dot-segment removal of RFC 3986 §5.2.4 (`.`/`..`) on the merged path — this is the fiddly part that hand-rolled string concatenation gets wrong and the main reason this function exists.

---

### withQuery

```txt
val withQuery: (u: Url, query: String) -> Url
```

Returns a copy of `u` with `query` replaced (pass the raw query string without the leading `?`). Convenience over record-update for the most common edit.

```txt
val u = parse("https://api/items?page=1")  // assume not Error
withQuery(u, "page=2")["query"]             // "page=2"
build(withQuery(u, "page=2"))               // "https://api/items?page=2"
```

To set a query from key/value pairs, encode with `std/encoding.buildQuery` first: `withQuery(u, buildQuery({ "page": "2" }))`.

---

### withPath

```txt
val withPath: (u: Url, path: String) -> Url
```

Returns a copy of `u` with `path` replaced. `path` should already be percent-encoded.

```txt
val u = parse("https://api/v1/users?x=1")  // assume not Error
build(withPath(u, "/v2/users"))            // "https://api/v2/users?x=1"
```

Any other field can be changed with record-update syntax directly (e.g. `{ u | "port": 8080 }`); `withQuery`/`withPath` exist only because path and query are by far the most frequently rewritten.

---

## Percent-encoding interaction

`parse` does **not** decode, and this is a deliberate, load-bearing choice:

1. **Round-tripping.** If `parse` decoded, `build` would have to re-encode, and there is no canonical re-encoding — `%2F` and `/` decode to the same byte but mean different things inside a path segment, so a decode-then-encode cycle is lossy. Keeping components raw makes `build(parse(s)) == s` provable.
2. **Component boundaries are defined on the *encoded* form.** A `/` inside a path segment must stay `%2F` to remain one segment; an `=` inside a query value must stay `%3D` so `parseQuery` does not see a spurious key boundary. Decoding before structural use destroys exactly the information the structure depends on.
3. **Separation of concerns.** Decoding is a `std/encoding` operation. A caller that wants a human-readable path writes `urlDecode(u["path"])`; a caller building structured query data writes `parseQuery(u["query"])`. `std/url` never duplicates that logic.

The division of labour:

| Concern | Module | Functions |
| --- | --- | --- |
| Split/join URL structure, reference resolution | `std/url` | `parse`, `build`, `join`, `withQuery`, `withPath` |
| Percent-encode/decode a component | `std/encoding` | `urlEncode`, `urlDecode` |
| Parse/build the `key=value&...` query grammar | `std/encoding` | `parseQuery`, `buildQuery` |

`std/url` validates that percent-escapes are *syntactically* well-formed (`%` followed by two hex digits) so that malformed input is an `Error` at `parse` time rather than a surprise at `urlDecode` time, but it never changes the bytes.

A typical client flow composing all three modules:

```txt
import { fetch } from "std/http"
import { parse, build, join, withQuery } from "std/url"
import { buildQuery } from "std/encoding"

val base = "https://api.example.com/search"
val u = parse(base)              // u : Url (assume not Error)
val withParams = withQuery(u, buildQuery({ "q": "lin lang", "page": "1" }))
val resp = fetch(build(withParams))
// follow a relative redirect from a Location header:
val next = base.join("/v2/search")   // next : String | Error
```

---

## Implementation notes

**Recommendation: implement `parse`, `build`, and `join` as a Rust intrinsic wrapping the `url` crate (plus a thin `percent-encoding`/manual split for the non-normalising contract), not as a pure-Lin parser over `std/string`.**

Rationale:

- **RFC 3986 §5 reference resolution is the hard, correctness-critical part.** The dot-segment removal, the "merge paths" step, and the precedence rules for scheme-relative vs absolute-path vs relative-path references are exactly the cases hand-rolled URL handling gets wrong, and they are what makes `join` worth shipping at all. A battle-tested implementation (the `url` crate, which implements the WHATWG algorithm that supersedes/aligns with RFC 3986) eliminates a long tail of bugs we would otherwise re-discover. `std/path.resolve`/`normalize` already follow this precedent — they are runtime intrinsics, not Lin loops — and `std/path` is the explicit model for this module.
- **IPv6 hosts, IDNA, and percent-escape validation** are non-trivial scanners that a pure-Lin codepoint loop would re-implement slowly and incompletely. The `byteAt`-based O(1) scanning fix exists, but this is grammar we should not own.
- **The contract is *non-normalising*, so the crate is used as a validator/splitter, not a transformer.** The intrinsic layer parses to confirm well-formedness and to find component boundaries, then returns the **raw** substrings (offsets into the original string) into the `Url` record — it must NOT hand back the crate's normalised/decoded serialisation, or `build(parse(s)) == s` breaks. `join` is the one place the crate's serialisation is the desired output. This split keeps the round-trip guarantee while still leveraging the crate for the fiddly resolution.

Suggested foreign surface (mirrors `stdlib/path.lin`'s style — intrinsics return primitives, the `.lin` wrapper assembles the record):

```txt
import foreign "lin-runtime"
  val lin_url_parse: (String) => String   // returns a JSON splitter result or "" on error
  val lin_url_join:  (String, String) => String  // "" on error
  val lin_url_build: (...) => String
```

`build`, `withQuery`, and `withPath` are cheap enough (string concatenation and record-update) to write in **pure Lin** over the foreign `parse`/`join` core, keeping the intrinsic surface minimal — only `parse` (split + validate) and `join` (resolve) genuinely need the crate.

Composition:

- **with `std/encoding`:** `std/url` and `std/encoding` are orthogonal and acyclic — `std/url` does structure, `std/encoding` does bytes-in-a-component and the query grammar. Neither imports the other; they compose at the call site (`urlDecode(u["path"])`, `parseQuery(u["query"])`). This avoids the stdlib-move import ripple that comes from one module owning another's logic.
- **with `std/http`:** `std/http` keeps its `String` URL surface unchanged (no breaking change). `std/url` is the optional structured layer a client reaches for when it needs to manipulate URLs — most usefully `base.join(location)` for redirect-following and `build(withQuery(parse(url), ...))` for query construction. A future `std/http` enhancement could accept a `Url` overload, but that is out of scope for this proposal.
