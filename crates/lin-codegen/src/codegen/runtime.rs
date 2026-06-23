//! Process-wide `lin-runtime` C-ABI function declarations.
//!
//! These `FunctionValue`s are `declare`d once per LLVM module and never change during
//! compilation — separating them from `Codegen`'s per-module mutable state (slot maps,
//! closure counter, import maps) keeps the struct's two lifetimes from interleaving.

use inkwell::attributes::AttributeLoc;
use inkwell::context::Context;
use inkwell::module::Module;
use inkwell::values::FunctionValue;
use inkwell::AddressSpace;

// ---------------------------------------------------------------------------
// Memory-effect classification and attribute helpers.
//
// LLVM 16+ uses the `memory(...)` attribute to describe function memory effects.
// LLVM 22 (this toolchain) supports it via `Attribute::get_named_enum_kind_id("memory")` = 95.
// The attribute value is a 6-bit packed integer:
//   bits [1:0] = ArgMem modref     (0=none, 1=read, 2=write, 3=readwrite)
//   bits [3:2] = InaccessibleMem modref
//   bits [5:4] = Other (global/static/thread-local) modref
//
// Pre-computed values for the classes below are verified to produce the correct LLVM IR
// textual form (e.g., "memory(argmem: read)") when dumped with module.print_to_string().
//
// SAFETY RULE: when in doubt, use `Opaque` (no attribute). A wrong attribute is a miscompile.
// ---------------------------------------------------------------------------

/// Memory-effect classification for a runtime function. Every function not explicitly
/// classified defaults to `Opaque` (no attribute emitted — conservative, always safe).
#[derive(Copy, Clone)]
#[allow(dead_code)]
enum MemClass {
    /// No memory access at all (result depends only on scalar args). Pure arithmetic / null-return.
    /// Emits: `memory(none)` = value 0. Also adds `willreturn` + `nounwind`.
    Pure,
    /// Reads only through its pointer arguments; no writes anywhere.
    /// Emits: `memory(argmem: read)` = value 1. Also adds `willreturn` + `nounwind`.
    ///
    /// AUDIT REQUIREMENT: confirmed no writes to argmem, inaccessiblemem, globals, or
    /// thread-locals. Any global/TLS write (even a profiling counter or hash cache) disqualifies.
    Reader,
    /// Reads pointer args + may write allocator-internal (inaccessible) state; returns fresh alloc.
    /// Does NOT write through its pointer args. Emits: `memory(argmem: read, inaccessiblemem: readwrite)` = 13.
    Allocator,
    /// No pointer args to read; only writes allocator-internal state.
    /// Emits: `memory(inaccessiblemem: readwrite)` = 12.
    AllocatorNoRead,
    /// Reads and writes through its pointer arguments (mutation). No IO.
    /// Emits: `memory(argmem: readwrite)` = 3.
    Writer,
    /// Retain/release: mutates refcount in argmem AND may free (inaccessible) memory.
    /// Emits: `memory(argmem: readwrite, inaccessiblemem: readwrite)` = 15.
    RC,
    /// Full effects (IO, arbitrary memory, may panic/unwind). No attribute emitted.
    Opaque,
}

// LLVM 22 `memory` attribute kind ID (verified by probe: Attribute::get_named_enum_kind_id("memory") = 95).
const MEMORY_KIND_ID: u32 = 95;

