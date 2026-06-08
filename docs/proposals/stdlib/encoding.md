## Status: proposal

`std/encoding` provides the canonical byte/text transport codecs that almost every
networked or file-format program reaches for: Base64 (standard and URL-safe), hex,
URL/percent encoding, and query-string parse/build. These are the encodings that sit
on the seam between `UInt8[]` byte buffers (`std/bytes`, §35.1) and `String` text, and
the query-string half is the direct partner to `std/http`, whose `HttpRequest.query`
field is currently handed to callers as an unparsed raw `String`. Following the
`std/bytes` precedent, the codecs are written in pure Lin on top of the bitwise
operators (§35.2) and the `std/number` narrowing casts (§26); only one new
runtime intrinsic is required (UTF-8 `bytes → String` decode), and it is shared, not
codec-specific. Structural URL *parsing* (scheme/host/path/fragment) is explicitly out
of scope and belongs to the separate `std/url` proposal — this module only encodes and
decodes.

---

## std/encoding

Base64, hex, URL/percent, and query-string (en|de)coding between `UInt8[]` byte buffers
and `String` text. Encoding never fails. Decoding is fallible — malformed Base64, hex,
or percent input returns the standard `Error` shape `{ "type": "error", "message": String }`,
matched with `is Error` (the `Error` arm must come first in a `match`). The alphabets and
output strings of Base64 and hex are pure ASCII, so they are assembled with
[`fromCodePoints`](STDLIB.md#fromCodePoints); decoding scans input bytes with the O(1)
[`byteAt`](STDLIB.md#byteAt) primitive. For structural URL parsing (splitting a URL into
scheme/host/path/query/fragment) see [`std/url`](url.md); this module handles only the
encoding layer and the query-string key/value codec.

Import:

```txt
import {
  base64Encode, base64Decode,
  base64UrlEncode, base64UrlDecode,
  hexEncode, hexDecode,
  urlEncode, urlDecode,
  parseQuery, buildQuery
} from "std/encoding"
```

### Functions

| Function | Signature | Description |
| --- | --- | --- |
| `base64Encode` | `(UInt8[]) -> String` | Standard Base64 (`+/`, `=`-padded) |
| `base64Decode` | `(String) -> UInt8[] \| Error` | Decode standard Base64; rejects bad chars/length |
| `base64UrlEncode` | `(UInt8[]) -> String` | URL-safe Base64 (`-_`, no padding) |
| `base64UrlDecode` | `(String) -> UInt8[] \| Error` | Decode URL-safe Base64 (padding optional) |
| `base64EncodeString` | `(String) -> String` | UTF-8 encode then standard Base64 |
| `base64DecodeString` | `(String) -> String \| Error` | Standard Base64 decode then UTF-8 decode |
| `hexEncode` | `(UInt8[]) -> String` | Lower-case hex, two chars per byte |
| `hexDecode` | `(String) -> UInt8[] \| Error` | Decode hex; rejects odd length / non-hex |
| `urlEncode` | `(String) -> String` | Percent-encode a URI **component** (RFC 3986 unreserved kept) |
| `urlDecode` | `(String) -> String \| Error` | Decode percent-escapes (+ malformed → Error) |
| `parseQuery` | `(String) -> { String: String[] }` | Parse `a=1&b=2&a=3` into a multimap |
| `buildQuery` | `({ String: String[] }) -> String` | Serialize a multimap back to a query string |

---

### base64Encode

```txt
val base64Encode: (bytes: UInt8[]) -> String
```

Encodes a byte buffer as standard Base64 (RFC 4648 §4): the 64-character alphabet
`A–Z a–z 0–9 + /`, with `=` padding so the output length is a multiple of four. The
result is pure ASCII. Encoding never fails.

```txt
base64Encode([72, 105])        // "SGk="          ("Hi")
base64Encode([])               // ""
base64Encode([255, 255, 255])  // "////"
```

---

### base64Decode

```txt
val base64Decode: (s: String) -> UInt8[] | Error
```

Decodes standard Base64 back to a `UInt8[]`. ASCII whitespace (space, tab, CR, LF) is
ignored so wrapped/pretty-printed input decodes. Returns an `Error` if the input
contains a character outside the alphabet (other than padding/whitespace), if padding
is malformed, or if the unpadded length is `≡ 1 (mod 4)` (an impossible Base64 length).

```txt
val r = base64Decode("SGk=")
match r
  is Error => print("bad base64: ${r["message"]}")
  else     => print(toString(length(r)))   // 2

base64Decode("SGk")     // [72, 105]  ("Hi") — length 3 is valid (≡ 3 mod 4)
base64Decode("SGkx=")   // Error — length 5 ≡ 1 (mod 4), impossible
base64Decode("@@@@")    // Error — non-alphabet character
```

---

### base64UrlEncode

```txt
val base64UrlEncode: (bytes: UInt8[]) -> String
```

Encodes using the **URL- and filename-safe** alphabet (RFC 4648 §5): `+`→`-`, `/`→`_`,
and **no** `=` padding (the common convention for JWTs, URL slugs, and HTTP tokens). The
output is therefore safe to drop into a URL path or query value without further
escaping.

```txt
base64UrlEncode([255, 255, 255])  // "____"   (vs "////" standard)
base64UrlEncode([251, 255])       // "-_8"    (no trailing "=")
```

---

### base64UrlDecode

```txt
val base64UrlDecode: (s: String) -> UInt8[] | Error
```

Decodes the URL-safe alphabet. Padding is **optional**: input is accepted whether or not
it carries trailing `=`. Standard-alphabet characters (`+`/`/`) are rejected — use
[`base64Decode`](#base64decode) for those — keeping the two codecs strict and
unambiguous.

```txt
base64UrlDecode("____")   // [255, 255, 255]
base64UrlDecode("-_8")    // [251, 255]
base64UrlDecode("+/==")   // Error — standard chars in URL-safe input
```

---

### base64EncodeString / base64DecodeString

```txt
val base64EncodeString: (s: String) -> String
val base64DecodeString: (s: String) -> String | Error
```

String convenience wrappers around the standard codec. `base64EncodeString` UTF-8
encodes `s` to bytes, then Base64-encodes; `base64DecodeString` Base64-decodes, then
UTF-8 decodes the bytes back to a `String` (and surfaces an `Error` on malformed Base64
**or** invalid UTF-8). Use these when the payload is text; use the `UInt8[]` forms for
binary.

```txt
base64EncodeString("Hi")        // "SGk="
base64DecodeString("SGk=")      // "Hi"
base64DecodeString("café")      // (encode "café" first — round-trips through UTF-8)
```

---

### hexEncode

```txt
val hexEncode: (bytes: UInt8[]) -> String
```

Encodes each byte as two **lower-case** hex digits (`00`–`ff`), most-significant nibble
first. The result length is exactly `2 * length(bytes)`. Encoding never fails.

```txt
hexEncode([255, 0, 16])   // "ff0010"
hexEncode([])             // ""
```

---

### hexDecode

```txt
val hexDecode: (s: String) -> UInt8[] | Error
```

Decodes a hex string back to bytes. Accepts both upper- and lower-case digits. Returns
an `Error` if the input has an odd length or contains any non-hex character. Unlike some
implementations it does **not** skip whitespace or accept a `0x` prefix — the input must
be exactly hex pairs (callers needing leniency should `trim`/strip first).

```txt
val r = hexDecode("ff0010")
match r
  is Error => print(r["message"])
  else     => print(toString(length(r)))   // 3

hexDecode("FFF")     // Error — odd length
hexDecode("zz")      // Error — non-hex character
```

---

### urlEncode

```txt
val urlEncode: (s: String) -> String
```

Percent-encodes `s` as a **URI component** (RFC 3986). Every byte of the UTF-8 encoding
is escaped to `%XX` (upper-case hex) **except** the *unreserved* set `A–Z a–z 0–9 - . _ ~`,
which is emitted literally. This is the strict, component-safe encoder: it escapes `/`,
`?`, `#`, `&`, `=`, `+`, space, and all reserved/delimiter characters, so the output is
safe to splice into either a path segment **or** a query value. (Space becomes `%20`, not
`+`; the `+`-for-space convention is `application/x-www-form-urlencoded`-only and is
handled by `buildQuery`.)

A full-URI encoder (one that preserves reserved delimiters like `/` and `?`) is
deliberately **not** offered: encoding an already-structured URL is the job of
[`std/url`](url.md), which assembles each component with this component encoder. Choosing
a single, predictable, maximal-escaping `urlEncode` avoids the classic double-encoding
and under-encoding footguns of a `encodeURI`/`encodeURIComponent` pair.

```txt
urlEncode("a b/c")        // "a%20b%2Fc"
urlEncode("café")         // "caf%C3%A9"   (UTF-8 bytes of é)
urlEncode("safe-._~")     // "safe-._~"    (unreserved, unchanged)
```

---

### urlDecode

```txt
val urlDecode: (s: String) -> String | Error
```

Decodes percent-escapes. `%XX` triples are converted to their byte value, the resulting
byte sequence is UTF-8 decoded back to a `String`. For symmetry with form bodies, a
literal `+` is decoded to a space. Returns an `Error` if a `%` is not followed by two hex
digits, or if the decoded bytes are not valid UTF-8.

```txt
urlDecode("a%20b%2Fc")    // "a b/c"
urlDecode("caf%C3%A9")    // "café"
urlDecode("a+b")          // "a b"
urlDecode("%zz")          // Error — bad escape
urlDecode("100%")         // Error — truncated escape
```

---

### parseQuery

```txt
val parseQuery: (s: String) -> { String: String[] }
```

Parses a URL query string (the part after `?`, or `std/http`'s `HttpRequest.query`) into
a map from key to the **list** of values for that key. Pairs are split on `&` (and `;` is
**not** treated as a separator — modern WHATWG behaviour); each pair splits on the first
`=`. Keys and values are percent-decoded with `+`→space (form semantics). A key with no
`=` (`"flag"`) maps to a single `""` value. A leading `?` is tolerated and stripped.

The value type is `String[]`, not `String`, because repeated keys (`tag=a&tag=b`,
`checkbox[]`-style forms, OpenSearch `fq=` facets) are first-class in real query strings
and a `{ String: String }` map would silently drop all but one. Callers that want a
scalar take `m["k"][0]`; the multimap is the lossless representation and pairs cleanly
with `buildQuery`. Malformed percent-escapes in a key or value are **lenient** here —
they are left as their literal bytes rather than failing the whole parse — because a
query string is attacker/legacy-controlled input that a server should not 500 on; use
[`urlDecode`](#urldecode) directly when strict validation is wanted.

```txt
parseQuery("a=1&b=2&a=3")     // { "a": ["1", "3"], "b": ["2"] }
parseQuery("?q=hello+world")  // { "q": ["hello world"] }
parseQuery("flag")            // { "flag": [""] }
parseQuery("")                // {}
```

---

### buildQuery

```txt
val buildQuery: (params: { String: String[] }) -> String
```

Serializes a multimap back to a query string: each key is paired with each of its values
(`k=v1&k=v2`), keys and values percent-encoded with the component rules **plus** the
form convention of encoding space as `+`. The inverse of `parseQuery`; round-trips up to
key ordering (map iteration order). Returns `""` for an empty map. No leading `?` is
emitted — the caller prepends it (or `std/http`/`std/url` does).

```txt
buildQuery({ "a": ["1", "3"], "b": ["2"] })   // "a=1&a=3&b=2"
buildQuery({ "q": ["hello world"] })          // "q=hello+world"
buildQuery({})                                // ""
```

A scalar-map convenience overload is intentionally omitted: callers with a
`{ String: String }` wrap each value in a one-element array (`{ "a": ["1"] }`), keeping a
single code path and a single, lossless type.

---

## Implementation notes

**Almost entirely pure Lin.** Like `std/bytes`, this module is written in Lin on the
existing primitives, with the codec tables and bit-twiddling done in-language:

- **Base64 / hex encode** — pure Lin. The alphabets are `Int32[]` codepoint tables
  (`fromCodePoints`-ready). Encoding groups input bytes (3-at-a-time for Base64,
  1-at-a-time for hex), assembles the output codepoints with shifts/masks (`>>`, `&`,
  `|`) and the `std/number` narrowing casts (`toUInt8`, etc.), and builds the final
  `String` with [`fromCodePoints`](STDLIB.md#fromCodePoints) — sound because all output
  is ASCII (`< 128`), so codepoint == byte.
- **Base64 / hex decode** — pure Lin. Scan the input with O(1)
  [`byteAt`](STDLIB.md#byteAt) (so a whole string is O(n), avoiding the O(n²) trap of
  `codePointAt` in a loop), map each ASCII char back through a reverse-lookup (a small
  `Int32[]` of size 128 or a `match`), reassemble bytes with shifts, and `push` into a
  `UInt8[]` accumulator. Validation (non-alphabet char, bad length/padding, odd hex
  length) returns the `{ "type": "error", "message": String }` literal.
- **urlEncode** — pure Lin. Needs the UTF-8 bytes of the input `String`: obtained with
  no new intrinsic by looping [`byteAt`](STDLIB.md#byteAt) from index 0 until it returns
  `-1` (the documented out-of-range sentinel), giving the raw UTF-8 byte stream O(n).
  Each byte is either passed through (unreserved) or emitted as `%` + two hex digits
  (reuse the hex nibble table).
- **parseQuery / buildQuery** — pure Lin over `std/string` (`split`/`indexOf`/
  `substring`) and the map literal type `{ String: String[] }` already used by
  `std/http` headers. They delegate escaping to `urlEncode`/`urlDecode`.

**The one intrinsic: UTF-8 `bytes → String`.** Decoding (`base64DecodeString`,
`base64Decode`'s string wrapper, and `urlDecode`) must turn a `UInt8[]` of UTF-8 bytes
back into a `String`. This **cannot** be done with `fromCodePoints`, which interprets
each integer as a *codepoint* — feeding it raw multi-byte UTF-8 would mojibake (e.g. the
two bytes of `é` would become two separate Latin-1 codepoints). A single shared runtime
intrinsic is required:

```txt
import foreign "lin-runtime"
  val lin_string_from_utf8: (UInt8[]) -> String | Error   // validates UTF-8; Error on invalid sequence
```

This is the natural inverse of `byteAt` and is genuinely missing from the stdlib today
(there is `String → bytes` via `byteAt`, but no validated `bytes → String`). It is **not**
a `std/encoding`-specific intrinsic — it is a general string primitive that arguably
belongs in `std/string` (as e.g. `fromUtf8`) and would also serve `std/fs`/`std/stream`
text decoding. Recommendation: land `lin_string_from_utf8` as a `std/string.fromUtf8`
export and have `std/encoding` import it, rather than hiding it inside this module.
(`std/stream`'s `readText` already does UTF-8 decode in the runtime, so the capability
exists — this just surfaces it as a pure function.)

**Stand alone — do not fold into `std/bytes`.** `std/bytes` is deliberately a small,
allocation-light, *infallible* numeric-serialization module (endian reads/writes, float
bits) whose entire surface is `UInt8[] ↔ integer/float`. `std/encoding` is a different
concern: it is `bytes ↔ text`, it is **fallible** (every decoder returns `T | Error`), it
depends on `std/string`/`std/number`/maps and the new `fromUtf8`, and its primary
audience is web/transport code (the `std/http` partner) rather than binary-format code.
Merging them would bloat `std/bytes`'s dependency footprint and blur a clean line. Keep
`std/encoding` as a sibling module that *uses* `std/bytes`-style techniques. The query
half (`parseQuery`/`buildQuery`) is the standout addition relative to the mainstream
(Node `querystring`, Go `net/url.Values`, Python `urllib.parse`) and is the piece
`std/http` most needs today.
