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
/// KEEP-PACKED unboxed sum node (`*SumNode`, `crate::sumnode`) wrapped by-pointer in a 16-byte
/// `TaggedVal` so an unboxed sum value can live in a BOXED record/object FIELD slot WITHOUT
/// materializing to a `LinObject` (the keep-packed-through-record-fields optimization, ADR-062
/// Stage 4 follow-up). A SumNode has the SEALED header shape (`[rc|size|desc|tag|pad|payload]`), NOT
/// the `LinObject` shape, so the slot MUST carry this distinct tag — dispatching its release/retain
/// to `lin_sumnode_*` and its display/equality/serialization/transfer to a MATERIALIZE-on-demand
/// boundary. A `TAG_OBJECT` here would type-confuse `lin_object_release` (offset-4 size read as len).
pub const TAG_SUMNODE: u8 = 21;
/// Arbitrary-precision integer (`std/bignum`, `crate::bignum`). An opaque, immutable, refcounted
/// heap handle wrapping a `num_bigint::BigInt`, in the `TAG_STREAM`/`TAG_SHARED` opaque-handle
/// family. The payload is a `*const BigNumBox`; its RC dispatches through the tag-aware
/// retain/release (the `TAG_BIGNUM` arm calls `lin_bignum_retain_box`/`lin_bignum_release_box`,
/// the final drop freeing the boxed Rust value). The Lin surface aliases `BigInt = Json`, so the
/// handle flows through the universal boxed-TaggedVal* representation like any opaque value.
pub const TAG_BIGNUM: u8 = 22;
/// Exact base-10 fixed-point decimal (`std/decimal`, `crate::decimal`). Same opaque-handle shape
/// as `TAG_BIGNUM`: a `*const DecimalBox` wrapping a `rust_decimal::Decimal`, refcounted via the
/// `TAG_DECIMAL` arm of the tag-aware retain/release.
pub const TAG_DECIMAL: u8 = 23;
/// `TarEntry` — an opaque, generation-stamped handle to a single tar archive entry. Carries a
/// copy of the header metadata (always valid) and a shared cursor into the parent byte stream
/// (valid only while this entry is current). RC-managed like `TAG_STREAM`/`TAG_BIGNUM`: the
/// payload is a `*const TarEntryBox` whose refcount is decremented by `lin_tar_entry_release_box`
/// (the TAG_TAR_ENTRY arm of tag-aware release). Non-transferable across worker threads (shares a
/// live cursor; the checker + transfer path both reject it).
pub const TAG_TAR_ENTRY: u8 = 24;
