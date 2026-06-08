# std/bytes

std/bytes — slicing and endian (de)serialization on `UInt8[]` byte buffers.

These functions read and write big-endian (`Be`) and little-endian (`Le`) integers and
IEEE-754 floats at byte offsets into packed `UInt8[]` buffers, and reinterpret float bit
patterns. They are the byte-level layer used to build and parse binary protocols and file
formats.

  import { u32FromBe, u32ToLe, f64ToBe, f64FromBe } from "std/bytes"

A `UInt8[]` buffer is a packed scalar array of bytes (`0..255`). Construct one with a
type-annotated array literal, e.g. `val buf: UInt8[] = [0, 0, 0, 0]`. Each `*To*` function
returns a freshly allocated `UInt8[]`; each `*From*` reads at a byte offset into an existing
buffer. The endianness suffix controls byte order: `Be` writes the most-significant byte
first, `Le` the least-significant byte first.

Maintainer note: built on §35.1 (UInt8[] packed byte buffers), §35.2 (bitwise operators),
the explicit narrowing casts from std/number (§26), and the float bit-reinterpret intrinsics
declared below (a float's bit pattern cannot be obtained by shift-and-mask, so those four are
runtime intrinsics).

## Reference

### slice

#### `slice`

```lin
val slice = (buf: UInt8[], start: Int32, end: Int32): UInt8[]
```

Copy the sub-buffer of `buf` from `start` (inclusive) to `end` (exclusive). Slicing a
`UInt8[]` yields a `UInt8[]`.
- **`buf`** — the source byte buffer.
- **`start`** — the start index; clamped to [0, length].
- **`end`** — the end index (exclusive); clamped to [0, length].
- **Returns** a new `UInt8[]` containing the bytes in the range.
- **Example:** slice([10, 20, 30, 40, 50], 1, 4)   // [20, 30, 40]

### bigendian reads

#### `u16FromBe`

```lin
val u16FromBe = (b: UInt8[], off: Int32): UInt16
```

Read a big-endian UInt16 from `b` at byte offset `off`.
- **`b`** — the byte buffer.
- **`off`** — the offset of the first (most-significant) byte.
- **Returns** the 16-bit value read from `b[off..off+2]`.
- **Example:** u16FromBe([0xBE, 0xEF], 0)   // 0xBEEF

#### `u32FromBe`

```lin
val u32FromBe = (b: UInt8[], off: Int32): UInt32
```

Read a big-endian UInt32 from `b` at byte offset `off`.
- **`b`** — the byte buffer.
- **`off`** — the offset of the first (most-significant) byte.
- **Returns** the 32-bit value read from `b[off..off+4]`.

#### `u64FromBe`

```lin
val u64FromBe = (b: UInt8[], off: Int32): UInt64
```

Read a big-endian UInt64 from `b` at byte offset `off`.
- **`b`** — the byte buffer.
- **`off`** — the offset of the first (most-significant) byte.
- **Returns** the 64-bit value read from `b[off..off+8]`.

### littleendian reads

#### `u16FromLe`

```lin
val u16FromLe = (b: UInt8[], off: Int32): UInt16
```

Read a little-endian UInt16 from `b` at byte offset `off`.
- **`b`** — the byte buffer.
- **`off`** — the offset of the first (least-significant) byte.
- **Returns** the 16-bit value read from `b[off..off+2]`.

#### `u32FromLe`

```lin
val u32FromLe = (b: UInt8[], off: Int32): UInt32
```

Read a little-endian UInt32 from `b` at byte offset `off`.
- **`b`** — the byte buffer.
- **`off`** — the offset of the first (least-significant) byte.
- **Returns** the 32-bit value read from `b[off..off+4]`.

#### `u64FromLe`

```lin
val u64FromLe = (b: UInt8[], off: Int32): UInt64
```

Read a little-endian UInt64 from `b` at byte offset `off`.
- **`b`** — the byte buffer.
- **`off`** — the offset of the first (least-significant) byte.
- **Returns** the 64-bit value read from `b[off..off+8]`.

### bigendian writes

#### `u16ToBe`

```lin
val u16ToBe = (v: UInt16): UInt8[]
```

Serialize a UInt16 to its 2-byte big-endian representation.
- **`v`** — the value to encode.
- **Returns** a 2-byte `UInt8[]`, most-significant byte first.
- **Example:** u16ToBe(0xBEEF)   // [0xBE, 0xEF]

#### `u32ToBe`

```lin
val u32ToBe = (v: UInt32): UInt8[]
```

Serialize a UInt32 to its 4-byte big-endian representation.
- **`v`** — the value to encode.
- **Returns** a 4-byte `UInt8[]`, most-significant byte first.

#### `u64ToBe`

```lin
val u64ToBe = (v: UInt64): UInt8[]
```

Serialize a UInt64 to its 8-byte big-endian representation.
- **`v`** — the value to encode.
- **Returns** an 8-byte `UInt8[]`, most-significant byte first.

### littleendian writes

#### `u16ToLe`

```lin
val u16ToLe = (v: UInt16): UInt8[]
```

