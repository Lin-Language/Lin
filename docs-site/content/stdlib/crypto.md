# std/crypto

std/crypto — security-grade hashing, HMAC, CSPRNG, UUID, and constant-time compare.

Distinct from std/hash (a non-cryptographic structural hash for map keys/equality, never for
security). Digests and MACs operate over UInt8[] byte buffers; the *Hex variants return lowercase
hex strings.

Security: sha1 and md5 are cryptographically broken and present only for legacy interop (old Git
ids, S3 ETags, file checksums). Avoid them for signatures or adversarial integrity; use
sha256/sha512. randomBytes is the OS CSPRNG, not std/math `random` (which is predictable).

## Reference

#### `Hasher`

```lin
type Hasher = Int64
```

An opaque incremental-hash handle: an Int64 id the runtime interprets, not subscriptable. Created
by newHasher, fed by update, finalised by digest/digestHex.

### SHA256

#### `sha256`

```lin
val sha256 = (data: UInt8[]): UInt8[]
```

Hash bytes with SHA-256. Hashing never fails. Use sha256Hex for a printable id; prefer the raw
form when the digest feeds back into more bytes.
- **`data`** — the bytes to hash.
- **Returns** the 32-byte raw digest.

#### `sha256Hex`

```lin
val sha256Hex = (data: UInt8[]): String
```

SHA-256 as a 64-char lowercase hex string (content-addressed ids, HTTP ETags).
- **`data`** — the bytes to hash.
- **Returns** the digest as lowercase hex.

### SHA512

#### `sha512`

```lin
val sha512 = (data: UInt8[]): UInt8[]
```

Hash bytes with SHA-512 (faster than SHA-256 on 64-bit hardware).
- **`data`** — the bytes to hash.
- **Returns** the 64-byte raw digest.

#### `sha512Hex`

```lin
val sha512Hex = (data: UInt8[]): String
```

SHA-512 as a 128-char lowercase hex string.
- **`data`** — the bytes to hash.
- **Returns** the digest as lowercase hex.

### SHA1 (legacy / insecure)

#### `sha1`

```lin
val sha1 = (data: UInt8[]): UInt8[]
```

Hash bytes with SHA-1. Broken (practical collisions) — interop only (e.g. Git object ids).
- **`data`** — the bytes to hash.
- **Returns** the 20-byte raw digest.

#### `sha1Hex`

```lin
val sha1Hex = (data: UInt8[]): String
```

SHA-1 as a 40-char lowercase hex string. Legacy interop only; see sha1.
- **`data`** — the bytes to hash.
- **Returns** the digest as lowercase hex.

### MD5 (legacy / insecure)

#### `md5`

```lin
val md5 = (data: UInt8[]): UInt8[]
```

Hash bytes with MD5. Thoroughly broken — legacy interop only (old checksums, some S3 ETags).
- **`data`** — the bytes to hash.
- **Returns** the 16-byte raw digest.

#### `md5Hex`

```lin
val md5Hex = (data: UInt8[]): String
```

MD5 as a 32-char lowercase hex string. Legacy interop only; see md5.
- **`data`** — the bytes to hash.
- **Returns** the digest as lowercase hex.

### hashString

#### `hashString`

```lin
val hashString = (s: String, algorithm: String): String
```

UTF-8 hash a string by named algorithm, returning lowercase hex. Prefer the dedicated shaNNNHex
functions when the algorithm is fixed; use this for the dynamic case.
- **`s`** — the text to UTF-8 encode and hash.
- **`algorithm`** — "sha256" | "sha512" | "sha1" | "md5" (case-insensitive); an unknown name
                  falls back to sha256.
- **Returns** the digest as lowercase hex.

**Example:**

```lin
hashString("abc", "sha256")  // "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
```

### HMACSHA256

#### `hmacSha256`

```lin
val hmacSha256 = (key: UInt8[], message: UInt8[]): UInt8[]
```

