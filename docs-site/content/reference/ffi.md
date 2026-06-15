# FFI Reference

Lin provides a C-compatible Foreign Function Interface for calling into native libraries.

## `import foreign` syntax

```lin
import foreign "./libmath.a"
  val sqrt: (Float64) => Float64
  val pow:  (Float64, Float64) => Float64
```

The library path is a string literal on the `import foreign` line. Each subsequent indented line declares one binding: `val name: FunctionType`.

Multiple foreign libraries may be imported in the same file:

```lin
import foreign "./libfoo.a"
  val fooInit: () => Null
  val fooProcess: (String, Int32) => Int32

import foreign "./libbar.a"
  val barVersion: () => String
```

Foreign bindings are used exactly like any other function:

```lin
val result = pow(2.0, 10.0)   // 1024.0
```

## Legal types in foreign signatures

Only a restricted set of types may appear in `import foreign` signatures:

| Lin type | C equivalent |
| --- | --- |
| `Int8` | `int8_t` |
| `Int16` | `int16_t` |
| `Int32` | `int32_t` |
| `Int64` | `int64_t` |
| `UInt8` | `uint8_t` |
| `UInt16` | `uint16_t` |
| `UInt32` | `uint32_t` |
| `UInt64` | `uint64_t` |
| `Float32` | `float` |
| `Float64` | `double` |
| `Boolean` | `uint8_t` (0=false, 1=true) |
| `Null` (return only) | `void` |
| `String` | `LinString` struct (see below) |

`AnyVal`, object types, array types, `Iterator`, and `Function` are not permitted in foreign signatures.

## String passing convention

Lin strings are UTF-8 length-prefixed and do not have a null terminator. They are passed as the `LinString` struct:

```c
typedef struct {
    const uint8_t *ptr;
    size_t         len;
} LinString;
```

The C function receives a `LinString` by value. The bytes are owned by the Lin runtime and must not be freed or stored beyond the call.

Returning `String` from a foreign function is not supported in v1.

## Rust libraries

To expose a Rust crate as an FFI library:

1. Add `crate-type = ["staticlib"]` to `Cargo.toml`.
2. Mark exported functions `#[no_mangle] pub extern "C"`.
3. Use C-compatible types only.

```rust
#[no_mangle]
pub extern "C" fn add_ints(a: i32, b: i32) -> i32 {
    a + b
}
```

```lin
import foreign "./libadd.a"
  val addInts: (Int32, Int32) => Int32
```

## Reserved path: `lin-runtime`

The path `"lin-runtime"` is reserved for stdlib files that declare their dependencies on the Lin runtime library:

```lin
import foreign "lin-runtime"
  val lin_fs_read_file: (String) => AnyVal
```

User code cannot use this path meaningfully — the runtime is always linked automatically.

## Design notes

- **C ABI only.** Lin uses the C calling convention. Rust libraries must expose `extern "C"` functions.
- **Static linking.** Foreign libraries are resolved at `lin build` time. There is no `dlopen`.
- **Trust the declaration.** The compiler trusts the declared types without checking the library. A mismatch between the Lin declaration and the actual C signature is undefined behaviour.
- **Unsafe by nature.** FFI calls bypass the type safety guarantees of the rest of the language.
