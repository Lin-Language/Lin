// mathlib.c — a small C math library used by the Lin FFI example.
//
// Compiled to a committed static archive (libmathlib.a) that `lin build`/`lin test`
// link against. To rebuild after editing:
//   cc -c examples/sdl/clib/mathlib.c -o examples/sdl/clib/mathlib.o
//   ar rcs examples/sdl/clib/libmathlib.a examples/sdl/clib/mathlib.o
//
// All signatures use the C ABI types Lin's FFI maps to (int32_t -> Int32,
// double -> Float64). See crates/lin-runtime/lin.h for the interop header.
#include <stdint.h>
#include <math.h>

int32_t add(int32_t a, int32_t b) {
    return a + b;
}

double square(double x) {
    return x * x;
}

// Clamp x into [lo, hi].
double clampf(double x, double lo, double hi) {
    if (x < lo) return lo;
    if (x > hi) return hi;
    return x;
}

// 2D vector magnitude (hypotenuse).
double magnitude2(double x, double y) {
    return sqrt(x * x + y * y);
}

// Greatest common divisor (Euclid) — an integer-ABI example.
int32_t gcd(int32_t a, int32_t b) {
    if (a < 0) a = -a;
    if (b < 0) b = -b;
    while (b != 0) {
        int32_t t = b;
        b = a % b;
        a = t;
    }
    return a;
}
