# std/ffi

std/ffi — raw-memory + C-string helpers for richer FFI (prototype keystone).

These wrap the `lin_ffi_*` runtime symbols (crates/lin-runtime/src/ffi.rs). They let pure Lin
marshal `String` arguments into NUL-terminated C strings, allocate scratch/out-param buffers,
and read/write fixed-layout structs returned through a C `void*`.

`Ptr` is a prototype pointer type aliased to Int64 (ABI-identical to a 64-bit void*). It stays a
scalar, so a `Ptr` value is never refcounted and can be passed straight back into another
foreign function.

LIFETIME: prefer `withCstr` — it allocates a C string, runs your callback with it, then frees
it, so you never leak. Use the bare `cstr`/`free` pair only when the C API RETAINS the pointer
and you must manage its lifetime explicitly. (`cstr` alone does not free — see ffi.rs.)

## Reference

#### `cstr`

```lin
val cstr = (s: String): Ptr
```

Allocate a NUL-terminated C-string copy of `s`.
- **`s`** — the string to copy into C memory.
- **Returns** a `Ptr` to the buffer. NOT freed for you — pair with `free` (or prefer `withCstr`)
  unless the C API takes ownership of the pointer.

#### `withCstr`

```lin
val withCstr = <T>(s: String, body: (Ptr)
```

Run `body` with a scoped, auto-freed C-string copy of `s`. The recommended, leak-free idiom for
the common "C copies the string during the call" case, e.g.
  withCstr(title, (p) => SDL_CreateWindow(p, 320, 240, 0))
- **`s`** — the string to copy into C memory for the duration of `body`.
- **`body`** — callback receiving the `Ptr` to the C string.
- **Returns** whatever `body` returned. The buffer is freed after `body` returns.
  CAVEAT: Lin has no try/finally, so a faulting callback leaks the buffer (accepted for this
  prototype).

#### `alloc`

```lin
val alloc = (n: Int64): Ptr
```

Allocate `n` bytes of raw scratch memory.
- **`n`** — the byte count to allocate.
- **Returns** a `Ptr` to the buffer; release with `free`.

#### `free`

```lin
val free = (p: Ptr): Null
```

Free a buffer previously returned by `alloc` or `cstr`.
- **`p`** — the pointer to free.

#### `peekU8`

```lin
val peekU8 = (p: Ptr, off: Int64): UInt8
```

Read a primitive at byte offset `off` from pointer `p`. One reader per width/type; each returns
the value loaded at `p + off`.

#### `peekU16`

```lin
val peekU16 = (p: Ptr, off: Int64): UInt16
```


#### `peekU32`

```lin
val peekU32 = (p: Ptr, off: Int64): UInt32
```


#### `peekU64`

```lin
val peekU64 = (p: Ptr, off: Int64): UInt64
```


#### `peekI32`

```lin
val peekI32 = (p: Ptr, off: Int64): Int32
```


#### `peekI64`

```lin
val peekI64 = (p: Ptr, off: Int64): Int64
```


#### `peekF32`

```lin
val peekF32 = (p: Ptr, off: Int64): Float32
```


#### `peekF64`

```lin
val peekF64 = (p: Ptr, off: Int64): Float64
```


#### `peekPtr`

```lin
val peekPtr = (p: Ptr, off: Int64): Ptr
```


#### `pokeU8`

```lin
val pokeU8 = (p: Ptr, off: Int64, v: UInt8): Null
```

Write a primitive `v` at byte offset `off` to pointer `p`. One writer per width/type; each stores
`v` at `p + off`.

#### `pokeU16`

```lin
val pokeU16 = (p: Ptr, off: Int64, v: UInt16): Null
```


#### `pokeU32`

```lin
val pokeU32 = (p: Ptr, off: Int64, v: UInt32): Null
```


#### `pokeU64`

```lin
val pokeU64 = (p: Ptr, off: Int64, v: UInt64): Null
```


#### `pokeI32`

```lin
val pokeI32 = (p: Ptr, off: Int64, v: Int32): Null
```


#### `pokeI64`

```lin
val pokeI64 = (p: Ptr, off: Int64, v: Int64): Null
```


#### `pokeF32`

```lin
val pokeF32 = (p: Ptr, off: Int64, v: Float32): Null
```


#### `pokeF64`

```lin
val pokeF64 = (p: Ptr, off: Int64, v: Float64): Null
```


#### `pokePtr`

```lin
val pokePtr = (p: Ptr, off: Int64, v: Ptr): Null
```

