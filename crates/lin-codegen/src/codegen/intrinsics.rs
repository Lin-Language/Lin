use super::builder_ext::BuilderExt;
use inkwell::values::BasicValueEnum;
use inkwell::AddressSpace;

use lin_check::types::Type;
use lin_ir::ir as lir;
use super::Codegen;

impl<'ctx> Codegen<'ctx> {
    pub(crate) fn value_to_string_simple(
        &mut self,
        val: BasicValueEnum<'ctx>,
        ty: &Type,
    ) -> BasicValueEnum<'ctx> {
        // TypeVar values are TaggedVal* — use runtime dispatch.
        // Exception: when a TypeVar was coerced to a concrete int (e.g. from TypeVar+TypeVar
        // binary ops), dispatch on the LLVM value type to produce a correct toString.
        if matches!(ty, Type::TypeVar(_)) {
            if val.is_pointer_value() {
                return self.builder
                    .build_call(self.rt.tagged_to_string, &[val.into()], "ttos")
                    .unwrap()
                    .try_as_basic_value()
                    .unwrap_basic();
            } else if val.is_int_value() {
                // Concretized int (e.g. after TypeVar+TypeVar arithmetic) — use int toString.
                let i64_ty = self.context.i64_type();
                let i64_val = self.builder
                    .build_int_s_extend_or_bit_cast(val.into_int_value(), i64_ty, "tv_iext")
                    .unwrap();
                return self.builder
                    .build_call(self.rt.int_to_string, &[i64_val.into()], "tv_itos")
                    .unwrap()
                    .try_as_basic_value()
                    .unwrap_basic();
            } else if val.is_float_value() {
                let f64_ty = self.context.f64_type();
                let f64_val = self.builder
                    .build_float_ext(val.into_float_value(), f64_ty, "tv_fext")
                    .unwrap();
                return self.builder
                    .build_call(self.rt.float_to_string, &[f64_val.into()], "tv_ftos")
                    .unwrap()
                    .try_as_basic_value()
                    .unwrap_basic();
            }
        }
        match ty {
            // Uniform-ownership contract (ADR: interp-string transient-use leak fix): `ToString`
            // ALWAYS returns an OWNED (+1) string. The numeric/bool/null/Array/Object arms below
            // allocate a FRESH string (already +1); the Str arm would otherwise hand back the input
            // BORROWED (+0), so RETAIN it here to match. This lets the IR treat the `ToString`
            // result uniformly (`lower_intrinsic_call` already `register_owned`s it, and
            // `lower_string_interp` releases each per-part temp after the concat). Without the
            // retain, the IR-level release of the (assumed-owned) result would over-decrement a
            // borrowed string → use-after-free. `lin_rc_retain` is immortal-safe (interned literals
            // saturate), so retaining a string literal is a no-op.
            Type::Str | Type::StrLit(_) => {
                if val.is_pointer_value() {
                    self.builder.call(self.rt.rc_retain, &[val.into()], "");
                }
                val
            }
            // UInt64 must stringify UNSIGNED: a value >= 2^63 read as a signed i64 prints a
            // negative number. UInt8/16/32 zero-extend into an always-positive i64, so the
            // signed lin_int_to_string is correct for them.
            Type::UInt64 => {
                let i64_ty = self.context.i64_type();
                let u64_val = self.builder.int_z_extend_or_bit_cast(val.into_int_value(), i64_ty, "uext");
                self.builder
                    .build_call(self.rt.uint_to_string, &[u64_val.into()], "utos")
                    .unwrap()
                    .try_as_basic_value()
                    .unwrap_basic()
            }
            Type::Int8 | Type::Int16 | Type::Int32 | Type::Int64
            | Type::UInt8 | Type::UInt16 | Type::UInt32 => {
                let i64_ty = self.context.i64_type();
                let i64_val = if ty.is_signed() {
                    self.builder.int_s_extend_or_bit_cast(val.into_int_value(), i64_ty, "iext")
                } else {
                    self.builder.int_z_extend_or_bit_cast(val.into_int_value(), i64_ty, "iext")
                };
                self.builder
                    .build_call(self.rt.int_to_string, &[i64_val.into()], "itos")
                    .unwrap()
                    .try_as_basic_value()
                    .unwrap_basic()
            }
            Type::Float32 => {
                let f64_val = self.builder.float_ext(val.into_float_value(), self.context.f64_type(), "fext");
                self.builder
                    .build_call(self.rt.float_to_string, &[f64_val.into()], "ftos")
                    .unwrap()
                    .try_as_basic_value()
                    .unwrap_basic()
            }
            Type::Float64 => {
                self.builder
                    .build_call(self.rt.float_to_string, &[val.into()], "ftos")
                    .unwrap()
                    .try_as_basic_value()
                    .unwrap_basic()
            }
            Type::Bool => {
                self.builder
                    .build_call(self.rt.bool_to_string, &[val.into()], "btos")
                    .unwrap()
                    .try_as_basic_value()
                    .unwrap_basic()
            }
            Type::Null => {
                self.builder
                    .build_call(self.rt.null_to_string, &[], "ntos")
                    .unwrap()
                    .try_as_basic_value()
                    .unwrap_basic()
            }
            Type::Array(_) if Self::sealed_array_elem(ty).is_some() => {
                // Sealed-record array (Stage 3): materialize to the tagged Object[] view, stringify,
                // release the temp. Produces the same JSON-ish output as a boxed Object[].
                let ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
                let tagged = self.sealed_array_to_tagged(val, ty);
                let f = self.get_or_declare_fn("lin_array_to_string", ptr_ty.fn_type(&[ptr_ty.into()], false));
                let s = self.builder.call(f, &[tagged.into()], "satos").try_as_basic_value().unwrap_basic();
                self.builder.call(self.rt.array_release, &[tagged.into()], "");
                s
            }
            Type::Array(elem_box) => {
                let ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
                // For flat scalar arrays, convert to tagged format first.
                let arr_val = if Self::is_flat_scalar(elem_box) {
                    let suffix = Self::flat_suffix(elem_box);
                    let conv_fn = self.get_or_declare_fn(
                        &format!("lin_flat_to_tagged_{}", suffix),
                        ptr_ty.fn_type(&[ptr_ty.into()], false),
                    );
                    self.builder.call(conv_fn, &[val.into()], "flat2t").try_as_basic_value().unwrap_basic()
                } else {
                    val
                };
                let f = self.get_or_declare_fn("lin_array_to_string",
                    ptr_ty.fn_type(&[ptr_ty.into()], false));
                self.builder.call(f, &[arr_val.into()], "atos").try_as_basic_value().unwrap_basic()
            }
            Type::FixedArray(_) => {
                let ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
                let f = self.get_or_declare_fn("lin_array_to_string",
                    ptr_ty.fn_type(&[ptr_ty.into()], false));
                self.builder.call(f, &[val.into()], "atos").try_as_basic_value().unwrap_basic()
            }
            Type::Object { .. } => {
                let ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
                let f = self.get_or_declare_fn("lin_object_to_string",
                    ptr_ty.fn_type(&[ptr_ty.into()], false));
                self.builder.call(f, &[val.into()], "otos").try_as_basic_value().unwrap_basic()
            }
            _ => {
                // For unknown complex types, fall back to runtime tagged dispatch.
                if val.is_pointer_value() {
                    self.builder
                        .build_call(self.rt.tagged_to_string, &[val.into()], "ttos")
                        .unwrap()
                        .try_as_basic_value()
                        .unwrap_basic()
                } else {
                    self.compile_string_lit("[object]")
                }
            }
        }
    }

    pub(crate) fn compile_ir_intrinsic(&mut self, intrinsic: &lir::Intrinsic, args: &[BasicValueEnum<'ctx>], arg_tys: &[Type], arg_reprs: &[lin_ir::repr::Repr], ret_ty: &Type) -> BasicValueEnum<'ctx> {
        use lir::Intrinsic;
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        match intrinsic {
            Intrinsic::Print => {
                if let Some(&arg) = args.first() {
                    // lin_print takes a raw LinString*. The argument may be any value (its
                    // declared param is Json), so convert it to a string first — mirroring
                    // the AST `lin_print` handler. Without this, a boxed TaggedVal* would be
                    // dereferenced as a LinString and print garbage.
                    let in_ty = arg_tys.first().cloned().unwrap_or(Type::Null);
                    let str_val = self.compile_to_string_value(arg, &in_ty);
                    self.builder.call(self.rt.print, &[str_val.into()], "");
                    // `compile_to_string_value`/`ToString` now returns an OWNED (+1) string for
                    // EVERY input type (the Str arm retains; numeric/bool/json arms allocate fresh).
                    // `lin_print` only BORROWS its argument, so this transient string is otherwise
                    // never released — release it here. (Previously the numeric/json fresh-string
                    // case leaked one string per `print(nonStr)`; the Str case was +0 and balanced.
                    // With uniform ownership both are +1, so a single unconditional release is now
                    // correct for all input types.)
                    self.builder.call(self.rt.string_release, &[str_val.into()], "");
                }
                ptr_ty.const_null().into()
            }
            Intrinsic::ToString => {
                let arg = args.first().copied().unwrap_or_else(|| ptr_ty.const_null().into());
                let in_ty = arg_tys.first().cloned().unwrap_or(Type::Null);
                self.compile_to_string_value(arg, &in_ty)
            }
            Intrinsic::Length => {
                let Some(&arg) = args.first() else {
                    return self.coerce_int_width(self.context.i64_type().const_zero().into(), ret_ty);
                };
                let i32_ty = self.context.i32_type();
                let i64_ty = self.context.i64_type();
                let arg_ty = arg_tys.first().cloned().unwrap_or(Type::Null);
                // Dispatch on the argument's STATIC type, mirroring the AST `lin_length`:
                // string→lin_string_length, array→lin_array_length, object→lin_object_length,
                // Json/union→lin_length_dyn (runtime tag dispatch). Calling lin_array_length on
                // a boxed Json (TaggedVal*) would read garbage.
                let raw_len = match &arg_ty {
                    Type::Str | Type::StrLit(_) => {
                        self.builder.call(self.rt.string_length, &[arg.into()], "ir_slen").try_as_basic_value().unwrap_basic()
                    }
                    Type::Array(_) | Type::FixedArray(_) | Type::Iterator(_) => {
                        let len_fn = self.get_or_declare_fn("lin_array_length",
                            i64_ty.fn_type(&[ptr_ty.into()], false));
                        self.builder.call(len_fn, &[arg.into()], "ir_alen").try_as_basic_value().unwrap_basic()
                    }
                    Type::Object { .. } | Type::Named(_) => {
                        let obj_len_fn = self.get_or_declare_fn("lin_object_length",
                            i64_ty.fn_type(&[ptr_ty.into()], false));
                        self.builder.call(obj_len_fn, &[arg.into()], "ir_olen").try_as_basic_value().unwrap_basic()
                    }
                    // Typed index-signature map `{ String: T }` (ADR-055): entry count.
                    Type::Map(_) => {
                        let map_len_fn = self.get_or_declare_fn("lin_map_length",
                            i64_ty.fn_type(&[ptr_ty.into()], false));
                        self.builder.call(map_len_fn, &[arg.into()], "ir_mlen").try_as_basic_value().unwrap_basic()
                    }
                    _ => {
                        // Json / TypeVar / Union — dynamic dispatch on the runtime tag.
                        let len_dyn_fn = self.get_or_declare_fn("lin_length_dyn",
                            i32_ty.fn_type(&[ptr_ty.into()], false));
                        self.builder.call(len_dyn_fn, &[arg.into()], "ir_dynlen").try_as_basic_value().unwrap_basic()
                    }
                };
                // Narrow/widen to the declared result width (length is usually Int32).
                self.coerce_int_width(raw_len, ret_ty)
            }
            Intrinsic::Push => {
                if args.len() >= 2 {
                    let arr = args[0];
                    let elem = args[1];
                    let arr_ty = arg_tys.first().cloned().unwrap_or(Type::Null);
                    let elem_ty = arg_tys.get(1).cloned().unwrap_or_else(|| Type::TypeVar(u32::MAX));
                    // A statically FLAT-scalar array (e.g. `val r: Int32[] = []` then `push(r, x)`):
                    // the `[]` literal allocated a FLAT buffer (lin_flat_array_alloc_*), so a tagged
                    // push (`lin_array_push_tagged`) would corrupt it. Route through `lin_push_dyn`,
                    // which reads the array's runtime `elem_tag` and coerces the boxed element into
                    // the flat slot. Scalars carry no refcount, so no RC balancing is needed (the
                    // boxed element shell is a fresh cached/heap box freed after the move). This is
                    // the `[]`+push flat-representation consistency fix (ADR-044).
                    //
                    // A SEALED-RECORD ARRAY (Stage 3): contiguous, header-less, unboxed element
                    // payloads (`lin_sealed_array_alloc`, elem_tag 0xFE, packed-scalar stride). The
                    // element value arrives as a STANDALONE sealed-struct pointer (the monomorphized
                    // `push$<T>` body constructs it). Copy its payload into a fresh element slot via
                    // `lin_sealed_array_push_struct_retaining` (verbatim byte copy for a scalar-only
                    // record — NULL descriptor; per-field retain otherwise). The source struct stays
                    // owned — `lower.rs` `push_into_sealed_array` skips the ownership transfer — and
                    // is released at scope exit, balancing the retain. WITHOUT this, a sealed-array
                    // `push` fell through to `tagged_array_push_value`, which materialized a BOXED
                    // LinObject and tagged-pushed it (TAG_OBJECT) into the packed-scalar buffer → a
                    // pointer-sized tagged element written into a scalar-stride slot → heap-buffer
                    // overflow (`realloc(): invalid next size`; ASan: heap-buffer-overflow in
                    // lin_array_push). Stage-3 tests dodged this by using array LITERALS only. Scalar
                    // sealed arrays ONLY (`sealed_array_elem` gates on all-scalar fields); heap-field
                    // record arrays fail safe to the boxed `Object[]` path.
                    // STAGE 3: the packed-sealed-array decision is read from the array operand's
                    // repr (`func.repr`, threaded in as `arg_reprs[0]`) — the pass already applied
                    // the `sealed_array_elem` all-scalar gate. Oracle-proven equal to the former
                    // `sealed_array_elem(&arr_ty).is_some()` decision.
                    if arg_reprs.first().and_then(|r| r.packed_sealed_array_layout()).is_some() {
                        let push_fn = self.get_or_declare_fn(
                            "lin_sealed_array_push_struct_retaining",
                            self.context.void_type().fn_type(&[ptr_ty.into(), ptr_ty.into()], false));
                        self.builder.call(push_fn, &[arr.into(), elem.into()], "");
                        return ptr_ty.const_null().into();
                    }
                    let arr_elem_flat = matches!(&arr_ty, Type::Array(e) if Self::is_flat_scalar(e));
                    // A `Json[]` array (`Array(TypeVar(MAX))`, the Json/wildcard element) can hold a
                    // FLAT runtime array at runtime — a concrete `Int32[]` boxed into a `Json`-typed
                    // slot keeps its flat representation. A static tagged push would do a 16-byte
                    // TaggedVal write into a 4-byte flat slot → heap corruption (`free(): invalid
                    // pointer`). Route the dynamic-element case through `lin_push_dyn`, which reads
                    // the array's runtime `elem_tag` and coerces the boxed element into the correct
                    // (flat or tagged) slot. This is the `push$Json` / `push(jsonArr, …)` case (e.g.
                    // `(a: Json) => push(a, 1)` applied to a flat `Int32[]`).
                    let arr_elem_dynamic = matches!(&arr_ty, Type::Array(e) if Self::is_union_type(e));
                    if Self::is_union_type(&arr_ty) || arr_elem_flat || arr_elem_dynamic {
                        // arr may be a boxed TaggedVal* wrapping a LinArray* (flat or tagged), or a
                        // raw flat LinArray*. Unbox if boxed, then lin_push_dyn dispatches on elem_tag.
                        let arr_raw = if Self::is_union_type(&arr_ty) {
                            self.builder.call(self.rt.unbox_ptr, &[arr.into()], "ir_push_arr").try_as_basic_value().unwrap_basic()
                        } else {
                            arr
                        };
                        let elem_is_fresh_box = !Self::is_union_type(&elem_ty);
                        let elem_tagged = if elem_is_fresh_box {
                            self.box_value(elem, &elem_ty)
                        } else { elem };
                        let push_dyn_fn = self.get_or_declare_fn("lin_push_dyn",
                            self.context.void_type().fn_type(&[ptr_ty.into(), ptr_ty.into()], false));
                        self.builder.call(push_dyn_fn, &[arr_raw.into(), elem_tagged.into()], "");
                        if elem_is_fresh_box && elem_tagged.is_pointer_value() {
                            self.builder.call(self.rt.tagged_release, &[elem_tagged.into()], "");
                        }
                    } else {
                        // arr is a raw tagged LinArray* of known element type (move semantics).
                        self.tagged_array_push_value(arr, elem, &elem_ty);
                    }
                }
                ptr_ty.const_null().into()
            }
            Intrinsic::ArrayAlloc => {
                // Empty tagged array (capacity grows on push).
                let cap = self.context.i64_type().const_int(4, false);
                self.builder.call(self.rt.array_alloc, &[cap.into()], "ir_arr_alloc").try_as_basic_value().unwrap_basic()
            }
            Intrinsic::FlatArrayAlloc(kind) => {
                let cap = self.context.i64_type().const_int(4, false);
                let alloc_fn = self.get_or_declare_fn(
                    &format!("lin_flat_array_alloc_{}", kind.suffix()),
                    ptr_ty.fn_type(&[self.context.i64_type().into()], false));
                self.builder.call(alloc_fn, &[cap.into()], "ir_falloc").try_as_basic_value().unwrap_basic()
            }
            Intrinsic::FlatArrayPush(kind) => {
                if args.len() >= 2 {
                    let elem_ty = kind.elem_type();
                    self.flat_array_push(args[0], args[1], &elem_ty);
                }
                ptr_ty.const_null().into()
            }
            Intrinsic::StringConcat | Intrinsic::Concat => {
                if args.len() >= 2 {
                    let a = args[0];
                    let b = args[1];
                    let rt_concat = self.get_or_declare_fn("lin_string_concat",
                        ptr_ty.fn_type(&[ptr_ty.into(), ptr_ty.into()], false));
                    self.builder.call(rt_concat, &[a.into(), b.into()], "ir_cat").try_as_basic_value().unwrap_basic()
                } else { ptr_ty.const_null().into() }
            }
            Intrinsic::Async => {
                // async(thunk): hand the UNEVALUATED thunk closure to the runtime, which spawns
                // an OS thread, deep-copies the captured env (Option C), runs the thunk inside a
                // fault boundary, and returns a LinPromise*. The thunk may arrive boxed (a
                // Json-typed parameter, as in std/async's `async(f: Json)`) — unbox to the raw
                // closure struct first.
                //
                // The `pool.async(f)` dot form desugars to `lin_async(pool, f)` (2 args): the
                // first arg is the ThreadPool. We route that to lin_pool_async_one (enqueue) so
                // the thunk runs on a pool worker instead of a fresh thread (spec §24.5).
                let thunk = args.last().copied().unwrap_or_else(|| ptr_ty.const_null().into());
                let thunk_ty = arg_tys.last().cloned().unwrap_or(Type::Null);
                let thunk = if Self::is_union_type(&thunk_ty) && thunk.is_pointer_value() {
                    self.builder.call(self.rt.unbox_ptr, &[thunk.into()], "ir_async_cls").try_as_basic_value().unwrap_basic()
                } else { thunk };
                let raw = if args.len() >= 2 {
                    // pool.async(f): args[0] is the pool handle, boxed as TAG_HANDLE — unbox to
                    // the raw *LinThreadPool.
                    let pool = self.unbox_handle(args[0]);
                    let pool_async_fn = self.get_or_declare_fn("lin_pool_async_one",
                        ptr_ty.fn_type(&[ptr_ty.into(), ptr_ty.into()], false));
                    self.builder.call(pool_async_fn, &[pool.into(), thunk.into()], "ir_pool_async").try_as_basic_value().unwrap_basic()
                } else {
                    let spawn_fn = self.get_or_declare_fn("lin_async_spawn",
                        ptr_ty.fn_type(&[ptr_ty.into()], false));
                    self.builder.call(spawn_fn, &[thunk.into()], "ir_async_spawn").try_as_basic_value().unwrap_basic()
                };
                // Box the raw *LinPromise into a TaggedVal*(TAG_PROMISE) so it round-trips
                // through TypeVar slots and arrays (e.g. race([...])).
                self.box_promise(raw)
            }
            Intrinsic::Await => {
                let promise = args.first().copied().unwrap_or_else(|| ptr_ty.const_null().into());
                // The promise arrives boxed (TAG_PROMISE); unbox to the raw *LinPromise.
                let raw = self.unbox_promise(promise);
                let await_fn = self.get_or_declare_fn("lin_await_promise",
                    ptr_ty.fn_type(&[ptr_ty.into()], false));
                let tagged = self.builder.call(await_fn, &[raw.into()], "ir_await").try_as_basic_value().unwrap_basic();
                // Unbox to the (concrete) result type if needed.
                if !Self::is_union_type(ret_ty) && *ret_ty != Type::Null {
                    self.unbox_tagged_val_to_type(tagged, ret_ty)
                } else {
                    tagged
                }
            }
            Intrinsic::Exit => {
                if let Some(&code) = args.first() {
                    let exit_fn = self.get_or_declare_fn("exit",
                        self.context.void_type().fn_type(&[self.context.i32_type().into()], false));
                    if code.is_int_value() {
                        self.builder.call(exit_fn, &[code.into_int_value().into()], "");
                    }
                }
                ptr_ty.const_null().into()
            }
            // parallel(tasks): hand the (unboxed) array of thunk closures to the runtime, which
            // spawns all of them on OS threads then joins in order (order-preserving) and
            // returns a fresh tagged array of boxed results. A faulting task yields an Error in
            // its slot. `tasks` arrives boxed (Json-typed param) — unbox to the raw LinArray*.
            Intrinsic::Parallel => {
                let tasks = args.first().copied().unwrap_or_else(|| ptr_ty.const_null().into());
                let arr_unboxed = if tasks.is_pointer_value() {
                    self.builder.call(self.rt.unbox_ptr, &[tasks.into()], "ir_par_arr").try_as_basic_value().unwrap_basic()
                } else { ptr_ty.const_null().into() };
                let par_fn = self.get_or_declare_fn("lin_parallel",
                    ptr_ty.fn_type(&[ptr_ty.into()], false));
                self.builder.call(par_fn, &[arr_unboxed.into()], "ir_parallel").try_as_basic_value().unwrap_basic()
            }
            // race(promises): unbox the array, hand to lin_race → first-to-complete promise.
            Intrinsic::Race => {
                let promises = args.first().copied().unwrap_or_else(|| ptr_ty.const_null().into());
                let arr_unboxed = if promises.is_pointer_value() {
                    self.builder.call(self.rt.unbox_ptr, &[promises.into()], "ir_race_arr").try_as_basic_value().unwrap_basic()
                } else { ptr_ty.const_null().into() };
                let race_fn = self.get_or_declare_fn("lin_race",
                    ptr_ty.fn_type(&[ptr_ty.into()], false));
                let raw = self.builder.call(race_fn, &[arr_unboxed.into()], "ir_race").try_as_basic_value().unwrap_basic();
                self.box_promise(raw)
            }
            // timeout(promise, ms): lin_timeout(promise, ms) → settled promise (orig value or null).
            Intrinsic::Timeout => {
                let i32_ty = self.context.i32_type();
                let promise = args.first().copied().unwrap_or_else(|| ptr_ty.const_null().into());
                let ms = args.get(1).copied().unwrap_or_else(|| i32_ty.const_zero().into());
                let ms_i32 = if ms.is_int_value() {
                    self.builder.int_s_extend_or_bit_cast(ms.into_int_value(), i32_ty, "ir_to_ms")
                } else { i32_ty.const_zero() };
                let raw_p = self.unbox_promise(promise);
                let to_fn = self.get_or_declare_fn("lin_timeout",
                    ptr_ty.fn_type(&[ptr_ty.into(), i32_ty.into()], false));
                let raw = self.builder.call(to_fn, &[raw_p.into(), ms_i32.into()], "ir_timeout").try_as_basic_value().unwrap_basic();
                self.box_promise(raw)
            }
            // retry(thunk, n): unbox the thunk closure, lin_retry(thunk, n) → settled promise.
            Intrinsic::Retry => {
                let i32_ty = self.context.i32_type();
                let thunk = args.first().copied().unwrap_or_else(|| ptr_ty.const_null().into());
                let thunk_ty = arg_tys.first().cloned().unwrap_or(Type::Null);
                let thunk = if Self::is_union_type(&thunk_ty) && thunk.is_pointer_value() {
                    self.builder.call(self.rt.unbox_ptr, &[thunk.into()], "ir_retry_cls").try_as_basic_value().unwrap_basic()
                } else { thunk };
                let n = args.get(1).copied().unwrap_or_else(|| i32_ty.const_int(1, false).into());
                let n_i32 = if n.is_int_value() {
                    self.builder.int_s_extend_or_bit_cast(n.into_int_value(), i32_ty, "ir_retry_n")
                } else { i32_ty.const_int(1, false) };
                let retry_fn = self.get_or_declare_fn("lin_retry",
                    ptr_ty.fn_type(&[ptr_ty.into(), i32_ty.into()], false));
                let raw = self.builder.call(retry_fn, &[thunk.into(), n_i32.into()], "ir_retry").try_as_basic_value().unwrap_basic();
                self.box_promise(raw)
            }
            // threadPool(n) → lin_thread_pool_new(n).
            Intrinsic::ThreadPool => {
                let i32_ty = self.context.i32_type();
                let n = args.first().copied().unwrap_or_else(|| i32_ty.const_int(2, false).into());
                let n_i32 = if n.is_int_value() { n.into_int_value() } else { i32_ty.const_int(2, false) };
                let pool_fn = self.get_or_declare_fn("lin_thread_pool_new",
                    ptr_ty.fn_type(&[i32_ty.into()], false));
                let raw = self.builder.call(pool_fn, &[n_i32.into()], "ir_pool").try_as_basic_value().unwrap_basic();
                // Box the raw *LinThreadPool so it round-trips through TypeVar slots / Json params.
                self.box_handle(raw)
            }
            // worker(handler, onClose) → lin_worker_new(h_fn, h_env, h_has, c_fn, c_env, c_has).
            // Both handler and onClose arrive as (possibly boxed) closure values; extract each
            // closure's (fn_ptr, env_ptr, has_env). The worker thread keeps the handler env
            // (thread-confined, §24.6.4) for its lifetime.
            Intrinsic::Worker => {
                let i8_ty = self.context.i8_type();
                let handler = args.first().copied().unwrap_or_else(|| ptr_ty.const_null().into());
                let handler_ty = arg_tys.first().cloned().unwrap_or(Type::Null);
                let (h_fn, h_env, h_has) = self.extract_closure_fields(handler, &handler_ty);
                let onclose = args.get(1).copied().unwrap_or_else(|| ptr_ty.const_null().into());
                let onclose_ty = arg_tys.get(1).cloned().unwrap_or(Type::Null);
                let (c_fn, c_env, c_has) = self.extract_closure_fields(onclose, &onclose_ty);
                let worker_fn = self.get_or_declare_fn("lin_worker_new",
                    ptr_ty.fn_type(&[ptr_ty.into(), ptr_ty.into(), i8_ty.into(), ptr_ty.into(), ptr_ty.into(), i8_ty.into()], false));
                let raw = self.builder.call(worker_fn,
                    &[h_fn.into(), h_env.into(), h_has.into(), c_fn.into(), c_env.into(), c_has.into()],
                    "ir_worker").try_as_basic_value().unwrap_basic();
                // Box the raw *LinWorker so it round-trips through TypeVar slots / Json params.
                self.box_handle(raw)
            }
            // serve(handler, port) → lin_serve(h_fn, h_env, h_has, port). Dot-syntax
            // `router.serve(3000)` desugars to `serve(router, 3000)`, so args[0] is the
            // handler closure and args[1] is the port. Blocks forever (returns Null).
            Intrinsic::Serve => {
                let i8_ty = self.context.i8_type();
                let i32_ty = self.context.i32_type();
                let handler = args.first().copied().unwrap_or_else(|| ptr_ty.const_null().into());
                let handler_ty = arg_tys.first().cloned().unwrap_or(Type::Null);
                let (h_fn, h_env, h_has) = self.extract_closure_fields(handler, &handler_ty);
                let port = args.get(1).copied().unwrap_or_else(|| i32_ty.const_zero().into());
                let port_i32 = if port.is_int_value() {
                    self.builder.int_s_extend_or_bit_cast(port.into_int_value(), i32_ty, "ir_serve_port")
                } else {
                    i32_ty.const_zero()
                };
                let serve_fn = self.get_or_declare_fn("lin_serve",
                    ptr_ty.fn_type(&[ptr_ty.into(), ptr_ty.into(), i8_ty.into(), i32_ty.into()], false));
                self.builder.call(serve_fn,
                    &[h_fn.into(), h_env.into(), h_has.into(), port_i32.into()],
                    "ir_serve").try_as_basic_value().unwrap_basic()
            }
            // shared(v) → lin_shared_new(boxed v) → boxed Shared (TAG_SHARED). v may arrive
            // concrete; box it so the runtime receives a TaggedVal* to deep-copy in.
            Intrinsic::SharedNew => {
                let v = args.first().copied().unwrap_or_else(|| ptr_ty.const_null().into());
                let v_ty = arg_tys.first().cloned().unwrap_or(Type::Null);
                let v_boxed = if Self::is_union_type(&v_ty) || v.is_pointer_value() { v } else { self.box_value(v, &v_ty) };
                let f = self.get_or_declare_fn("lin_shared_new", ptr_ty.fn_type(&[ptr_ty.into()], false));
                self.builder.call(f, &[v_boxed.into()], "ir_shared_new").try_as_basic_value().unwrap_basic()
            }
            // get(s) → lin_shared_get(s) → boxed snapshot (unboxed to the concrete result type).
            Intrinsic::SharedGet => {
                let s = args.first().copied().unwrap_or_else(|| ptr_ty.const_null().into());
                let f = self.get_or_declare_fn("lin_shared_get", ptr_ty.fn_type(&[ptr_ty.into()], false));
                let tagged = self.builder.call(f, &[s.into()], "ir_shared_get").try_as_basic_value().unwrap_basic();
                if !Self::is_union_type(ret_ty) && *ret_ty != Type::Null {
                    self.unbox_tagged_val_to_type(tagged, ret_ty)
                } else { tagged }
            }
            // set(s, v) → lin_shared_set(s, boxed v) → Null.
            Intrinsic::SharedSet => {
                let s = args.first().copied().unwrap_or_else(|| ptr_ty.const_null().into());
                let v = args.get(1).copied().unwrap_or_else(|| ptr_ty.const_null().into());
                let v_ty = arg_tys.get(1).cloned().unwrap_or(Type::Null);
                let v_boxed = if Self::is_union_type(&v_ty) || v.is_pointer_value() { v } else { self.box_value(v, &v_ty) };
                let f = self.get_or_declare_fn("lin_shared_set", ptr_ty.fn_type(&[ptr_ty.into(), ptr_ty.into()], false));
                self.builder.call(f, &[s.into(), v_boxed.into()], "ir_shared_set").try_as_basic_value().unwrap_basic()
            }
            // withLock(s, f) → lin_shared_with_lock(s, fclosure) → boxed result (unboxed to ret).
            Intrinsic::SharedWithLock => {
                let s = args.first().copied().unwrap_or_else(|| ptr_ty.const_null().into());
                let func = args.get(1).copied().unwrap_or_else(|| ptr_ty.const_null().into());
                let func_ty = arg_tys.get(1).cloned().unwrap_or(Type::Null);
                let func = if Self::is_union_type(&func_ty) && func.is_pointer_value() {
                    self.builder.call(self.rt.unbox_ptr, &[func.into()], "ir_wl_cls").try_as_basic_value().unwrap_basic()
                } else { func };
                let f = self.get_or_declare_fn("lin_shared_with_lock", ptr_ty.fn_type(&[ptr_ty.into(), ptr_ty.into()], false));
                let tagged = self.builder.call(f, &[s.into(), func.into()], "ir_shared_wl").try_as_basic_value().unwrap_basic();
                if !Self::is_union_type(ret_ty) && *ret_ty != Type::Null {
                    self.unbox_tagged_val_to_type(tagged, ret_ty)
                } else { tagged }
            }
            // frozen(v) → deep immortal seal of v's graph; returns v with its ORIGINAL type
            // (readers use the plain type transparently). For a concrete heap value (raw
            // LinArray*/LinObject*/LinString*) we box it just to hand a TaggedVal* to lin_freeze,
            // which seals the graph in place; we then return the ORIGINAL value `v` unchanged.
            // For a boxed/union value we freeze it directly and return the same box.
            Intrinsic::Freeze => {
                let v = args.first().copied().unwrap_or_else(|| ptr_ty.const_null().into());
                let v_ty = arg_tys.first().cloned().unwrap_or(Type::Null);
                let freeze_fn = self.get_or_declare_fn("lin_freeze", ptr_ty.fn_type(&[ptr_ty.into()], false));
                if Self::is_union_type(&v_ty) {
                    // Already a boxed TaggedVal* — freeze it and return the same box.
                    self.builder.call(freeze_fn, &[v.into()], "ir_freeze").try_as_basic_value().unwrap_basic()
                } else if v.is_pointer_value() {
                    // Concrete heap value: box transiently to seal the graph, free the transient
                    // box shell, return the original concrete pointer (its graph is now frozen).
                    let boxed = self.box_value(v, &v_ty);
                    self.builder.call(freeze_fn, &[boxed.into()], "ir_freeze");
                    if boxed.is_pointer_value() {
                        self.builder.call(self.rt.tagged_release, &[boxed.into()], "");
                    }
                    v
                } else {
                    // Scalar: nothing to freeze; return as-is.
                    v
                }
            }
            // w.request(msg) → lin_worker_request(w, boxed msg) → result (unboxed if concrete).
            Intrinsic::Request => {
                let worker_boxed = args.first().copied().unwrap_or_else(|| ptr_ty.const_null().into());
                let worker = self.unbox_handle(worker_boxed);
                let msg = args.get(1).copied().unwrap_or_else(|| ptr_ty.const_null().into());
                let msg_ty = arg_tys.get(1).cloned().unwrap_or(Type::Null);
                let msg_ptr = if Self::is_union_type(&msg_ty) || msg.is_pointer_value() { msg } else { self.box_value(msg, &msg_ty) };
                let req_fn = self.get_or_declare_fn("lin_worker_request",
                    ptr_ty.fn_type(&[ptr_ty.into(), ptr_ty.into()], false));
                let tagged = self.builder.call(req_fn, &[worker.into(), msg_ptr.into()], "ir_w_reply").try_as_basic_value().unwrap_basic();
                if !Self::is_union_type(ret_ty) && *ret_ty != Type::Null {
                    self.unbox_tagged_val_to_type(tagged, ret_ty)
                } else { tagged }
            }
            // w.message(msg) → lin_worker_message(w, boxed msg) (void).
            Intrinsic::Message => {
                let worker_boxed = args.first().copied().unwrap_or_else(|| ptr_ty.const_null().into());
                let worker = self.unbox_handle(worker_boxed);
                let msg = args.get(1).copied().unwrap_or_else(|| ptr_ty.const_null().into());
                let msg_ty = arg_tys.get(1).cloned().unwrap_or(Type::Null);
                let msg_ptr = if Self::is_union_type(&msg_ty) || msg.is_pointer_value() { msg } else { self.box_value(msg, &msg_ty) };
                let msg_fn = self.get_or_declare_fn("lin_worker_message",
                    self.context.void_type().fn_type(&[ptr_ty.into(), ptr_ty.into()], false));
                self.builder.call(msg_fn, &[worker.into(), msg_ptr.into()], "");
                ptr_ty.const_null().into()
            }
            // w.close() → lin_worker_close(w) (void).
            Intrinsic::Close => {
                let worker_boxed = args.first().copied().unwrap_or_else(|| ptr_ty.const_null().into());
                let worker = self.unbox_handle(worker_boxed);
                let close_fn = self.get_or_declare_fn("lin_worker_close",
                    self.context.void_type().fn_type(&[ptr_ty.into()], false));
                self.builder.call(close_fn, &[worker.into()], "");
                ptr_ty.const_null().into()
            }
            // openRead(path) → lin_fs_open(path) → boxed Stream<UInt8[]> (TAG_STREAM) OR a
            // boxed Error object. Both are TaggedVal*, so the union result needs no unboxing.
            // The path arg is a String (raw LinString* or boxed Str); pass it through — the
            // runtime's resolve_lin_str tolerates both shapes (mirrors the lin_fs_* family).
            Intrinsic::StreamOpen => {
                let path = args.first().copied().unwrap_or_else(|| ptr_ty.const_null().into());
                let f = self.get_or_declare_fn("lin_fs_open", ptr_ty.fn_type(&[ptr_ty.into()], false));
                self.builder.call(f, &[path.into()], "ir_stream_open").try_as_basic_value().unwrap_basic()
            }
            // read(stream) → lin_stream_read(stream) → boxed `T | Null | Error` (chunk = flat
            // UInt8[] array box, EOF = null, error = Error object). Always a TaggedVal*; the
            // result type is a union, so leave it boxed for the caller to inspect.
            Intrinsic::StreamRead => {
                let s = args.first().copied().unwrap_or_else(|| ptr_ty.const_null().into());
                let f = self.get_or_declare_fn("lin_stream_read", ptr_ty.fn_type(&[ptr_ty.into()], false));
                self.builder.call(f, &[s.into()], "ir_stream_read").try_as_basic_value().unwrap_basic()
            }
            // close(stream) → lin_stream_close(stream) → Null (idempotent).
            Intrinsic::StreamClose => {
                let s = args.first().copied().unwrap_or_else(|| ptr_ty.const_null().into());
                let f = self.get_or_declare_fn("lin_stream_close", ptr_ty.fn_type(&[ptr_ty.into()], false));
                self.builder.call(f, &[s.into()], "ir_stream_close").try_as_basic_value().unwrap_basic()
            }
            // Lazy adapters returning a new Stream (TaggedVal*(TAG_STREAM)). map/filter pass a
            // transform closure (unboxed to a raw closure ptr if it arrived boxed); take/chunks an
            // Int64 count; lines no extra arg; write a String path. All return a boxed Stream.
            Intrinsic::StreamMap | Intrinsic::StreamFilter => {
                let s = args.first().copied().unwrap_or_else(|| ptr_ty.const_null().into());
                let func = args.get(1).copied().unwrap_or_else(|| ptr_ty.const_null().into());
                let func_ty = arg_tys.get(1).cloned().unwrap_or(Type::Null);
                // The runtime tolerates either a boxed Function or a raw closure ptr; pass through.
                let func = if Self::is_union_type(&func_ty) && func.is_pointer_value() {
                    self.builder.call(self.rt.unbox_ptr, &[func.into()], "ir_stream_cls").try_as_basic_value().unwrap_basic()
                } else { func };
                let name = if matches!(intrinsic, Intrinsic::StreamMap) { "lin_stream_map" } else { "lin_stream_filter" };
                let fnv = self.get_or_declare_fn(name, ptr_ty.fn_type(&[ptr_ty.into(), ptr_ty.into()], false));
                self.builder.call(fnv, &[s.into(), func.into()], "ir_stream_adapt").try_as_basic_value().unwrap_basic()
            }
            Intrinsic::StreamTake | Intrinsic::StreamChunks | Intrinsic::StreamLines => {
                // All three take (stream, i64): take→count, chunks→size, lines→max line bytes
                // (≤0 selects the default cap). A one-arg `lines(s)` call lowers `n` to 0 here.
                let i64_ty = self.context.i64_type();
                let s = args.first().copied().unwrap_or_else(|| ptr_ty.const_null().into());
                let n = args.get(1).copied().unwrap_or_else(|| i64_ty.const_zero().into());
                let n_i64 = if n.is_int_value() {
                    self.builder.int_s_extend_or_bit_cast(n.into_int_value(), i64_ty, "ir_stream_n")
                } else { i64_ty.const_zero() };
                let name = match intrinsic {
                    Intrinsic::StreamTake => "lin_stream_take",
                    Intrinsic::StreamChunks => "lin_stream_chunks",
                    _ => "lin_stream_lines",
                };
                let fnv = self.get_or_declare_fn(name, ptr_ty.fn_type(&[ptr_ty.into(), i64_ty.into()], false));
                self.builder.call(fnv, &[s.into(), n_i64.into()], "ir_stream_take_chunks_lines").try_as_basic_value().unwrap_basic()
            }
            // drop(stream, n): (stream, i64) → Stream. Mirrors take's int-count handling.
            Intrinsic::StreamDrop => {
                let i64_ty = self.context.i64_type();
                let s = args.first().copied().unwrap_or_else(|| ptr_ty.const_null().into());
                let n = args.get(1).copied().unwrap_or_else(|| i64_ty.const_zero().into());
                let n_i64 = if n.is_int_value() {
                    self.builder.int_s_extend_or_bit_cast(n.into_int_value(), i64_ty, "ir_stream_drop_n")
                } else { i64_ty.const_zero() };
                let fnv = self.get_or_declare_fn("lin_stream_drop", ptr_ty.fn_type(&[ptr_ty.into(), i64_ty.into()], false));
                self.builder.call(fnv, &[s.into(), n_i64.into()], "ir_stream_drop").try_as_basic_value().unwrap_basic()
            }
            // Closure-bearing lazy adapters: (stream, closure) → Stream. Unbox a boxed Function
            // arg to a raw closure ptr exactly like map/filter.
            Intrinsic::StreamTakeWhile | Intrinsic::StreamDropWhile | Intrinsic::StreamFlatMap => {
                let s = args.first().copied().unwrap_or_else(|| ptr_ty.const_null().into());
                let func = args.get(1).copied().unwrap_or_else(|| ptr_ty.const_null().into());
                let func_ty = arg_tys.get(1).cloned().unwrap_or(Type::Null);
                let func = if Self::is_union_type(&func_ty) && func.is_pointer_value() {
                    self.builder.call(self.rt.unbox_ptr, &[func.into()], "ir_stream_cls").try_as_basic_value().unwrap_basic()
                } else { func };
                let name = match intrinsic {
                    Intrinsic::StreamTakeWhile => "lin_stream_take_while",
                    Intrinsic::StreamDropWhile => "lin_stream_drop_while",
                    _ => "lin_stream_flat_map",
                };
                let fnv = self.get_or_declare_fn(name, ptr_ty.fn_type(&[ptr_ty.into(), ptr_ty.into()], false));
                self.builder.call(fnv, &[s.into(), func.into()], "ir_stream_adapt2").try_as_basic_value().unwrap_basic()
            }
            // flatten(stream): (stream) → Stream.
            Intrinsic::StreamFlatten => {
                let s = args.first().copied().unwrap_or_else(|| ptr_ty.const_null().into());
                let fnv = self.get_or_declare_fn("lin_stream_flatten", ptr_ty.fn_type(&[ptr_ty.into()], false));
                self.builder.call(fnv, &[s.into()], "ir_stream_flatten").try_as_basic_value().unwrap_basic()
            }
            // manifest(stream)/files(stream): (stream) → Stream<Object>. Single-stream-arg adapters
            // (the parent is moved in), modelled on flatten.
            Intrinsic::StreamManifest | Intrinsic::StreamFiles => {
                let s = args.first().copied().unwrap_or_else(|| ptr_ty.const_null().into());
                let name = match intrinsic {
                    Intrinsic::StreamManifest => "lin_stream_manifest",
                    _ => "lin_stream_files",
                };
                let fnv = self.get_or_declare_fn(name, ptr_ty.fn_type(&[ptr_ty.into()], false));
                self.builder.call(fnv, &[s.into()], "ir_stream_archive_adapt").try_as_basic_value().unwrap_basic()
            }
            // Streaming compression byte-adapters (std/compress): (stream) → Stream. Each wraps an
            // upstream Stream<UInt8[]> in a (de)compressing adapter. Single-stream-arg shape,
            // modelled on flatten.
            Intrinsic::StreamGunzip
            | Intrinsic::StreamGzip
            | Intrinsic::StreamInflate
            | Intrinsic::StreamDeflate => {
                let s = args.first().copied().unwrap_or_else(|| ptr_ty.const_null().into());
                let name = match intrinsic {
                    Intrinsic::StreamGunzip => "lin_stream_gunzip",
                    Intrinsic::StreamGzip => "lin_stream_gzip",
                    Intrinsic::StreamInflate => "lin_stream_inflate",
                    _ => "lin_stream_deflate",
                };
                let fnv = self.get_or_declare_fn(name, ptr_ty.fn_type(&[ptr_ty.into()], false));
                self.builder.call(fnv, &[s.into()], "ir_stream_compress").try_as_basic_value().unwrap_basic()
            }
            // concat(a, b): (stream, stream) → Stream. BOTH streams are moved into the adapter.
            Intrinsic::StreamConcat => {
                let a = args.first().copied().unwrap_or_else(|| ptr_ty.const_null().into());
                let b = args.get(1).copied().unwrap_or_else(|| ptr_ty.const_null().into());
                let fnv = self.get_or_declare_fn("lin_stream_concat", ptr_ty.fn_type(&[ptr_ty.into(), ptr_ty.into()], false));
                self.builder.call(fnv, &[a.into(), b.into()], "ir_stream_concat").try_as_basic_value().unwrap_basic()
            }
            Intrinsic::StreamWrite | Intrinsic::StreamWriteLines => {
                let s = args.first().copied().unwrap_or_else(|| ptr_ty.const_null().into());
                let path = args.get(1).copied().unwrap_or_else(|| ptr_ty.const_null().into());
                let name = match intrinsic {
                    Intrinsic::StreamWriteLines => "lin_stream_write_lines",
                    _ => "lin_stream_write",
                };
                let fnv = self.get_or_declare_fn(name, ptr_ty.fn_type(&[ptr_ty.into(), ptr_ty.into()], false));
                self.builder.call(fnv, &[s.into(), path.into()], "ir_stream_write").try_as_basic_value().unwrap_basic()
            }
            // Terminal drivers (sync, calling thread). Each returns a boxed union value
            // (Null|Error / UInt8[]|Error / String|Error) — leave it boxed.
            Intrinsic::StreamDrain | Intrinsic::StreamCollect | Intrinsic::StreamReadText => {
                let s = args.first().copied().unwrap_or_else(|| ptr_ty.const_null().into());
                let name = match intrinsic {
                    Intrinsic::StreamDrain => "lin_stream_drain",
                    Intrinsic::StreamCollect => "lin_stream_collect",
                    _ => "lin_stream_read_text",
                };
                let fnv = self.get_or_declare_fn(name, ptr_ty.fn_type(&[ptr_ty.into()], false));
                self.builder.call(fnv, &[s.into()], "ir_stream_term").try_as_basic_value().unwrap_basic()
            }
            // Unified OS sources → boxed Stream<UInt8[]>. tcp takes an i32 fd; stdout an i64
            // handle; stdin takes no args.
            Intrinsic::StreamTcp => {
                let i32_ty = self.context.i32_type();
                let fd = args.first().copied().unwrap_or_else(|| i32_ty.const_zero().into());
                let fd_i32 = if fd.is_int_value() {
                    self.builder.int_s_extend_or_bit_cast(fd.into_int_value(), i32_ty, "ir_tcp_fd")
                } else { i32_ty.const_zero() };
                let fnv = self.get_or_declare_fn("lin_net_tcp_stream", ptr_ty.fn_type(&[i32_ty.into()], false));
                self.builder.call(fnv, &[fd_i32.into()], "ir_stream_tcp").try_as_basic_value().unwrap_basic()
            }
            Intrinsic::StreamStdout => {
                let i64_ty = self.context.i64_type();
                let h = args.first().copied().unwrap_or_else(|| i64_ty.const_zero().into());
                let h_i64 = if h.is_int_value() {
                    self.builder.int_s_extend_or_bit_cast(h.into_int_value(), i64_ty, "ir_proc_h")
                } else { i64_ty.const_zero() };
                let fnv = self.get_or_declare_fn("lin_process_stdout_stream", ptr_ty.fn_type(&[i64_ty.into()], false));
                self.builder.call(fnv, &[h_i64.into()], "ir_stream_stdout").try_as_basic_value().unwrap_basic()
            }
            Intrinsic::StreamStdin => {
                let fnv = self.get_or_declare_fn("lin_io_stdin_stream", ptr_ty.fn_type(&[], false));
                self.builder.call(fnv, &[], "ir_stream_stdin").try_as_basic_value().unwrap_basic()
            }
            // .promise() → lin_stream_promise(s) → *LinPromise, boxed as TAG_PROMISE so it
            // round-trips through await/parallel like any other promise. The stream is MOVED in
            // (the IR suppressed the caller's release).
            Intrinsic::StreamPromise => {
                let s = args.first().copied().unwrap_or_else(|| ptr_ty.const_null().into());
                let fnv = self.get_or_declare_fn("lin_stream_promise", ptr_ty.fn_type(&[ptr_ty.into()], false));
                let raw = self.builder.call(fnv, &[s.into()], "ir_stream_promise").try_as_basic_value().unwrap_basic();
                self.box_promise(raw)
            }
            // .for(fn) over a Stream → lin_stream_for(stream, closure) → boxed Null | Error. The
            // body closure may arrive boxed; unbox to the raw closure ptr like map/filter.
            Intrinsic::StreamFor => {
                let s = args.first().copied().unwrap_or_else(|| ptr_ty.const_null().into());
                let func = args.get(1).copied().unwrap_or_else(|| ptr_ty.const_null().into());
                let func_ty = arg_tys.get(1).cloned().unwrap_or(Type::Null);
                let func = if Self::is_union_type(&func_ty) && func.is_pointer_value() {
                    self.builder.call(self.rt.unbox_ptr, &[func.into()], "ir_streamfor_cls").try_as_basic_value().unwrap_basic()
                } else { func };
                let fnv = self.get_or_declare_fn("lin_stream_for", ptr_ty.fn_type(&[ptr_ty.into(), ptr_ty.into()], false));
                self.builder.call(fnv, &[s.into(), func.into()], "ir_stream_for").try_as_basic_value().unwrap_basic()
            }
            // untar(stream, body) → lin_stream_untar(stream, closure) → boxed Null | Error. A TERMINAL
            // driver taking a 2-arg body (meta, data); the closure may arrive boxed, unbox it like for.
            Intrinsic::StreamUntar => {
                let s = args.first().copied().unwrap_or_else(|| ptr_ty.const_null().into());
                let func = args.get(1).copied().unwrap_or_else(|| ptr_ty.const_null().into());
                let func_ty = arg_tys.get(1).cloned().unwrap_or(Type::Null);
                let func = if Self::is_union_type(&func_ty) && func.is_pointer_value() {
                    self.builder.call(self.rt.unbox_ptr, &[func.into()], "ir_untar_cls").try_as_basic_value().unwrap_basic()
                } else { func };
                let fnv = self.get_or_declare_fn("lin_stream_untar", ptr_ty.fn_type(&[ptr_ty.into(), ptr_ty.into()], false));
                self.builder.call(fnv, &[s.into(), func.into()], "ir_stream_untar").try_as_basic_value().unwrap_basic()
            }
            // Stream terminals taking a single predicate/body closure: find/some/every/while →
            // (stream, closure) → boxed (X | Error). The closure may arrive boxed; unbox it.
            Intrinsic::StreamFind | Intrinsic::StreamSome | Intrinsic::StreamEvery | Intrinsic::StreamWhile => {
                let s = args.first().copied().unwrap_or_else(|| ptr_ty.const_null().into());
                let func = args.get(1).copied().unwrap_or_else(|| ptr_ty.const_null().into());
                let func_ty = arg_tys.get(1).cloned().unwrap_or(Type::Null);
                let func = if Self::is_union_type(&func_ty) && func.is_pointer_value() {
                    self.builder.call(self.rt.unbox_ptr, &[func.into()], "ir_streamterm_cls").try_as_basic_value().unwrap_basic()
                } else { func };
                let name = match intrinsic {
                    Intrinsic::StreamFind => "lin_stream_find",
                    Intrinsic::StreamSome => "lin_stream_some",
                    Intrinsic::StreamEvery => "lin_stream_every",
                    _ => "lin_stream_while",
                };
                let fnv = self.get_or_declare_fn(name, ptr_ty.fn_type(&[ptr_ty.into(), ptr_ty.into()], false));
                self.builder.call(fnv, &[s.into(), func.into()], "ir_stream_term1").try_as_basic_value().unwrap_basic()
            }
            // reduce(stream, init, f) → (stream, boxed init, closure) → boxed (U | Error). The init
            // accumulator is passed boxed (TaggedVal*); the closure may arrive boxed (unbox it).
            Intrinsic::StreamReduce => {
                let s = args.first().copied().unwrap_or_else(|| ptr_ty.const_null().into());
                let init = args.get(1).copied().unwrap_or_else(|| ptr_ty.const_null().into());
                let init_ty = arg_tys.get(1).cloned().unwrap_or(Type::Null);
                // The runtime takes the accumulator as a boxed TaggedVal*; box a concrete/scalar
                // init. An already-boxed union/Json init is passed through (no double-box).
                let init = if Self::is_union_type(&init_ty) {
                    init
                } else {
                    self.box_value(init, &init_ty)
                };
                let func = args.get(2).copied().unwrap_or_else(|| ptr_ty.const_null().into());
                let func_ty = arg_tys.get(2).cloned().unwrap_or(Type::Null);
                let func = if Self::is_union_type(&func_ty) && func.is_pointer_value() {
                    self.builder.call(self.rt.unbox_ptr, &[func.into()], "ir_streamreduce_cls").try_as_basic_value().unwrap_basic()
                } else { func };
                let fnv = self.get_or_declare_fn("lin_stream_reduce", ptr_ty.fn_type(&[ptr_ty.into(), ptr_ty.into(), ptr_ty.into()], false));
                self.builder.call(fnv, &[s.into(), init.into(), func.into()], "ir_stream_reduce").try_as_basic_value().unwrap_basic()
            }
            // lin_object_set(obj, key, val) => Null. Unbox obj→LinObject*, key→LinString*,
            // box val→TaggedVal*, then call the runtime. Mirrors the AST handler.
            Intrinsic::ObjectSetDyn => {
                if args.len() >= 3 {
                    let obj_ty = arg_tys.first().cloned().unwrap_or(Type::Null);
                    let key_ty = arg_tys.get(1).cloned().unwrap_or(Type::Null);
                    let val_ty = arg_tys.get(2).cloned().unwrap_or(Type::Null);
                    let obj_ptr = self.ir_as_raw_ptr(args[0], &obj_ty);
                    let key_ptr = self.ir_as_raw_ptr(args[1], &key_ty);
                    self.emit_object_set(obj_ptr, key_ptr, args[2], &val_ty);
                }
                ptr_ty.const_null().into()
            }
            // lin_array_set(arr, idx, val) => Null. Unbox arr→LinArray*, idx→i64, box val.
            Intrinsic::ArraySetDyn => {
                if args.len() >= 3 {
                    let arr_ty = arg_tys.first().cloned().unwrap_or(Type::Null);
                    let val_ty = arg_tys.get(2).cloned().unwrap_or(Type::Null);
                    let arr_ptr = if Self::is_union_type(&arr_ty) {
                        self.builder.call(self.rt.unbox_ptr, &[args[0].into()], "set_arr").try_as_basic_value().unwrap_basic()
                    } else { args[0] };
                    let idx_i64 = self.index_value_to_i64(args[1]);
                    self.emit_array_set(arr_ptr, idx_i64, args[2], &val_ty);
                }
                ptr_ty.const_null().into()
            }
            // lin_keys(obj) => String[]. Unbox to LinObject*, call lin_object_keys.
            Intrinsic::Keys => {
                if let Some(&obj_v) = args.first() {
                    let arg_ty = arg_tys.first().cloned().unwrap_or(Type::Null);
                    let obj_ptr = self.ir_as_raw_ptr(obj_v, &arg_ty);
                    let f = self.get_or_declare_fn("lin_object_keys",
                        ptr_ty.fn_type(&[ptr_ty.into()], false));
                    self.builder.call(f, &[obj_ptr.into()], "ir_keys").try_as_basic_value().unwrap_basic()
                } else { ptr_ty.const_null().into() }
            }
            // lin_value_key(val) => String. Box val→TaggedVal*, call lin_value_key.
            Intrinsic::ValueKey => {
                if let Some(&v) = args.first() {
                    let arg_ty = arg_tys.first().cloned().unwrap_or(Type::Null);
                    let tagged = if Self::is_union_type(&arg_ty) && v.is_pointer_value() {
                        v
                    } else {
                        self.box_value(v, &arg_ty)
                    };
                    let vk_fn = self.get_or_declare_fn("lin_value_key",
                        ptr_ty.fn_type(&[ptr_ty.into()], false));
                    self.builder.call(vk_fn, &[tagged.into()], "ir_vkey").try_as_basic_value().unwrap_basic()
                } else { ptr_ty.const_null().into() }
            }
            // lin_to_json(val) => String. The arg is declared Json, so it may already be a boxed
            // TaggedVal* (union/Json) or a concrete value needing boxing. Box if concrete, then
            // call the read-only recursive serializer. lin_to_json BORROWS its input (no RC
            // balancing needed) and returns a fresh owned String.
            Intrinsic::ToJson => {
                if let Some(&v) = args.first() {
                    let arg_ty = arg_tys.first().cloned().unwrap_or(Type::Null);
                    // Json/union/TypeVar values are already boxed TaggedVal* — pass through;
                    // a concrete value (Int, Str, Array, Object, …) needs boxing first.
                    let already_boxed = (Self::is_union_type(&arg_ty)
                        || matches!(arg_ty, Type::TypeVar(_)))
                        && v.is_pointer_value();
                    let tagged = if already_boxed {
                        v
                    } else {
                        self.box_value(v, &arg_ty)
                    };
                    let tj_fn = self.get_or_declare_fn("lin_to_json",
                        ptr_ty.fn_type(&[ptr_ty.into()], false));
                    self.builder.call(tj_fn, &[tagged.into()], "ir_tojson").try_as_basic_value().unwrap_basic()
                } else { ptr_ty.const_null().into() }
            }
            // lin_array_allocate(n) => T[]. When the result element type is a CONCRETE scalar
            // (e.g. a monomorphized `Int32[]`), allocate a FLAT array so the producer's
            // representation matches the concrete-typed reader (which already reads flat via
            // `lin_flat_array_get_<suffix>`). `lin_array_allocate(n)` yields a length-`n` array
            // of default slots, so the flat path uses the filled-with-zero allocator to pre-size
            // to `n` — a subsequent `result[i] = v` index-set then lands in-bounds, exactly like
            // the tagged `alloc_null(n)`. For Json/TypeVar/union or heap (String, …) element
            // types stay TAGGED (existing behaviour). The READ path already dispatches on the
            // static element type; this aligns the WRITE path so a flat-allocated array is only
            // ever flat-written and flat-read (alloc/set/get/length/release all agree).
            Intrinsic::ArrayAllocate => {
                let i64_ty = self.context.i64_type();
                let n_i64 = self.ir_n_to_i64(args.first().copied(), arg_tys.first());
                // Only a uniform `Array(T)` has a single element type. `FixedArray` is a
                // heterogeneous tuple shape (Vec<Type>) and never a flat scalar array.
                let elem_ty = match ret_ty {
                    Type::Array(inner) => Some((**inner).clone()),
                    _ => None,
                };
                if let Some(elem) = elem_ty.filter(Self::is_flat_scalar) {
                    let suffix = Self::flat_suffix(&elem);
                    let fn_name = format!("lin_flat_array_alloc_filled_{}", suffix);
                    let llvm_elem_ty = self.llvm_type(&elem);
                    let zero: BasicValueEnum<'ctx> = if llvm_elem_ty.is_float_type() {
                        llvm_elem_ty.into_float_type().const_zero().into()
                    } else {
                        llvm_elem_ty.into_int_type().const_zero().into()
                    };
                    let alloc_fn = self.get_or_declare_fn(&fn_name,
                        ptr_ty.fn_type(&[i64_ty.into(), llvm_elem_ty.into()], false));
                    return self.builder.call(alloc_fn, &[n_i64.into(), zero.into()], "ir_alloc_flat").try_as_basic_value().unwrap_basic();
                }
                let alloc_fn = self.get_or_declare_fn("lin_array_alloc_null",
                    ptr_ty.fn_type(&[i64_ty.into()], false));
                self.builder.call(alloc_fn, &[n_i64.into()], "ir_alloc_arr").try_as_basic_value().unwrap_basic()
            }
            // lin_array_allocate_filled(n, val) => T[]. Flat fast path for scalars; otherwise
            // null-allocate then fill each slot with the boxed value.
            Intrinsic::ArrayAllocateFilled => {
                let i64_ty = self.context.i64_type();
                let n_i64 = self.ir_n_to_i64(args.first().copied(), arg_tys.first());
                let fill_ty = arg_tys.get(1).cloned().unwrap_or(Type::Null);
                let fill_val = args.get(1).copied().unwrap_or_else(|| ptr_ty.const_null().into());
                if Self::is_flat_scalar(&fill_ty) {
                    let suffix = Self::flat_suffix(&fill_ty);
                    let fn_name = format!("lin_flat_array_alloc_filled_{}", suffix);
                    let llvm_elem_ty = self.llvm_type(&fill_ty);
                    let alloc_fn = self.get_or_declare_fn(&fn_name,
                        ptr_ty.fn_type(&[i64_ty.into(), llvm_elem_ty.into()], false));
                    self.builder.call(alloc_fn, &[n_i64.into(), fill_val.into()], "ir_fillflat").try_as_basic_value().unwrap_basic()
                } else {
                    let alloc_fn = self.get_or_declare_fn("lin_array_alloc_null",
                        ptr_ty.fn_type(&[i64_ty.into()], false));
                    let arr = self.builder.call(alloc_fn, &[n_i64.into()], "ir_fillgen").try_as_basic_value().unwrap_basic();
                    // The fill must reach `lin_array_set` as a `TaggedVal*`. When `fill_ty` is a
                    // union/Json type the value arrives ALREADY boxed as a `TaggedVal*` (e.g. the
                    // stdlib wrapper's `fill: Json` param), so pass it through directly —
                    // re-wrapping it via `build_tagged_val_alloca` would build a NULL-tagged box
                    // around the box pointer (every slot would read back as `null`). Only a
                    // concrete unboxed scalar needs a fresh stack `TaggedVal` built around it.
                    let is_boxed_union = Self::is_union_type(&fill_ty) && fill_val.is_pointer_value();
                    let tagged = if is_boxed_union {
                        fill_val.into_pointer_value()
                    } else {
                        self.build_tagged_val_alloca(&fill_val, &fill_ty)
                    };
                    // Does the fill carry a heap (refcounted) payload? A boxed union/Json arrives as
                    // a TaggedVal* that may wrap one; a CONCRETE heap type (String/Array/Object/
                    // Function — `ty_is_concrete_rc`) also does. Either way the SAME heap value is
                    // stored into all n slots, and `lin_array_release` will release each slot's
                    // payload, so each slot needs its OWN reference. A flat scalar / Bool / Null fill
                    // carries no heap payload and needs no retain. (Before generics, `arrayAllocateFilled`
                    // was `(n, fill: Json)`, so this was ALWAYS the boxed-union case; a monomorphized
                    // concrete heap fill like `arrayAllocateFilled(3, [1,2])` now hits the concrete-rc
                    // branch — without this retain, releasing the result double-freed the shared array.)
                    let fill_has_heap_payload = is_boxed_union || Self::ty_is_concrete_rc(&fill_ty);
                    let set_fn = self.get_or_declare_fn("lin_array_set",
                        self.context.void_type().fn_type(&[ptr_ty.into(), i64_ty.into(), ptr_ty.into()], false));
                    // `lin_array_set` raw-copies the 16-byte TaggedVal into the slot WITHOUT
                    // retaining the inner payload (it CONSUMES one owning reference). The caller's
                    // original borrowed reference is left intact for its own scope.
                    let retain_fn = self.get_or_declare_fn("lin_tagged_retain",
                        self.context.void_type().fn_type(&[ptr_ty.into()], false));
                    let llvm_fn = self.builder.get_insert_block().unwrap().get_parent().unwrap();
                    let i_alloc = self.builder.alloca(i64_ty, "ir_fi");
                    self.builder.store(i_alloc, i64_ty.const_zero());
                    let check = self.context.append_basic_block(llvm_fn, "ir_fill_check");
                    let body = self.context.append_basic_block(llvm_fn, "ir_fill_body");
                    let exit = self.context.append_basic_block(llvm_fn, "ir_fill_exit");
                    self.builder.unconditional_branch(check);
                    self.builder.position_at_end(check);
                    let cur = self.builder.load(i64_ty, i_alloc, "ir_fi_v").into_int_value();
                    let cond = self.builder.int_compare(inkwell::IntPredicate::SLT, cur, n_i64, "ir_fill_cond");
                    self.builder.conditional_branch(cond, body, exit);
                    self.builder.position_at_end(body);
                    if fill_has_heap_payload {
                        self.builder.call(retain_fn, &[tagged.into()], "");
                    }
                    self.builder.call(set_fn, &[arr.into(), cur.into(), tagged.into()], "");
                    let next = self.builder.int_add(cur, i64_ty.const_int(1, false), "ir_fi_n");
                    self.builder.store(i_alloc, next);
                    self.builder.unconditional_branch(check);
                    self.builder.position_at_end(exit);
                    arr
                }
            }
            // fromJson(value, descriptor) => T | Error (ADR-031). Emit the compile-time schema
            // descriptor as a static i8 global, then call the generic runtime walker. `args[0]`
            // is the (already boxed-to-Json) input value. The result is owned by the caller
            // (the input retained on success, or a fresh Error).
            Intrinsic::FromJson { target, named_defs } => {
                let value = args.first().copied().unwrap_or_else(|| ptr_ty.const_null().into());
                let desc_ptr = self.emit_from_json_descriptor(target, named_defs);
                let from_json_fn = self.get_or_declare_fn(
                    "lin_from_json",
                    ptr_ty.fn_type(&[ptr_ty.into(), ptr_ty.into()], false),
                );
                self.builder
                    .call(from_json_fn, &[value.into(), desc_ptr.into()], "from_json")
                    .try_as_basic_value()
                    .unwrap_basic()
            }
            _ => ptr_ty.const_null().into(),
        }
    }

    /// Emit the `fromJson` schema descriptor for `target` as a static, constant `i8` global and
    /// return a pointer to its first byte. The descriptor is a self-contained, position-relative
    /// bytecode the runtime `lin_from_json` walker interprets in lockstep with the value. Object
    /// keys are inlined (length-prefixed UTF-8); recursion uses absolute byte offsets into the
    /// same blob, so recursive/cyclic types terminate. `named_defs` supplies the resolved bodies
    /// of every `Named` type reachable from `target` (codegen has no type environment).
    pub(crate) fn emit_from_json_descriptor(
        &self,
        target: &Type,
        named_defs: &[(String, Type)],
    ) -> inkwell::values::PointerValue<'ctx> {
        let named: std::collections::HashMap<String, Type> =
            named_defs.iter().cloned().collect();
        let mut enc = DescEncoder::new(&named);
        enc.encode(target);
        let bytes = enc.finish();

        let i8_ty = self.context.i8_type();
        let arr_ty = i8_ty.array_type(bytes.len() as u32);
        let consts: Vec<_> = bytes.iter().map(|&b| i8_ty.const_int(b as u64, false)).collect();
        let const_arr = i8_ty.const_array(&consts);
        let global = self.module.add_global(arr_ty, None, "lin_from_json_desc");
        global.set_constant(true);
        global.set_initializer(&const_arr);
        global.as_pointer_value()
    }

    /// Box a raw `*LinPromise` (from a runtime spawn/combinator) into a TaggedVal*(TAG_PROMISE)
    /// so it round-trips through TypeVar slots and arrays like any other tagged value.
    fn box_promise(&mut self, raw: BasicValueEnum<'ctx>) -> BasicValueEnum<'ctx> {
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let f = self.get_or_declare_fn("lin_box_promise", ptr_ty.fn_type(&[ptr_ty.into()], false));
        self.builder.call(f, &[raw.into()], "ir_box_promise").try_as_basic_value().unwrap_basic()
    }

    /// Unbox a boxed promise (TAG_PROMISE) back to the raw `*LinPromise` for await/combinators.
    fn unbox_promise(&mut self, boxed: BasicValueEnum<'ctx>) -> BasicValueEnum<'ctx> {
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let f = self.get_or_declare_fn("lin_unbox_promise", ptr_ty.fn_type(&[ptr_ty.into()], false));
        self.builder.call(f, &[boxed.into()], "ir_unbox_promise").try_as_basic_value().unwrap_basic()
    }

    /// Extract `(fn_ptr, env_ptr, has_env)` from a (possibly boxed) closure value. A null/
    /// non-pointer value yields `(null, null, 0)`. Used by the Worker intrinsic to pull the
    /// handler and onClose closures apart for the runtime.
    fn extract_closure_fields(
        &mut self,
        cls: BasicValueEnum<'ctx>,
        cls_ty: &Type,
    ) -> (BasicValueEnum<'ctx>, BasicValueEnum<'ctx>, BasicValueEnum<'ctx>) {
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i8_ty = self.context.i8_type();
        if !cls.is_pointer_value() {
            return (ptr_ty.const_null().into(), ptr_ty.const_null().into(), i8_ty.const_zero().into());
        }
        let cls_ptr = if Self::is_union_type(cls_ty) {
            self.builder.call(self.rt.unbox_ptr, &[cls.into()], "ir_cls_unbox").try_as_basic_value().unwrap_basic().into_pointer_value()
        } else {
            cls.into_pointer_value()
        };
        let cls_struct_ty = self.closure_struct_type();
        let fn_f = self.builder.struct_gep(cls_struct_ty, cls_ptr, 2, "ir_cls_fn_f");
        let fp = self.builder.load(ptr_ty, fn_f, "ir_cls_fn");
        let env_f = self.builder.struct_gep(cls_struct_ty, cls_ptr, 3, "ir_cls_env_f");
        let ep = self.builder.load(ptr_ty, env_f, "ir_cls_env");
        (fp, ep, i8_ty.const_int(1, false).into())
    }

    /// Box an opaque runtime handle (ThreadPool/Worker) as TaggedVal*(TAG_HANDLE) so it
    /// round-trips through TypeVar slots / Json params like any tagged value.
    fn box_handle(&mut self, raw: BasicValueEnum<'ctx>) -> BasicValueEnum<'ctx> {
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let f = self.get_or_declare_fn("lin_box_handle", ptr_ty.fn_type(&[ptr_ty.into()], false));
        self.builder.call(f, &[raw.into()], "ir_box_handle").try_as_basic_value().unwrap_basic()
    }

    /// Unbox a boxed handle (TAG_HANDLE) back to the raw ThreadPool/Worker pointer.
    fn unbox_handle(&mut self, boxed: BasicValueEnum<'ctx>) -> BasicValueEnum<'ctx> {
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let f = self.get_or_declare_fn("lin_unbox_handle", ptr_ty.fn_type(&[ptr_ty.into()], false));
        self.builder.call(f, &[boxed.into()], "ir_unbox_handle").try_as_basic_value().unwrap_basic()
    }

    /// Compile a `toString` call on a typed value (used by LinIR intrinsic path).
    /// Stringify a value for the IR path. Delegates to the type-driven
    /// `value_to_string_simple` so that Str returns as-is, numerics use the right
    /// width, and tagged/Array/Object values use the correct runtime dispatch.
    /// `ty` MUST be the input value's type, not the (always-Str) result type.
    pub(crate) fn compile_to_string_value(&mut self, val: BasicValueEnum<'ctx>, ty: &Type) -> BasicValueEnum<'ctx> {
        self.value_to_string_simple(val, ty)
    }

}

// ---------------------------------------------------------------------------
// fromJson schema-descriptor encoding (ADR-031)
//
// The descriptor is a flat byte blob describing the target type tree. The runtime walker
// `lin_from_json` interprets it in lockstep with the Json value. All multi-byte integers are
// little-endian. Offsets are absolute byte indices into the same blob (so recursion is just a
// back-edge to an already-emitted node). The encoding MUST match the runtime decoder in
// `crates/lin-runtime/src/decode.rs`.
//
//   KIND_JSON   (0)                            accept any value
//   KIND_NULL   (1)
//   KIND_BOOL   (2)
//   KIND_STRING (3)
//   KIND_INT    (4)  u8 width_bytes, u8 signed     1/2/4/8 ; signed 0|1
//   KIND_FLOAT  (5)  u8 width_bytes                4|8
//   KIND_ARRAY  (6)  u32 elem_offset
//   KIND_FIXED  (7)  u32 len, then len * u32 offsets
//   KIND_OBJECT (8)  u32 nfields, then nfields * { u16 key_len, key_bytes, u8 nullable, u32 off }
//   KIND_UNION  (9)  u32 nvariants, then nvariants * u32 offsets
//   KIND_STRLIT (10) u16 lit_len, lit_bytes      value must be a string equal to this literal
const KIND_JSON: u8 = 0;
const KIND_NULL: u8 = 1;
const KIND_BOOL: u8 = 2;
const KIND_STRING: u8 = 3;
const KIND_INT: u8 = 4;
const KIND_FLOAT: u8 = 5;
const KIND_ARRAY: u8 = 6;
const KIND_FIXED: u8 = 7;
const KIND_OBJECT: u8 = 8;
const KIND_UNION: u8 = 9;
const KIND_STRLIT: u8 = 10;

struct DescEncoder<'a> {
    buf: Vec<u8>,
    /// Memo: type Display string → byte offset of its already-emitted node. Lets recursive and
    /// repeated types share one node (and makes cycles terminate).
    memo: std::collections::HashMap<String, u32>,
    named: &'a std::collections::HashMap<String, Type>,
}

impl<'a> DescEncoder<'a> {
    fn new(named: &'a std::collections::HashMap<String, Type>) -> Self {
        Self { buf: Vec::new(), memo: std::collections::HashMap::new(), named }
    }

    fn finish(self) -> Vec<u8> {
        self.buf
    }

    fn put_u8(&mut self, b: u8) {
        self.buf.push(b);
    }
    fn put_u16(&mut self, v: u16) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    fn put_u32(&mut self, v: u32) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    /// Reserve a u32 slot, returning its byte offset so it can be back-patched once the referent
    /// node's offset is known.
    fn reserve_u32(&mut self) -> usize {
        let at = self.buf.len();
        self.buf.extend_from_slice(&[0, 0, 0, 0]);
        at
    }
    fn patch_u32(&mut self, at: usize, v: u32) {
        self.buf[at..at + 4].copy_from_slice(&v.to_le_bytes());
    }

    /// Encode `ty` as a node, returning its byte offset. The node header is always written at the
    /// returned offset; container children are appended after the header and referenced by
    /// back-patched absolute offsets, so a recursive child can back-edge to this node. Memoised
    /// (by Named name, else by type display) so repeated/recursive types reuse one node.
    fn encode(&mut self, ty: &Type) -> u32 {
        // Resolve a Named reference to its body, keying the memo on the NAME so a recursive
        // back-edge resolves to the in-progress node's offset.
        if let Type::Named(n) = ty {
            if let Some(&off) = self.memo.get(n) {
                return off;
            }
            if let Some(body) = self.named.get(n).cloned() {
                let off = self.buf.len() as u32;
                self.memo.insert(n.clone(), off);
                self.write_node(&body);
                return off;
            }
            let off = self.buf.len() as u32;
            self.put_u8(KIND_JSON);
            return off;
        }

        let key = format!("{}", ty);
        if let Some(&off) = self.memo.get(&key) {
            return off;
        }
        let off = self.buf.len() as u32;
        self.memo.insert(key, off);
        self.write_node(ty);
        off
    }

    /// Write the node header for `ty` at the current buffer position (which the caller has
    /// already recorded as this node's offset), appending and back-patching any children.
    fn write_node(&mut self, ty: &Type) {
        match ty {
            Type::Null => self.put_u8(KIND_NULL),
            Type::Bool => self.put_u8(KIND_BOOL),
            Type::Str => self.put_u8(KIND_STRING),
            // A string-literal type validates the JSON value is a string AND equals the exact
            // literal — this is what makes `Result.fromJson(...)` reject a wrong discriminant
            // tag at decode time, so union variants discriminate correctly (ADR-034).
            Type::StrLit(s) => {
                self.put_u8(KIND_STRLIT);
                let lb = s.as_bytes();
                self.put_u16(lb.len() as u16);
                self.buf.extend_from_slice(lb);
            }
            Type::Int8 => { self.put_u8(KIND_INT); self.put_u8(1); self.put_u8(1); }
            Type::Int16 => { self.put_u8(KIND_INT); self.put_u8(2); self.put_u8(1); }
            Type::Int32 => { self.put_u8(KIND_INT); self.put_u8(4); self.put_u8(1); }
            Type::Int64 => { self.put_u8(KIND_INT); self.put_u8(8); self.put_u8(1); }
            Type::UInt8 => { self.put_u8(KIND_INT); self.put_u8(1); self.put_u8(0); }
            Type::UInt16 => { self.put_u8(KIND_INT); self.put_u8(2); self.put_u8(0); }
            Type::UInt32 => { self.put_u8(KIND_INT); self.put_u8(4); self.put_u8(0); }
            Type::UInt64 => { self.put_u8(KIND_INT); self.put_u8(8); self.put_u8(0); }
            Type::Float32 => { self.put_u8(KIND_FLOAT); self.put_u8(4); }
            Type::Float64 => { self.put_u8(KIND_FLOAT); self.put_u8(8); }
            // Json / unconstrained TypeVar / opaque handles / functions / iterators / Shared:
            // accept any. `Shared<T>` is an opaque mutable-state box (ADR-029), not a JSON shape,
            // so a `fromJson` target containing it imposes no structural check.
            Type::TypeVar(_)
            | Type::Iterator(_)
            | Type::Function { .. }
            | Type::Never
            | Type::Shared(_)
            // `Stream<T>` is opaque and never a `fromJson` target (not JSON-shaped, not
            // spellable in annotations); included only for match exhaustiveness — accept-any.
            | Type::Stream(_)
            // A typed index-signature map `{ String: T }` (ADR-055) is NOT a v1 `fromJson` decode
            // target — the decoder produces a `LinObject`, not a `LinMap`, so decoding INTO a map
            // would yield the wrong representation. Treated as accept-any here only for
            // exhaustiveness; a `fromJson<{String:T}>` is not part of v1 (see ADR-055).
            | Type::Map(_) => self.put_u8(KIND_JSON),
            Type::Array(inner) => {
                self.put_u8(KIND_ARRAY);
                let slot = self.reserve_u32();
                let elem_off = self.encode(inner);
                self.patch_u32(slot, elem_off);
            }
            Type::FixedArray(elems) => {
                self.put_u8(KIND_FIXED);
                self.put_u32(elems.len() as u32);
                let mut slots = Vec::with_capacity(elems.len());
                for _ in elems {
                    slots.push(self.reserve_u32());
                }
                for (e, slot) in elems.iter().zip(slots) {
                    let off = self.encode(e);
                    self.patch_u32(slot, off);
                }
            }
            Type::Object { fields, .. } => {
                self.put_u8(KIND_OBJECT);
                self.put_u32(fields.len() as u32);
                // Header rows are variable-length (inline keys), so emit each row then its value
                // node right after the whole header is impossible to pre-size; instead emit all
                // rows with reserved value-offset slots, then encode value nodes and patch.
                let mut value_slots = Vec::with_capacity(fields.len());
                for (key, vty) in fields.iter() {
                    let kb = key.as_bytes();
                    self.put_u16(kb.len() as u16);
                    self.buf.extend_from_slice(kb);
                    let nullable = field_is_nullable(vty);
                    self.put_u8(if nullable { 1 } else { 0 });
                    value_slots.push((self.reserve_u32(), vty.clone()));
                }
                for (slot, vty) in value_slots {
                    let off = self.encode(&vty);
                    self.patch_u32(slot, off);
                }
            }
            Type::Union(variants) => {
                self.put_u8(KIND_UNION);
                self.put_u32(variants.len() as u32);
                let mut slots = Vec::with_capacity(variants.len());
                for _ in variants {
                    slots.push(self.reserve_u32());
                }
                for (v, slot) in variants.iter().zip(slots) {
                    let off = self.encode(v);
                    self.patch_u32(slot, off);
                }
            }
            Type::Named(n) => {
                // Reached only if `encode` did not resolve it (no body provided): back-edge if
                // memoised, else opaque Json.
                if let Some(&off) = self.memo.get(n) {
                    // Re-emit as an alias node is not supported; the simplest correct behaviour is
                    // to inline a back-edge via KIND_UNION-of-one is overkill. We instead duplicate
                    // by encoding Json (safe, accept-any). Recursive named types always have a body
                    // in `named`, so this path is for truly-unknown names only.
                    let _ = off;
                    self.put_u8(KIND_JSON);
                } else {
                    self.put_u8(KIND_JSON);
                }
            }
        }
    }
}

/// A target object FIELD is "nullable" (its absence in the Json is allowed) when its type
/// includes `Null` — mirrors the object compatibility rule in `lin-check/src/compat.rs`.
fn field_is_nullable(t: &Type) -> bool {
    match t {
        Type::Null => true,
        Type::Union(variants) => variants.iter().any(field_is_nullable),
        _ => false,
    }
}