//! Process-wide `lin-runtime` C-ABI function declarations.
//!
//! These `FunctionValue`s are `declare`d once per LLVM module and never change during
//! compilation — separating them from `Codegen`'s per-module mutable state (slot maps,
//! closure counter, import maps) keeps the struct's two lifetimes from interleaving.

use inkwell::context::Context;
use inkwell::module::Module;
use inkwell::values::FunctionValue;
use inkwell::AddressSpace;

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
//   lin_object_get(borrow obj, borrow key)        -> borrow   (interior pointer; caller must clone
//                                                              before it escapes — see own_for_read)
//   lin_object_set(inout obj, own key, own val)   -> own/void (obj mutated in place; key+val stored)
//   lin_object_has / lin_object_eq / lin_string_eq(borrow, borrow) -> scalar
//   lin_array_get(borrow arr, own i)              -> borrow   (borrowed element; the `_tagged`
//                                                              variant returns a fresh +1 instead)
//   lin_push / lin_array_push(inout arr, own v)   -> void     (canonical (inout, own))
//   lin_*_length / lin_string_length(borrow)      -> scalar
//   lin_string_concat(borrow, borrow)             -> own      (fresh string; inputs copied)
//   lin_array_alloc / lin_object_alloc / lin_map_alloc(own cap) -> own (fresh +1)
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
    /// Interns a string *literal*: returns a cached, immortal LinString for the given `@str_data`
    /// global, allocating it once for the whole run. Emitted by `compile_string_lit` for all
    /// compile-time string constants. Genuine runtime string creation (concat, interpolation, fs
    /// reads, etc.) calls `lin_string_from_bytes` directly via `get_or_declare_fn`, not this struct.
    pub string_literal: FunctionValue<'ctx>,
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
    pub box_object: FunctionValue<'ctx>,
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
    pub object_alloc: FunctionValue<'ctx>,
    pub object_set: FunctionValue<'ctx>,
    pub object_set_fresh: FunctionValue<'ctx>,
    pub object_get: FunctionValue<'ctx>,
    pub object_eq: FunctionValue<'ctx>,
    pub tagged_to_string: FunctionValue<'ctx>,
    pub rc_retain: FunctionValue<'ctx>,
    pub string_release: FunctionValue<'ctx>,
    pub array_release: FunctionValue<'ctx>,
    pub object_release: FunctionValue<'ctx>,
    pub closure_release: FunctionValue<'ctx>,
    pub tagged_release: FunctionValue<'ctx>,
    /// Sealed-record (sealed-records Stages 1–2): `lin_sealed_alloc(size: i64, heap_desc: ptr, named_desc: ptr) -> ptr`
    /// allocates a zeroed, refcount-1 packed struct carrying the static heap descriptor `heap_desc`
    /// (NULL for a scalar-only record) and the named descriptor `named_desc` (NULL when not needed for
    /// TAG_RECORD lookup; non-NULL stores the named desc at offset 16 of the header for Stage 6a);
    /// `lin_sealed_release(ptr, size: i64)` decrements its refcount and, on zero, releases each HEAP
    /// field per the descriptor then frees the struct.
    pub sealed_alloc: FunctionValue<'ctx>,
    pub sealed_release: FunctionValue<'ctx>,
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
    /// Int-keyed map entry points (key_kind = 1).
    pub map_get_int: FunctionValue<'ctx>,
    pub map_set_int: FunctionValue<'ctx>,
}

