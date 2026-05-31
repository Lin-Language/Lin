# FFI

Call a small C math library from Lin via `import foreign`. The compiled static
archive (`libmathlib.a`) is committed, so `lin run` and `lin test` link against it
directly — no build step needed.

## What it demonstrates

- `import foreign "<path-to-.a>"` to declare external C symbols with Lin types.
- The C ABI type mapping: `int32_t` ↔ `Int32`, `double` ↔ `Float64`
  (`add`, `square`, `clampf`, `magnitude2`, `gcd`).
- The compiler emits LLVM `declare`s and passes the library to the linker; linking
  `magnitude2` (which calls `sqrt`) pulls in libm.
- Foreign bindings are **file-local**: the library is linked only for the file that
  declares `import foreign`, so `main.lin` and `integration.test.lin` each declare
  their own block (you cannot re-export foreign bindings from a wrapper module).

## Structure

- **`mathlib.c`** — the C source (`add`, `square`, `clampf`, `magnitude2`, `gcd`).
- **`libmathlib.a`** — the committed compiled static archive `lin` links against.
- **`main.lin`** — declares the foreign functions and prints each result.
- **`integration.test.lin`** — asserts every foreign function's result end-to-end.

## Run / Test

```bash
lin run examples/ffi/main.lin
lin test examples/ffi/
```

## Rebuilding the C archive

After editing `mathlib.c`, regenerate the committed archive:

```bash
cc -c examples/ffi/mathlib.c -o examples/ffi/mathlib.o
ar rcs examples/ffi/libmathlib.a examples/ffi/mathlib.o
```

See `crates/lin-runtime/lin.h` for the C/C++ interop header.
