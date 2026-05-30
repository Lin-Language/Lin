use super::builder_ext::BuilderExt;
use inkwell::types::{BasicMetadataTypeEnum, BasicTypeEnum};
use inkwell::values::{
    BasicMetadataValueEnum, BasicValueEnum, FunctionValue, PointerValue,
};
use inkwell::AddressSpace;

use lin_check::types::Type;
use super::Codegen;

impl<'ctx> Codegen<'ctx> {
    /// Wrap a named (top-level) LLVM function in a closure struct with a thin adapter.
    /// Named functions have signature `(T1, T2, ...) -> R` (no env_ptr).
    /// The closure ABI expects `(ptr env, T1, T2, ...) -> R`.
    /// We generate a wrapper `__cls_wrap_N(ptr _env, T1, T2, ...) -> R` that forwards the call.
    /// IR-path variant of `wrap_named_fn_as_closure`: the wrapper returns a boxed
    /// TaggedVal* (ptr), matching the uniform closure ABI the IR indirect-call path uses
    /// (where every closure returns Json and the caller unboxes). The wrapped function's
    /// concrete scalar/pointer return is boxed before returning.
    pub(crate) fn wrap_named_fn_as_closure_boxed(&mut self, named_fn: FunctionValue<'ctx>) -> BasicValueEnum<'ctx> {
        let ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
        let named_ret_ty = named_fn.get_type().get_return_type();
        let mut wrapper_param_types: Vec<BasicMetadataTypeEnum> = vec![ptr_ty.into()];
        for i in 0..named_fn.count_params() {
            wrapper_param_types.push(named_fn.get_nth_param(i).unwrap().get_type().into());
        }
        // Uniform ABI: always return ptr.
        let wrapper_fn_ty = ptr_ty.fn_type(&wrapper_param_types, false);
        let wrapper_name = format!("__cls_wrapb_{}", named_fn.get_name().to_str().unwrap_or("fn"));
        let wrapper_fn = if let Some(existing) = self.module.get_function(&wrapper_name) {
            existing
        } else {
            let wf = self.module.add_function(&wrapper_name, wrapper_fn_ty, None);
            self.add_fn_attrs(wf, &["nounwind"]);
            let saved_block = self.builder.get_insert_block().unwrap();
            let entry = self.context.append_basic_block(wf, "entry");
            self.builder.position_at_end(entry);
            let fwd_args: Vec<BasicMetadataValueEnum> = (1..wf.count_params())
                .map(|i| wf.get_nth_param(i).unwrap().into())
                .collect();
            let call = self.builder.call(named_fn, &fwd_args, "wfwd");
            // Box the concrete return to a TaggedVal* using the LLVM return kind.
            let boxed: BasicValueEnum<'ctx> = match named_ret_ty {
                Some(rt) => {
                    let rv = call.try_as_basic_value().basic().unwrap();
                    let lin_ty = if rt.is_int_type() {
                        match rt.into_int_type().get_bit_width() { 1 => Type::Bool, 8 => Type::Int8, 16 => Type::Int16, 64 => Type::Int64, _ => Type::Int32 }
                    } else if rt.is_float_type() {
                        if rt.into_float_type() == self.context.f32_type() { Type::Float32 } else { Type::Float64 }
                    } else {
                        // Already a pointer (Str/Array/Object/Json) — box as-is via TypeVar dispatch.
                        Type::TypeVar(u32::MAX)
                    };
                    if matches!(lin_ty, Type::TypeVar(_)) { rv } else { self.box_value(rv, &lin_ty) }
                }
                None => ptr_ty.const_null().into(),
            };
            self.builder.r#return(Some(&boxed));
            self.builder.position_at_end(saved_block);
            wf
        };
        // Build {rc, _pad, fn_ptr, null_env} closure struct.
        let lin_alloc_fn = self.get_or_declare_fn("lin_alloc",
            ptr_ty.fn_type(&[self.context.i64_type().into()], false));
        let cls_mem = self.builder.call(lin_alloc_fn,
            &[self.context.i64_type().const_int(32, false).into()], "wnfnb_cls").try_as_basic_value().unwrap_basic().into_pointer_value();
        let cls_ty = self.closure_struct_type();
        let rc_field = self.builder.struct_gep(cls_ty, cls_mem, 0, "wnfnb_rc");
        self.builder.store(rc_field, self.context.i32_type().const_int(1, false));
        let fn_field = self.builder.struct_gep(cls_ty, cls_mem, 2, "wnfnb_fp");
        self.builder.store(fn_field, wrapper_fn.as_global_value().as_pointer_value());
        let env_field = self.builder.struct_gep(cls_ty, cls_mem, 3, "wnfnb_ep");
        self.builder.store(env_field, ptr_ty.const_null());
        cls_mem.into()
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
        let mut wrapper_param_tys: Vec<BasicMetadataTypeEnum> = vec![ptr_ty.into()];
        for p in remaining_params {
            wrapper_param_tys.push(self.llvm_type(p).into());
        }
        // Uniform closure ABI: the wrapper returns a boxed TaggedVal* (ptr), so a partial
        // application is callable through an opaque Function value like any other closure.
        let wrapper_fn_ty = ptr_ty.fn_type(&wrapper_param_tys, false);
        let wrapper_fn = self.module.add_function(&wrapper_name, wrapper_fn_ty, None);
        self.add_fn_attrs(wrapper_fn, &["nounwind"]);

        let cls_struct_ty = self.closure_struct_type();
        let cls_ptr = self.builder.call(self.rt.alloc, &[self.context.i64_type().const_int(32, false).into()], "papp_cls").try_as_basic_value().unwrap_basic().into_pointer_value();
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
            for i in 0..remaining_params.len() {
                let p = wrapper_fn.get_nth_param(1 + i as u32).unwrap();
                call_args.push(p.into());
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

        // Wrapper: (env_ptr, ...remaining_params) -> ptr (boxed ABI).
        let wrapper_name = format!("__papp_cls_ir_{}", self.closure_count);
        self.closure_count += 1;
        let mut wrapper_param_types: Vec<BasicMetadataTypeEnum> = vec![ptr_ty.into()];
        for t in remaining_params {
            wrapper_param_types.push(self.llvm_param_type(t));
        }
        let wrapper_fn_ty = ptr_ty.fn_type(&wrapper_param_types, false);
        let wrapper_fn = self.module.add_function(&wrapper_name, wrapper_fn_ty, None);
        self.add_fn_attrs(wrapper_fn, &["nounwind"]);

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
        for (i, t) in remaining_params.iter().enumerate() {
            let p = wrapper_fn.get_nth_param((i + 1) as u32).unwrap();
            call_param_types.push(self.llvm_param_type(t));
            call_args.push(p.into());
        }
        // Inner closure uses the uniform boxed ABI: returns a TaggedVal* (ptr).
        let inner_fn_ty = ptr_ty.fn_type(&call_param_types, false);
        let inner_call = self.builder.indirect_call(inner_fn_ty, inner_fn_ptr, &call_args, "papp_inner");
        let inner_result = inner_call.try_as_basic_value().unwrap_basic();
        self.builder.r#return(Some(&inner_result));
        self.builder.position_at_end(saved_block);

        // Build the outer closure struct { rc, _pad, fn_ptr, env_ptr }.
        let cls_struct_ty = self.closure_struct_type();
        let cls_ptr = self.builder.call(self.rt.alloc, &[self.context.i64_type().const_int(32, false).into()], "papp_cls")
            .try_as_basic_value().unwrap_basic().into_pointer_value();
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
        cls_ptr.into()
    }

    /// Call a thunk closure value `(env) -> ptr` (closures use the uniform boxed ABI).
    /// Returns the boxed Json result. Used by the async intrinsics on the IR path.
    pub(crate) fn call_thunk_value(&mut self, thunk: BasicValueEnum<'ctx>) -> BasicValueEnum<'ctx> {
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        if !thunk.is_pointer_value() { return ptr_ty.const_null().into(); }
        let cls_ptr = thunk.into_pointer_value();
        let cls_ty = self.closure_struct_type();
        let fn_field = self.builder.struct_gep(cls_ty, cls_ptr, 2, "thunk_fn_f");
        let fn_ptr = self.builder.load(ptr_ty, fn_field, "thunk_fn").into_pointer_value();
        let env_field = self.builder.struct_gep(cls_ty, cls_ptr, 3, "thunk_env_f");
        let env_ptr = self.builder.load(ptr_ty, env_field, "thunk_env");
        let fn_ty = ptr_ty.fn_type(&[ptr_ty.into()], false);
        self.builder.indirect_call(fn_ty, fn_ptr, &[env_ptr.into()], "thunk_res").try_as_basic_value().unwrap_basic()
    }

    /// Make a closure struct {i32 rc=1, i32 _pad, fn_ptr, env_ptr} with optional captured env.
    pub(crate) fn make_closure_struct(&mut self, fn_ptr: BasicValueEnum<'ctx>, captures: &[BasicValueEnum<'ctx>]) -> BasicValueEnum<'ctx> {
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i64_ty = self.context.i64_type();
        let cls_size = i64_ty.const_int(32, false);
        let cls_mem = self.builder.call(self.rt.alloc, &[cls_size.into()], "ir_cls").try_as_basic_value().unwrap_basic().into_pointer_value();
        let cls_ty = self.closure_struct_type();
        let rc_f = self.builder.struct_gep(cls_ty, cls_mem, 0, "ir_rc");
        self.builder.store(rc_f, self.context.i32_type().const_int(1, false));
        let fp_f = self.builder.struct_gep(cls_ty, cls_mem, 2, "ir_fp");
        self.builder.store(fp_f, fn_ptr);

        if captures.is_empty() {
            let ep_f = self.builder.struct_gep(cls_ty, cls_mem, 3, "ir_ep");
            self.builder.store(ep_f, ptr_ty.const_null());
        } else {
            // Build an env struct.
            // Layout: {u64 size, cap0, cap1, ...}
            let n = captures.len();
            let env_size_bytes = 8u64 + (n as u64 * 8); // size header + 8 bytes per capture (ptr/i64)
            let env_size_val = i64_ty.const_int(env_size_bytes, false);
            let env_mem = self.builder.call(self.rt.alloc, &[env_size_val.into()], "ir_env").try_as_basic_value().unwrap_basic().into_pointer_value();
            // Write size at offset 0.
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

}