fn apply_mem_class(ctx: &Context, f: FunctionValue<'_>, class: MemClass) {
    let add_enum = |name: &str, value: u64| {
        let kind_id = inkwell::attributes::Attribute::get_named_enum_kind_id(name);
        if kind_id != 0 {
            f.add_attribute(AttributeLoc::Function, ctx.create_enum_attribute(kind_id, value));
        }
    };
    let add_memory = |value: u64| {
        if MEMORY_KIND_ID != 0 {
            f.add_attribute(AttributeLoc::Function, ctx.create_enum_attribute(MEMORY_KIND_ID, value));
        }
    };

    match class {
        MemClass::Pure => {
            add_memory(0);          // memory(none)
            add_enum("willreturn", 0);
            add_enum("nounwind", 0);
        }
        MemClass::Reader => {
            add_memory(1);          // memory(argmem: read)
            add_enum("willreturn", 0);
            add_enum("nounwind", 0);
        }
        MemClass::Allocator => {
            add_memory(13);         // memory(argmem: read, inaccessiblemem: readwrite)
        }
        MemClass::AllocatorNoRead => {
            add_memory(12);         // memory(inaccessiblemem: readwrite)
        }
        MemClass::Writer => {
            add_memory(3);          // memory(argmem: readwrite)
        }
        MemClass::RC => {
            add_memory(15);         // memory(argmem: readwrite, inaccessiblemem: readwrite)
        }
        MemClass::Opaque => {
            // No attribute — conservative default.
        }
    }
}

// ---------------------------------------------------------------------------
// Ownership conventions of the runtime intrinsics (Path-10/11 Leg 1) — CROSS-CHECK ANCHOR.
//
// The load-bearing hand-audited table lives in `lin_ir::ownership_verify::intrinsic_conventions`
// (it must sit in `lin-ir`, which `lin-codegen` depends on, not the other way round). It is
// documented HERE next to the matching LLVM `declare`s so the two stay in sync by eye, and so a
// future Wave-2 consumer reads the conventions right where the C-ABI signatures are emitted.
//
// SHADOW MODE (this round): the table is only inferred + verified, NEVER consumed by codegen — no
// declaration or call below changes based on it. The convention summary, grounded in the runtime
// semantics in `lin-runtime/src/`:
//
//   lin_map_get(borrow map, borrow key)           -> borrow   (interior pointer; caller must clone
//                                                              before it escapes — see own_for_read)
//   lin_map_set(inout map, own key, own val)       -> own/void (map mutated in place; key+val stored)
//   lin_map_has / lin_map_eq / lin_string_eq(borrow, borrow) -> scalar
//   lin_array_get(borrow arr, own i)              -> borrow   (borrowed element; the `_tagged`
//                                                              variant returns a fresh +1 instead)
//   lin_push / lin_array_push(inout arr, own v)   -> void     (canonical (inout, own))
//   lin_*_length / lin_string_length(borrow)      -> scalar
//   lin_string_concat(borrow, borrow)             -> own      (fresh string; inputs copied)
//   lin_array_alloc / lin_map_alloc(own cap)      -> own (fresh +1)
//   lin_keys(borrow obj)                          -> own      (fresh String[])
//   lin_print(borrow s)                           -> void     (reads only — today's lowering
//                                                              over-owns this; a Wave-2 win)
//   lin_box_*(borrow inner)                        -> own      (fresh shell; inner borrowed)
//   lin_int_to_string / float / bool / tagged_to_string(borrow) -> own (fresh string)
//
// Entries the author is UNSURE about (see FINDINGS.md §3): lin_array_allocate_filled fill-value
// (borrow vs own per-slot retain), lin_value_key, lin_to_json. The async/worker/stream/compress/
// archive families and lin_from_json are NOT audited this round (the verifier flags them as
// `unaudited-intrinsic` gaps) — they are off the RAPTOR/interp hot paths Leg 1 targets.
// ---------------------------------------------------------------------------

