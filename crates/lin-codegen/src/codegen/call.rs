use super::builder_ext::BuilderExt;
use inkwell::types::{BasicMetadataTypeEnum, BasicTypeEnum};
use inkwell::values::{
    BasicMetadataValueEnum, BasicValueEnum, FunctionValue, PointerValue,
};
use inkwell::AddressSpace;

use lin_check::types::Type;
use super::Codegen;

impl<'ctx> Codegen<'ctx> {
    /// Byte size of every closure struct. Layout: rc@0, _pad@4, fn_ptr@8, env_ptr@16,
    /// env_size@24, default-arg descriptor@32, capture descriptor@40 (ADR-056). All closures
    /// are this fixed size so `lin_closure_release` frees them with one layout. MUST match the
    /// `CLOSURE_SIZE`/free size in `lin-runtime/src/memory.rs`.
    pub(crate) const CLOSURE_SIZE: u64 = 48;

    /// Wrap a named (top-level) LLVM function in a closure struct with a thin adapter.
    /// Named functions have signature `(T1, T2, ...) -> R` (no env_ptr).
    /// The closure ABI expects `(ptr env, T1, T2, ...) -> R`.
    /// We generate a wrapper `__cls_wrap_N(ptr _env, T1, T2, ...) -> R` that forwards the call.
    /// IR-path variant of `wrap_named_fn_as_closure`: the wrapper returns a boxed
    /// TaggedVal* (ptr), matching the uniform closure ABI the IR indirect-call path uses
    /// (where every closure returns Json and the caller unboxes). The wrapped function's
    /// concrete scalar/pointer return is boxed before returning.
    /// Build (or reuse) a boxed-ABI wrapper `__cls_wrapb_<fn>(ptr env, args...) -> ptr` that
    /// ignores the env, forwards `args` to `named_fn`, and boxes the concrete return into a
    /// TaggedVal*. This is the uniform calling convention every indirect/closure call uses.
    /// Shared by closure construction and default-argument descriptor entries.
    pub(crate) fn boxed_abi_wrapper(&mut self, named_fn: FunctionValue<'ctx>) -> FunctionValue<'ctx> {
        self.boxed_abi_wrapper_ret(named_fn, None, None)
    }

    /// As `boxed_abi_wrapper`, but with the wrapped function's true Lin return type when known.
    /// This disambiguates a raw pointer return (Str/Array/Object — must be boxed) from an
    /// already-boxed Json/union return (passed through). Without it, only the LLVM return kind
    /// is available and every pointer is assumed already-boxed, which crashes the indirect
    /// caller when it unboxes a raw String*. Pass `None` for closures that already use the
    /// uniform boxed (Json) return ABI.
    pub(crate) fn boxed_abi_wrapper_ret(
        &mut self,
        named_fn: FunctionValue<'ctx>,
        lin_ret_ty: Option<&Type>,
        lin_param_tys: Option<&[Type]>,
    ) -> FunctionValue<'ctx> {
        self.boxed_abi_wrapper_full(named_fn, lin_ret_ty, lin_param_tys, false)
    }