Serialize a UInt16 to its 2-byte little-endian representation.
- **`v`** — the value to encode.
- **Returns** a 2-byte `UInt8[]`, least-significant byte first.

#### `u32ToLe`

```lin
val u32ToLe = (v: UInt32): UInt8[]
```

Serialize a UInt32 to its 4-byte little-endian representation.
- **`v`** — the value to encode.
- **Returns** a 4-byte `UInt8[]`, least-significant byte first.
- **Example:** u32ToLe(0x11223344)   // [0x44, 0x33, 0x22, 0x11]

#### `u64ToLe`

```lin
val u64ToLe = (v: UInt64): UInt8[]
```

Serialize a UInt64 to its 8-byte little-endian representation.
- **`v`** — the value to encode.
- **Returns** an 8-byte `UInt8[]`, least-significant byte first.

### float bit reinterpret

#### `f32ToBits`

```lin
val f32ToBits = (f: Float32): UInt32
```

`f32ToBits`/`f64ToBits` expose the raw IEEE-754 bit pattern of a float (and the `*FromBits`
inverses) without going through a byte buffer — the only way to inspect those bits, since a
float's bit pattern cannot be obtained by shift-and-mask.
Reinterpret the bit pattern of a Float32 as a UInt32 (IEEE-754 raw bits).
- **`f`** — the float.
- **Returns** the 32-bit pattern of `f`.

#### `f32FromBits`

```lin
val f32FromBits = (u: UInt32): Float32
```

Reinterpret a UInt32 bit pattern as a Float32 (IEEE-754).
- **`u`** — the 32-bit pattern.
- **Returns** the Float32 with those bits.

#### `f64ToBits`

```lin
val f64ToBits = (f: Float64): UInt64
```

Reinterpret the bit pattern of a Float64 as a UInt64 (IEEE-754 raw bits).
- **`f`** — the double.
- **Returns** the 64-bit pattern of `f`.
- **Example:** f64ToBits(1.0)                  // 0x3FF0000000000000
- **Example:** f64FromBits(f64ToBits(1.0))     // 1.0

#### `f64FromBits`

```lin
val f64FromBits = (u: UInt64): Float64
```

Reinterpret a UInt64 bit pattern as a Float64 (IEEE-754).
- **`u`** — the 64-bit pattern.
- **Returns** the Float64 with those bits.

### float (de)serialization (bigendian)

#### `f32ToBe`

```lin
val f32ToBe = (f: Float32): UInt8[]
```

Serialize a Float32 to its 4-byte big-endian IEEE-754 representation.
- **`f`** — the value to encode.
- **Returns** a 4-byte `UInt8[]`, most-significant byte first.

#### `f32FromBe`

```lin
val f32FromBe = (b: UInt8[], off: Int32): Float32
```

Read a big-endian IEEE-754 Float32 from `b` at byte offset `off`.
- **`b`** — the byte buffer.
- **`off`** — the offset of the first byte.
- **Returns** the decoded Float32 from `b[off..off+4]`.

#### `f64ToBe`

```lin
val f64ToBe = (f: Float64): UInt8[]
```

Serialize a Float64 to its 8-byte big-endian IEEE-754 representation.
- **`f`** — the value to encode.
- **Returns** an 8-byte `UInt8[]`, most-significant byte first.
- **Example:** val bytes: UInt8[] = f64ToBe(3.14159)   // 8 bytes
- **Example:** f64FromBe(bytes, 0)                      // 3.14159

#### `f64FromBe`

```lin
val f64FromBe = (b: UInt8[], off: Int32): Float64
```

Read a big-endian IEEE-754 Float64 from `b` at byte offset `off`.
- **`b`** — the byte buffer.
- **`off`** — the offset of the first byte.
- **Returns** the decoded Float64 from `b[off..off+8]`.

### float (de)serialization (littleendian)

#### `f32ToLe`

```lin
val f32ToLe = (f: Float32): UInt8[]
```

Serialize a Float32 to its 4-byte little-endian IEEE-754 representation.
- **`f`** — the value to encode.
- **Returns** a 4-byte `UInt8[]`, least-significant byte first.

#### `f32FromLe`

```lin
val f32FromLe = (b: UInt8[], off: Int32): Float32
```

Read a little-endian IEEE-754 Float32 from `b` at byte offset `off`.
- **`b`** — the byte buffer.
- **`off`** — the offset of the first byte.
- **Returns** the decoded Float32 from `b[off..off+4]`.

#### `f64ToLe`

```lin
val f64ToLe = (f: Float64): UInt8[]
```

Serialize a Float64 to its 8-byte little-endian IEEE-754 representation.
- **`f`** — the value to encode.
- **Returns** an 8-byte `UInt8[]`, least-significant byte first.

#### `f64FromLe`

```lin
val f64FromLe = (b: UInt8[], off: Int32): Float64
```

Read a little-endian IEEE-754 Float64 from `b` at byte offset `off`.
- **`b`** — the byte buffer.
- **`off`** — the offset of the first byte.
- **Returns** the decoded Float64 from `b[off..off+8]`.