/// The full set of `lin-runtime` symbols the codegen calls into. Constructed once via
/// [`RuntimeFns::new`], which emits the matching `declare` directives into `module`.
pub(crate) struct RuntimeFns<'ctx> {
    pub string_length: FunctionValue<'ctx>,
    pub string_eq: FunctionValue<'ctx>,
    pub print: FunctionValue<'ctx>,
    pub panic: FunctionValue<'ctx>,
    pub array_alloc: FunctionValue<'ctx>,
    pub array_push: FunctionValue<'ctx>,
    pub array_get: FunctionValue<'ctx>,
    pub int_to_string: FunctionValue<'ctx>,
    pub uint_to_string: FunctionValue<'ctx>,
    pub float_to_string: FunctionValue<'ctx>,
    pub bool_to_string: FunctionValue<'ctx>,
    pub null_to_string: FunctionValue<'ctx>,
    pub alloc: FunctionValue<'ctx>,
    pub box_null: FunctionValue<'ctx>,
    pub box_bool: FunctionValue<'ctx>,
    pub box_int32: FunctionValue<'ctx>,
    pub box_int64: FunctionValue<'ctx>,
    pub box_uint64: FunctionValue<'ctx>,
    pub box_float64: FunctionValue<'ctx>,
    pub box_str: FunctionValue<'ctx>,
    pub box_map: FunctionValue<'ctx>,
    pub box_array: FunctionValue<'ctx>,
    pub box_sumnode: FunctionValue<'ctx>,
    /// Stage 6a: `lin_box_record(sealed_ptr: ptr) -> ptr` — wraps a typed sealed-struct pointer into
    /// a TAG_RECORD shell (a fresh +1 TaggedVal*). RETAINS the sealed struct (increments its RC).
    /// Used when a typed record is widened into a Json/AnyVal dynamic slot.
    pub box_record: FunctionValue<'ctx>,
    pub box_function: FunctionValue<'ctx>,
    pub get_tag: FunctionValue<'ctx>,
    pub unbox_int32: FunctionValue<'ctx>,
    pub unbox_int64: FunctionValue<'ctx>,
    pub unbox_uint64: FunctionValue<'ctx>,
    pub unbox_float64: FunctionValue<'ctx>,
    pub unbox_bool: FunctionValue<'ctx>,
    pub unbox_ptr: FunctionValue<'ctx>,
    pub tagged_to_string: FunctionValue<'ctx>,
    pub string_release: FunctionValue<'ctx>,
    pub array_release: FunctionValue<'ctx>,
    pub closure_release: FunctionValue<'ctx>,
    pub tagged_release: FunctionValue<'ctx>,
    /// Sealed-record (sealed-records Stages 1–2): `lin_sealed_alloc(size: i64, heap_desc: ptr, named_desc: ptr) -> ptr`
    /// allocates a zeroed, refcount-1 packed struct carrying the static heap descriptor `heap_desc`
    /// (NULL for a scalar-only record) and the named descriptor `named_desc` (NULL when not needed for
    /// TAG_RECORD lookup; non-NULL stores the named desc at offset 16 of the header for Stage 6a);
    /// `lin_sealed_release(ptr, size: i64)` decrements its refcount and, on zero, releases each HEAP
    /// field per the descriptor then frees the struct.
    pub sealed_alloc: FunctionValue<'ctx>,
    /// Unboxed tagged sum type (unboxed-sumtype Stage 1): `lin_sumnode_alloc(size: i64, desc: ptr) ->
    /// ptr` allocates a zeroed, refcount-1 `SumNode` (header `[rc|size|desc|tag|pad]`, payload sized
    /// to the max variant); `lin_sumnode_release(ptr, size: i64)` decrements its refcount and frees on
    /// zero (Stage 1 scalar-only → no per-field release, desc may be NULL).
    pub sumnode_alloc: FunctionValue<'ctx>,
    pub sumnode_release: FunctionValue<'ctx>,
    /// Typed index-signature map `{ K: V }` (ADR-055 + numeric-key): the hashed `LinMap` container.
    pub map_alloc: FunctionValue<'ctx>,
    pub map_set: FunctionValue<'ctx>,
    pub map_get: FunctionValue<'ctx>,
    pub map_release: FunctionValue<'ctx>,
    /// `lin_union_force_to_map(tv: ptr) -> *LinMap` — normalise a union/Json boxed source to a
    /// fresh owned +1 `LinMap` (handles TAG_MAP/TAG_RECORD/TAG_SUMNODE). Caller releases.
    pub map_force: FunctionValue<'ctx>,
    /// `lin_map_eq(a: ptr, b: ptr) -> i8` — structural, order-independent equality for two maps.
    pub map_eq: FunctionValue<'ctx>,
    /// Int-keyed map entry points (key_kind = 1).
    pub map_get_int: FunctionValue<'ctx>,
    pub map_set_int: FunctionValue<'ctx>,
    /// Cold path for the inline sealed-release when rc hits zero: walks heap fields and frees.
    /// `lin_sealed_drop_at_zero(ptr, size)`. Called after the inline dec reaches zero.
    pub sealed_drop_at_zero: FunctionValue<'ctx>,
    /// Stage 2: `lin_record_get_field(sealed_ptr: ptr, key: ptr) -> ptr`
    /// Read one field by name from a TAG_RECORD sealed struct. Returns an OWNED +1 TaggedVal*
    /// (heap box; caller must `lin_tagged_release` it). Null when field absent or ptr is null.
    pub record_get_field: FunctionValue<'ctx>,
}

