## Status: proposal

Lin can hash structurally (`std/hash`) and shuffle bytes (`std/bytes`), but it has no security-grade
cryptography: there is no way to fingerprint content, sign a request, verify a webhook, mint a session
token, or generate a collision-resistant identifier without shelling out to `std/process`. `std/crypto`
fills that gap with the small, ubiquitous set of primitives that application code actually reaches for —
**content-addressing** (`sha256` of a blob → a stable id, e.g. a Git-style object store or a CAS cache
key), **HTTP ETags** (`sha256Hex` of a response body), **webhook / API signatures** (`hmacSha256` with a
shared secret, verified in constant time), **auth tokens and salts** (`randomBytes` from the OS CSPRNG),
and **record / request identifiers** (`uuidV4`, time-ordered `uuidV7`). It is deliberately *not* a
full TLS/asymmetric/AEAD suite — just the digest, MAC, CSPRNG, and UUID building blocks that every other
standard library ships. This module is distinct from [`std/hash`](../../STDLIB.md#stdhash), which is a
*non-cryptographic* structural hash for map keys and equality and must never be used for security.

Every mainstream runtime ships this set: Python (`hashlib`, `hmac`, `secrets`, `uuid`), Go
(`crypto/sha256`, `crypto/hmac`, `crypto/rand`, `github.com/google/uuid`), Java (`MessageDigest`,
`Mac`, `SecureRandom`, `java.util.UUID`), and Node (`crypto.createHash` / `createHmac` /
`randomBytes` / `randomUUID`).

## std/crypto

Security-grade hashing, message authentication, cryptographically-secure randomness, and UUID
generation. Digests and MACs operate over `UInt8[]` byte buffers (the canonical binary type, §35.1);
`String` convenience wrappers UTF-8-encode their input first. All digest output is available both as a
lowercase hex `String` (the common case — fits in URLs, ETags, JSON) and as a raw `UInt8[]` (for
re-hashing, concatenation, or binary protocols).

These primitives **cannot be written in Lin** — like the float bit-reinterpret intrinsics in
[`std/bytes`](../../STDLIB.md#stdbytes), they require operations Lin does not expose (rotate-heavy block
compression, access to the OS entropy pool). The whole module therefore lowers to Rust runtime
intrinsics; see [Implementation notes](#implementation-notes).

> **Security warning.** `sha1` and `md5` are included only for interoperability with legacy systems
> (old Git object ids, S3 ETags, existing file checksums). They are **cryptographically broken** —
> collisions are practical — and must never be used for signatures, integrity against an adversary, or
> password handling. Use `sha256` (or `sha512`) for anything security-sensitive. For passwords
> specifically, none of these are appropriate; a dedicated password hash (Argon2/bcrypt/scrypt) is out
> of scope for this proposal.

Import:

```txt
import { sha256Hex, hmacSha256, randomBytes, uuidV4, constantTimeEqual } from "std/crypto"
```

`Hasher` is an opaque runtime type returned by `newHasher` — an incremental digest accumulator (see
[Streaming hashes](#streaming-hashes)). Like `Timer` (`std/time`) and `Stream<T>` (`std/stream`), it is
a handle the runtime interprets; it is not JSON and not subscriptable.

### Functions

| Function | Signature | Summary |
| --- | --- | --- |
| [`sha256`](#sha256) | `(UInt8[]) -> UInt8[]` | SHA-256 digest, 32 raw bytes |
| [`sha256Hex`](#sha256Hex) | `(UInt8[]) -> String` | SHA-256 digest as 64-char lowercase hex |
| [`sha512`](#sha512) | `(UInt8[]) -> UInt8[]` | SHA-512 digest, 64 raw bytes |
| [`sha512Hex`](#sha512Hex) | `(UInt8[]) -> String` | SHA-512 digest as 128-char lowercase hex |
| [`sha1`](#sha1) | `(UInt8[]) -> UInt8[]` | **Legacy/insecure** SHA-1 digest, 20 raw bytes |
| [`sha1Hex`](#sha1Hex) | `(UInt8[]) -> String` | **Legacy/insecure** SHA-1 as 40-char hex |
| [`md5`](#md5) | `(UInt8[]) -> UInt8[]` | **Legacy/insecure** MD5 digest, 16 raw bytes |
| [`md5Hex`](#md5Hex) | `(UInt8[]) -> String` | **Legacy/insecure** MD5 as 32-char hex |
| [`hashString`](#hashString) | `(String, String) -> String` | UTF-8 hash a string by named algorithm, hex out |
| [`hmacSha256`](#hmacSha256) | `(UInt8[], UInt8[]) -> UInt8[]` | HMAC-SHA-256 tag, 32 raw bytes |
| [`hmacSha256Hex`](#hmacSha256Hex) | `(UInt8[], UInt8[]) -> String` | HMAC-SHA-256 tag as 64-char hex |
| [`randomBytes`](#randomBytes) | `(Int32) -> UInt8[]` | n cryptographically-secure random bytes |
| [`uuidV4`](#uuidV4) | `() -> String` | Random (version 4) UUID string |
| [`uuidV7`](#uuidV7) | `() -> String` | Time-ordered (version 7) UUID string |
| [`constantTimeEqual`](#constantTimeEqual) | `(UInt8[], UInt8[]) -> Boolean` | Timing-safe byte-buffer comparison |
| [`toBytes`](#toBytes) | `(String) -> UInt8[]` | UTF-8 encode a string to bytes |
| [`fromHex`](#fromHex) | `(String) -> UInt8[] \| Error` | Decode a hex string to bytes |
| [`toHex`](#toHex) | `(UInt8[]) -> String` | Lowercase-hex encode bytes |
| [`newHasher`](#newHasher) | `(String) -> Hasher \| Error` | Create an incremental hasher by algorithm name |
| [`update`](#update) | `(Hasher, UInt8[]) -> Hasher` | Feed bytes into a hasher |
| [`digest`](#digest) | `(Hasher) -> UInt8[]` | Finalise a hasher to its raw digest |
| [`digestHex`](#digestHex) | `(Hasher) -> String` | Finalise a hasher to hex |

---

### sha256

```txt
val sha256: (data: UInt8[]) -> UInt8[]
```

Computes the SHA-256 digest of `data`, returning the 32 raw output bytes. Hashing never fails, so there
is no `| Error` arm. Use [`sha256Hex`](#sha256Hex) when you want a printable id; use the raw form when
the digest feeds back into more bytes (concatenation, a second hash, an HMAC key).

```txt
import { sha256, toBytes } from "std/crypto"

val d = sha256(toBytes("hello"))
length(d)   // 32
```

---

### sha256Hex

```txt
val sha256Hex: (data: UInt8[]) -> String
```

Like [`sha256`](#sha256) but returns the digest as a 64-character lowercase hexadecimal string —
the form used for content-addressed ids and HTTP ETags.

```txt
import { sha256Hex, toBytes } from "std/crypto"

sha256Hex(toBytes("hello"))
// "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
```

To ETag a response body or content-address a file read with `readFileBytes` (`std/fs`):

```txt
import { sha256Hex } from "std/crypto"
import { readFileBytes } from "std/fs"

val body = readFileBytes("page.html")
val etag = body is Error ? body : "\"${sha256Hex(body)}\""
```

---

### sha512 {#sha512}

```txt
val sha512: (data: UInt8[]) -> UInt8[]
```

Computes the SHA-512 digest of `data` (64 raw bytes). Faster than SHA-256 on 64-bit hardware and a
reasonable default when output length is not a constraint.

```txt
length(sha512(toBytes("hello")))   // 64
```

---

### sha512Hex {#sha512Hex}

```txt
val sha512Hex: (data: UInt8[]) -> String
```

SHA-512 as a 128-character lowercase hex string.

---

### sha1 {#sha1}

```txt
val sha1: (data: UInt8[]) -> UInt8[]
```

**Legacy / insecure.** Computes the SHA-1 digest (20 raw bytes). SHA-1 is cryptographically broken
(practical collisions) — use it *only* to interoperate with systems that mandate it (e.g. reading
existing Git object ids). Never use it for signatures or adversarial integrity.

```txt
length(sha1(toBytes("hello")))   // 20
```

---

### sha1Hex {#sha1Hex}

```txt
val sha1Hex: (data: UInt8[]) -> String
```

**Legacy / insecure.** SHA-1 as a 40-character lowercase hex string. See [`sha1`](#sha1).

---

### md5 {#md5}

```txt
val md5: (data: UInt8[]) -> UInt8[]
```

**Legacy / insecure.** Computes the MD5 digest (16 raw bytes). MD5 is thoroughly broken — present only
for legacy interop (old file checksums, some S3 ETags). Never use it for security.

---

### md5Hex {#md5Hex}

```txt
val md5Hex: (data: UInt8[]) -> String
```

**Legacy / insecure.** MD5 as a 32-character lowercase hex string. See [`md5`](#md5).

```txt
md5Hex(toBytes(""))   // "d41d8cd98f00b204e9800998ecf8427e"
```

---

### hashString {#hashString}

```txt
val hashString: (s: String, algorithm: String) -> String
```

Convenience wrapper that UTF-8-encodes `s` and hashes it with the named `algorithm`, returning a
lowercase-hex string. `algorithm` is one of `"sha256"`, `"sha512"`, `"sha1"`, or `"md5"` (matched
case-insensitively). An unknown algorithm name is a compile-time-checkable mistake only when the literal
is known; at runtime an unrecognised name falls back to `"sha256"` — prefer the dedicated
`shaNNNHex(toBytes(s))` functions when the algorithm is fixed, and reserve `hashString` for when the
algorithm is selected dynamically (e.g. from a config field).

```txt
import { hashString } from "std/crypto"

hashString("hello", "sha256")
// "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
```

> Design note: because `algorithm` is a plain `String`, this avoids a string-typo silently producing the
> wrong digest only by the fixed allow-list above. The typed, per-algorithm functions are the
> recommended surface; `hashString` exists for the genuinely dynamic case.

---

### hmacSha256 {#hmacSha256}

```txt
val hmacSha256: (key: UInt8[], message: UInt8[]) -> UInt8[]
```

Computes the HMAC-SHA-256 authentication tag of `message` under secret `key`, returning the 32 raw tag
bytes. HMAC is the standard way to authenticate a message with a shared secret (webhook signatures,
signed cookies, API request signing). The key may be any length; HMAC handles padding/hashing internally.

To **verify** a tag, recompute it and compare with [`constantTimeEqual`](#constantTimeEqual) — never with
`==`, which short-circuits and leaks timing.

```txt
import { hmacSha256, constantTimeEqual, toBytes } from "std/crypto"

val key = toBytes("s3cret")
val tag = hmacSha256(key, toBytes("payload"))

// verify an incoming tag
val ok = constantTimeEqual(tag, incoming)
```

---

### hmacSha256Hex {#hmacSha256Hex}

```txt
val hmacSha256Hex: (key: UInt8[], message: UInt8[]) -> String
```

Like [`hmacSha256`](#hmacSha256) but returns the tag as a 64-character lowercase hex string — the form
most webhook providers (Stripe, GitHub, …) send in a signature header.

```txt
import { hmacSha256Hex, toBytes } from "std/crypto"

hmacSha256Hex(toBytes("key"), toBytes("The quick brown fox jumps over the lazy dog"))
// "f7bc83f430538424b13298e6aa6fb143ef4d59a14946175997479dbc2d1a3cd8"
```

> Note: to verify a *hex* signature header safely, prefer decoding the incoming hex with
> [`fromHex`](#fromHex) and comparing the raw bytes with [`constantTimeEqual`](#constantTimeEqual),
> rather than string-comparing hex.

---

### randomBytes {#randomBytes}

```txt
val randomBytes: (n: Int32) -> UInt8[]
```

Returns `n` bytes drawn from the operating system's cryptographically-secure random number generator
(CSPRNG) — `getrandom(2)` / `/dev/urandom` on Linux, `BCryptGenRandom` on Windows. Use this for
anything an attacker must not predict: session tokens, salts, nonces, key material.

**This is *not* `math.random`.** [`std/math`](../../STDLIB.md#stdmath) `random` is a fast, seedable PRNG
for simulations and sampling and is **trivially predictable** — never use it for secrets. `randomBytes`
is the secure one. `n` must be ≥ 0; `randomBytes(0)` returns an empty buffer.

```txt
import { randomBytes, toHex } from "std/crypto"

val token = toHex(randomBytes(32))   // a 256-bit, URL-safe-ish hex token
```

---

### uuidV4 {#uuidV4}

```txt
val uuidV4: () -> String
```

Generates a random (version 4) UUID as a canonical lowercase 8-4-4-4-12 hyphenated string. The 122
random bits come from the CSPRNG (the same source as [`randomBytes`](#randomBytes)), so collisions are
negligible. This is the right default for surrogate ids that need no ordering.

```txt
import { uuidV4 } from "std/crypto"

uuidV4()   // e.g. "f47ac10b-58cc-4372-a567-0e02b2c3d479"
```

---

### uuidV7 {#uuidV7}

```txt
val uuidV7: () -> String
```

Generates a time-ordered (version 7) UUID. The leading 48 bits are the Unix millisecond timestamp (the
same clock as [`std/time`](../../STDLIB.md#stdtime) `now`), the remainder is CSPRNG-random. v7 values
sort chronologically as strings and as bytes, which makes them far better than v4 as **database primary
keys** (index locality, no page-split churn) while remaining globally unique.

```txt
import { uuidV7 } from "std/crypto"

val a = uuidV7()
val b = uuidV7()
a < b   // true (b minted later) — lexicographically time-ordered
```

---

### constantTimeEqual {#constantTimeEqual}

```txt
val constantTimeEqual: (a: UInt8[], b: UInt8[]) -> Boolean
```

Compares two byte buffers for equality in time that depends only on their length, never on *where* they
first differ. A normal `==` (or a hand-written byte loop) returns as soon as it finds a mismatch, so an
attacker who can measure response time can recover a secret tag byte-by-byte. Always use
`constantTimeEqual` when comparing HMAC tags, password-derived material, or any secret-dependent bytes.

Returns `false` immediately for differing lengths (length is not itself secret in these protocols); for
equal-length inputs the comparison touches every byte.

```txt
import { hmacSha256, constantTimeEqual, toBytes } from "std/crypto"

val expected = hmacSha256(key, body)
val ok = constantTimeEqual(expected, providedTag)   // timing-safe
// NOT: expected == providedTag   <- leaks timing
```

---

### toBytes {#toBytes}

```txt
val toBytes: (s: String) -> UInt8[]
```

UTF-8-encodes a string to its byte buffer — the bridge between codepoint-aware `String` (§9) and the
`UInt8[]` the digest/HMAC functions consume. Provided here so `std/crypto` is usable without pulling in
another module; semantically identical to a UTF-8 encode.

```txt
toBytes("hi")   // [0x68, 0x69]
toBytes("é")    // [0xC3, 0xA9]  (2 UTF-8 bytes, 1 codepoint)
```

---

### toHex {#toHex}

```txt
val toHex: (data: UInt8[]) -> String
```

Encodes a byte buffer as a lowercase hexadecimal string (two chars per byte, no separators). The inverse
of [`fromHex`](#fromHex). Hex encoding never fails.

```txt
toHex([0xDE, 0xAD, 0xBE, 0xEF])   // "deadbeef"
```

---

### fromHex {#fromHex}

```txt
val fromHex: (s: String) -> UInt8[] | Error
```

Decodes a hex string back to bytes. Accepts upper- or lowercase digits. Returns an `Error` if `s` has an
odd length or contains a non-hex character — the only fallible function in this module, since parsing
untrusted input can fail (most crypto operations cannot).

```txt
fromHex("deadbeef")   // [0xDE, 0xAD, 0xBE, 0xEF]
fromHex("xyz")        // { "type": "error", "message": "invalid hex" }
fromHex("abc")        // { "type": "error", "message": "odd-length hex" }
```

---

### Streaming hashes

A streaming, incremental hasher lets you fingerprint data larger than memory — a multi-gigabyte upload, a
file read through [`std/stream`](../../STDLIB.md#stdstream) — without ever holding the whole input in a
`UInt8[]`. This fits Lin's streaming ethos: feed each chunk as it arrives, then finalise. The `Hasher` is
an opaque handle (like `Timer`/`Stream`), so the in-progress block state never crosses into user code.

`update` returns the same `Hasher` so calls chain naturally with dot-application and fold cleanly over a
stream's chunks. `digest`/`digestHex` consume the accumulated state and return the final digest;
finalising leaves the handle spent (a further `update` is a no-op-then-error in debug builds).

#### newHasher {#newHasher}

```txt
val newHasher: (algorithm: String) -> Hasher | Error
```

Creates an incremental hasher for `algorithm` (`"sha256"`, `"sha512"`, `"sha1"`, `"md5"`,
case-insensitive). Returns an `Error` for an unknown algorithm — the construction is the validation
point, so `update`/`digest` themselves never fail.

#### update {#update}

```txt
val update: (h: Hasher, data: UInt8[]) -> Hasher
```

Feeds `data` into the running digest and returns `h` for chaining. Equivalent to having concatenated all
the fed buffers before a one-shot hash.

#### digest {#digest}

```txt
val digest: (h: Hasher) -> UInt8[]
```

Finalises and returns the raw digest bytes (length per algorithm — 32 for SHA-256, etc.).

#### digestHex {#digestHex}

```txt
val digestHex: (h: Hasher) -> String
```

Finalises and returns the digest as a lowercase hex string.

```txt
import { newHasher, update, digestHex } from "std/crypto"
import { readStream, chunks, for } from "std/stream"

// Content-address a large file without materialising it
val h = newHasher("sha256")
val fingerprint = h is Error ? h :
  block:
    chunks(readStream("big.iso"), 65536).for((c: UInt8[]) => update(h, c))
    digestHex(h)
// fingerprint: the file's SHA-256, computed in 64 KiB windows
```

The one-shot [`sha256`](#sha256) family is exactly `digest(update(newHasher("sha256"), data))`; prefer
the one-shot functions for small in-memory buffers and the `Hasher` for streamed or chunked input.

---

## Implementation notes

Everything in `std/crypto` is a **Rust runtime intrinsic** — none of it is expressible in Lin (it needs
block-compression primitives and the OS entropy pool, just as `std/bytes`' float bit-reinterprets are
intrinsics). The `.lin` stub mirrors `std/bytes`: a thin `export val` per function that forwards to a
`lin_crypto_*` foreign symbol declared via `import foreign "lin-runtime"`, so the public types/docs live
in Lin while the work lives in Rust.

Suggested crates (all mature, audited, no-OpenSSL pure-Rust where possible):

- **Digests** — `sha2` (SHA-256/512), `sha1`, `md-5`, all behind the `digest` trait crate. `lin_crypto_sha256(buf) -> buf`, etc. The hex variants reuse the raw intrinsic plus a `hex`-crate encode (`toHex`/`fromHex` themselves wrap `hex::encode` / `hex::decode`).
- **HMAC** — `hmac` over `sha2::Sha256` (`Hmac<Sha256>`). `lin_crypto_hmac_sha256(key, msg) -> tag`.
- **CSPRNG** — `getrandom` directly for `randomBytes` (it is exactly the OS CSPRNG, no userspace PRNG state to seed or fork-unsafely share). Contrast `std/math` `random`, which is a userspace PRNG.
- **UUID** — `uuid` with the `v4`/`v7` + `getrandom` features; v7's timestamp comes from the same monotonic-ish wall clock backing `std/time` `now`.
- **Constant-time compare** — `subtle` (`ConstantTimeEq` / `slices_equal`), or a hand-rolled XOR-accumulate loop with a compiler fence. `lin_crypto_ct_eq(a, b) -> bool`, with the early `false` on length-mismatch in the Lin wrapper.

The streaming `Hasher` follows the established opaque-handle pattern (`Timer`, `Stream<T>`,
`ProcessHandle`): the runtime allocates a boxed enum over the per-algorithm `digest` state
(`Sha256(sha2::Sha256)` | `Sha512(...)` | `Sha1(...)` | `Md5(...)`), hands back an opaque id/pointer the
checker treats as the abstract `Hasher` type, and exposes `lin_crypto_hasher_new`,
`lin_crypto_hasher_update`, `lin_crypto_hasher_digest`. `update` mutates the boxed state in place and
returns the same handle; `digest`/`digestHex` call `finalize` and mark the handle spent. Buffer arguments
and results are the runtime's existing `UInt8[]` representation (packed byte buffers, §35.1), so no
boxing is needed across the FFI boundary beyond the standard array ABI.

Open questions for review:

1. Whether `hashString`/`newHasher` should take a closed string-literal-union type (e.g. an
   `"sha256" | "sha512" | "sha1" | "md5"` literal-union) instead of a free `String`, to make a typo a
   compile error rather than a runtime fallback. Depends on literal-union ergonomics at the call site.
2. Whether to add base64 (`toBase64`/`fromBase64`) here or in `std/bytes` — many crypto outputs are
   base64-encoded in the wild (JWTs, basic-auth, some signatures). Leaning toward `std/bytes` since it is
   an encoding, not a crypto primitive, but listing it here for discoverability.
3. Whether `md5`/`sha1` should be gated behind a separate `std/crypto/legacy` import path to make their
   use grep-able and intentional, rather than sitting next to the secure functions.
