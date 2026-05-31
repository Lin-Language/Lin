# codec — TLV binary codec + bit twiddling

A tiny **TLV (tag–length–value)** binary codec over flat byte buffers, plus
low-level bit helpers. Encodes a list of `{ tag, bytes }` fields into one packed
`UInt8[]` and decodes it back. The wire format, repeated per field, is:

```
tag    : 1 byte   (UInt8)
length : 2 bytes  (UInt16, big-endian) — number of value bytes
value  : <length> bytes
```

## What it demonstrates

- **Flat scalar arrays** (`UInt8[]`, `Int8[]`) with unboxed element storage.
- Big-endian byte packing via `std/bytes` (`u16ToBe`/`u16FromBe`).
- In-place `push` onto a `var` buffer with runtime element coercion.
- Recursion over a buffer with an index cursor (encode and decode).
- Bitwise operators and nibble packing (`<<`, `>>`, `&`, `|`, `^`).
- A **named record alias** `Field = { "tag": Int32, "bytes": Int32[] }` typing
  the `encode` input and `decode` output (`Field[]`).

## Structure

| File | What it is |
| --- | --- |
| `tlv.lin` | `encode(Field[]): UInt8[]` and `decode(UInt8[]): Field[]`. |
| `bits.lin` | Nibble packing, NAL-type extraction, XOR checksum (Int32-typed). |
| `main.lin` | A round-trip demo plus bit-twiddling examples. |
| `codec.test.lin` | Byte-exact encoding, round-trips, bitwise, flat-array typing. |

Note: `appendBytes`'s `src` parameter stays `Json` on purpose — it is called
with both a `Field`'s `Int32[]` value and the `UInt8[]` length prefix, whose
flat element widths differ; a single concrete element type there would misread
one representation (silent data corruption, not a type error).

## Run / Test

```sh
lin run examples/codec/main.lin
lin test examples/codec/
```
