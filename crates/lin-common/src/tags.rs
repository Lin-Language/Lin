//! Canonical runtime value tags — the SINGLE source of truth shared by the runtime
//! (`lin-runtime`, which stores/reads tagged values) and the compiler backend
//! (`lin-codegen`, which emits the tag bytes). Re-encoding these as magic integers in
//! codegen is what allowed the Float32/Float64 tag disagreement bug; both sides must
//! reference these constants so a tag can never drift from how the runtime reads it.
//!
//! Layout of a boxed TaggedVal: heap-allocated `{ u8 tag, [7]u8 pad, u64 payload }`.
//!
//! Note on float boxing: ALL boxed float scalars are stored as `TAG_FLOAT64` with an
//! f64-bits payload (codegen fpext's a Float32 to f64 before boxing). `TAG_FLOAT32`
//! therefore only ever appears as a *flat-array elem_tag* (dense f32 storage), never on
//! a boxed scalar. Likewise UInt8/16/32 boxed scalars use `TAG_INT64` (zero-extended,
//! always-positive); `TAG_UINT32` is only a flat-array elem_tag, and `TAG_UINT64` is the
//! one unsigned tag that appears on boxed scalars.

pub const TAG_NULL: u8 = 0;
pub const TAG_BOOL: u8 = 1;
pub const TAG_INT32: u8 = 2;
pub const TAG_INT64: u8 = 3;
pub const TAG_FLOAT32: u8 = 4;
pub const TAG_FLOAT64: u8 = 5;
pub const TAG_STR: u8 = 6;
pub const TAG_OBJECT: u8 = 7;
pub const TAG_ARRAY: u8 = 8;
pub const TAG_FUNCTION: u8 = 9;
pub const TAG_UINT8: u8 = 10;
pub const TAG_INT8: u8 = 11;
pub const TAG_UINT16: u8 = 12;
pub const TAG_INT16: u8 = 13;
pub const TAG_UINT64: u8 = 14;
pub const TAG_UINT32: u8 = 15;
pub const TAG_PROMISE: u8 = 16;
pub const TAG_HANDLE: u8 = 17;
pub const TAG_SHARED: u8 = 18;
pub const TAG_STREAM: u8 = 19;
/// Typed index-signature map `{ String: T }` — the hashed `LinMap` container (ADR-055).
pub const TAG_MAP: u8 = 20;