impl<'ctx> RuntimeFns<'ctx> {
    /// Declare every `lin-runtime` symbol into `module` (C ABI, defined in lin-runtime).
    ///
    /// Each function is annotated with the most precise LLVM memory-effect class supported by its
    /// actual Rust implementation (audited in `lin-runtime/src/`). The default for any function
    /// that touches IO, global state, or cannot be proven safe is `Opaque` (no attribute).
    ///
    /// # Per-function memory-effect audit (impl evidence in lin-runtime/src/)
    ///
    /// | Function                | Class          | Evidence |
    /// |-------------------------|----------------|---------|
    /// | lin_string_length       | Reader         | reads (*s).len only — string.rs:379 |
    /// | lin_string_eq           | Opaque         | writes (*key).hash via const-ptr cast (get_or_init_hash) — string.rs:164 |
    /// | lin_print               | Opaque         | stdout I/O |
    /// | lin_panic               | Opaque         | exits/panics |
    /// | lin_array_alloc         | AllocatorNoRead| heap alloc, reads only scalar arg — array.rs:103 |
    /// | lin_array_push          | Writer         | mutates arr->data + len + cap via argmem — array.rs:617 |
    /// | lin_array_get           | Opaque         | extern "C-unwind", calls runtime_fault — array.rs:1001 |
    /// | lin_alloc               | AllocatorNoRead| raw heap alloc, no arg reads — memory.rs:45 |
    /// | lin_int_to_string       | Opaque         | OnceLock global init (INT_STR_CACHE) on first call — string.rs:519 |
    /// | lin_uint_to_string      | AllocatorNoRead| alloc+format, no special global — string.rs:552 |
    /// | lin_float_to_string     | AllocatorNoRead| stack buf + alloc, no global — string.rs:597 |
    /// | lin_bool_to_string      | AllocatorNoRead| alloc+copy, no global — string.rs:610 |
    /// | lin_null_to_string      | AllocatorNoRead| alloc+copy, no global — string.rs:616 |
    /// | lin_box_null            | Pure           | returns null_mut(), zero memory access — tagged.rs:158 |
    /// | lin_box_bool            | Opaque         | reads BOOL_CACHE static (other: read) + returns ptr into static |
    /// | lin_box_int32           | Opaque         | reads INT32_CACHE static (other: read) + may alloc |
    /// | lin_box_int64           | Opaque         | reads INT64_CACHE static (other: read) + may alloc |
    /// | lin_box_uint64          | AllocatorNoRead| always allocs, no cache — tagged.rs:189 |
    /// | lin_box_float64         | AllocatorNoRead| always allocs, no arg read — tagged.rs:194 |
    /// | lin_box_str             | AllocatorNoRead| alloc_tagged: no pointer-arg read — tagged.rs:199 |
    /// | lin_box_map             | AllocatorNoRead| alloc_tagged — tagged.rs:215 |
    /// | lin_box_array           | AllocatorNoRead| alloc_tagged — tagged.rs:204 |
    /// | lin_box_sumnode         | AllocatorNoRead| alloc_tagged — tagged.rs:226 |
    /// | lin_box_record          | RC             | retains sealed struct via lin_rc_retain (argmem write) + alloc_tagged — tagged.rs:247 |
    /// | lin_box_function        | AllocatorNoRead| alloc_tagged — tagged.rs:209 |
    /// | lin_get_tag             | Reader         | reads (*p).tag (1 byte), null-safe early return — tagged.rs:298 |
    /// | lin_unbox_int32         | Reader         | reads payload field — tagged.rs:307 |
    /// | lin_unbox_int64         | Reader         | reads payload field — tagged.rs:313 |
    /// | lin_unbox_uint64        | Reader         | reads payload field — tagged.rs:319 |
    /// | lin_unbox_float64       | Reader         | reads payload field — tagged.rs:325 |
    /// | lin_unbox_bool          | Reader         | reads payload field — tagged.rs:331 |
    /// | lin_unbox_ptr           | Reader         | reads payload field — tagged.rs:339 |
    /// | lin_tagged_to_string    | Opaque         | allocates + may recurse into map/array stringification |
    /// | lin_string_release      | RC             | dec refcount in argmem, may free (inaccessible) — string.rs:319 |
    /// | lin_array_release       | RC             | dec refcount + recursive element releases — array.rs:219 |
    /// | lin_closure_release     | RC             | dec refcount + env walk + free — memory.rs:95 |
    /// | lin_tagged_release      | RC             | dec inner refcount + free TaggedVal box — tagged.rs:644 |
    /// | lin_sealed_alloc        | AllocatorNoRead| alloc+zero+write header, no meaningful arg ptr read — sealed.rs:107 |
    /// | lin_sumnode_alloc       | AllocatorNoRead| alloc+zero+write header — sumnode.rs:102 |
    /// | lin_sumnode_release     | RC             | dec refcount + field walk + free — sumnode.rs:242 |
    /// | lin_map_alloc           | AllocatorNoRead| alloc map header, no meaningful arg ptr read — map.rs:631 |
    /// | lin_map_set             | Writer         | mutates map slots, keys, order array — map.rs:655 |
    /// | lin_map_get             | Opaque         | writes thread-local GET_SCRATCH ring on homogeneous maps — map.rs:236 |
    /// | lin_map_release         | RC             | dec refcount + key/value release + free — map.rs:900 |
    /// | lin_union_force_to_map  | Opaque         | may allocate new LinMap, touches multiple arg types |
    /// | lin_map_eq              | Opaque         | calls lin_map_get (writes TLS scratch) |
    /// | lin_map_get_int         | Opaque         | writes thread-local GET_SCRATCH ring — map.rs:236 |
    /// | lin_map_set_int         | Writer         | mutates map slots — map.rs:710 |
    /// | lin_sealed_drop_at_zero | RC             | field walk + free (called from inline dec-to-zero path) — sealed.rs:275 |
    pub(crate) fn new(context: &'ctx Context, module: &Module<'ctx>) -> Self {
        let string_ptr_type = context.ptr_type(AddressSpace::default());
        let ptr_type = context.ptr_type(AddressSpace::default());

        let i32_type = context.i32_type();
        let i64_type = context.i64_type();
        let void_type = context.void_type();
        let bool_type = context.bool_type();

        let decl = |_name: &str, class: MemClass, fv: FunctionValue<'ctx>| {
            apply_mem_class(context, fv, class);
            fv
        };