    /// Core boxed-ABI wrapper builder. `body_is_closure` ⇒ the wrapped function's param 0 is an
    /// implicit env pointer (a capturing closure body); the wrapper forwards its own env param
    /// straight through and unboxes only the real arguments. For a non-closure named function
    /// the wrapper passes a null env to nothing (there is no env param) and unboxes every arg.
    ///
    /// Either way the wrapper is `(ptr env, ptr boxedArg...) -> ptr`: the single uniform ABI
    /// every INDIRECT closure call uses. Args arrive as boxed `TaggedVal*` and are unboxed to
    /// the body's concrete param types; the concrete return is boxed back. (Previously the
    /// wrapper copied the body's CONCRETE param types, so a boxed `ptr` arg landing in an `i32`
    /// slot reinterpreted the pointer bits → garbage / misaligned deref — the wrapper-ABI bug.)
    pub(crate) fn boxed_abi_wrapper_full(
        &mut self,
        named_fn: FunctionValue<'ctx>,
        lin_ret_ty: Option<&Type>,
        lin_param_tys: Option<&[Type]>,
        body_is_closure: bool,
    ) -> FunctionValue<'ctx> {
        let ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
        let named_ret_ty = named_fn.get_type().get_return_type();
        // Real argument params of the body = total params minus the leading env param (if any).
        let total_body_params = named_fn.count_params() as usize;
        let n_args = if body_is_closure { total_body_params.saturating_sub(1) } else { total_body_params };
        // Wrapper signature: (ptr env, ptr boxedArg...) -> ptr.
        let mut wrapper_param_types: Vec<BasicMetadataTypeEnum> = vec![ptr_ty.into()];
        for _ in 0..n_args {
            wrapper_param_types.push(ptr_ty.into());
        }
        let wrapper_fn_ty = ptr_ty.fn_type(&wrapper_param_types, false);
        let wrapper_name = format!("__cls_wrapb_{}", named_fn.get_name().to_str().unwrap_or("fn"));
        if let Some(existing) = self.module.get_function(&wrapper_name) {
            return existing;
        }
        let wf = self.module.add_function(&wrapper_name, wrapper_fn_ty, None);
        // User-emitted Lin functions never unwind (value-based errors), and this wrapper
        // only forwards + boxes — mark it nounwind like other emitted functions.
        self.mark_user_fn_nounwind(wf);
        let saved_block = self.builder.get_insert_block();
        let entry = self.context.append_basic_block(wf, "entry");
        self.builder.position_at_end(entry);
        // Forwarded args to the concrete body. For a closure body, pass the env (wrapper param 0)
        // as the body's first arg; then unbox each real arg to the body's concrete param type.
        let mut fwd_args: Vec<BasicMetadataValueEnum> = Vec::with_capacity(total_body_params);
        if body_is_closure {
            fwd_args.push(wf.get_nth_param(0).unwrap().into());
        }
        for i in 0..n_args {
            let boxed_arg = wf.get_nth_param((i + 1) as u32).unwrap();
            // Concrete param type for the body's nth real argument. Prefer the true Lin type;
            // otherwise infer from the body's concrete LLVM param type (skipping the env slot).
            let llvm_param_index = if body_is_closure { i + 1 } else { i };
            let lin_ty: Type = match lin_param_tys.and_then(|tys| tys.get(i)) {
                Some(t) => t.clone(),
                None => {
                    let pt = named_fn.get_nth_param(llvm_param_index as u32).unwrap().get_type();
                    if pt.is_int_type() {
                        match pt.into_int_type().get_bit_width() {
                            1 => Type::Bool,
                            8 => Type::Int8,
                            16 => Type::Int16,
                            64 => Type::Int64,
                            _ => Type::Int32,
                        }
                    } else if pt.is_float_type() {
                        if pt.into_float_type() == self.context.f32_type() { Type::Float32 } else { Type::Float64 }
                    } else {
                        // A pointer param of unknown Lin type — assume already-boxed Json.
                        Type::TypeVar(u32::MAX)
                    }
                }
            };
            // Union/Json params are already a boxed `ptr` — pass through (unbox_value returns the
            // pointer unchanged for these). Concrete types are unboxed to their scalar/raw
            // pointer representation matching the body's declared param type.
            let unboxed = self.unbox_value(boxed_arg, &lin_ty);
            fwd_args.push(unboxed.into());
        }
        let call = self.builder.call(named_fn, &fwd_args, "wfwd");
        // Box the concrete return to a TaggedVal*. Prefer the true Lin return type; fall back
        // to inferring from the LLVM return kind (scalars only — a bare pointer of unknown Lin
        // type is assumed already-boxed Json).
        let boxed: BasicValueEnum<'ctx> = match named_ret_ty {
            Some(rt) => {
                let rv = call.try_as_basic_value().basic().unwrap();
                let lin_ty = match lin_ret_ty {
                    // Known Lin return type: box exactly per that type (Str/Array/Object are raw
                    // pointers that MUST be boxed; union/Json values are already boxed).
                    Some(t) => t.clone(),
                    None if rt.is_int_type() => {
                        match rt.into_int_type().get_bit_width() { 1 => Type::Bool, 8 => Type::Int8, 16 => Type::Int16, 64 => Type::Int64, _ => Type::Int32 }
                    }
                    None if rt.is_float_type() => {
                        if rt.into_float_type() == self.context.f32_type() { Type::Float32 } else { Type::Float64 }
                    }
                    // Already a pointer of unknown Lin type — assume boxed Json (TypeVar).
                    None => Type::TypeVar(u32::MAX),
                };
                // Union/Json/Named values arrive already boxed; pass through. Everything else
                // (scalars, Str, Array, Object) is boxed into a TaggedVal*.
                if Self::is_union_type(&lin_ty) { rv } else { self.box_value(rv, &lin_ty) }
            }
            None => ptr_ty.const_null().into(),
        };
        self.builder.r#return(Some(&boxed));
        if let Some(sb) = saved_block {
            self.builder.position_at_end(sb);
        }
        wf
    }

    pub(crate) fn wrap_named_fn_as_closure_boxed(&mut self, named_fn: FunctionValue<'ctx>) -> BasicValueEnum<'ctx> {
        self.wrap_named_fn_as_closure_boxed_desc(named_fn, None)
    }

    /// Variant of `wrap_named_fn_as_closure_boxed` that attaches a default-argument descriptor
    /// (closure offset 32) so an indirect under-arity call on this capture-less function value
    /// dispatches through the descriptor to the right default-fill adapter.
    pub(crate) fn wrap_named_fn_as_closure_boxed_desc(
        &mut self,
        named_fn: FunctionValue<'ctx>,
        descriptor: Option<PointerValue<'ctx>>,
    ) -> BasicValueEnum<'ctx> {
        self.wrap_named_fn_as_closure_boxed_desc_ret(named_fn, descriptor, None, None)
    }

    /// As `wrap_named_fn_as_closure_boxed_desc`, with the wrapped function's true Lin return
    /// type so the boxed-ABI wrapper boxes a raw Str/Array/Object return correctly.
    pub(crate) fn wrap_named_fn_as_closure_boxed_desc_ret(
        &mut self,
        named_fn: FunctionValue<'ctx>,
        descriptor: Option<PointerValue<'ctx>>,
        lin_ret_ty: Option<&Type>,
        lin_param_tys: Option<&[Type]>,
    ) -> BasicValueEnum<'ctx> {
        let wrapper_fn = self.boxed_abi_wrapper_ret(named_fn, lin_ret_ty, lin_param_tys);
        let fn_ptr = wrapper_fn.as_global_value().as_pointer_value();
        // No captures: capture-less closure with a null env, descriptor at offset 32.
        self.make_closure_struct_desc(fn_ptr.into(), &[], descriptor)
    }

    /// Value-input port of `build_partial_application` for the LinIR path: the partial
    /// arguments arrive as already-compiled LLVM values rather than TypedExprs. Builds a
    /// closure {wrapper_fn, env} capturing the partials; the wrapper loads them and calls
    /// `llvm_fn` with partials ++ remaining params.
    pub(crate) fn build_partial_application_values(
        &mut self,
        llvm_fn: FunctionValue<'ctx>,
        compiled_partials: &[BasicValueEnum<'ctx>],
        remaining_params: &[Type],
        final_ret: &Type,
    ) -> BasicValueEnum<'ctx> {
        let ptr_ty = self.context.ptr_type(AddressSpace::default());

        let env_field_types: Vec<BasicTypeEnum> = compiled_partials.iter().map(|v| v.get_type()).collect();
        let env_struct_ty = self.context.struct_type(&env_field_types, false);
        let env_size = env_struct_ty.size_of().unwrap();
        let env_size_i64 = self.builder.int_z_extend_or_bit_cast(env_size, self.context.i64_type(), "papp_env_sz");
        let env_ptr = self.builder.call(self.rt.alloc, &[env_size_i64.into()], "papp_env").try_as_basic_value().unwrap_basic().into_pointer_value();
        for (i, val) in compiled_partials.iter().enumerate() {
            let field = self.builder.struct_gep(env_struct_ty, env_ptr, i as u32, "papp_f");
            self.builder.store(field, *val);
        }

        let wrapper_name = format!("__papp_ir_{}", self.closure_count);
        self.closure_count += 1;
        // Uniform closure ABI: each remaining argument arrives as a boxed TaggedVal* (ptr) and
        // the wrapper returns a boxed TaggedVal* (ptr), so a partial application is callable
        // through an opaque `Function` value like any other closure. The wrapper unboxes each
        // remaining arg to its concrete type before forwarding to `llvm_fn`.
        let mut wrapper_param_tys: Vec<BasicMetadataTypeEnum> = vec![ptr_ty.into()];
        for _ in remaining_params {
            wrapper_param_tys.push(ptr_ty.into());
        }
        let wrapper_fn_ty = ptr_ty.fn_type(&wrapper_param_tys, false);
        let wrapper_fn = self.module.add_function(&wrapper_name, wrapper_fn_ty, None);
        self.mark_user_fn_nounwind(wrapper_fn);

        let cls_struct_ty = self.closure_struct_type();
        // CLOSURE_SIZE bytes: header (32) + env_size@24 + default-arg descriptor@32 (null — a
        // partial application has no default args) + capture descriptor@40 (null — partial-app
        // captures use BORROW semantics; the inner closure / partial args are released by their
        // own owners, not by this closure's free). All closures are one fixed size.
        let cls_ptr = self.alloc_closure();
        let rc_field = self.builder.struct_gep(cls_struct_ty, cls_ptr, 0, "papp_cls_rc");
        self.builder.store(rc_field, self.context.i32_type().const_int(1, false));
        let fn_field = self.builder.struct_gep(cls_struct_ty, cls_ptr, 2, "papp_cls_fn");
        self.builder.store(fn_field, wrapper_fn.as_global_value().as_pointer_value());
        let env_field = self.builder.struct_gep(cls_struct_ty, cls_ptr, 3, "papp_cls_env");
        self.builder.store(env_field, env_ptr);
        // env_size at offset 24 so lin_closure_release frees the env with the right layout
        // (lin_alloc does NOT zero, so this MUST be written explicitly).
        let env_sz_gep = unsafe { self.builder.gep(
            self.context.i8_type(), cls_ptr, &[self.context.i64_type().const_int(24, false)], "papp_env_sz_f"
        ) };
        self.builder.store(env_sz_gep, env_size_i64);
        // Null default-arg descriptor at offset 32 + null capture descriptor at offset 40.
        let desc_gep = unsafe { self.builder.gep(
            self.context.i8_type(), cls_ptr, &[self.context.i64_type().const_int(32, false)], "papp_desc_f"
        ) };
        self.builder.store(desc_gep, ptr_ty.const_null());
        self.store_capture_descriptor(cls_ptr, ptr_ty.const_null());

        let current_block = self.builder.get_insert_block().unwrap();
        {
            let entry = self.context.append_basic_block(wrapper_fn, "entry");
            self.builder.position_at_end(entry);
            let env_arg = wrapper_fn.get_nth_param(0).unwrap().into_pointer_value();
            let mut call_args: Vec<BasicMetadataValueEnum> = Vec::new();
            for (i, field_ty) in env_field_types.iter().enumerate() {
                let fp = self.builder.struct_gep(env_struct_ty, env_arg, i as u32, "papp_load_f");
                let v = self.builder.load(*field_ty, fp, "papp_v");
                call_args.push(v.into());
            }
            for (i, rp) in remaining_params.iter().enumerate() {
                // Each remaining arg arrives boxed (ptr); unbox to its concrete type before
                // forwarding to the direct-ABI `llvm_fn` (union/Json params pass through).
                let p = wrapper_fn.get_nth_param(1 + i as u32).unwrap();
                let unboxed = self.unbox_value(p, rp);
                call_args.push(unboxed.into());
            }
            let call = self.builder.call(llvm_fn, &call_args, "papp_call");
            // Box the concrete result to a TaggedVal* (uniform closure return ABI).
            match call.try_as_basic_value().basic() {
                Some(v) => {
                    let boxed = self.box_value(v, final_ret);
                    self.builder.r#return(Some(&boxed));
                }
                None => { self.builder.r#return(Some(&ptr_ty.const_null())); }
            }
        }
        self.builder.position_at_end(current_block);
        cls_ptr.into()
    }

    /// Value-input port of `build_closure_call`'s partial-application branch (LinIR path).
    /// Under-applying a closure *value* (`step1(2)` where `step1: (Int,Int)=>Int`) yields a
    /// new closure capturing the inner closure + the supplied args, taking the remaining
    /// params. The wrapper uses the uniform boxed ABI (returns a TaggedVal*), and completes
    /// the call by invoking the inner closure (also boxed ABI) with stored ++ remaining args.
    pub(crate) fn build_closure_partial_application_values(
        &mut self,
        closure_ptr: PointerValue<'ctx>,
        partial_args: &[BasicValueEnum<'ctx>],
        remaining_params: &[Type],
    ) -> BasicValueEnum<'ctx> {
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let arg_types: Vec<BasicTypeEnum> = partial_args.iter().map(|v| v.get_type()).collect();

        // Env struct: { ptr (inner closure), arg0, arg1, ... }.
        let mut env_field_types: Vec<BasicTypeEnum> = vec![ptr_ty.into()];
        env_field_types.extend_from_slice(&arg_types);
        let env_struct_ty = self.context.struct_type(&env_field_types, false);
        let env_size = env_struct_ty.size_of().unwrap();
        let env_size_i64 = self.builder.int_z_extend_or_bit_cast(env_size, self.context.i64_type(), "papp_env_sz");
        let env_ptr2 = self.builder.call(self.rt.alloc, &[env_size_i64.into()], "papp_env")
            .try_as_basic_value().unwrap_basic().into_pointer_value();
        let cls_field = self.builder.struct_gep(env_struct_ty, env_ptr2, 0, "papp_cls_f");
        self.builder.store(cls_field, closure_ptr);
        // The env borrows the inner closure (does not retain it), mirroring the AST path's
        // build_closure_call. The inner closure is a longer-lived binding (a top-level val
        // stored to a module global, retained there), so the borrow stays valid.
        for (i, val) in partial_args.iter().enumerate() {
            let f = self.builder.struct_gep(env_struct_ty, env_ptr2, (i + 1) as u32, "papp_f");
            self.builder.store(f, *val);
        }

        // Wrapper: (env_ptr, ...remaining_args) -> ptr (uniform boxed ABI: every remaining arg
        // arrives as a boxed TaggedVal* / ptr). The stored partials are already boxed ptrs.
        let wrapper_name = format!("__papp_cls_ir_{}", self.closure_count);
        self.closure_count += 1;
        let mut wrapper_param_types: Vec<BasicMetadataTypeEnum> = vec![ptr_ty.into()];
        for _ in remaining_params {
            wrapper_param_types.push(ptr_ty.into());
        }
        let wrapper_fn_ty = ptr_ty.fn_type(&wrapper_param_types, false);
        let wrapper_fn = self.module.add_function(&wrapper_name, wrapper_fn_ty, None);
        self.mark_user_fn_nounwind(wrapper_fn);

        let saved_block = self.builder.get_insert_block().unwrap();
        let wrapper_entry = self.context.append_basic_block(wrapper_fn, "entry");
        self.builder.position_at_end(wrapper_entry);

        let w_env_ptr = wrapper_fn.get_nth_param(0).unwrap().into_pointer_value();
        let cls_fp = self.builder.struct_gep(env_struct_ty, w_env_ptr, 0, "wcls_p");
        let inner_cls_ptr = self.builder.load(ptr_ty, cls_fp, "inner_cls").into_pointer_value();

        // Load the inner closure's fn_ptr / env_ptr.
        let cls_ty = self.closure_struct_type();
        let inner_fn_gep = self.builder.struct_gep(cls_ty, inner_cls_ptr, 2, "inner_fp");
        let inner_fn_ptr = self.builder.load(ptr_ty, inner_fn_gep, "inner_fnp").into_pointer_value();
        let inner_env_gep = self.builder.struct_gep(cls_ty, inner_cls_ptr, 3, "inner_ep");
        let inner_env_ptr = self.builder.load(ptr_ty, inner_env_gep, "inner_envp");

        // Complete the call: inner_fn(inner_env, stored_args..., remaining_params...).
        let mut call_param_types: Vec<BasicMetadataTypeEnum> = vec![ptr_ty.into()];
        let mut call_args: Vec<BasicMetadataValueEnum> = vec![inner_env_ptr.into()];
        for (i, ty) in arg_types.iter().enumerate() {
            let fp = self.builder.struct_gep(env_struct_ty, w_env_ptr, (i + 1) as u32, "warg_p");
            let v = self.builder.load(*ty, fp, "warg");
            call_param_types.push((*ty).into());
            call_args.push(v.into());
        }
        for i in 0..remaining_params.len() {
            // Each remaining arg is already a boxed ptr (uniform ABI) — forward as-is.
            let p = wrapper_fn.get_nth_param((i + 1) as u32).unwrap();
            call_param_types.push(ptr_ty.into());
            call_args.push(p.into());
        }
        // Inner closure uses the uniform boxed ABI: returns a TaggedVal* (ptr).
        let inner_fn_ty = ptr_ty.fn_type(&call_param_types, false);
        let inner_call = self.builder.indirect_call(inner_fn_ty, inner_fn_ptr, &call_args, "papp_inner");
        let inner_result = inner_call.try_as_basic_value().unwrap_basic();
        self.builder.r#return(Some(&inner_result));
        self.builder.position_at_end(saved_block);

        // Build the outer closure struct { rc, _pad, fn_ptr, env_ptr } (CLOSURE_SIZE bytes; the
        // offset-32 default-arg descriptor and offset-40 capture descriptor are both null here —
        // a partial application has no defaults and uses borrow semantics for its captures).
        let cls_struct_ty = self.closure_struct_type();
        let cls_ptr = self.alloc_closure();
        let rc_field = self.builder.struct_gep(cls_struct_ty, cls_ptr, 0, "papp_cls_rc");
        self.builder.store(rc_field, self.context.i32_type().const_int(1, false));
        let fn_field = self.builder.struct_gep(cls_struct_ty, cls_ptr, 2, "papp_cls_fn");
        self.builder.store(fn_field, wrapper_fn.as_global_value().as_pointer_value());
        let env_field = self.builder.struct_gep(cls_struct_ty, cls_ptr, 3, "papp_cls_env");
        self.builder.store(env_field, env_ptr2);
        // env_size at offset 24 (lin_alloc does NOT zero — must write explicitly so
        // lin_closure_release frees the env with the correct layout).
        let env_sz_gep = unsafe { self.builder.gep(
            self.context.i8_type(), cls_ptr, &[self.context.i64_type().const_int(24, false)], "papp_env_sz_f"
        ) };
        self.builder.store(env_sz_gep, env_size_i64);
        // Null default-arg descriptor at offset 32 + null capture descriptor at offset 40.
        let desc_gep = unsafe { self.builder.gep(
            self.context.i8_type(), cls_ptr, &[self.context.i64_type().const_int(32, false)], "papp_desc_f"
        ) };
        self.builder.store(desc_gep, ptr_ty.const_null());
        self.store_capture_descriptor(cls_ptr, ptr_ty.const_null());
        cls_ptr.into()
    }

    /// Allocate one closure struct (`CLOSURE_SIZE` = 48 bytes). Layout: rc@0, _pad@4, fn@8,
    /// env@16, env_size@24, default-arg descriptor@32, capture descriptor@40 (ADR-056). The
    /// single allocator for every closure so the size stays in one place.
    pub(crate) fn alloc_closure(&mut self) -> PointerValue<'ctx> {
        let size = self.context.i64_type().const_int(Self::CLOSURE_SIZE, false);
        self.builder.call(self.rt.alloc, &[size.into()], "cls").try_as_basic_value().unwrap_basic().into_pointer_value()
    }

    /// Store the capture-descriptor pointer at closure offset 40 (`lin_closure_release` reads it
    /// to release owning captures). `desc` is null when the closure has no owning captures.
    pub(crate) fn store_capture_descriptor(&mut self, cls_mem: PointerValue<'ctx>, desc: PointerValue<'ctx>) {
        let gep = unsafe { self.builder.gep(
            self.context.i8_type(), cls_mem, &[self.context.i64_type().const_int(40, false)], "cls_capdesc"
        ) };
        self.builder.store(gep, desc);
    }

    /// Make a closure struct {i32 rc=1, i32 _pad, fn_ptr, env_ptr} with optional captured env.
    pub(crate) fn make_closure_struct(&mut self, fn_ptr: BasicValueEnum<'ctx>, captures: &[BasicValueEnum<'ctx>]) -> BasicValueEnum<'ctx> {
        self.make_closure_struct_desc_caps(fn_ptr, captures, None, None)
    }

    /// Like `make_closure_struct`, but also stores a default-argument descriptor pointer at
    /// closure offset 32 (null when `descriptor` is None). The closure is allocated at 40 bytes
    /// so the descriptor slot is always present; `lin_closure_release` frees 40 bytes to match.
    pub(crate) fn make_closure_struct_desc(
        &mut self,
        fn_ptr: BasicValueEnum<'ctx>,
        captures: &[BasicValueEnum<'ctx>],
        descriptor: Option<PointerValue<'ctx>>,
    ) -> BasicValueEnum<'ctx> {
        self.make_closure_struct_desc_caps(fn_ptr, captures, descriptor, None)
    }

    /// Full closure builder. `capture_kinds`, when present, gives one `transfer::CAP_*` byte per
    /// capture; a static capture descriptor `{u32 count, u8 kinds[count]}` is emitted and its
    /// pointer stored at the env's offset-0 word (otherwise a redundant size, read by no one).
    /// The async spawn path uses that descriptor to deep-copy the env across a thread boundary
    /// (Option C, ADR-042). When `capture_kinds` is None the env offset-0 word stays the size
    /// (legacy behaviour; such a closure simply isn't transferable and the spawn path runs it
    /// inline).
    pub(crate) fn make_closure_struct_desc_caps(
        &mut self,
        fn_ptr: BasicValueEnum<'ctx>,
        captures: &[BasicValueEnum<'ctx>],
        descriptor: Option<PointerValue<'ctx>>,
        capture_kinds: Option<&[u8]>,
    ) -> BasicValueEnum<'ctx> {
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i64_ty = self.context.i64_type();
        // 48-byte closure (CLOSURE_SIZE): 32-byte header + env_size@24 + default-arg
        // descriptor@32 + CAPTURE descriptor@40 (ADR-056). The capture descriptor records each
        // owning capture's release kind; `lin_closure_release` walks it to release captures.
        let cls_mem = self.alloc_closure();
        let cls_ty = self.closure_struct_type();
        let rc_f = self.builder.struct_gep(cls_ty, cls_mem, 0, "ir_rc");
        self.builder.store(rc_f, self.context.i32_type().const_int(1, false));
        let fp_f = self.builder.struct_gep(cls_ty, cls_mem, 2, "ir_fp");
        self.builder.store(fp_f, fn_ptr);
        // Default-arg descriptor at offset 32 (null if this function has no default args).
        let desc_gep = unsafe { self.builder.gep(
            self.context.i8_type(), cls_mem, &[i64_ty.const_int(32, false)], "ir_desc"
        ) };
        self.builder.store(desc_gep, descriptor.unwrap_or_else(|| ptr_ty.const_null()));
        // Capture descriptor at offset 40: a static `{u32 count, u8 kinds[]}` global, or null
        // when there are no owning captures. Always written so the slot is initialized
        // (lin_alloc does not zero).
        let capdesc: PointerValue<'ctx> = match capture_kinds {
            Some(kinds) if !kinds.is_empty() => self.emit_capture_descriptor(kinds),
            _ => ptr_ty.const_null(),
        };
        self.store_capture_descriptor(cls_mem, capdesc);

        if captures.is_empty() {
            let ep_f = self.builder.struct_gep(cls_ty, cls_mem, 3, "ir_ep");
            self.builder.store(ep_f, ptr_ty.const_null());
        } else {
            // Build an env struct.
            // Layout: {u64 size, cap0, cap1, ...}  (offset 0 is the redundant size header, read
            // by no one; the capture descriptor lives in the CLOSURE at offset 40, not here).
            let n = captures.len();
            let env_size_bytes = 8u64 + (n as u64 * 8); // size header + 8 bytes per capture (ptr/i64)
            let env_size_val = i64_ty.const_int(env_size_bytes, false);
            let env_mem = self.builder.call(self.rt.alloc, &[env_size_val.into()], "ir_env").try_as_basic_value().unwrap_basic().into_pointer_value();
            self.builder.store(env_mem, env_size_val);
            // Write captures at offsets 8, 16, ...
            for (i, &cap) in captures.iter().enumerate() {
                let offset = 8u64 + (i as u64 * 8);
                let offset_v = i64_ty.const_int(offset, false);
                let cap_gep = unsafe { self.builder.gep(
                    self.context.i8_type(),
                    env_mem,
                    &[offset_v],
                    &format!("ir_cap{}", i)
                ) };
                self.builder.store(cap_gep, cap);
            }
            let ep_f = self.builder.struct_gep(cls_ty, cls_mem, 3, "ir_ep");
            self.builder.store(ep_f, env_mem);
            // env_size at offset 24.
            let env_size_gep = unsafe { self.builder.gep(
                self.context.i8_type(),
                cls_mem,
                &[i64_ty.const_int(24, false)],
                "ir_env_sz"
            ) };
            self.builder.store(env_size_gep, env_size_val);
        }
        cls_mem.into()
    }

    /// Emit (and cache) a static read-only capture-descriptor global `{ i32 count, [count x i8]
    /// kinds }` and return a pointer to it. Used by the async spawn path to deep-copy a closure
    /// env across a thread boundary.
    fn emit_capture_descriptor(&mut self, kinds: &[u8]) -> PointerValue<'ctx> {
        let i8_ty = self.context.i8_type();
        let i32_ty = self.context.i32_type();
        // Cache key: the kind sequence, so identical descriptors are shared.
        let key: String = format!("__capdesc_{}", kinds.iter().map(|k| (b'0' + k) as char).collect::<String>());
        if let Some(g) = self.module.get_global(&key) {
            return g.as_pointer_value();
        }
        let count_const = i32_ty.const_int(kinds.len() as u64, false);
        let kind_consts: Vec<_> = kinds.iter().map(|&k| i8_ty.const_int(k as u64, false)).collect();
        let kinds_arr = i8_ty.const_array(&kind_consts);
        let desc_ty = self.context.struct_type(&[i32_ty.into(), kinds_arr.get_type().into()], false);
        let desc_val = self.context.const_struct(&[count_const.into(), kinds_arr.into()], false);
        let global = self.module.add_global(desc_ty, None, &key);
        global.set_initializer(&desc_val);
        global.set_constant(true);
        // Match the default-argument descriptor globals (codegen/mod.rs): a plain named constant
        // with default linkage, NO unnamed_addr. Setting unnamed_addr pushes the small descriptor
        // into the mergeable `.rodata.cstN` section, whose entries can't take a 32-bit absolute
        // relocation under PIE (R_X86_64_32S link error). A uniquely-named constant lands in
        // ordinary .rodata and links cleanly under the default reloc model.
        global.as_pointer_value()
    }

}