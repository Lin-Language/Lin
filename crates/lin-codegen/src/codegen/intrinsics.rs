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
            Type::Str => val,
            Type::Int8 | Type::Int16 | Type::Int32 | Type::Int64
            | Type::UInt8 | Type::UInt16 | Type::UInt32 | Type::UInt64 => {
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
            Type::Object(_) => {
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

    pub(crate) fn compile_ir_intrinsic(&mut self, intrinsic: &lir::Intrinsic, args: &[BasicValueEnum<'ctx>], arg_tys: &[Type], ret_ty: &Type) -> BasicValueEnum<'ctx> {
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
                    Type::Str => {
                        self.builder.call(self.rt.string_length, &[arg.into()], "ir_slen").try_as_basic_value().unwrap_basic()
                    }
                    Type::Array(_) | Type::FixedArray(_) | Type::Iterator(_) => {
                        let len_fn = self.get_or_declare_fn("lin_array_length",
                            i64_ty.fn_type(&[ptr_ty.into()], false));
                        self.builder.call(len_fn, &[arg.into()], "ir_alen").try_as_basic_value().unwrap_basic()
                    }
                    Type::Object(_) | Type::Named(_) => {
                        let obj_len_fn = self.get_or_declare_fn("lin_object_length",
                            i64_ty.fn_type(&[ptr_ty.into()], false));
                        self.builder.call(obj_len_fn, &[arg.into()], "ir_olen").try_as_basic_value().unwrap_basic()
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
                    if Self::is_union_type(&arr_ty) {
                        // arr is a boxed TaggedVal* wrapping a LinArray* (flat or tagged).
                        // Unbox to the raw array, then lin_push_dyn dispatches on its elem_tag.
                        // Calling lin_array_push_tagged on the boxed pointer corrupts the heap.
                        let arr_raw = self.builder.call(self.rt.unbox_ptr, &[arr.into()], "ir_push_arr").try_as_basic_value().unwrap_basic();
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
                        // arr is a raw LinArray* of known element type.
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
                // the thunk runs on a pool worker instead of a fresh thread (spec §32.5).
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
            // (thread-confined, §32.6.4) for its lifetime.
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
            // lin_object_set(obj, key, val) => Null. Unbox obj→LinObject*, key→LinString*,
            // box val→TaggedVal*, then call the runtime. Mirrors the AST handler.
            Intrinsic::ObjectSetDyn => {
                if args.len() >= 3 {
                    let obj_ty = arg_tys.first().cloned().unwrap_or(Type::Null);
                    let key_ty = arg_tys.get(1).cloned().unwrap_or(Type::Null);
                    let val_ty = arg_tys.get(2).cloned().unwrap_or(Type::Null);
                    let obj_ptr = self.ir_as_raw_ptr(args[0], &obj_ty);
                    let key_ptr = self.ir_as_raw_ptr(args[1], &key_ty);
                    let val_is_fresh_box = !Self::is_union_type(&val_ty);
                    let val_tagged = if val_is_fresh_box {
                        self.box_value(args[2], &val_ty)
                    } else { args[2] };
                    self.builder.call(self.rt.object_set,
                        &[obj_ptr.into(), key_ptr.into(), val_tagged.into()], "");
                    if val_is_fresh_box && val_tagged.is_pointer_value() {
                        self.builder.call(self.rt.tagged_release, &[val_tagged.into()], "");
                    }
                }
                ptr_ty.const_null().into()
            }
            // lin_array_set(arr, idx, val) => Null. Unbox arr→LinArray*, idx→i64, box val.
            Intrinsic::ArraySetDyn => {
                if args.len() >= 3 {
                    let arr_ty = arg_tys.first().cloned().unwrap_or(Type::Null);
                    let val_ty = arg_tys.get(2).cloned().unwrap_or(Type::Null);
                    let i64_ty = self.context.i64_type();
                    let void_ty = self.context.void_type();
                    let arr_ptr = if Self::is_union_type(&arr_ty) {
                        self.builder.call(self.rt.unbox_ptr, &[args[0].into()], "set_arr").try_as_basic_value().unwrap_basic()
                    } else { args[0] };
                    let idx_i64 = self.index_value_to_i64(args[1]);
                    let elem_tagged = if Self::is_union_type(&val_ty) {
                        args[2]
                    } else {
                        self.box_value(args[2], &val_ty)
                    };
                    let set_fn = self.get_or_declare_fn("lin_array_set",
                        void_ty.fn_type(&[ptr_ty.into(), i64_ty.into(), ptr_ty.into()], false));
                    self.builder.call(set_fn, &[arr_ptr.into(), idx_i64.into(), elem_tagged.into()], "");
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
            // lin_array_allocate(n) => Json[]  (null-filled tagged array of length n).
            Intrinsic::ArrayAllocate => {
                let i64_ty = self.context.i64_type();
                let n_i64 = self.ir_n_to_i64(args.first().copied(), arg_tys.first());
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
                    let tagged = self.build_tagged_val_alloca(&fill_val, &fill_ty);
                    let set_fn = self.get_or_declare_fn("lin_array_set",
                        self.context.void_type().fn_type(&[ptr_ty.into(), i64_ty.into(), ptr_ty.into()], false));
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
                    self.builder.call(set_fn, &[arr.into(), cur.into(), tagged.into()], "");
                    let next = self.builder.int_add(cur, i64_ty.const_int(1, false), "ir_fi_n");
                    self.builder.store(i_alloc, next);
                    self.builder.unconditional_branch(check);
                    self.builder.position_at_end(exit);
                    arr
                }
            }
            _ => ptr_ty.const_null().into(),
        }
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