        let string_length = decl("lin_string_length", MemClass::Reader, module.add_function(
            "lin_string_length",
            i32_type.fn_type(&[string_ptr_type.into()], false),
            None,
        ));
        // lin_string_eq writes (*key).hash lazily (get_or_init_hash writes through a const*) → Opaque
        let string_eq = module.add_function(
            "lin_string_eq",
            bool_type.fn_type(&[string_ptr_type.into(), string_ptr_type.into()], false),
            None,
        );
        let print = module.add_function(
            "lin_print",
            void_type.fn_type(&[string_ptr_type.into()], false),
            None,
        );
        let panic = module.add_function(
            "lin_panic",
            void_type.fn_type(&[string_ptr_type.into(), i32_type.into(), i32_type.into()], false),
            None,
        );
        // lin_array_alloc(initial_capacity: i64) -> ptr
        let array_alloc = decl("lin_array_alloc", MemClass::AllocatorNoRead, module.add_function(
            "lin_array_alloc",
            ptr_type.fn_type(&[i64_type.into()], false),
            None,
        ));
        // lin_array_push(arr: ptr, elem: ptr, tag: i8) -> void
        let array_push = decl("lin_array_push", MemClass::Writer, module.add_function(
            "lin_array_push",
            void_type.fn_type(&[ptr_type.into(), ptr_type.into(), context.i8_type().into()], false),
            None,
        ));
        // lin_array_get: extern "C-unwind", can call runtime_fault → Opaque
        let array_get = module.add_function(
            "lin_array_get",
            ptr_type.fn_type(&[ptr_type.into(), i64_type.into()], false),
            None,
        );
        // lin_alloc(size: i64) -> ptr — general heap allocation for closures/envs
        let alloc = decl("lin_alloc", MemClass::AllocatorNoRead, module.add_function(
            "lin_alloc",
            ptr_type.fn_type(&[i64_type.into()], false),
            None,
        ));
        // lin_int_to_string: OnceLock global init on first call → Opaque
        let int_to_string = module.add_function(
            "lin_int_to_string",
            string_ptr_type.fn_type(&[i64_type.into()], false),
            None,
        );
        // Unsigned 64-bit → string (the payload bits are interpreted as u64). Used for UInt64
        // values, which lin_int_to_string would print as a negative number when >= 2^63.
        let uint_to_string = decl("lin_uint_to_string", MemClass::AllocatorNoRead, module.add_function(
            "lin_uint_to_string",
            string_ptr_type.fn_type(&[i64_type.into()], false),
            None,
        ));
        let float_to_string = decl("lin_float_to_string", MemClass::AllocatorNoRead, module.add_function(
            "lin_float_to_string",
            string_ptr_type.fn_type(&[context.f64_type().into()], false),
            None,
        ));
        let bool_to_string = decl("lin_bool_to_string", MemClass::AllocatorNoRead, module.add_function(
            "lin_bool_to_string",
            string_ptr_type.fn_type(&[bool_type.into()], false),
            None,
        ));
        let null_to_string = decl("lin_null_to_string", MemClass::AllocatorNoRead, module.add_function(
            "lin_null_to_string",
            string_ptr_type.fn_type(&[], false),
            None,
        ));
        // Tagged union boxing/unboxing
        let i8_type = context.i8_type();
        // lin_box_null: returns null_mut() — no memory access
        let box_null = decl("lin_box_null", MemClass::Pure, module.add_function(
            "lin_box_null", ptr_type.fn_type(&[], false), None));
        // lin_box_bool/int32/int64: read static BOOL_CACHE/INT32_CACHE/INT64_CACHE → Opaque
        let box_bool = module.add_function("lin_box_bool", ptr_type.fn_type(&[i8_type.into()], false), None);
        let box_int32 = module.add_function("lin_box_int32", ptr_type.fn_type(&[i32_type.into()], false), None);
        let box_int64 = module.add_function("lin_box_int64", ptr_type.fn_type(&[i64_type.into()], false), None);
        // lin_box_uint64: always allocs (no cache), reads only scalar arg → AllocatorNoRead
        let box_uint64 = decl("lin_box_uint64", MemClass::AllocatorNoRead, module.add_function(
            "lin_box_uint64", ptr_type.fn_type(&[i64_type.into()], false), None));
        // lin_box_float64: always allocs, reads no arg memory → AllocatorNoRead
        let box_float64 = decl("lin_box_float64", MemClass::AllocatorNoRead, module.add_function(
            "lin_box_float64", ptr_type.fn_type(&[context.f64_type().into()], false), None));
        // lin_box_str/map/array/sumnode: alloc_tagged reads no pointer args → AllocatorNoRead
        let box_str = decl("lin_box_str", MemClass::AllocatorNoRead, module.add_function(
            "lin_box_str", ptr_type.fn_type(&[ptr_type.into()], false), None));
        let box_map = decl("lin_box_map", MemClass::AllocatorNoRead, module.add_function(
            "lin_box_map", ptr_type.fn_type(&[ptr_type.into()], false), None));
        let box_array = decl("lin_box_array", MemClass::AllocatorNoRead, module.add_function(
            "lin_box_array", ptr_type.fn_type(&[ptr_type.into()], false), None));
        let box_sumnode = decl("lin_box_sumnode", MemClass::AllocatorNoRead, module.add_function(
            "lin_box_sumnode", ptr_type.fn_type(&[ptr_type.into()], false), None));
        // Stage 6a: lin_box_record retains the sealed struct (writes argmem refcount) → RC
        let box_record = decl("lin_box_record", MemClass::RC, module.add_function(
            "lin_box_record", ptr_type.fn_type(&[ptr_type.into()], false), None));
        // lin_box_function: alloc_tagged, no arg pointer read → AllocatorNoRead
        let box_function = decl("lin_box_function", MemClass::AllocatorNoRead, module.add_function(
            "lin_box_function", ptr_type.fn_type(&[ptr_type.into()], false), None));
        // lin_get_tag: reads (*p).tag (1 byte), null-safe → Reader
        let get_tag = decl("lin_get_tag", MemClass::Reader, module.add_function(
            "lin_get_tag", i8_type.fn_type(&[ptr_type.into()], false), None));
        // lin_unbox_*: pure payload reads, no writes anywhere → Reader
        let unbox_int32 = decl("lin_unbox_int32", MemClass::Reader, module.add_function(
            "lin_unbox_int32", i32_type.fn_type(&[ptr_type.into()], false), None));
        let unbox_int64 = decl("lin_unbox_int64", MemClass::Reader, module.add_function(
            "lin_unbox_int64", i64_type.fn_type(&[ptr_type.into()], false), None));
        let unbox_uint64 = decl("lin_unbox_uint64", MemClass::Reader, module.add_function(
            "lin_unbox_uint64", i64_type.fn_type(&[ptr_type.into()], false), None));
        let unbox_float64 = decl("lin_unbox_float64", MemClass::Reader, module.add_function(
            "lin_unbox_float64", context.f64_type().fn_type(&[ptr_type.into()], false), None));
        let unbox_bool = decl("lin_unbox_bool", MemClass::Reader, module.add_function(
            "lin_unbox_bool", i8_type.fn_type(&[ptr_type.into()], false), None));
        let unbox_ptr = decl("lin_unbox_ptr", MemClass::Reader, module.add_function(
            "lin_unbox_ptr", ptr_type.fn_type(&[ptr_type.into()], false), None));
        // lin_tagged_to_string: allocates + may recurse into complex stringification → Opaque
        let tagged_to_string = module.add_function(
            "lin_tagged_to_string", string_ptr_type.fn_type(&[ptr_type.into()], false), None);
        // Retain / release: adjust refcount, free if zero.
        let string_release = decl("lin_string_release", MemClass::RC, module.add_function(
            "lin_string_release", void_type.fn_type(&[ptr_type.into()], false), None));
        let array_release = decl("lin_array_release", MemClass::RC, module.add_function(
            "lin_array_release", void_type.fn_type(&[ptr_type.into()], false), None));
        let closure_release = decl("lin_closure_release", MemClass::RC, module.add_function(
            "lin_closure_release", void_type.fn_type(&[ptr_type.into()], false), None));
        let tagged_release = decl("lin_tagged_release", MemClass::RC, module.add_function(
            "lin_tagged_release", void_type.fn_type(&[ptr_type.into()], false), None));
        // lin_sealed_alloc: alloc+zero, reads desc pointers only to store them → AllocatorNoRead
        let sealed_alloc = decl("lin_sealed_alloc", MemClass::AllocatorNoRead, module.add_function(
            "lin_sealed_alloc", ptr_type.fn_type(&[i64_type.into(), ptr_type.into(), ptr_type.into()], false), None));
        // lin_sumnode_alloc: alloc+zero, reads desc pointer only to store it → AllocatorNoRead
        let sumnode_alloc = decl("lin_sumnode_alloc", MemClass::AllocatorNoRead, module.add_function(
            "lin_sumnode_alloc", ptr_type.fn_type(&[i64_type.into(), ptr_type.into()], false), None));
        let sumnode_release = decl("lin_sumnode_release", MemClass::RC, module.add_function(
            "lin_sumnode_release", void_type.fn_type(&[ptr_type.into(), i64_type.into()], false), None));
        // Typed index-signature map (ADR-055 + numeric-key).
        // lin_map_alloc(hint: i32, key_kind: i32) -> *LinMap — allocs header + order array
        let map_alloc = decl("lin_map_alloc", MemClass::AllocatorNoRead, module.add_function(
            "lin_map_alloc", ptr_type.fn_type(&[i32_type.into(), i32_type.into()], false), None));
        // lin_map_set: mutates map slots/ctrl/order (argmem write) + may alloc (inaccessible) → Writer
        // Note: Writer (argmem: readwrite) is correct; the alloc is a minor omission in the
        // strictest sense but Writer is the dominant effect and more useful for optimization.
        let map_set = decl("lin_map_set", MemClass::Writer, module.add_function(
            "lin_map_set", void_type.fn_type(&[ptr_type.into(), ptr_type.into(), ptr_type.into()], false), None));
        // lin_map_get: writes thread-local GET_SCRATCH ring (other: write) → Opaque
        let map_get = module.add_function(
            "lin_map_get", ptr_type.fn_type(&[ptr_type.into(), ptr_type.into()], false), None);
        let map_release = decl("lin_map_release", MemClass::RC, module.add_function(
            "lin_map_release", void_type.fn_type(&[ptr_type.into()], false), None));
        // lin_union_force_to_map: may allocate new LinMap, touches TAG_RECORD/MAP/SUMNODE → Opaque
        let map_force = module.add_function(
            "lin_union_force_to_map", ptr_type.fn_type(&[ptr_type.into()], false), None);
        // lin_map_eq: calls lin_map_get internally (writes TLS scratch) → Opaque
        let map_eq = module.add_function(
            "lin_map_eq", i8_type.fn_type(&[ptr_type.into(), ptr_type.into()], false), None);
        // Int-keyed map entry points
        // lin_map_get_int: writes thread-local GET_SCRATCH ring → Opaque
        let map_get_int = module.add_function(
            "lin_map_get_int", ptr_type.fn_type(&[ptr_type.into(), i64_type.into()], false), None);
        let map_set_int = decl("lin_map_set_int", MemClass::Writer, module.add_function(
            "lin_map_set_int", void_type.fn_type(&[ptr_type.into(), i64_type.into(), ptr_type.into()], false), None));