Compute an HMAC-SHA-256 message authentication tag (webhook signatures, signed cookies, API
signing). Verify a received tag with constantTimeEqual, never `==`.
- **`key`** — the shared secret key bytes.
- **`message`** — the bytes to authenticate.
- **Returns** the 32-byte raw tag.

#### `hmacSha256Hex`

```lin
val hmacSha256Hex = (key: UInt8[], message: UInt8[]): String
```

HMAC-SHA-256 tag as a 64-char lowercase hex string (the form most webhook providers send).
- **`key`** — the shared secret key bytes.
- **`message`** — the bytes to authenticate.
- **Returns** the tag as lowercase hex.

### CSPRNG

#### `randomBytes`

```lin
val randomBytes = (n: Int32): UInt8[]
```

Generate cryptographically-secure random bytes from the OS CSPRNG (getrandom/urandom). For
tokens, salts, nonces, key material. Not std/math `random`.
- **`n`** — the number of bytes; should be >= 0 (0 returns an empty buffer).
- **Returns** a fresh buffer of `n` random bytes.

### UUID

#### `uuidV4`

```lin
val uuidV4 = (): String
```

Generate a random (version 4) UUID — 122 random bits from the CSPRNG.
- **Returns** the UUID as canonical lowercase 8-4-4-4-12.

#### `uuidV7`

```lin
val uuidV7 = (): String
```

Generate a time-ordered (version 7) UUID: leading 48 bits are the Unix-ms timestamp, remainder
CSPRNG. v7 values sort chronologically — better than v4 as database primary keys.
- **Returns** the UUID as canonical lowercase 8-4-4-4-12.

### constanttime compare

#### `constantTimeEqual`

```lin
val constantTimeEqual = (a: UInt8[], b: UInt8[]): Boolean
```

Compare two byte buffers in constant time: the runtime depends only on length, never on where
they differ. Use for HMAC tags / secret-dependent bytes.
- **`a`** — the first buffer.
- **`b`** — the second buffer.
- **Returns** true if equal; false immediately for differing lengths.

### codecs

#### `toBytes`

```lin
val toBytes = (s: String): UInt8[]
```

UTF-8-encode a string to its byte buffer (the bridge from codepoint String to UInt8[]).
- **`s`** — the text to encode.
- **Returns** the UTF-8 bytes.

#### `toHex`

```lin
val toHex = (data: UInt8[]): String
```

Lowercase-hex encode a byte buffer (two chars/byte, no separators). Never fails.
- **`data`** — the bytes to encode.
- **Returns** the lowercase hex string.

#### `fromHex`

```lin
val fromHex = (s: String): UInt8[] | Error
```

Decode a hex string to bytes (upper/lowercase accepted) — the only fallible codec here.
- **`s`** — the hex string to decode.
- **Returns** the decoded bytes (`UInt8[]`), or an Error for odd length or a non-hex character.

### streaming Hasher

#### `newHasher`

```lin
val newHasher = (algorithm: String): Hasher | Error
```

Create an incremental hasher. Construction is the validation point, so update/digest never fail.
- **`algorithm`** — "sha256" | "sha512" | "sha1" | "md5" (case-insensitive).
- **Returns** a Hasher handle, or an Error for an unknown algorithm.

#### `update`

```lin
val update = (h: Hasher, data: UInt8[]): Hasher
```

Feed bytes into a running digest.
- **`h`** — the hasher handle.
- **`data`** — the bytes to absorb.
- **Returns** the same handle, for chaining.

#### `digest`

```lin
val digest = (h: Hasher): UInt8[]
```

Finalise a hasher to its raw digest bytes (length per algorithm). Leaves the handle spent.
- **`h`** — the hasher handle.
- **Returns** the raw digest bytes.

#### `digestHex`

```lin
val digestHex = (h: Hasher): String
```

Finalise a hasher to a lowercase hex string. Leaves the handle spent.
- **`h`** — the hasher handle.
- **Returns** the digest as lowercase hex.
