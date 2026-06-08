# std/encoding

std/encoding — byte/text transport codecs: Base64 (standard + URL-safe), hex,
URL/percent encoding, and query-string parse/build.

These sit on the seam between `UInt8[]` byte buffers (std/bytes, §35.1) and `String`
text. Following the std/bytes precedent the codecs are pure Lin over the bitwise
operators (§35.2) and the std/number narrowing casts (§26); the one capability that
cannot be expressed in Lin — validated UTF-8 bytes → String — is borrowed from
std/string.fromUtf8 (a shared runtime primitive, not a codec-specific intrinsic).

Encoding never fails. Decoding is fallible: malformed Base64/hex/percent input returns
the standard Error shape `{ "type": "error", "message": String }`, matched with
`is Error` (the Error arm comes first in a match). The Base64 and hex alphabets and
output are pure ASCII, so output strings are assembled with `fromCodePoints` (sound
because codepoint == byte for ASCII); decoding scans input bytes with the O(1) `byteAt`
primitive (NOT codePointAt-in-a-loop, which would be O(n²)). Lin has no imperative
`while`, so the byte scans are written as tail-recursive helpers over a byte index.

Structural URL parsing (scheme/host/path/fragment) is out of scope — see std/url.

## Reference

### base64

#### `base64Encode`

```lin
val base64Encode = (bytes: UInt8[]): String
```

Encode bytes as standard Base64 (RFC 4648 §4, `+`/`/` alphabet) with `=` padding. Never fails.
- **`bytes`** — the bytes to encode.
- **Returns** the Base64 string.

#### `base64UrlEncode`

```lin
val base64UrlEncode = (bytes: UInt8[]): String
```

Encode bytes as URL-safe Base64 (RFC 4648 §5, `-`/`_` alphabet) WITHOUT padding. Never fails.
- **`bytes`** — the bytes to encode.
- **Returns** the URL-safe Base64 string.

#### `base64Decode`

```lin
val base64Decode = (s: String): UInt8[] | Error
```

Decode standard Base64 back to bytes. ASCII whitespace is ignored.
- **`s`** — the Base64 string to decode.
- **Returns** the decoded bytes, or an Error on a non-alphabet character, malformed padding, or an
         impossible length (≡ 1 mod 4).

#### `base64UrlDecode`

```lin
val base64UrlDecode = (s: String): UInt8[] | Error
```

Decode URL-safe Base64 back to bytes. Trailing `=` padding is tolerated but optional;
standard-alphabet characters (`+`/`/`) are rejected — use base64Decode for those.
- **`s`** — the URL-safe Base64 string to decode.
- **Returns** the decoded bytes, or an Error on a non-alphabet character or malformed padding.

### hex

#### `hexEncode`

```lin
val hexEncode = (bytes: UInt8[]): String
```

Encode bytes as lower-case hex, two chars per byte, most-significant nibble first. Never fails.
- **`bytes`** — the bytes to encode.
- **Returns** the hex string (length exactly 2 * length(bytes)).

#### `hexDecode`

```lin
val hexDecode = (s: String): UInt8[] | Error
```

Decode a hex string back to bytes. Accepts upper- and lower-case; does NOT skip whitespace or
accept a `0x` prefix.
- **`s`** — the hex string to decode.
- **Returns** the decoded bytes, or an Error on odd length or any non-hex character.

### url / percent

#### `urlEncode`

```lin
val urlEncode = (s: String): String
```

Percent-encode `s` as a URI COMPONENT (RFC 3986): the strict, maximal-escaping component
encoder, safe for both path segments and query values. Every UTF-8 byte is escaped to %XX
(upper-case hex) except the unreserved set; space becomes %20 (NOT +).
- **`s`** — the text to encode.
- **Returns** the percent-encoded string.

#### `urlDecode`

```lin
val urlDecode = (s: String): String | Error
```

Decode percent-escapes back to a String. `%XX` triples become their byte value, a literal `+`
decodes to a space (form symmetry), and the resulting bytes are UTF-8 decoded.
- **`s`** — the percent-encoded string.
- **Returns** the decoded text, or an Error on a `%` not followed by two hex digits, or invalid UTF-8.

### query string

#### `parseQuery`

```lin
val parseQuery = (s: String): { String: String[] }
```

Parse a query string into a multimap of key -> list of values. Pairs split on `&`; each pair
splits on the first `=`. Keys/values are percent-decoded with `+`->space (form semantics),
leniently (malformed escapes are kept literal). A bare key (`flag`) maps to a single `""` value.
A leading `?` is tolerated and stripped. Always succeeds.
- **`s`** — the query string (with or without a leading `?`).
- **Returns** the parsed multimap; each key maps to its list of values in input order.

#### `buildQuery`

```lin
val buildQuery = (params: { String: String[] }): String
```

Serialize a multimap back to a query string: each key paired with each value (`k=v1&k=v2`), keys
and values percent-encoded with the component rules PLUS the form convention of encoding space as
`+`. Inverse of parseQuery (up to key/iteration order). No leading `?` is emitted.
- **`params`** — the multimap of keys to value lists.
- **Returns** the query string, or "" for an empty map.

### string convenience wrappers

#### `base64EncodeString`

```lin
val base64EncodeString = (s: String): String
```

UTF-8 encode `s` to bytes, then standard Base64. Use for text payloads. Never fails.
- **`s`** — the text to encode.
- **Returns** the Base64 string.

#### `base64DecodeString`

```lin
val base64DecodeString = (s: String): String | Error
```

Standard Base64 decode, then UTF-8 decode the bytes back to a String.
- **`s`** — the Base64 string to decode.
- **Returns** the decoded text, or an Error on malformed Base64 OR invalid UTF-8.

### structural hash

#### `hash`

```lin
val hash = <T>(x: T): String
```

Compute a stable, canonical, type-tagged structural hash key for any value. The key matches
Lin's structural equality (spec §14): equal values hash equal, objects hash independently of
key order, and arrays hash order-sensitively. The type tag means values of different types
never collide — `hash(42)` is `"i:42"` while `hash("42")` is `"s:42"`. Use it to deduplicate
values or index them by structural identity (e.g. as object keys in a hand-rolled set/map, or
to bucket structurally-equal records with std/array's `countBy`). Generic over the input type —
it walks the runtime value, so T can be anything. (Folded in from the former std/hash module.)
- **`x`** — the value to hash.
- **Returns** the structural hash key as a String.
- **Example:** hash(null)     // "N"
- **Example:** hash(42)       // "i:42"
- **Example:** hash("hello")  // "s:hello"
- **Example:** hash([1, 2, 3]) == hash([1, 2, 3])   // true (order-sensitive: [1,2] != [2,1])
- **Example:** hash({ "x": 1, "y": 2 }) == hash({ "y": 2, "x": 1 })   // true (order-independent)
- **Example:** hash(42) == hash("42")   // false (type-tagged: never collides across types)
