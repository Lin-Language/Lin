// mathlib.c — simple C library for the Lin FFI example (examples/ffi_c.lin).
//
// Compile to a static library:
//   cc -c mathlib.c -o mathlib.o
//   ar rcs libmathlib.a mathlib.o
#include <stdint.h>

int32_t add(int32_t a, int32_t b) {
    return a + b;
}

double square(double x) {
    return x * x;
}