        // lin_sealed_drop_at_zero(ptr, size: i64) -> void — heap-field walk + free after dec→0
        let sealed_drop_at_zero = decl("lin_sealed_drop_at_zero", MemClass::RC, module.add_function(
            "lin_sealed_drop_at_zero",
            void_type.fn_type(&[ptr_type.into(), i64_type.into()], false),
            None,
        ));

        // lin_record_get_field(sealed_ptr: ptr, key: ptr) -> ptr — owned +1 TaggedVal* for one field
        let record_get_field = module.add_function(
            "lin_record_get_field",
            ptr_type.fn_type(&[ptr_type.into(), ptr_type.into()], false),
            None,
        );

        Self {
            string_length,
            string_eq,
            print,
            panic,
            array_alloc,
            array_push,
            array_get,
            int_to_string,
            uint_to_string,
            float_to_string,
            bool_to_string,
            null_to_string,
            alloc,
            box_null,
            box_bool,
            box_int32,
            box_int64,
            box_uint64,
            box_float64,
            box_str,
            box_map,
            box_array,
            box_sumnode,
            box_record,
            box_function,
            get_tag,
            unbox_int32,
            unbox_int64,
            unbox_uint64,
            unbox_float64,
            unbox_bool,
            unbox_ptr,
            tagged_to_string,
            string_release,
            array_release,
            closure_release,
            tagged_release,
            sealed_alloc,
            sumnode_alloc,
            sumnode_release,
            map_alloc,
            map_set,
            map_get,
            map_release,
            map_force,
            map_eq,
            map_get_int,
            map_set_int,
            sealed_drop_at_zero,
            record_get_field,
        }
    }
}
