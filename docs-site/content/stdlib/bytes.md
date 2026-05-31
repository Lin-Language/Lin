# std/bytes

Slicing and endian (de)serialization on `UInt8[]` byte buffers. These functions read and write big-endian (`Be`) and little-endian (`Le`) integers and IEEE-754 floats at byte offsets into packed `UInt8[]` buffers, and reinterpret float bit patterns. They are the byte-level layer used to build and parse binary protocols and file formats.

```lin
import { u32FromBe, u32ToLe, f64ToBe, f64FromBe } from "std/bytes"
```

A `UInt8[]` buffer is a packed scalar array of bytes (`0..255`). Construct one with a type-annotated array literal, e.g. `val buf: UInt8[] = [0, 0, 0, 0]`.

## Function reference

| Function | Signature | Description |
| --- | --- | --- |
| `slice` | `(UInt8[], Int32, Int32) -> UInt8[]` | Copy of `[start, end)`; clamps to bounds |
| `u16FromBe` | `(UInt8[], Int32) -> UInt16` | Read big-endian u16 at offset |
| `u32FromBe` | `(UInt8[], Int32) -> UInt32` | Read big-endian u32 at offset |
| `u64FromBe` | `(UInt8[], Int32) -> UInt64` | Read big-endian u64 at offset |
| `u16FromLe` | `(UInt8[], Int32) -> UInt16` | Read little-endian u16 at offset |
| `u32FromLe` | `(UInt8[], Int32) -> UInt32` | Read little-endian u32 at offset |
| `u64FromLe` | `(UInt8[], Int32) -> UInt64` | Read little-endian u64 at offset |
| `u16ToBe` | `(UInt16) -> UInt8[]` | Encode u16 as 2 big-endian bytes |
| `u32ToBe` | `(UInt32) -> UInt8[]` | Encode u32 as 4 big-endian bytes |
| `u64ToBe` | `(UInt64) -> UInt8[]` | Encode u64 as 8 big-endian bytes |
| `u16ToLe` | `(UInt16) -> UInt8[]` | Encode u16 as 2 little-endian bytes |
| `u32ToLe` | `(UInt32) -> UInt8[]` | Encode u32 as 4 little-endian bytes |
| `u64ToLe` | `(UInt64) -> UInt8[]` | Encode u64 as 8 little-endian bytes |
| `f32ToBits` | `(Float32) -> UInt32` | Reinterpret a Float32 as its raw bits |
| `f32FromBits` | `(UInt32) -> Float32` | Reinterpret raw bits as a Float32 |
| `f64ToBits` | `(Float64) -> UInt64` | Reinterpret a Float64 as its raw bits |
| `f64FromBits` | `(UInt64) -> Float64` | Reinterpret raw bits as a Float64 |
| `f32ToBe` | `(Float32) -> UInt8[]` | Encode f32 as 4 big-endian bytes |
| `f32FromBe` | `(UInt8[], Int32) -> Float32` | Read big-endian f32 at offset |
| `f64ToBe` | `(Float64) -> UInt8[]` | Encode f64 as 8 big-endian bytes |
| `f64FromBe` | `(UInt8[], Int32) -> Float64` | Read big-endian f64 at offset |
| `f32ToLe` | `(Float32) -> UInt8[]` | Encode f32 as 4 little-endian bytes |
| `f32FromLe` | `(UInt8[], Int32) -> Float32` | Read little-endian f32 at offset |
| `f64ToLe` | `(Float64) -> UInt8[]` | Encode f64 as 8 little-endian bytes |
| `f64FromLe` | `(UInt8[], Int32) -> Float64` | Read little-endian f64 at offset |

---

### Integer encode / decode

Each `*To*` function returns a freshly allocated `UInt8[]`; each `*From*` reads at a byte offset into an existing buffer.

```lin
val v: UInt16 = 0xBEEF
val b: UInt8[] = u16ToBe(v)   // [0xBE, 0xEF]
u16FromBe(b, 0)               // 0xBEEF

val w: UInt32 = 0x11223344
u32ToLe(w)                    // [0x44, 0x33, 0x22, 0x11]
```

The endianness suffix controls byte order: `Be` writes the most-significant byte first, `Le` writes the least-significant byte first.

---

### Float encode / decode

```lin
val f: Float64 = 3.14159
val bytes: UInt8[] = f64ToBe(f)   // 8 bytes
f64FromBe(bytes, 0)               // 3.14159
```

---

### Float bit reinterpret

`f32ToBits` / `f64ToBits` expose the raw IEEE-754 bit pattern of a float (and the `*FromBits` inverses), without going through a byte buffer. A float's bit pattern cannot be obtained by shift-and-mask, so these are the only way to inspect it.

```lin
f64ToBits(1.0)        // 0x3FF0000000000000
f64FromBits(f64ToBits(1.0))   // 1.0
```

---

### `slice`

Copies the half-open range `[start, end)` into a new `UInt8[]`. `start` and `end` clamp to `[0, length]`. (Re-exported from `std/array`; slicing a `UInt8[]` yields a `UInt8[]`.)

```lin
val buf: UInt8[] = [10, 20, 30, 40, 50]
slice(buf, 1, 4)   // [20, 30, 40]
```
