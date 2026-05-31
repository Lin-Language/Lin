/**
 * lin.h — Public C/C++ header for interoperability with the Lin runtime.
 *
 * Include this header in C or C++ libraries that are called from Lin via
 * `import foreign` (spec §26). It defines the data structures that the Lin
 * compiler uses to pass and return non-primitive values across the FFI boundary.
 *
 * For numeric types (Int8–Int64, UInt8–UInt64, Float32, Float64) and Boolean,
 * Lin uses the standard C ABI: the value is passed directly in a register.
 * No special struct is needed.
 *
 * For String arguments, Lin passes a pointer to a LinString struct. The bytes
 * are UTF-8, not NUL-terminated. Copy the bytes if you need to retain them
 * after the call returns.
 */

#ifndef LIN_H
#define LIN_H

#include <stdint.h>
#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

/**
 * Heap-allocated UTF-8 string value.
 * Owned by the Lin runtime; do not free.
 * The `data` pointer is valid only for the duration of the call.
 */
typedef struct {
    int64_t len;   /* Number of bytes (not code points) */
    uint8_t *data; /* UTF-8 encoded bytes, NOT NUL-terminated */
} LinString;

/**
 * Heap-allocated array value.
 * The element layout depends on the declared element type.
 * For `T[]` where T is a numeric primitive, elements are laid out densely.
 * For other element types, each element is a pointer-sized tagged value.
 */
typedef struct {
    int64_t len;      /* Number of elements */
    int64_t capacity; /* Allocated capacity in elements */
    void   *data;     /* Pointer to element storage */
} LinArray;

#ifdef __cplusplus
}
#endif

#endif /* LIN_H */
