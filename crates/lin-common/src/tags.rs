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
/// RETIRED — no producers; reserved tag value 7. Kept so defensive match arms compile.
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
/// materializing to a `LinMap` (the keep-packed-through-record-fields optimization, ADR-062
/// Stage 4 follow-up). A SumNode has the SEALED header shape (`[rc|size|desc|tag|pad|payload]`), NOT
/// the `LinMap` shape, so the slot MUST carry this distinct tag — dispatching its release/retain
/// to `lin_sumnode_*` and its display/equality/serialization/transfer to a MATERIALIZE-on-demand
/// boundary. Using TAG_MAP here would type-confuse `lin_map_release` (offset-4 reads size, not rc).
pub const TAG_SUMNODE: u8 = 21;
/// Arbitrary-precision integer (`std/bignum`, `crate::bignum`). An opaque, immutable, refcounted
/// heap handle wrapping a `num_bigint::BigInt`, in the `TAG_STREAM`/`TAG_SHARED` opaque-handle
/// family. The payload is a `*const BigNumBox`; its RC dispatches through the tag-aware
/// retain/release (the `TAG_BIGNUM` arm calls `lin_bignum_retain_box`/`lin_bignum_release_box`,
/// the final drop freeing the boxed Rust value). The Lin surface aliases `BigInt = AnyVal`, so the
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
/// STAGE 6a — sealed-record pointer wrapped in a TaggedVal for AnyVal/Json slots. The payload is
/// a `*sealed-struct` (same shape as a `lin_sealed_alloc` allocation: `[u32 rc | u32 size |
/// u64 desc_ptr | payload...]`). The descriptor pointer is recoverable from the struct at offset 8
/// (`SealedDesc`), which also provides field names+kinds for display/equality/field-access. This
/// avoids materialising a `LinMap` on widening (O(1) wrap vs O(n) copy). All dynamic consumers
/// (`lin_tagged_eq` / `lin_tagged_to_string` / `push_json_value` / worker transfer / field-access)
/// read fields via the named descriptor exactly as `TAG_SUMNODE` does via `lin_sumnode_materialize`.
/// A `TAG_RECORD` and a `TAG_MAP` with identical fields compare EQUAL (`lin_tagged_eq`).
pub const TAG_RECORD: u8 = 25;

// ── Named-descriptor field kind codes (NKIND_*) ───────────────────────────────────────────────────
// Shared by `lin-runtime` (sealed.rs) and `lin-codegen` (types.rs). Both crates MUST reference
// these constants and call `nkind_size_align` for field-size derivation — never re-derive locally.
pub const NKIND_INT32: u32 = 1; // Int8/Int16/Int32 → 4 bytes
pub const NKIND_INT64: u32 = 2; // Int64, UInt8/UInt16/UInt32 (zero-extended) → 8 bytes
pub const NKIND_UINT64: u32 = 3; // UInt64 → 8 bytes
pub const NKIND_FLOAT64: u32 = 4; // Float64 → 8 bytes
pub const NKIND_BOOL: u32 = 5; // Bool → 1 byte
pub const NKIND_STRING: u32 = 6; // *LinString heap field → 8-byte pointer
pub const NKIND_ARRAY: u32 = 7; // *LinArray heap field → 8-byte pointer
pub const NKIND_SEALED: u32 = 8; // *sealed-struct heap field → 8-byte pointer
pub const NKIND_MAP: u32 = 9; // *LinMap heap field → 8-byte pointer
/// Float32 field stored as 4 bytes in-struct (physical f32), boxed as TAG_FLOAT64 via fpext.
/// Distinct from NKIND_FLOAT64 so `nkind_size_align` returns (4,4) and `struct_size_from_named_desc`
/// reconstructs the correct 4-byte slot size instead of over-sizing to 8 bytes.
pub const NKIND_FLOAT32: u32 = 10; // Float32 → 4 bytes (fpext to f64 on dynamic read)

/// Returns `(byte_size, alignment)` for a named-descriptor field kind code.
/// This is the SINGLE SOURCE OF TRUTH for the nkind → layout mapping shared by the
/// runtime (`lin_runtime::sealed`) and the codegen (`lin_codegen::codegen::types`).
/// Heap-pointer field kinds (String/Array/Sealed/Map) all occupy an 8-byte pointer slot.
/// Scalar kinds use their natural width (which equals their natural alignment for all cases here).
/// Returns `(8, 8)` for any unknown code as a safe fail-open (pointer-sized slot).
#[inline]
pub const fn nkind_size_align(nkind: u32) -> (u32, u32) {
    match nkind {
        NKIND_INT32   => (4, 4),
        NKIND_INT64   => (8, 8),
        NKIND_UINT64  => (8, 8),
        NKIND_FLOAT64 => (8, 8),
        NKIND_FLOAT32 => (4, 4),
        NKIND_BOOL    => (1, 1),
        // All heap-pointer kinds: String, Array, Sealed, Map — and any future additions.
        _ => (8, 8),
    }
}
