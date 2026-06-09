# std/encoding

std/encoding — byte/text transport codecs: Base64 (standard and URL-safe), hex,
URL/percent encoding, and query-string parse/build.

These sit on the seam between `UInt8[]` byte buffers (std/bytes) and `String` text.

Encoding never fails. Decoding is fallible: malformed Base64, hex, or percent input returns
the standard Error shape `{ "type": "error", "message": String }`, matched with `is Error`.
The Base64 and hex alphabets and output are pure ASCII.

Structural URL parsing (scheme, host, path, fragment) is out of scope — see std/url.

## Reference

### shared tables

#### `utf8Bytes`

```lin
val utf8Bytes = (s: String): UInt8[]
```

UTF-8-encode a string to its byte buffer — the inverse of `std/string.fromUtf8`. This is the
String to UInt8[] companion to the byte/text codecs here.
- **`s`** — the text to encode.
- **Returns** the UTF-8 bytes of `s`.

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

Encode bytes as URL-safe Base64 (RFC 4648 §5, `-`/`_` alphabet) without padding. Never fails.
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

Decode a hex string back to bytes. Accepts upper- and lower-case; it does not skip whitespace
or accept a `0x` prefix.
- **`s`** — the hex string to decode.
- **Returns** the decoded bytes, or an Error on odd length or any non-hex character.

### url / percent

#### `urlEncode`

```lin
val urlEncode = (s: String): String
```

Percent-encode `s` as a URI component (RFC 3986): the strict, maximal-escaping component
encoder, safe for both path segments and query values. Every UTF-8 byte is escaped to %XX
(upper-case hex) except the unreserved set; space becomes %20, not `+`.
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
and values percent-encoded with the component rules plus the form convention of encoding space as
`+`. This is the inverse of parseQuery (up to key/iteration order). No leading `?` is emitted.
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
- **Returns** the decoded text, or an Error on malformed Base64 or invalid UTF-8.

### structural hash

#### `hash`

```lin
val hash = <T>(x: T): String
```

Compute a stable, canonical, type-tagged structural hash key for any value. The key matches
Lin's structural equality: equal values hash equal, objects hash independently of key order,
and arrays hash order-sensitively. The type tag means values of different types never collide —
`hash(42)` is `"i:42"` while `hash("42")` is `"s:42"`. Use it to deduplicate values or index
them by structural identity (for example, as object keys in a hand-rolled set or map, or to
bucket structurally-equal records with std/array's `countBy`). It is generic over the input
type and walks the runtime value, so `T` can be anything.
- **`x`** — the value to hash.
- **Returns** the structural hash key as a String.

**Example:**

```lin
hash(null)     // "N"
```

**Example:**

```lin
hash(42)       // "i:42"
```

**Example:**

```lin
hash("hello")  // "s:hello"
```

**Example:**

```lin
hash([1, 2, 3]) == hash([1, 2, 3])   // true (order-sensitive: [1,2] != [2,1])
```

**Example:**

```lin
hash({ "x": 1, "y": 2 }) == hash({ "y": 2, "x": 1 })   // true (order-independent)
```

**Example:**

```lin
hash(42) == hash("42")   // false (type-tagged: never collides across types)
```