impl<'ctx> RuntimeFns<'ctx> {
    /// Declare every `lin-runtime` symbol into `module` (C ABI, defined in lin-runtime).
    pub(crate) fn new(context: &'ctx Context, module: &Module<'ctx>) -> Self {
        let string_ptr_type = context.ptr_type(AddressSpace::default());
        let ptr_type = context.ptr_type(AddressSpace::default());

        let i32_type = context.i32_type();
        let i64_type = context.i64_type();
        let void_type = context.void_type();
        let bool_type = context.bool_type();

        let string_literal = module.add_function(
            "lin_string_literal",
            string_ptr_type.fn_type(&[ptr_type.into(), i32_type.into()], false),
            None,
        );
        let string_length = module.add_function(
            "lin_string_length",
            i32_type.fn_type(&[string_ptr_type.into()], false),
            None,
        );
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
        let array_alloc = module.add_function(
            "lin_array_alloc",
            ptr_type.fn_type(&[i64_type.into()], false),
            None,
        );
        // lin_array_push(arr: ptr, elem: ptr, tag: i8) -> void
        let array_push = module.add_function(
            "lin_array_push",
            void_type.fn_type(&[ptr_type.into(), ptr_type.into(), context.i8_type().into()], false),
            None,
        );
        // lin_array_get(arr: ptr, idx: i64) -> ptr (tagged element)
        let array_get = module.add_function(
            "lin_array_get",
            ptr_type.fn_type(&[ptr_type.into(), i64_type.into()], false),
            None,
        );
        // lin_alloc(size: i64) -> ptr — general heap allocation for closures/envs
        let alloc = module.add_function(
            "lin_alloc",
            ptr_type.fn_type(&[i64_type.into()], false),
            None,
        );
        // Numeric to string conversions
        let int_to_string = module.add_function(
            "lin_int_to_string",
            string_ptr_type.fn_type(&[i64_type.into()], false),
            None,
        );
        // Unsigned 64-bit → string (the payload bits are interpreted as u64). Used for UInt64
        // values, which lin_int_to_string would print as a negative number when >= 2^63.
        let uint_to_string = module.add_function(
            "lin_uint_to_string",
            string_ptr_type.fn_type(&[i64_type.into()], false),
            None,
        );
        let float_to_string = module.add_function(
            "lin_float_to_string",
            string_ptr_type.fn_type(&[context.f64_type().into()], false),
            None,
        );
        let bool_to_string = module.add_function(
            "lin_bool_to_string",
            string_ptr_type.fn_type(&[bool_type.into()], false),
            None,
        );
        let null_to_string = module.add_function(
            "lin_null_to_string",
            string_ptr_type.fn_type(&[], false),
            None,
        );
        // Tagged union boxing/unboxing
        let i8_type = context.i8_type();
        let box_null = module.add_function("lin_box_null", ptr_type.fn_type(&[], false), None);
        let box_bool = module.add_function("lin_box_bool", ptr_type.fn_type(&[i8_type.into()], false), None);
        let box_int32 = module.add_function("lin_box_int32", ptr_type.fn_type(&[i32_type.into()], false), None);
        let box_int64 = module.add_function("lin_box_int64", ptr_type.fn_type(&[i64_type.into()], false), None);
        let box_uint64 = module.add_function("lin_box_uint64", ptr_type.fn_type(&[i64_type.into()], false), None);
        let box_float64 = module.add_function("lin_box_float64", ptr_type.fn_type(&[context.f64_type().into()], false), None);
        let box_str = module.add_function("lin_box_str", ptr_type.fn_type(&[ptr_type.into()], false), None);
        let box_object = module.add_function("lin_box_object", ptr_type.fn_type(&[ptr_type.into()], false), None);
        let box_array = module.add_function("lin_box_array", ptr_type.fn_type(&[ptr_type.into()], false), None);
        let box_sumnode = module.add_function("lin_box_sumnode", ptr_type.fn_type(&[ptr_type.into()], false), None);
        // Stage 6a: lin_box_record(sealed_ptr: ptr) -> ptr (TaggedVal* with TAG_RECORD tag)
        let box_record = module.add_function("lin_box_record", ptr_type.fn_type(&[ptr_type.into()], false), None);
        let box_function = module.add_function("lin_box_function", ptr_type.fn_type(&[ptr_type.into()], false), None);
        let get_tag = module.add_function("lin_get_tag", i8_type.fn_type(&[ptr_type.into()], false), None);
        let unbox_int32 = module.add_function("lin_unbox_int32", i32_type.fn_type(&[ptr_type.into()], false), None);
        let unbox_int64 = module.add_function("lin_unbox_int64", i64_type.fn_type(&[ptr_type.into()], false), None);
        let unbox_uint64 = module.add_function("lin_unbox_uint64", i64_type.fn_type(&[ptr_type.into()], false), None);
        let unbox_float64 = module.add_function("lin_unbox_float64", context.f64_type().fn_type(&[ptr_type.into()], false), None);
        let unbox_bool = module.add_function("lin_unbox_bool", i8_type.fn_type(&[ptr_type.into()], false), None);
        let unbox_ptr = module.add_function("lin_unbox_ptr", ptr_type.fn_type(&[ptr_type.into()], false), None);
        // lin_tagged_to_string(tagged: ptr) -> ptr (LinString*)
        let tagged_to_string = module.add_function("lin_tagged_to_string", string_ptr_type.fn_type(&[ptr_type.into()], false), None);
        // lin_object_alloc(initial_cap: i32) -> ptr
        let object_alloc = module.add_function("lin_object_alloc", ptr_type.fn_type(&[i32_type.into()], false), None);
        // lin_object_set(obj: ptr, key: ptr, val: ptr) -> void
        let object_set = module.add_function("lin_object_set", void_type.fn_type(&[ptr_type.into(), ptr_type.into(), ptr_type.into()], false), None);
        // lin_object_set_fresh(obj: ptr, key: ptr, val: ptr) -> void — no-dup-check literal append
        let object_set_fresh = module.add_function("lin_object_set_fresh", void_type.fn_type(&[ptr_type.into(), ptr_type.into(), ptr_type.into()], false), None);
        // lin_object_get(obj: ptr, key: ptr) -> ptr (points to TaggedVal, or null)
        let object_get = module.add_function("lin_object_get", ptr_type.fn_type(&[ptr_type.into(), ptr_type.into()], false), None);
        // lin_object_eq(a: ptr, b: ptr) -> i8
        let object_eq = module.add_function("lin_object_eq", i8_type.fn_type(&[ptr_type.into(), ptr_type.into()], false), None);
        // Retain / release: adjust refcount, free if zero.
        let rc_retain = module.add_function("lin_rc_retain", void_type.fn_type(&[ptr_type.into()], false), None);
        let string_release = module.add_function("lin_string_release", void_type.fn_type(&[ptr_type.into()], false), None);
        let array_release = module.add_function("lin_array_release", void_type.fn_type(&[ptr_type.into()], false), None);
        let object_release = module.add_function("lin_object_release", void_type.fn_type(&[ptr_type.into()], false), None);
        let closure_release = module.add_function("lin_closure_release", void_type.fn_type(&[ptr_type.into()], false), None);
        let tagged_release = module.add_function("lin_tagged_release", void_type.fn_type(&[ptr_type.into()], false), None);
        let sealed_alloc = module.add_function("lin_sealed_alloc", ptr_type.fn_type(&[i64_type.into(), ptr_type.into(), ptr_type.into()], false), None);
        let sealed_release = module.add_function("lin_sealed_release", void_type.fn_type(&[ptr_type.into(), i64_type.into()], false), None);
        let sumnode_alloc = module.add_function("lin_sumnode_alloc", ptr_type.fn_type(&[i64_type.into(), ptr_type.into()], false), None);
        let sumnode_release = module.add_function("lin_sumnode_release", void_type.fn_type(&[ptr_type.into(), i64_type.into()], false), None);
        // Typed index-signature map (ADR-055 + numeric-key).
        // lin_map_alloc(hint: i32, key_kind: i32) -> *LinMap
        let map_alloc = module.add_function("lin_map_alloc", ptr_type.fn_type(&[i32_type.into(), i32_type.into()], false), None);
        let map_set = module.add_function("lin_map_set", void_type.fn_type(&[ptr_type.into(), ptr_type.into(), ptr_type.into()], false), None);
        let map_get = module.add_function("lin_map_get", ptr_type.fn_type(&[ptr_type.into(), ptr_type.into()], false), None);
        let map_release = module.add_function("lin_map_release", void_type.fn_type(&[ptr_type.into()], false), None);
        // Int-keyed map entry points: get(map, i64)->ptr, set(map, i64, val_ptr)->void
        let map_get_int = module.add_function("lin_map_get_int", ptr_type.fn_type(&[ptr_type.into(), i64_type.into()], false), None);
        let map_set_int = module.add_function("lin_map_set_int", void_type.fn_type(&[ptr_type.into(), i64_type.into(), ptr_type.into()], false), None);

        Self {
            string_literal,
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
            box_object,
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
            object_alloc,
            object_set,
            object_set_fresh,
            object_get,
            object_eq,
            tagged_to_string,
            rc_retain,
            string_release,
            array_release,
            object_release,
            closure_release,
            tagged_release,
            sealed_alloc,
            sealed_release,
            sumnode_alloc,
            sumnode_release,
            map_alloc,
            map_set,
            map_get,
            map_release,
            map_get_int,
            map_set_int,
        }
    }
}
