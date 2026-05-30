use inkwell::builder::Builder;
use inkwell::context::Context;
use inkwell::module::Module;
use inkwell::passes::PassBuilderOptions;
use inkwell::targets::{
    CodeModel, FileType, InitializationConfig, RelocMode, Target, TargetMachine,
};
use inkwell::types::{BasicMetadataTypeEnum, BasicType, BasicTypeEnum};
use inkwell::values::{
    BasicMetadataValueEnum, BasicValueEnum, FunctionValue, PointerValue,
};
use inkwell::{AddressSpace, IntPredicate, FloatPredicate, OptimizationLevel};
use std::collections::HashMap;
use std::path::Path;

use lin_check::typed_ir::*;
use lin_check::types::Type;
use lin_parse::ast::BinOp;
use lin_ir::ir as lir;
use crate::coverage::{self, CoverageEmitter};

pub struct Codegen<'ctx> {
    context: &'ctx Context,
    module: Module<'ctx>,
    builder: Builder<'ctx>,
    // Runtime function declarations
    rt_string_from_bytes: FunctionValue<'ctx>,
    rt_string_length: FunctionValue<'ctx>,
    rt_string_eq: FunctionValue<'ctx>,
    rt_print: FunctionValue<'ctx>,
    rt_panic: FunctionValue<'ctx>,
    rt_array_alloc: FunctionValue<'ctx>,
    rt_array_push: FunctionValue<'ctx>,
    rt_array_get: FunctionValue<'ctx>,
    rt_int_to_string: FunctionValue<'ctx>,
    rt_float_to_string: FunctionValue<'ctx>,
    rt_bool_to_string: FunctionValue<'ctx>,
    rt_null_to_string: FunctionValue<'ctx>,
    rt_alloc: FunctionValue<'ctx>,
    // Tagged union boxing/unboxing runtime functions
    rt_box_null: FunctionValue<'ctx>,
    rt_box_bool: FunctionValue<'ctx>,
    rt_box_int32: FunctionValue<'ctx>,
    rt_box_int64: FunctionValue<'ctx>,
    rt_box_float64: FunctionValue<'ctx>,
    rt_box_str: FunctionValue<'ctx>,
    rt_box_object: FunctionValue<'ctx>,
    rt_box_array: FunctionValue<'ctx>,
    rt_box_function: FunctionValue<'ctx>,
    rt_get_tag: FunctionValue<'ctx>,
    rt_unbox_int32: FunctionValue<'ctx>,
    rt_unbox_int64: FunctionValue<'ctx>,
    rt_unbox_float64: FunctionValue<'ctx>,
    rt_unbox_bool: FunctionValue<'ctx>,
    rt_unbox_ptr: FunctionValue<'ctx>,
    // Dynamic object runtime functions
    rt_object_alloc: FunctionValue<'ctx>,
    rt_object_set: FunctionValue<'ctx>,
    rt_object_get: FunctionValue<'ctx>,
    rt_object_eq: FunctionValue<'ctx>,
    rt_tagged_to_string: FunctionValue<'ctx>,
    // Single-allocation multi-part string build
    // Retain function (increment refcount)
    rt_rc_retain: FunctionValue<'ctx>,
    // Release functions (decrement refcount, free if zero)
    rt_string_release: FunctionValue<'ctx>,
    rt_array_release: FunctionValue<'ctx>,
    rt_object_release: FunctionValue<'ctx>,
    rt_closure_release: FunctionValue<'ctx>,
    rt_tagged_release: FunctionValue<'ctx>,
    // Cached LLVM types
    string_ptr_type: inkwell::types::PointerType<'ctx>,
    array_ptr_type: inkwell::types::PointerType<'ctx>,
    // Named functions (for call resolution and TCO detection)
    named_fns: HashMap<String, FunctionValue<'ctx>>,
    // Intrinsic slot -> name map from type checker
    intrinsic_slots: HashMap<usize, String>,
    // Global function slots: slot -> FunctionValue (top-level named functions)
    // Counter for anonymous closures
    closure_count: usize,
    // Map from (module_path, export_name) -> FunctionValue for compiled imports
    imported_fns: HashMap<(String, String), FunctionValue<'ctx>>,
    // Map from (module_path, export_name) -> FunctionValue for non-function exported vals.
    // Each wrapper is a zero-arg function that computes and returns the val's value.
    imported_val_wrappers: HashMap<(String, String), FunctionValue<'ctx>>,
    /// Paths to foreign libraries collected from ForeignImport statements (for the linker).
    pub foreign_lib_paths: Vec<String>,
    /// Global val slots: slot -> LLVM GlobalValue (for non-function top-level vals).
    /// Closures without explicit captures access these via load instructions.
    /// Module-level slot map active while compiling a module. Closures compiled inside
    /// imported module bodies use this to resolve sibling function calls.
    /// Symbol prefix for anonymous (`__lin_fn_<id>`) functions emitted by
    /// `compile_module_from_ir`. Empty for the main module; set to a per-module key (e.g.
    /// `std_test_`) while compiling an imported module on the IR path, so anonymous-function
    /// symbols don't collide across modules (each module's lowering numbers FuncIds from 0).
    ir_anon_prefix: String,
    /// Coverage emitter: Some if compiling with coverage instrumentation.
    pub coverage: Option<CoverageEmitter<'ctx>>,
    /// The source file currently being compiled, used to map IR block spans to
    /// coverage regions: (file index into the coverage emitter, source text). `None`
    /// when coverage is off or the current module's source isn't tracked (suppresses
    /// instrumentation, e.g. for stdlib imports).
    current_source: Option<(u32, std::rc::Rc<str>)>,
}

impl<'ctx> Codegen<'ctx> {
    pub fn new(context: &'ctx Context, module_name: &str, coverage_enabled: bool) -> Self {
        let module = context.create_module(module_name);
        let builder = context.create_builder();

        // Opaque pointer for string (ptr to LinString struct in runtime)
        let string_ptr_type = context.ptr_type(AddressSpace::default());
        let array_ptr_type = context.ptr_type(AddressSpace::default());
        let ptr_type = context.ptr_type(AddressSpace::default());

        // Declare runtime functions (C ABI, defined in lin-runtime)
        let i32_type = context.i32_type();
        let i64_type = context.i64_type();
        let void_type = context.void_type();
        let bool_type = context.bool_type();

        let rt_string_from_bytes = module.add_function(
            "lin_string_from_bytes",
            string_ptr_type.fn_type(&[ptr_type.into(), i32_type.into()], false),
            None,
        );
        let rt_string_length = module.add_function(
            "lin_string_length",
            i32_type.fn_type(&[string_ptr_type.into()], false),
            None,
        );
        let rt_string_eq = module.add_function(
            "lin_string_eq",
            bool_type.fn_type(&[string_ptr_type.into(), string_ptr_type.into()], false),
            None,
        );
        let rt_print = module.add_function(
            "lin_print",
            void_type.fn_type(&[string_ptr_type.into()], false),
            None,
        );
        let rt_panic = module.add_function(
            "lin_panic",
            void_type.fn_type(&[string_ptr_type.into(), i32_type.into(), i32_type.into()], false),
            None,
        );
        // lin_array_alloc(initial_capacity: i64) -> ptr
        let rt_array_alloc = module.add_function(
            "lin_array_alloc",
            ptr_type.fn_type(&[i64_type.into()], false),
            None,
        );
        // lin_array_push(arr: ptr, elem: ptr, tag: i8) -> void
        let rt_array_push = module.add_function(
            "lin_array_push",
            void_type.fn_type(&[ptr_type.into(), ptr_type.into(), context.i8_type().into()], false),
            None,
        );
        // lin_array_get(arr: ptr, idx: i64) -> ptr (tagged element)
        let rt_array_get = module.add_function(
            "lin_array_get",
            ptr_type.fn_type(&[ptr_type.into(), i64_type.into()], false),
            None,
        );
        // lin_array_get_tagged(arr: ptr, idx: i64) -> ptr (TaggedVal*) — handles flat + tagged arrays
        // lin_array_length(arr: ptr) -> i64
        // lin_alloc(size: i64) -> ptr — general heap allocation for closures/envs
        let rt_alloc = module.add_function(
            "lin_alloc",
            ptr_type.fn_type(&[i64_type.into()], false),
            None,
        );
        // Numeric to string conversions
        let rt_int_to_string = module.add_function(
            "lin_int_to_string",
            string_ptr_type.fn_type(&[i64_type.into()], false),
            None,
        );
        let rt_float_to_string = module.add_function(
            "lin_float_to_string",
            string_ptr_type.fn_type(&[context.f64_type().into()], false),
            None,
        );
        let rt_bool_to_string = module.add_function(
            "lin_bool_to_string",
            string_ptr_type.fn_type(&[bool_type.into()], false),
            None,
        );
        let rt_null_to_string = module.add_function(
            "lin_null_to_string",
            string_ptr_type.fn_type(&[], false),
            None,
        );
        // Tagged union boxing/unboxing
        let i8_type = context.i8_type();
        let rt_box_null = module.add_function("lin_box_null", ptr_type.fn_type(&[], false), None);
        let rt_box_bool = module.add_function("lin_box_bool", ptr_type.fn_type(&[i8_type.into()], false), None);
        let rt_box_int32 = module.add_function("lin_box_int32", ptr_type.fn_type(&[i32_type.into()], false), None);
        let rt_box_int64 = module.add_function("lin_box_int64", ptr_type.fn_type(&[i64_type.into()], false), None);
        let rt_box_float64 = module.add_function("lin_box_float64", ptr_type.fn_type(&[context.f64_type().into()], false), None);
        let rt_box_str = module.add_function("lin_box_str", ptr_type.fn_type(&[ptr_type.into()], false), None);
        let rt_box_object = module.add_function("lin_box_object", ptr_type.fn_type(&[ptr_type.into()], false), None);
        let rt_box_array = module.add_function("lin_box_array", ptr_type.fn_type(&[ptr_type.into()], false), None);
        let rt_box_function = module.add_function("lin_box_function", ptr_type.fn_type(&[ptr_type.into()], false), None);
        let rt_get_tag = module.add_function("lin_get_tag", i8_type.fn_type(&[ptr_type.into()], false), None);
        let rt_unbox_int32 = module.add_function("lin_unbox_int32", i32_type.fn_type(&[ptr_type.into()], false), None);
        let rt_unbox_int64 = module.add_function("lin_unbox_int64", i64_type.fn_type(&[ptr_type.into()], false), None);
        let rt_unbox_float64 = module.add_function("lin_unbox_float64", context.f64_type().fn_type(&[ptr_type.into()], false), None);
        let rt_unbox_bool = module.add_function("lin_unbox_bool", i8_type.fn_type(&[ptr_type.into()], false), None);
        let rt_unbox_ptr = module.add_function("lin_unbox_ptr", ptr_type.fn_type(&[ptr_type.into()], false), None);
        // Dynamic object runtime functions
        // lin_tagged_to_string(tagged: ptr) -> ptr (LinString*)
        let rt_tagged_to_string = module.add_function("lin_tagged_to_string", string_ptr_type.fn_type(&[ptr_type.into()], false), None);
        // lin_object_alloc(initial_cap: i32) -> ptr
        let rt_object_alloc = module.add_function("lin_object_alloc", ptr_type.fn_type(&[i32_type.into()], false), None);
        // lin_object_set(obj: ptr, key: ptr, val: ptr) -> void
        let rt_object_set = module.add_function("lin_object_set", void_type.fn_type(&[ptr_type.into(), ptr_type.into(), ptr_type.into()], false), None);
        // lin_object_get(obj: ptr, key: ptr) -> ptr (points to TaggedVal, or null)
        let rt_object_get = module.add_function("lin_object_get", ptr_type.fn_type(&[ptr_type.into(), ptr_type.into()], false), None);
        // lin_object_has(obj: ptr, key: ptr) -> i8
        // lin_object_eq(a: ptr, b: ptr) -> i8
        let rt_object_eq = module.add_function("lin_object_eq", i8_type.fn_type(&[ptr_type.into(), ptr_type.into()], false), None);
        // lin_string_build_n(parts: ptr, n: i32) -> ptr — single-allocation multi-part concat
        // Release functions: decrement refcount, free if zero.
        let rt_rc_retain = module.add_function("lin_rc_retain", void_type.fn_type(&[ptr_type.into()], false), None);
        let rt_string_release = module.add_function("lin_string_release", void_type.fn_type(&[ptr_type.into()], false), None);
        let rt_array_release = module.add_function("lin_array_release", void_type.fn_type(&[ptr_type.into()], false), None);
        let rt_object_release = module.add_function("lin_object_release", void_type.fn_type(&[ptr_type.into()], false), None);
        let rt_closure_release = module.add_function("lin_closure_release", void_type.fn_type(&[ptr_type.into()], false), None);
        let rt_tagged_release = module.add_function("lin_tagged_release", void_type.fn_type(&[ptr_type.into()], false), None);

        Self {
            context,
            module,
            builder,
            rt_string_from_bytes,
            rt_string_length,
            rt_string_eq,
            rt_print,
            rt_panic,
            rt_array_alloc,
            rt_array_push,
            rt_array_get,
            rt_int_to_string,
            rt_float_to_string,
            rt_bool_to_string,
            rt_null_to_string,
            rt_alloc,
            rt_box_null,
            rt_box_bool,
            rt_box_int32,
            rt_box_int64,
            rt_box_float64,
            rt_box_str,
            rt_box_object,
            rt_box_array,
            rt_box_function,
            rt_get_tag,
            rt_unbox_int32,
            rt_unbox_int64,
            rt_unbox_float64,
            rt_unbox_bool,
            rt_unbox_ptr,
            rt_object_alloc,
            rt_object_set,
            rt_object_get,
            rt_object_eq,
            rt_tagged_to_string,
            rt_rc_retain,
            rt_string_release,
            rt_array_release,
            rt_object_release,
            rt_closure_release,
            rt_tagged_release,
            string_ptr_type,
            array_ptr_type,
            named_fns: HashMap::new(),
            intrinsic_slots: HashMap::new(),
            closure_count: 0,
            imported_fns: HashMap::new(),
            imported_val_wrappers: HashMap::new(),
            foreign_lib_paths: Vec::new(),
            ir_anon_prefix: String::new(),
            coverage: if coverage_enabled {
                // Source path is set by set_main_source; start with empty path.
                Some(CoverageEmitter::new(String::new()))
            } else {
                None
            },
            current_source: None,
        }
    }

    /// Set the main module's source path + text for coverage. Index 0 of the coverage
    /// emitter's source list is reserved for the main module.
    pub fn set_main_source(&mut self, path: &str, text: &str) {
        if let Some(cov) = &mut self.coverage {
            cov.source_files[0] = path.to_string();
            cov.source_texts[0] = text.to_string();
            self.current_source = Some((0, std::rc::Rc::from(text)));
        }
    }

    /// Emit the module-level coverage globals (covmap, covfun records, prf names). Call
    /// once, after every module (main + imports) has been compiled. No-op without coverage.
    pub fn finalize_coverage(&mut self) {
        if let Some(cov) = self.coverage.take() {
            cov.finalize(self.context, &self.module);
        }
    }

    /// IR-pipeline equivalent of `register_import`: lower the imported module to a LinModule
    /// (named functions + `__val` wrappers, no `main`), run RC elision, emit it via the same
    /// `compile_module_from_ir` codegen used for the main module, then register the emitted
    /// LLVM functions in `imported_fns` / `imported_val_wrappers` so the importing module's
    /// IR resolves them by mangled symbol name. This removes the IR path's dependency on the
    /// AST `compile_function_body` / `compile_expr` for imports.
    pub fn compile_import_from_ir(
        &mut self,
        path: &str,
        module: &TypedModule,
        src: Option<&(String, String)>,
    ) {
        // Merge the imported module's intrinsic slot map (same as register_import) so the
        // importer's lowering still recognises re-exported intrinsics.
        for (slot, name) in &module.intrinsics {
            self.intrinsic_slots.insert(*slot, name.clone());
        }

        let module_key = lin_ir::mangle_module_key(path);
        let mut ir_module = lin_ir::lower_import_module(module, &module_key);
        lin_ir::rc_elide::elide_rc(&mut ir_module);
        // Prefix this module's anonymous functions so `__lin_fn_<id>` symbols don't collide
        // with the main module's or other imports' (each module numbers FuncIds from 0).
        let saved_prefix = std::mem::replace(&mut self.ir_anon_prefix, format!("{}_", module_key));
        // Point coverage at this import's source (if any). Stdlib imports pass `None`, which
        // suppresses instrumentation for them (the compile pre-resolver only tracks
        // non-stdlib import sources).
        let saved_source = self.current_source.take();
        if self.coverage.is_some() {
            self.current_source = match src {
                Some((p, text)) => {
                    let idx = self.coverage.as_mut().unwrap().add_source_file(p, text);
                    Some((idx, std::rc::Rc::from(text.as_str())))
                }
                None => None,
            };
        }
        self.compile_module_from_ir(&ir_module);
        self.ir_anon_prefix = saved_prefix;
        self.current_source = saved_source;

        // Register each exported binding's emitted LLVM symbol so importers resolve it.
        // Function exports → `imported_fns[(path, name)]`; non-function vals → the
        // `imported_val_wrappers[(path, name)]` zero-arg wrapper.
        for stmt in &module.statements {
            if let TypedStmt::Val { value, name: Some(name), .. } = stmt {
                if matches!(value, TypedExpr::Function { .. }) {
                    let sym = format!("{}_{}", module_key, name);
                    if let Some(f) = self.module.get_function(&sym) {
                        self.imported_fns.insert((path.to_string(), name.clone()), f);
                        self.named_fns.insert(name.clone(), f);
                    }
                } else {
                    let sym = format!("{}_{}__val", module_key, name);
                    if let Some(f) = self.module.get_function(&sym) {
                        self.imported_val_wrappers.insert((path.to_string(), name.clone()), f);
                    }
                }
            }
        }
    }


    pub fn run_optimization_passes(&self) -> Result<(), String> {
        Target::initialize_all(&InitializationConfig::default());
        let triple = TargetMachine::get_default_triple();
        let target = Target::from_triple(&triple).map_err(|e| e.to_string())?;
        let cpu = TargetMachine::get_host_cpu_name();
        let features = TargetMachine::get_host_cpu_features();
        let machine = target
            .create_target_machine(
                &triple,
                cpu.to_str().unwrap_or("generic"),
                features.to_str().unwrap_or(""),
                OptimizationLevel::Aggressive,
                RelocMode::Default,
                CodeModel::Default,
            )
            .ok_or("Failed to create target machine for optimization")?;

        let options = PassBuilderOptions::create();
        self.module
            .run_passes("default<O2>", &machine, options)
            .map_err(|e| e.to_string())
    }

    pub fn emit_object_file(&self, output_path: &Path) -> Result<(), String> {
        Target::initialize_all(&InitializationConfig::default());

        let triple = TargetMachine::get_default_triple();
        let target = Target::from_triple(&triple).map_err(|e| e.to_string())?;
        let cpu = TargetMachine::get_host_cpu_name();
        let features = TargetMachine::get_host_cpu_features();

        let machine = target
            .create_target_machine(
                &triple,
                cpu.to_str().unwrap_or("generic"),
                features.to_str().unwrap_or(""),
                OptimizationLevel::Aggressive,
                RelocMode::Default,
                CodeModel::Default,
            )
            .ok_or("Failed to create target machine")?;

        machine
            .write_to_file(&self.module, FileType::Object, output_path)
            .map_err(|e| e.to_string())
    }

    pub fn emit_llvm_ir(&self, output_path: &Path) -> Result<(), String> {
        self.module
            .print_to_file(output_path)
            .map_err(|e| e.to_string())
    }

    pub fn verify(&self) -> Result<(), String> {
        self.module.verify().map_err(|e| e.to_string())
    }

    // -------------------------------------------------------------------------
    // LLVM type mapping
    // -------------------------------------------------------------------------

    fn llvm_type(&self, ty: &Type) -> BasicTypeEnum<'ctx> {
        match ty {
            Type::Bool => self.context.bool_type().into(),
            Type::Int8 => self.context.i8_type().into(),
            Type::Int16 => self.context.i16_type().into(),
            Type::Int32 => self.context.i32_type().into(),
            Type::Int64 => self.context.i64_type().into(),
            Type::UInt8 => self.context.i8_type().into(),
            Type::UInt16 => self.context.i16_type().into(),
            Type::UInt32 => self.context.i32_type().into(),
            Type::UInt64 => self.context.i64_type().into(),
            Type::Float32 => self.context.f32_type().into(),
            Type::Float64 => self.context.f64_type().into(),
            Type::Str => self.string_ptr_type.into(),
            Type::Null => {
                // Null is represented as a pointer (null ptr), same as Union/TypeVar.
                // This ensures Null-typed vars can hold tagged values assigned later.
                self.context.ptr_type(AddressSpace::default()).into()
            }
            Type::Array(_) | Type::FixedArray(_) => self.array_ptr_type.into(),
            Type::Object(_) => self.context.ptr_type(AddressSpace::default()).into(),
            Type::Union(_) => {
                // Tagged union: { i8 tag, [8 x i8] payload } — 9 bytes total.
                // We use an opaque pointer to a heap-allocated tagged value.
                self.context.ptr_type(AddressSpace::default()).into()
            }
            Type::Function { .. } => {
                // Closures are represented as { fn_ptr, env_ptr } pairs.
                // Returns a pointer to the closure struct.
                self.context.ptr_type(AddressSpace::default()).into()
            }
            Type::Iterator(_) => self.context.ptr_type(AddressSpace::default()).into(),
            Type::Never => self.context.i8_type().into(), // unreachable
            Type::TypeVar(_) => {
                // Unresolved type var — use opaque pointer (Json/"any" type at runtime)
                self.context.ptr_type(AddressSpace::default()).into()
            }
            Type::Named(_) => {
                // Named recursive type reference — use opaque pointer (heap-allocated object)
                self.context.ptr_type(AddressSpace::default()).into()
            }
        }
    }

    fn llvm_param_type(&self, ty: &Type) -> BasicMetadataTypeEnum<'ctx> {
        self.llvm_type(ty).into()
    }

    /// True if `ty` is a union or TypeVar (i.e., needs tagged representation).
    fn is_union_type(ty: &Type) -> bool {
        matches!(ty, Type::Union(_) | Type::TypeVar(_) | Type::Named(_))
    }


    /// Returns the LLVM struct type for a closure header.
    ///
    /// Layout (32 bytes):
    ///   field 0: i32  refcount
    ///   field 1: i32  _pad
    ///   field 2: ptr  fn_ptr
    ///   field 3: ptr  env_ptr
    ///
    /// A trailing u64 env_size lives at offset 24 and is written directly via GEP on the
    /// raw allocation rather than as a struct field, because the closure struct type is
    /// referenced in many places and keeping it to 4 fields keeps all the call-site GEPs
    /// consistent.  The env_size write is done once at closure creation.
    fn closure_struct_type(&self) -> inkwell::types::StructType<'ctx> {
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i32_ty = self.context.i32_type();
        self.context.struct_type(&[i32_ty.into(), i32_ty.into(), ptr_ty.into(), ptr_ty.into()], false)
    }





    /// Concrete (non-boxed) reference-counted heap types: the IR lowerer tracks these for
    /// scope-exit release (mirrors `lin_ir`'s `is_rc_type`), and a cell/global holding one
    /// owns an independent reference (the lowerer retains on store). Boxed Json/union/Named
    /// values are excluded: they follow the legacy borrow model where the value's true owner
    /// frees it, so a cell/global must NOT release them on reassignment (double-free).
    ///
    /// This MUST stay in lockstep with `lin_ir::lower::is_rc_type`: codegen releases the old
    /// value on reassignment only for types the lowerer also retained on store. A type present
    /// here but absent there would be released without a matching retain — a refcount underflow.
    /// (`Iterator` is deliberately omitted for that reason: the lowerer does not retain it.)
    fn ty_is_concrete_rc(ty: &Type) -> bool {
        matches!(
            ty,
            Type::Str
                | Type::Array(_)
                | Type::FixedArray(_)
                | Type::Object(_)
                | Type::Function { .. }
        )
    }

    /// Emit a type-dispatched release call for a heap-allocated value.
    /// No-op for scalars (non-pointer LLVM values) and null pointers.
    fn emit_release(&mut self, val: BasicValueEnum<'ctx>, ty: &Type) {
        if !val.is_pointer_value() { return; }
        let ptr = val.into_pointer_value();
        match ty {
            Type::Str => { self.builder.build_call(self.rt_string_release, &[ptr.into()], "").unwrap(); }
            Type::Array(_) | Type::FixedArray(_) | Type::Iterator(_) => { self.builder.build_call(self.rt_array_release, &[ptr.into()], "").unwrap(); }
            Type::Object(_) => { self.builder.build_call(self.rt_object_release, &[ptr.into()], "").unwrap(); }
            Type::Function { .. } => { self.builder.build_call(self.rt_closure_release, &[ptr.into()], "").unwrap(); }
            Type::TypeVar(_) | Type::Union(_) => { self.builder.build_call(self.rt_tagged_release, &[ptr.into()], "").unwrap(); }
            _ => {} // scalars: nothing to release
        }
    }


    /// Wrap a named (top-level) LLVM function in a closure struct with a thin adapter.
    /// Named functions have signature `(T1, T2, ...) -> R` (no env_ptr).
    /// The closure ABI expects `(ptr env, T1, T2, ...) -> R`.
    /// We generate a wrapper `__cls_wrap_N(ptr _env, T1, T2, ...) -> R` that forwards the call.
    /// IR-path variant of `wrap_named_fn_as_closure`: the wrapper returns a boxed
    /// TaggedVal* (ptr), matching the uniform closure ABI the IR indirect-call path uses
    /// (where every closure returns Json and the caller unboxes). The wrapped function's
    /// concrete scalar/pointer return is boxed before returning.
    fn wrap_named_fn_as_closure_boxed(&mut self, named_fn: FunctionValue<'ctx>) -> BasicValueEnum<'ctx> {
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
            let saved_block = self.builder.get_insert_block().unwrap();
            let entry = self.context.append_basic_block(wf, "entry");
            self.builder.position_at_end(entry);
            let fwd_args: Vec<BasicMetadataValueEnum> = (1..wf.count_params())
                .map(|i| wf.get_nth_param(i).unwrap().into())
                .collect();
            let call = self.builder.build_call(named_fn, &fwd_args, "wfwd").unwrap();
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
            self.builder.build_return(Some(&boxed)).unwrap();
            self.builder.position_at_end(saved_block);
            wf
        };
        // Build {rc, _pad, fn_ptr, null_env} closure struct.
        let lin_alloc_fn = self.get_or_declare_fn("lin_alloc",
            ptr_ty.fn_type(&[self.context.i64_type().into()], false));
        let cls_mem = self.builder.build_call(lin_alloc_fn,
            &[self.context.i64_type().const_int(32, false).into()], "wnfnb_cls")
            .unwrap().try_as_basic_value().unwrap_basic().into_pointer_value();
        let cls_ty = self.closure_struct_type();
        let rc_field = self.builder.build_struct_gep(cls_ty, cls_mem, 0, "wnfnb_rc").unwrap();
        self.builder.build_store(rc_field, self.context.i32_type().const_int(1, false)).unwrap();
        let fn_field = self.builder.build_struct_gep(cls_ty, cls_mem, 2, "wnfnb_fp").unwrap();
        self.builder.build_store(fn_field, wrapper_fn.as_global_value().as_pointer_value()).unwrap();
        let env_field = self.builder.build_struct_gep(cls_ty, cls_mem, 3, "wnfnb_ep").unwrap();
        self.builder.build_store(env_field, ptr_ty.const_null()).unwrap();
        cls_mem.into()
    }


    /// Box a value into a tagged union pointer (TaggedVal*).
    /// For concrete types, allocates and fills a TaggedVal with the appropriate tag.
    /// For TypeVar, dispatches on the actual LLVM type (int/float/pointer) to pick the right box call.
    fn box_value(&mut self, val: BasicValueEnum<'ctx>, val_ty: &Type) -> BasicValueEnum<'ctx> {
        let ptr = match val_ty {
            Type::Null => self.builder.build_call(self.rt_box_null, &[], "boxnull").unwrap()
                .try_as_basic_value().unwrap_basic(),
            Type::Bool => {
                let i8v = if val.is_int_value() {
                    // Bool is i1; zero-extend to i8 for lin_box_bool(i8).
                    self.builder.build_int_z_extend_or_bit_cast(val.into_int_value(), self.context.i8_type(), "btoi8").unwrap().into()
                } else { val };
                self.builder.build_call(self.rt_box_bool, &[i8v.into()], "boxbool").unwrap()
                    .try_as_basic_value().unwrap_basic()
            }
            Type::Int8 | Type::Int16 | Type::Int32 => {
                let i32v = self.builder.build_int_s_extend_or_bit_cast(val.into_int_value(), self.context.i32_type(), "toi32").unwrap();
                self.builder.build_call(self.rt_box_int32, &[i32v.into()], "boxi32").unwrap()
                    .try_as_basic_value().unwrap_basic()
            }
            Type::UInt8 | Type::UInt16 | Type::UInt32 => {
                let i32v = self.builder.build_int_z_extend_or_bit_cast(val.into_int_value(), self.context.i32_type(), "tou32").unwrap();
                self.builder.build_call(self.rt_box_int32, &[i32v.into()], "boxi32").unwrap()
                    .try_as_basic_value().unwrap_basic()
            }
            Type::Int64 | Type::UInt64 => {
                let i64v = val.into_int_value();
                self.builder.build_call(self.rt_box_int64, &[i64v.into()], "boxi64").unwrap()
                    .try_as_basic_value().unwrap_basic()
            }
            Type::Float32 => {
                let f64v = self.builder.build_float_ext(val.into_float_value(), self.context.f64_type(), "f32tof64").unwrap();
                self.builder.build_call(self.rt_box_float64, &[f64v.into()], "boxf64").unwrap()
                    .try_as_basic_value().unwrap_basic()
            }
            Type::Float64 => {
                self.builder.build_call(self.rt_box_float64, &[val.into()], "boxf64").unwrap()
                    .try_as_basic_value().unwrap_basic()
            }
            Type::Str => {
                self.builder.build_call(self.rt_box_str, &[val.into()], "boxstr").unwrap()
                    .try_as_basic_value().unwrap_basic()
            }
            Type::Object(_) => {
                self.builder.build_call(self.rt_box_object, &[val.into()], "boxobj").unwrap()
                    .try_as_basic_value().unwrap_basic()
            }
            Type::Array(_) if val.is_pointer_value() => {
                // Box the LinArray* directly (flat or tagged). The elem_tag field in LinArray
                // lets runtime functions (lin_array_get_tagged, lin_push_dyn, etc.) dispatch
                // correctly without needing a separate conversion copy.
                self.builder.build_call(self.rt_box_array, &[val.into()], "boxarr").unwrap()
                    .try_as_basic_value().unwrap_basic()
            }
            Type::Array(_) | Type::FixedArray(_) | Type::Iterator(_) => {
                // Iterator values have already been converted to tagged arrays by the intrinsic.
                self.builder.build_call(self.rt_box_array, &[val.into()], "boxarr").unwrap()
                    .try_as_basic_value().unwrap_basic()
            }
            Type::Function { .. } => {
                self.builder.build_call(self.rt_box_function, &[val.into()], "boxfn").unwrap()
                    .try_as_basic_value().unwrap_basic()
            }
            // Union type — if value is a pointer, box as object (most common case).
            // If it's already a tagged pointer, return as-is.
            Type::Union(variants) => {
                if val.is_pointer_value() {
                    // If all variants are Object types, this is a LinObject*.
                    let all_objects = variants.iter().all(|v| matches!(v, Type::Object(_)));
                    if all_objects {
                        self.builder.build_call(self.rt_box_object, &[val.into()], "boxobj").unwrap()
                            .try_as_basic_value().unwrap_basic()
                    } else {
                        // Already tagged (or unknown) — return as-is.
                        val
                    }
                } else {
                    val
                }
            }
            Type::TypeVar(_) => {
                // TypeVar value — box by actual LLVM type.
                if val.is_int_value() {
                    let iv = val.into_int_value();
                    let i32_ty = self.context.i32_type();
                    let i64_ty = self.context.i64_type();
                    let bit_width = iv.get_type().get_bit_width();
                    if bit_width <= 32 {
                        let i32v = self.builder.build_int_s_extend_or_bit_cast(iv, i32_ty, "tvi32").unwrap();
                        self.builder.build_call(self.rt_box_int32, &[i32v.into()], "tvboxi32").unwrap()
                            .try_as_basic_value().unwrap_basic()
                    } else {
                        let i64v = self.builder.build_int_s_extend_or_bit_cast(iv, i64_ty, "tvi64").unwrap();
                        self.builder.build_call(self.rt_box_int64, &[i64v.into()], "tvboxi64").unwrap()
                            .try_as_basic_value().unwrap_basic()
                    }
                } else if val.is_float_value() {
                    let fv = val.into_float_value();
                    let f64_ty = self.context.f64_type();
                    let f64v = self.builder.build_float_ext(fv, f64_ty, "tvf64").unwrap();
                    self.builder.build_call(self.rt_box_float64, &[f64v.into()], "tvboxf64").unwrap()
                        .try_as_basic_value().unwrap_basic()
                } else {
                    val
                }
            }
            _ => val,
        };
        ptr
    }

    /// Unbox a tagged union pointer to the concrete type `target_ty`.
    fn unbox_value(&mut self, ptr: BasicValueEnum<'ctx>, target_ty: &Type) -> BasicValueEnum<'ctx> {
        let ptr_val = ptr.into_pointer_value();
        match target_ty {
            Type::Null => self.context.ptr_type(AddressSpace::default()).const_null().into(),
            Type::Bool => {
                let v = self.builder.build_call(self.rt_unbox_bool, &[ptr_val.into()], "ubool").unwrap()
                    .try_as_basic_value().unwrap_basic();
                // Convert i8 to i1
                self.builder.build_int_truncate(v.into_int_value(), self.context.bool_type(), "utobool").unwrap().into()
            }
            Type::Int8 | Type::Int16 | Type::Int32 => {
                let v = self.builder.build_call(self.rt_unbox_int32, &[ptr_val.into()], "ui32").unwrap()
                    .try_as_basic_value().unwrap_basic();
                let ity = self.llvm_type(target_ty).into_int_type();
                self.builder.build_int_truncate_or_bit_cast(v.into_int_value(), ity, "toi").unwrap().into()
            }
            Type::UInt8 | Type::UInt16 | Type::UInt32 => {
                let v = self.builder.build_call(self.rt_unbox_int32, &[ptr_val.into()], "uu32").unwrap()
                    .try_as_basic_value().unwrap_basic();
                let ity = self.llvm_type(target_ty).into_int_type();
                self.builder.build_int_truncate_or_bit_cast(v.into_int_value(), ity, "toui").unwrap().into()
            }
            Type::Int64 | Type::UInt64 => {
                self.builder.build_call(self.rt_unbox_int64, &[ptr_val.into()], "ui64").unwrap()
                    .try_as_basic_value().unwrap_basic()
            }
            Type::Float32 | Type::Float64 => {
                let v = self.builder.build_call(self.rt_unbox_float64, &[ptr_val.into()], "uf64").unwrap()
                    .try_as_basic_value().unwrap_basic();
                if matches!(target_ty, Type::Float32) {
                    self.builder.build_float_trunc(v.into_float_value(), self.context.f32_type(), "tof32").unwrap().into()
                } else {
                    v
                }
            }
            Type::Str => {
                self.builder.build_call(self.rt_unbox_ptr, &[ptr_val.into()], "ustr").unwrap()
                    .try_as_basic_value().unwrap_basic()
            }
            Type::Object(_) | Type::Array(_) | Type::FixedArray(_) | Type::Function { .. } => {
                self.builder.build_call(self.rt_unbox_ptr, &[ptr_val.into()], "uptr").unwrap()
                    .try_as_basic_value().unwrap_basic()
            }
            // Already tagged — return as-is
            Type::Union(_) | Type::TypeVar(_) => ptr,
            _ => ptr,
        }
    }

    /// Tag constant for a concrete type (for `is` type checks).
    fn type_tag(ty: &Type) -> u8 {
        match ty {
            Type::Null => 0,
            Type::Bool => 1,
            Type::Int8 | Type::Int16 | Type::Int32 | Type::UInt8 | Type::UInt16 | Type::UInt32 => 2,
            Type::Int64 | Type::UInt64 => 3,
            Type::Float32 => 4,
            Type::Float64 => 5,
            Type::Str => 6,
            Type::Object(_) => 7,
            Type::Array(_) | Type::FixedArray(_) | Type::Iterator(_) => 8,
            Type::Function { .. } => 9,
            _ => 0,
        }
    }

    // -------------------------------------------------------------------------
    // Function declaration (without body — used for forward refs)
    // -------------------------------------------------------------------------


    // -------------------------------------------------------------------------
    // Function body compilation
    // -------------------------------------------------------------------------



    // -------------------------------------------------------------------------
    // Statement compilation
    // -------------------------------------------------------------------------


    // -------------------------------------------------------------------------
    // Expression compilation
    // -------------------------------------------------------------------------


    // -------------------------------------------------------------------------
    // Literals
    // -------------------------------------------------------------------------

    fn compile_int_lit(&self, v: i64, ty: &Type) -> BasicValueEnum<'ctx> {
        match ty {
            Type::Int8 | Type::UInt8 => self.context.i8_type().const_int(v as u64, ty.is_signed()).into(),
            Type::Int16 | Type::UInt16 => self.context.i16_type().const_int(v as u64, ty.is_signed()).into(),
            Type::Int32 | Type::UInt32 => self.context.i32_type().const_int(v as u64, ty.is_signed()).into(),
            Type::Int64 | Type::UInt64 => self.context.i64_type().const_int(v as u64, ty.is_signed()).into(),
            _ => self.context.i32_type().const_int(v as u64, true).into(),
        }
    }

    fn compile_float_lit(&self, v: f64, ty: &Type) -> BasicValueEnum<'ctx> {
        match ty {
            Type::Float32 => self.context.f32_type().const_float(v).into(),
            Type::Float64 => self.context.f64_type().const_float(v).into(),
            _ => self.context.f64_type().const_float(v).into(),
        }
    }

    fn compile_string_lit(&self, s: &str) -> BasicValueEnum<'ctx> {
        // Create a global constant for the byte data, then call lin_string_from_bytes.
        let bytes = s.as_bytes();
        let byte_array_type = self.context.i8_type().array_type(bytes.len() as u32);
        let const_bytes: Vec<_> = bytes
            .iter()
            .map(|&b| self.context.i8_type().const_int(b as u64, false))
            .collect();
        let const_array = self.context.i8_type().const_array(&const_bytes);
        let global = self.module.add_global(byte_array_type, None, "str_data");
        global.set_constant(true);
        global.set_initializer(&const_array);

        let ptr = global.as_pointer_value();
        let len = self.context.i32_type().const_int(bytes.len() as u64, false);

        self.builder
            .build_call(self.rt_string_from_bytes, &[ptr.into(), len.into()], "str")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
    }

    // -------------------------------------------------------------------------
    // Variables
    // -------------------------------------------------------------------------



    // -------------------------------------------------------------------------
    // Binary operators
    // -------------------------------------------------------------------------


    fn compile_add(
        &mut self,
        lv: BasicValueEnum<'ctx>,
        rv: BasicValueEnum<'ctx>,
        lty: &Type,
        _rty: &Type,
        _result_type: &Type,
    ) -> BasicValueEnum<'ctx> {
        if lty.is_float() {
            self.builder
                .build_float_add(lv.into_float_value(), rv.into_float_value(), "fadd")
                .unwrap()
                .into()
        } else {
            self.builder
                .build_int_add(lv.into_int_value(), rv.into_int_value(), "add")
                .unwrap()
                .into()
        }
    }

    fn compile_arith_op(
        &mut self,
        lv: BasicValueEnum<'ctx>,
        rv: BasicValueEnum<'ctx>,
        ty: &Type,
        op: &str,
    ) -> BasicValueEnum<'ctx> {
        if ty.is_float() {
            match op {
                "sub" => self.builder.build_float_sub(lv.into_float_value(), rv.into_float_value(), "fsub").unwrap().into(),
                "mul" => self.builder.build_float_mul(lv.into_float_value(), rv.into_float_value(), "fmul").unwrap().into(),
                _ => unreachable!(),
            }
        } else {
            match op {
                "sub" => self.builder.build_int_sub(lv.into_int_value(), rv.into_int_value(), "sub").unwrap().into(),
                "mul" => self.builder.build_int_mul(lv.into_int_value(), rv.into_int_value(), "mul").unwrap().into(),
                _ => unreachable!(),
            }
        }
    }

    fn compile_div(
        &mut self,
        lv: BasicValueEnum<'ctx>,
        rv: BasicValueEnum<'ctx>,
        ty: &Type,
    ) -> BasicValueEnum<'ctx> {
        if ty.is_float() {
            self.builder.build_float_div(lv.into_float_value(), rv.into_float_value(), "fdiv").unwrap().into()
        } else {
            self.emit_int_zero_check(rv, "division by zero");
            if ty.is_signed() {
                self.builder.build_int_signed_div(lv.into_int_value(), rv.into_int_value(), "sdiv").unwrap().into()
            } else {
                self.builder.build_int_unsigned_div(lv.into_int_value(), rv.into_int_value(), "udiv").unwrap().into()
            }
        }
    }

    fn compile_mod(
        &mut self,
        lv: BasicValueEnum<'ctx>,
        rv: BasicValueEnum<'ctx>,
        ty: &Type,
    ) -> BasicValueEnum<'ctx> {
        if ty.is_float() {
            self.builder.build_float_rem(lv.into_float_value(), rv.into_float_value(), "frem").unwrap().into()
        } else {
            self.emit_int_zero_check(rv, "modulo by zero");
            if ty.is_signed() {
                self.builder.build_int_signed_rem(lv.into_int_value(), rv.into_int_value(), "srem").unwrap().into()
            } else {
                self.builder.build_int_unsigned_rem(lv.into_int_value(), rv.into_int_value(), "urem").unwrap().into()
            }
        }
    }

    /// Emit a runtime panic if the integer value `val` is zero.
    fn emit_int_zero_check(&mut self, val: BasicValueEnum<'ctx>, msg: &str) {
        let llvm_fn = self.builder.get_insert_block().unwrap().get_parent().unwrap();
        let zero = val.into_int_value().get_type().const_zero();
        let is_zero = self.builder.build_int_compare(inkwell::IntPredicate::EQ, val.into_int_value(), zero, "divzero_chk").unwrap();
        let panic_bb = self.context.append_basic_block(llvm_fn, "divzero_panic");
        let ok_bb = self.context.append_basic_block(llvm_fn, "divzero_ok");
        self.builder.build_conditional_branch(is_zero, panic_bb, ok_bb).unwrap();
        self.builder.position_at_end(panic_bb);
        let panic_msg = self.compile_string_lit(msg);
        let zero_i32 = self.context.i32_type().const_zero();
        self.builder.build_call(self.rt_panic, &[panic_msg.into(), zero_i32.into(), zero_i32.into()], "").unwrap();
        self.builder.build_unreachable().unwrap();
        self.builder.position_at_end(ok_bb);
    }

    fn compile_eq(
        &mut self,
        lv: BasicValueEnum<'ctx>,
        rv: BasicValueEnum<'ctx>,
        ty: &Type,
        negate: bool,
    ) -> BasicValueEnum<'ctx> {
        let i64_ty = self.context.i64_type();
        let result = if *ty == Type::Str {
            self.builder
                .build_call(self.rt_string_eq, &[lv.into(), rv.into()], "seq")
                .unwrap()
                .try_as_basic_value()
                .unwrap_basic()
                .into_int_value()
        } else if matches!(ty, Type::Object(_)) {
            // Structural object equality via runtime (order-independent).
            let eq_i8 = self.builder
                .build_call(self.rt_object_eq, &[lv.into(), rv.into()], "oeq")
                .unwrap()
                .try_as_basic_value()
                .unwrap_basic()
                .into_int_value();
            self.builder.build_int_truncate(eq_i8, self.context.bool_type(), "oeq_b").unwrap()
        } else if let Type::Array(elem) = ty {
            let ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
            let fn_ty = self.context.i8_type().fn_type(&[ptr_ty.into(), ptr_ty.into()], false);
            let eq_i8 = if Self::is_flat_scalar(elem) {
                let suffix = Self::flat_suffix(elem);
                let eq_fn = self.get_or_declare_fn(&format!("lin_flat_array_eq_{}", suffix), fn_ty);
                self.builder.build_call(eq_fn, &[lv.into(), rv.into()], "aeq").unwrap()
                    .try_as_basic_value().unwrap_basic().into_int_value()
            } else {
                let eq_fn = self.get_or_declare_fn("lin_array_eq", fn_ty);
                self.builder.build_call(eq_fn, &[lv.into(), rv.into()], "aeq").unwrap()
                    .try_as_basic_value().unwrap_basic().into_int_value()
            };
            self.builder.build_int_truncate(eq_i8, self.context.bool_type(), "aeq_b").unwrap()
        } else if ty.is_float() {
            self.builder
                .build_float_compare(FloatPredicate::OEQ, lv.into_float_value(), rv.into_float_value(), "feq")
                .unwrap()
        } else if lv.is_pointer_value() || rv.is_pointer_value() {
            // Pointer comparison (closures, etc.) — compare addresses.
            let lp = if lv.is_pointer_value() {
                self.builder.build_ptr_to_int(lv.into_pointer_value(), i64_ty, "lpi").unwrap()
            } else {
                self.builder.build_int_s_extend_or_bit_cast(lv.into_int_value(), i64_ty, "lpx").unwrap()
            };
            let rp = if rv.is_pointer_value() {
                self.builder.build_ptr_to_int(rv.into_pointer_value(), i64_ty, "rpi").unwrap()
            } else {
                self.builder.build_int_s_extend_or_bit_cast(rv.into_int_value(), i64_ty, "rpx").unwrap()
            };
            self.builder.build_int_compare(IntPredicate::EQ, lp, rp, "peq").unwrap()
        } else {
            self.builder
                .build_int_compare(IntPredicate::EQ, lv.into_int_value(), rv.into_int_value(), "ieq")
                .unwrap()
        };

        if negate {
            self.builder.build_not(result, "neq").unwrap().into()
        } else {
            result.into()
        }
    }

    fn compile_cmp(
        &mut self,
        lv: BasicValueEnum<'ctx>,
        rv: BasicValueEnum<'ctx>,
        ty: &Type,
        signed_pred: IntPredicate,
        unsigned_pred: IntPredicate,
        float_pred: FloatPredicate,
    ) -> BasicValueEnum<'ctx> {
        // String comparison via runtime — pointer comparison is wrong.
        if ty == &Type::Str {
            let i32_ty = self.context.i32_type();
            let ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
            let cmp_fn_ty = i32_ty.fn_type(&[ptr_ty.into(), ptr_ty.into()], false);
            let cmp_fn = self.get_or_declare_fn("lin_string_cmp", cmp_fn_ty);
            let result = self.builder
                .build_call(cmp_fn, &[lv.into(), rv.into()], "scmp_result")
                .unwrap()
                .try_as_basic_value()
                .unwrap_basic()
                .into_int_value();
            let zero = i32_ty.const_zero();
            return self.builder
                .build_int_compare(signed_pred, result, zero, "scmp")
                .unwrap()
                .into();
        }

        let i64_ty = self.context.i64_type();
        // Normalize operand types: if either is a pointer, convert both to i64.
        let (lv, rv) = if lv.is_pointer_value() || rv.is_pointer_value() {
            let l = if lv.is_pointer_value() {
                self.builder.build_ptr_to_int(lv.into_pointer_value(), i64_ty, "lpc").unwrap().into()
            } else {
                self.builder.build_int_s_extend_or_bit_cast(lv.into_int_value(), i64_ty, "lext").unwrap().into()
            };
            let r = if rv.is_pointer_value() {
                self.builder.build_ptr_to_int(rv.into_pointer_value(), i64_ty, "rpc").unwrap().into()
            } else {
                self.builder.build_int_s_extend_or_bit_cast(rv.into_int_value(), i64_ty, "rext").unwrap().into()
            };
            (l, r)
        } else {
            (lv, rv)
        };

        if ty.is_float() {
            self.builder.build_float_compare(float_pred, lv.into_float_value(), rv.into_float_value(), "fcmp").unwrap().into()
        } else if ty.is_signed() || lv.is_int_value() && lv.into_int_value().get_type().get_bit_width() == 64 {
            self.builder.build_int_compare(signed_pred, lv.into_int_value(), rv.into_int_value(), "scmp").unwrap().into()
        } else {
            self.builder.build_int_compare(unsigned_pred, lv.into_int_value(), rv.into_int_value(), "ucmp").unwrap().into()
        }
    }

    // -------------------------------------------------------------------------
    // Numeric coercions (widening / narrowing)
    // -------------------------------------------------------------------------


    // -------------------------------------------------------------------------
    // Function calls
    // -------------------------------------------------------------------------






    /// Value-input port of `build_partial_application` for the LinIR path: the partial
    /// arguments arrive as already-compiled LLVM values rather than TypedExprs. Builds a
    /// closure {wrapper_fn, env} capturing the partials; the wrapper loads them and calls
    /// `llvm_fn` with partials ++ remaining params.
    fn build_partial_application_values(
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
        let env_size_i64 = self.builder.build_int_z_extend_or_bit_cast(env_size, self.context.i64_type(), "papp_env_sz").unwrap();
        let env_ptr = self.builder.build_call(self.rt_alloc, &[env_size_i64.into()], "papp_env")
            .unwrap().try_as_basic_value().unwrap_basic().into_pointer_value();
        for (i, val) in compiled_partials.iter().enumerate() {
            let field = self.builder.build_struct_gep(env_struct_ty, env_ptr, i as u32, "papp_f").unwrap();
            self.builder.build_store(field, *val).unwrap();
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

        let cls_struct_ty = self.closure_struct_type();
        let cls_ptr = self.builder.build_call(self.rt_alloc, &[self.context.i64_type().const_int(32, false).into()], "papp_cls")
            .unwrap().try_as_basic_value().unwrap_basic().into_pointer_value();
        let rc_field = self.builder.build_struct_gep(cls_struct_ty, cls_ptr, 0, "papp_cls_rc").unwrap();
        self.builder.build_store(rc_field, self.context.i32_type().const_int(1, false)).unwrap();
        let fn_field = self.builder.build_struct_gep(cls_struct_ty, cls_ptr, 2, "papp_cls_fn").unwrap();
        self.builder.build_store(fn_field, wrapper_fn.as_global_value().as_pointer_value()).unwrap();
        let env_field = self.builder.build_struct_gep(cls_struct_ty, cls_ptr, 3, "papp_cls_env").unwrap();
        self.builder.build_store(env_field, env_ptr).unwrap();
        // env_size at offset 24 so lin_closure_release frees the env with the right layout
        // (lin_alloc does NOT zero, so this MUST be written explicitly).
        let env_sz_gep = unsafe { self.builder.build_gep(
            self.context.i8_type(), cls_ptr, &[self.context.i64_type().const_int(24, false)], "papp_env_sz_f"
        ).unwrap() };
        self.builder.build_store(env_sz_gep, env_size_i64).unwrap();

        let current_block = self.builder.get_insert_block().unwrap();
        {
            let entry = self.context.append_basic_block(wrapper_fn, "entry");
            self.builder.position_at_end(entry);
            let env_arg = wrapper_fn.get_nth_param(0).unwrap().into_pointer_value();
            let mut call_args: Vec<BasicMetadataValueEnum> = Vec::new();
            for (i, field_ty) in env_field_types.iter().enumerate() {
                let fp = self.builder.build_struct_gep(env_struct_ty, env_arg, i as u32, "papp_load_f").unwrap();
                let v = self.builder.build_load(*field_ty, fp, "papp_v").unwrap();
                call_args.push(v.into());
            }
            for i in 0..remaining_params.len() {
                let p = wrapper_fn.get_nth_param(1 + i as u32).unwrap();
                call_args.push(p.into());
            }
            let call = self.builder.build_call(llvm_fn, &call_args, "papp_call").unwrap();
            // Box the concrete result to a TaggedVal* (uniform closure return ABI).
            match call.try_as_basic_value().basic() {
                Some(v) => {
                    let boxed = self.box_value(v, final_ret);
                    self.builder.build_return(Some(&boxed)).unwrap();
                }
                None => { self.builder.build_return(Some(&ptr_ty.const_null())).unwrap(); }
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
    fn build_closure_partial_application_values(
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
        let env_size_i64 = self.builder.build_int_z_extend_or_bit_cast(env_size, self.context.i64_type(), "papp_env_sz").unwrap();
        let env_ptr2 = self.builder.build_call(self.rt_alloc, &[env_size_i64.into()], "papp_env").unwrap()
            .try_as_basic_value().unwrap_basic().into_pointer_value();
        let cls_field = self.builder.build_struct_gep(env_struct_ty, env_ptr2, 0, "papp_cls_f").unwrap();
        self.builder.build_store(cls_field, closure_ptr).unwrap();
        // The env borrows the inner closure (does not retain it), mirroring the AST path's
        // build_closure_call. The inner closure is a longer-lived binding (a top-level val
        // stored to a module global, retained there), so the borrow stays valid.
        for (i, val) in partial_args.iter().enumerate() {
            let f = self.builder.build_struct_gep(env_struct_ty, env_ptr2, (i + 1) as u32, "papp_f").unwrap();
            self.builder.build_store(f, *val).unwrap();
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

        let saved_block = self.builder.get_insert_block().unwrap();
        let wrapper_entry = self.context.append_basic_block(wrapper_fn, "entry");
        self.builder.position_at_end(wrapper_entry);

        let w_env_ptr = wrapper_fn.get_nth_param(0).unwrap().into_pointer_value();
        let cls_fp = self.builder.build_struct_gep(env_struct_ty, w_env_ptr, 0, "wcls_p").unwrap();
        let inner_cls_ptr = self.builder.build_load(ptr_ty, cls_fp, "inner_cls").unwrap().into_pointer_value();

        // Load the inner closure's fn_ptr / env_ptr.
        let cls_ty = self.closure_struct_type();
        let inner_fn_gep = self.builder.build_struct_gep(cls_ty, inner_cls_ptr, 2, "inner_fp").unwrap();
        let inner_fn_ptr = self.builder.build_load(ptr_ty, inner_fn_gep, "inner_fnp").unwrap().into_pointer_value();
        let inner_env_gep = self.builder.build_struct_gep(cls_ty, inner_cls_ptr, 3, "inner_ep").unwrap();
        let inner_env_ptr = self.builder.build_load(ptr_ty, inner_env_gep, "inner_envp").unwrap();

        // Complete the call: inner_fn(inner_env, stored_args..., remaining_params...).
        let mut call_param_types: Vec<BasicMetadataTypeEnum> = vec![ptr_ty.into()];
        let mut call_args: Vec<BasicMetadataValueEnum> = vec![inner_env_ptr.into()];
        for (i, ty) in arg_types.iter().enumerate() {
            let fp = self.builder.build_struct_gep(env_struct_ty, w_env_ptr, (i + 1) as u32, "warg_p").unwrap();
            let v = self.builder.build_load(*ty, fp, "warg").unwrap();
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
        let inner_call = self.builder.build_indirect_call(inner_fn_ty, inner_fn_ptr, &call_args, "papp_inner").unwrap();
        let inner_result = inner_call.try_as_basic_value().unwrap_basic();
        self.builder.build_return(Some(&inner_result)).unwrap();
        self.builder.position_at_end(saved_block);

        // Build the outer closure struct { rc, _pad, fn_ptr, env_ptr }.
        let cls_struct_ty = self.closure_struct_type();
        let cls_ptr = self.builder.build_call(self.rt_alloc, &[self.context.i64_type().const_int(32, false).into()], "papp_cls").unwrap()
            .try_as_basic_value().unwrap_basic().into_pointer_value();
        let rc_field = self.builder.build_struct_gep(cls_struct_ty, cls_ptr, 0, "papp_cls_rc").unwrap();
        self.builder.build_store(rc_field, self.context.i32_type().const_int(1, false)).unwrap();
        let fn_field = self.builder.build_struct_gep(cls_struct_ty, cls_ptr, 2, "papp_cls_fn").unwrap();
        self.builder.build_store(fn_field, wrapper_fn.as_global_value().as_pointer_value()).unwrap();
        let env_field = self.builder.build_struct_gep(cls_struct_ty, cls_ptr, 3, "papp_cls_env").unwrap();
        self.builder.build_store(env_field, env_ptr2).unwrap();
        // env_size at offset 24 (lin_alloc does NOT zero — must write explicitly so
        // lin_closure_release frees the env with the correct layout).
        let env_sz_gep = unsafe { self.builder.build_gep(
            self.context.i8_type(), cls_ptr, &[self.context.i64_type().const_int(24, false)], "papp_env_sz_f"
        ).unwrap() };
        self.builder.build_store(env_sz_gep, env_size_i64).unwrap();
        cls_ptr.into()
    }






    // -------------------------------------------------------------------------
    // Intrinsic calls (runtime functions with known ABI)
    // -------------------------------------------------------------------------




    fn get_or_declare_fn(&self, name: &str, fn_type: inkwell::types::FunctionType<'ctx>) -> FunctionValue<'ctx> {
        if let Some(f) = self.module.get_function(name) {
            f
        } else {
            self.module.add_function(name, fn_type, None)
        }
    }

    // -------------------------------------------------------------------------
    // If / else
    // -------------------------------------------------------------------------


    // -------------------------------------------------------------------------
    // Closures
    // -------------------------------------------------------------------------


    // -------------------------------------------------------------------------
    // String interpolation
    // -------------------------------------------------------------------------





    fn value_to_string_simple(
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
                    .build_call(self.rt_tagged_to_string, &[val.into()], "ttos")
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
                    .build_call(self.rt_int_to_string, &[i64_val.into()], "tv_itos")
                    .unwrap()
                    .try_as_basic_value()
                    .unwrap_basic();
            } else if val.is_float_value() {
                let f64_ty = self.context.f64_type();
                let f64_val = self.builder
                    .build_float_ext(val.into_float_value(), f64_ty, "tv_fext")
                    .unwrap();
                return self.builder
                    .build_call(self.rt_float_to_string, &[f64_val.into()], "tv_ftos")
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
                    self.builder.build_int_s_extend_or_bit_cast(val.into_int_value(), i64_ty, "iext").unwrap()
                } else {
                    self.builder.build_int_z_extend_or_bit_cast(val.into_int_value(), i64_ty, "iext").unwrap()
                };
                self.builder
                    .build_call(self.rt_int_to_string, &[i64_val.into()], "itos")
                    .unwrap()
                    .try_as_basic_value()
                    .unwrap_basic()
            }
            Type::Float32 => {
                let f64_val = self.builder.build_float_ext(val.into_float_value(), self.context.f64_type(), "fext").unwrap();
                self.builder
                    .build_call(self.rt_float_to_string, &[f64_val.into()], "ftos")
                    .unwrap()
                    .try_as_basic_value()
                    .unwrap_basic()
            }
            Type::Float64 => {
                self.builder
                    .build_call(self.rt_float_to_string, &[val.into()], "ftos")
                    .unwrap()
                    .try_as_basic_value()
                    .unwrap_basic()
            }
            Type::Bool => {
                self.builder
                    .build_call(self.rt_bool_to_string, &[val.into()], "btos")
                    .unwrap()
                    .try_as_basic_value()
                    .unwrap_basic()
            }
            Type::Null => {
                self.builder
                    .build_call(self.rt_null_to_string, &[], "ntos")
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
                    self.builder.build_call(conv_fn, &[val.into()], "flat2t")
                        .unwrap().try_as_basic_value().unwrap_basic()
                } else {
                    val
                };
                let f = self.get_or_declare_fn("lin_array_to_string",
                    ptr_ty.fn_type(&[ptr_ty.into()], false));
                self.builder.build_call(f, &[arr_val.into()], "atos")
                    .unwrap().try_as_basic_value().unwrap_basic()
            }
            Type::FixedArray(_) => {
                let ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
                let f = self.get_or_declare_fn("lin_array_to_string",
                    ptr_ty.fn_type(&[ptr_ty.into()], false));
                self.builder.build_call(f, &[val.into()], "atos")
                    .unwrap().try_as_basic_value().unwrap_basic()
            }
            Type::Object(_) => {
                let ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
                let f = self.get_or_declare_fn("lin_object_to_string",
                    ptr_ty.fn_type(&[ptr_ty.into()], false));
                self.builder.build_call(f, &[val.into()], "otos")
                    .unwrap().try_as_basic_value().unwrap_basic()
            }
            _ => {
                // For unknown complex types, fall back to runtime tagged dispatch.
                if val.is_pointer_value() {
                    self.builder
                        .build_call(self.rt_tagged_to_string, &[val.into()], "ttos")
                        .unwrap()
                        .try_as_basic_value()
                        .unwrap_basic()
                } else {
                    self.compile_string_lit("[object]")
                }
            }
        }
    }


    // -------------------------------------------------------------------------
    // Arrays
    // -------------------------------------------------------------------------

    /// Returns true when the element type maps to a flat unboxed scalar array.
    /// Only concrete fixed-width numeric scalars qualify — not Bool (stored as i1,
    /// awkward to pack densely), not pointers, not unions.
    fn is_flat_scalar(ty: &Type) -> bool {
        matches!(ty,
            Type::Int8 | Type::UInt8 |
            Type::Int16 | Type::UInt16 |
            Type::Int32 | Type::UInt32 |
            Type::Int64 | Type::UInt64 |
            Type::Float32 | Type::Float64
        )
    }

    /// Suffix used in runtime function names for flat array variants.
    fn flat_suffix(ty: &Type) -> &'static str {
        match ty {
            Type::Int8 => "i8",
            Type::UInt8 => "u8",
            Type::Int16 => "i16",
            Type::UInt16 => "u16",
            Type::Int32 | Type::UInt32 => "i32",
            Type::Int64 | Type::UInt64 => "i64",
            Type::Float32 => "f32",
            Type::Float64 => "f64",
            _ => unreachable!("flat_suffix called with non-scalar type"),
        }
    }



    /// Push a scalar into a flat unboxed array (lin_flat_array_push_<suffix>).
    fn flat_array_push(&mut self, arr: BasicValueEnum<'ctx>, val: BasicValueEnum<'ctx>, elem_ty: &Type) {
        let suffix = Self::flat_suffix(elem_ty);
        let push_name = format!("lin_flat_array_push_{}", suffix);
        let ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
        let llvm_elem_ty = self.llvm_type(elem_ty);
        let push_fn = self.get_or_declare_fn(&push_name,
            self.context.void_type().fn_type(&[ptr_ty.into(), llvm_elem_ty.into()], false));
        self.builder.build_call(push_fn, &[arr.into(), val.into()], "").unwrap();
    }

    /// Load a scalar element from a flat unboxed array (lin_flat_array_get_<suffix>).
    fn flat_array_get(&mut self, arr: BasicValueEnum<'ctx>, idx: inkwell::values::IntValue<'ctx>, elem_ty: &Type) -> BasicValueEnum<'ctx> {
        let suffix = Self::flat_suffix(elem_ty);
        let get_name = format!("lin_flat_array_get_{}", suffix);
        let ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
        let i64_ty = self.context.i64_type();
        let llvm_elem_ty = self.llvm_type(elem_ty);
        let get_fn = self.get_or_declare_fn(&get_name,
            llvm_elem_ty.fn_type(&[ptr_ty.into(), i64_ty.into()], false));
        self.builder.build_call(get_fn, &[arr.into(), idx.into()], "flat_get")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
    }

    /// Push a dynamically-typed value (TypeVar or Union) into a tagged LinArray*.
    /// Ensures the value is a TaggedVal* before calling lin_array_push_tagged,
    /// boxing scalars (e.g. i32 from a TypeVar that resolved concretely) as needed.
    fn push_tagged_val(&mut self, arr: BasicValueEnum<'ctx>, val: BasicValueEnum<'ctx>, val_ty: &Type) {
        let val_ptr = if val.is_pointer_value() {
            val.into_pointer_value()
        } else {
            self.box_value(val, val_ty).into_pointer_value()
        };
        let ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
        let rt_push_tagged = self.get_or_declare_fn("lin_array_push_tagged",
            self.context.void_type().fn_type(&[ptr_ty.into(), ptr_ty.into()], false));
        self.builder.build_call(rt_push_tagged, &[arr.into(), val_ptr.into()], "").unwrap();
    }


    /// Push a value into a tagged LinArray* always using tagged format (never flat).
    /// Use this when the array was allocated with rt_array_alloc (tagged format).
    fn tagged_array_push_value(&mut self, arr: BasicValueEnum<'ctx>, val: BasicValueEnum<'ctx>, val_ty: &Type) {
        let i8_ty = self.context.i8_type();
        match val_ty {
            Type::TypeVar(_) | Type::Union(_) => self.push_tagged_val(arr, val, val_ty),
            _ => {
                let tag_val = Self::type_tag(val_ty);
                let tag = i8_ty.const_int(tag_val as u64, false);
                // lin_array_push copies a full 8 bytes from the cell into the payload, so the
                // cell must hold 8 defined bytes. Pointers are 8 bytes; small integers/bools
                // are zero-extended to i64; f32 is bit-widened via an i64 cell.
                let (store_val, store_llvm_ty): (BasicValueEnum<'ctx>, BasicTypeEnum<'ctx>) = match val_ty {
                    Type::Str | Type::Array(_) | Type::Object(_) | Type::Iterator(_) | Type::Function { .. } => {
                        (val, self.context.ptr_type(inkwell::AddressSpace::default()).as_basic_type_enum())
                    }
                    _ if val.is_int_value() => {
                        let i64_ty = self.context.i64_type();
                        let ext = self.builder.build_int_z_extend_or_bit_cast(val.into_int_value(), i64_ty, "arr_cell_ext").unwrap();
                        (ext.into(), i64_ty.as_basic_type_enum())
                    }
                    _ => (val, self.llvm_type(val_ty)),
                };
                let cell = self.builder.build_alloca(store_llvm_ty, "arr_cell").unwrap();
                self.builder.build_store(cell, store_val).unwrap();
                self.builder.build_call(self.rt_array_push, &[arr.into(), cell.into(), tag.into()], "arr_push").unwrap();
            }
        }
    }

    // -------------------------------------------------------------------------
    // Iteration
    // -------------------------------------------------------------------------













    // -------------------------------------------------------------------------
    // Index assignment (obj[key] = val, arr[i] = val)
    // -------------------------------------------------------------------------


    // -------------------------------------------------------------------------
    // Objects
    // -------------------------------------------------------------------------


    /// Build a stack-allocated TaggedVal from a value + type, return its alloca ptr.
    fn build_tagged_val_alloca(&mut self, val: &BasicValueEnum<'ctx>, val_ty: &Type) -> PointerValue<'ctx> {
        // TaggedVal layout: { tag: u8, pad: [u8;7], payload: u64 } = 16 bytes total
        let i8_ty = self.context.i8_type();
        let i64_ty = self.context.i64_type();
        let tagged_ty = self.context.struct_type(&[i8_ty.into(), i8_ty.array_type(7).into(), i64_ty.into()], false);
        let alloca = self.builder.build_alloca(tagged_ty, "tv").unwrap();

        let tag = Self::type_tag(val_ty);
        let tag_val = i8_ty.const_int(tag as u64, false);
        let tag_ptr = self.builder.build_struct_gep(tagged_ty, alloca, 0, "tv_tag").unwrap();
        self.builder.build_store(tag_ptr, tag_val).unwrap();

        // Write payload as u64.
        let payload_ptr = self.builder.build_struct_gep(tagged_ty, alloca, 2, "tv_payload").unwrap();
        let payload: inkwell::values::IntValue<'ctx> = match val_ty {
            Type::Null => i64_ty.const_zero(),
            Type::Bool => {
                let b = if val.is_int_value() {
                    self.builder.build_int_z_extend(val.into_int_value(), i64_ty, "bext").unwrap()
                } else { i64_ty.const_zero() };
                b
            }
            Type::Int8 | Type::Int16 | Type::Int32 | Type::UInt8 | Type::UInt16 | Type::UInt32 => {
                if val.is_int_value() {
                    self.builder.build_int_z_extend_or_bit_cast(val.into_int_value(), i64_ty, "iext").unwrap()
                } else { i64_ty.const_zero() }
            }
            Type::Int64 | Type::UInt64 => {
                if val.is_int_value() { val.into_int_value() } else { i64_ty.const_zero() }
            }
            Type::Float32 => {
                let fv = if val.is_float_value() { val.into_float_value() }
                    else { self.context.f32_type().const_float(0.0) };
                // Extend to f64 then bitcast bits to i64
                let fv64 = self.builder.build_float_ext(fv, self.context.f64_type(), "f32ext").unwrap();
                self.builder.build_bit_cast(fv64, i64_ty, "fbits").unwrap().into_int_value()
            }
            Type::Float64 => {
                let fv = if val.is_float_value() { val.into_float_value() }
                    else { self.context.f64_type().const_float(0.0) };
                // Bitcast f64 bits to i64 (reinterpret, not convert)
                self.builder.build_bit_cast(fv, i64_ty, "fbits").unwrap().into_int_value()
            }
            _ => {
                // Pointer types: str, array, object, function — store pointer as u64
                if val.is_pointer_value() {
                    self.builder.build_ptr_to_int(val.into_pointer_value(), i64_ty, "pti").unwrap()
                } else { i64_ty.const_zero() }
            }
        };
        self.builder.build_store(payload_ptr, payload).unwrap();
        alloca
    }



    // -------------------------------------------------------------------------
    // Match / pattern matching
    // -------------------------------------------------------------------------





    // =========================================================================
    // LinIR-consuming codegen (Phase 3)
    // =========================================================================

    /// Compile a `LinModule` (produced by `lin_ir::lower_module` + `elide_rc`) to LLVM IR.
    /// This is the sole compilation backend (the legacy TypedAST path has been removed).
    pub fn compile_module_from_ir(&mut self, module: &lir::LinModule) {
        use lir::{Instruction, Const, CallTarget, Terminator};
        use std::collections::HashMap as StdMap;

        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i32_ty = self.context.i32_type();
        let i64_ty = self.context.i64_type();
        let void_ty = self.context.void_type();

        // ---- Pass 1: pre-declare all LLVM functions (so cross-calls work) ----
        let mut ir_fn_to_llvm: StdMap<lir::FuncId, FunctionValue<'ctx>> = StdMap::new();
        // Exact emitted symbol name per FuncId, used by coverage to name its globals.
        let mut ir_fn_symbol: StdMap<lir::FuncId, String> = StdMap::new();
        for func in &module.functions {
            // Build LLVM function type from params/ret.
            let ret_ty = &func.ret_ty;
            let mut param_types: Vec<BasicMetadataTypeEnum> = Vec::new();
            for (_, ty) in &func.params {
                param_types.push(self.llvm_param_type(ty));
            }
            let name = if func.name.as_deref() == Some("main") || func.name.is_none() {
                if func.id == lir::FuncId(0) && self.ir_anon_prefix.is_empty() { "main".to_string() }
                // Prefix anonymous functions with the module key when compiling an import, so
                // `__lin_fn_<id>` symbols don't collide with the main module's (or another
                // import's) identically-numbered anonymous functions.
                else { format!("{}__lin_fn_{}", self.ir_anon_prefix, func.id.0) }
            } else {
                func.name.clone().unwrap()
            };

            let llvm_fn = if matches!(ret_ty, Type::Null | Type::Never) {
                let fn_ty = void_ty.fn_type(&param_types, false);
                if let Some(existing) = self.module.get_function(&name) { existing }
                else { self.module.add_function(&name, fn_ty, None) }
            } else {
                let ret_llvm = self.llvm_type(ret_ty);
                let fn_ty = ret_llvm.fn_type(&param_types, false);
                if let Some(existing) = self.module.get_function(&name) { existing }
                else { self.module.add_function(&name, fn_ty, None) }
            };
            self.named_fns.insert(name.clone(), llvm_fn);
            ir_fn_to_llvm.insert(func.id, llvm_fn);
            ir_fn_symbol.insert(func.id, name.clone());
        }

        // Module globals backing top-level non-function vals (GlobalValSet/Get), shared
        // across all functions so closures can read module-level vals.
        let mut ir_global_vals: StdMap<usize, inkwell::values::GlobalValue<'ctx>> = StdMap::new();

        // ---- Pass 2: compile each function body ----
        for func in &module.functions {
            let llvm_fn = ir_fn_to_llvm[&func.id];

            // Map BlockId → LLVM BasicBlock
            let mut ir_block_to_llvm: StdMap<lir::BlockId, inkwell::basic_block::BasicBlock<'ctx>> = StdMap::new();
            for block in &func.blocks {
                let label = block.label.as_deref().unwrap_or("bb");
                let bb = self.context.append_basic_block(llvm_fn, label);
                ir_block_to_llvm.insert(block.id, bb);
            }

            // Map Temp → LLVM value (populated as we emit instructions)
            let mut temp_map: StdMap<lir::Temp, BasicValueEnum<'ctx>> = StdMap::new();

            // Self-tail-call (TCO) support: if any block ends in TailCall, route params
            // through stack allocas so a tail call can update them and branch back to the
            // function's first IR block (the loop header) instead of recursing on the stack.
            let has_tail_call = func.blocks.iter().any(|b| matches!(b.terminator, Terminator::TailCall { .. }));
            let mut param_allocs: Vec<PointerValue<'ctx>> = Vec::new();
            if has_tail_call {
                // Emit allocas + initial stores in a dedicated entry block that branches to
                // the first IR block (which becomes the loop header).
                let tco_entry = self.context.append_basic_block(llvm_fn, "tco_entry");
                // Move the new entry before the first IR block so it is the function entry.
                if let Some(first_ir_bb) = func.blocks.first().and_then(|b| ir_block_to_llvm.get(&b.id)) {
                    tco_entry.move_before(*first_ir_bb).ok();
                }
                self.builder.position_at_end(tco_entry);
                for (i, (_temp, ty)) in func.params.iter().enumerate() {
                    let llvm_ty = self.llvm_type(ty);
                    let slot = self.builder.build_alloca(llvm_ty, "tco_param").unwrap();
                    if let Some(pv) = llvm_fn.get_nth_param(i as u32) {
                        self.builder.build_store(slot, pv).unwrap();
                    }
                    param_allocs.push(slot);
                }
                if let Some(first_ir_bb) = func.blocks.first().and_then(|b| ir_block_to_llvm.get(&b.id)) {
                    self.builder.build_unconditional_branch(*first_ir_bb).unwrap();
                }
            }

            // Pre-load params into temp_map. With TCO, params are loaded from their allocas
            // at the top of the loop-header block so each iteration sees the updated values.
            if has_tail_call {
                if let Some(first_ir_bb) = func.blocks.first().and_then(|b| ir_block_to_llvm.get(&b.id)) {
                    self.builder.position_at_end(*first_ir_bb);
                    for (i, (temp, ty)) in func.params.iter().enumerate() {
                        let llvm_ty = self.llvm_type(ty);
                        let loaded = self.builder.build_load(llvm_ty, param_allocs[i], "tco_pload").unwrap();
                        temp_map.insert(*temp, loaded);
                    }
                }
            } else {
                for (i, (temp, _ty)) in func.params.iter().enumerate() {
                    if let Some(param_val) = llvm_fn.get_nth_param(i as u32) {
                        temp_map.insert(*temp, param_val);
                    }
                }
            }

            // Pending phi nodes to backpatch after all blocks are compiled, so that
            // back-edge incoming values (e.g. a loop's `i+1`, defined in a block emitted
            // after the header) are available in temp_map when we wire up the edges.
            let mut pending_phis: Vec<(inkwell::values::PhiValue<'ctx>, Vec<(lir::Temp, lir::BlockId)>)> = Vec::new();

            // The LLVM block an IR block's control flow actually EXITS from. Some
            // instructions (HasPattern, ArrayLenCheck) emit internal branches and leave the
            // builder in a fresh block; the IR block's terminator and any phi that names this
            // IR block as a predecessor must use that exit block, not the entry block.
            let mut ir_block_exit: StdMap<lir::BlockId, inkwell::basic_block::BasicBlock<'ctx>> = StdMap::new();

            // ---- Coverage: assign one profile counter per span-carrying block ----
            // `block_counter` maps each instrumented block to its counter index; `profc` is
            // the `[n x i64]` counter array global (None when this function has no regions
            // or coverage is off). Only the main module + tracked (non-stdlib) imports are
            // instrumented (`current_source` is None otherwise).
            let mut block_counter: StdMap<lir::BlockId, u32> = StdMap::new();
            let mut profc: Option<inkwell::values::GlobalValue<'ctx>> = None;
            if self.coverage.is_some() {
                if let Some((file_idx, _)) = self.current_source {
                    let mut regions: Vec<coverage::Region> = Vec::new();
                    let mut next_counter = 0u32;
                    for block in &func.blocks {
                        if let Some(span) = block.span {
                            let counter = next_counter;
                            next_counter += 1;
                            block_counter.insert(block.id, counter);
                            let cov = self.coverage.as_ref().unwrap();
                            let (start_line, start_col) =
                                cov.offset_to_line_col_in(file_idx as usize, span.start);
                            let (end_line, end_col) =
                                cov.offset_to_line_col_in(file_idx as usize, span.end);
                            regions.push(coverage::Region {
                                counter,
                                start_line,
                                start_col,
                                end_line,
                                end_col,
                            });
                        }
                    }
                    if !regions.is_empty() {
                        let name = ir_fn_symbol[&func.id].clone();
                        let info = coverage::FnCovInfo { name, file_idx, regions };
                        // GlobalValue is Copy; collect into a local so we don't hold a
                        // &mut self.coverage borrow across the self.builder calls below.
                        profc = self.coverage.as_mut().unwrap().emit_function_globals(
                            self.context,
                            &self.module,
                            info,
                        );
                    }
                }
            }

            // Compile each block
            for block in &func.blocks {
                let bb = ir_block_to_llvm[&block.id];
                self.builder.position_at_end(bb);

                // Coverage: increment this block's counter on entry.
                if let (Some(profc), Some(&k)) = (profc, block_counter.get(&block.id)) {
                    let counter_arr_ty = i64_ty.array_type(block_counter.len() as u32);
                    let gep = unsafe {
                        self.builder.build_in_bounds_gep(
                            counter_arr_ty,
                            profc.as_pointer_value(),
                            &[i64_ty.const_zero(), i64_ty.const_int(k as u64, false)],
                            "covctr_ptr",
                        ).unwrap()
                    };
                    let cur = self.builder.build_load(i64_ty, gep, "covctr").unwrap().into_int_value();
                    let inc = self.builder.build_int_add(cur, i64_ty.const_int(1, false), "covctr_inc").unwrap();
                    self.builder.build_store(gep, inc).unwrap();
                }

                for instr in &block.instructions {
                    match instr {
                        Instruction::Const { dst, val } => {
                            let llvm_val = match val {
                                Const::Int(v, ty) => self.compile_int_lit(*v, ty),
                                Const::Float(v, ty) => self.compile_float_lit(*v, ty),
                                Const::Bool(b) => self.context.bool_type().const_int(*b as u64, false).into(),
                                Const::Null => ptr_ty.const_null().into(),
                                Const::Str(s) => self.compile_string_lit(s),
                            };
                            temp_map.insert(*dst, llvm_val);
                        }
                        Instruction::Copy { dst, src } => {
                            if let Some(&v) = temp_map.get(src) {
                                temp_map.insert(*dst, v);
                            }
                        }
                        Instruction::Phi { dst, ty, incomings } => {
                            // Create the phi now so its result is available to later
                            // instructions, but defer wiring the incoming edges until all
                            // blocks are compiled (a back-edge value may be defined later).
                            let phi_ty = self.llvm_type(ty);
                            let phi = self.builder.build_phi(phi_ty, "ir_phi").unwrap();
                            temp_map.insert(*dst, phi.as_basic_value());
                            pending_phis.push((phi, incomings.clone()));
                        }
                        Instruction::Binary { dst, op, lhs, rhs, operand_ty, ty } => {
                            // A missing operand temp means malformed IR (an undefined SSA temp) —
                            // the old null-pointer fallback silently miscompiled to garbage
                            // arithmetic. Fail loudly with the offending temp instead.
                            let lv = *temp_map.get(lhs).unwrap_or_else(|| panic!("Binary: undefined lhs temp {lhs:?}"));
                            let rv = *temp_map.get(rhs).unwrap_or_else(|| panic!("Binary: undefined rhs temp {rhs:?}"));
                            let rty = func.temp_types.get(rhs).cloned().unwrap_or(Type::Null);
                            let result = self.compile_binary_op_values(lv, rv, op, operand_ty, &rty, ty);
                            temp_map.insert(*dst, result);
                        }
                        Instruction::Retain { val, ty } => {
                            if let Some(&v) = temp_map.get(val) {
                                if v.is_pointer_value() {
                                    if Self::is_union_type(ty) {
                                        // A boxed TaggedVal*: bump the INNER payload's rc
                                        // (tag-aware). lin_rc_retain would hit the tag byte at
                                        // offset 0 and corrupt it.
                                        let retain_fn = self.get_or_declare_fn("lin_tagged_retain",
                                            self.context.void_type().fn_type(&[ptr_ty.into()], false));
                                        self.builder.build_call(retain_fn, &[v.into()], "").unwrap();
                                    } else {
                                        self.builder.build_call(self.rt_rc_retain, &[v.into()], "").unwrap();
                                    }
                                }
                            }
                        }
                        Instruction::Release { val, ty } => {
                            if let Some(&v) = temp_map.get(val) {
                                self.emit_release(v, ty);
                            }
                        }
                        Instruction::Call { dst, callee, args, ret_ty } => {
                            let arg_vals: Vec<BasicMetadataValueEnum> = args
                                .iter()
                                .filter_map(|a| temp_map.get(a).map(|v| (*v).into()))
                                .collect();
                            // Detect under-application: fewer args than the callee's arity
                            // and a Function result type ⇒ build a partial-application closure.
                            let partial_app = |s: &mut Self, callee_fn: FunctionValue<'ctx>| -> Option<BasicValueEnum<'ctx>> {
                                if (arg_vals.len() as u32) < callee_fn.count_params() {
                                    if let Type::Function { params: remaining, ret: final_ret } = ret_ty {
                                        let vals: Vec<BasicValueEnum> = arg_vals.iter().map(|a| match a {
                                            BasicMetadataValueEnum::IntValue(v) => (*v).into(),
                                            BasicMetadataValueEnum::FloatValue(v) => (*v).into(),
                                            BasicMetadataValueEnum::PointerValue(v) => (*v).into(),
                                            _ => s.context.ptr_type(AddressSpace::default()).const_null().into(),
                                        }).collect();
                                        return Some(s.build_partial_application_values(callee_fn, &vals, remaining, final_ret));
                                    }
                                }
                                None
                            };
                            let result = match callee {
                                CallTarget::Direct(fid) => {
                                    let callee_fn = ir_fn_to_llvm[fid];
                                    if let Some(p) = partial_app(self, callee_fn) { p }
                                    else {
                                        let call = self.builder.build_call(callee_fn, &arg_vals, "call").unwrap();
                                        if matches!(ret_ty, Type::Null | Type::Never) { ptr_ty.const_null().into() }
                                        else { call.try_as_basic_value().unwrap_basic() }
                                    }
                                }
                                CallTarget::Named(name) => {
                                    // Resolve the callee; if it's an undeclared runtime symbol
                                    // (e.g. lin_array_slice_tagged), declare it from the actual
                                    // argument LLVM types + return type so the call links.
                                    let callee_fn = match self.module.get_function(name) {
                                        Some(f) => f,
                                        None => {
                                            let param_types: Vec<BasicMetadataTypeEnum> = args.iter()
                                                .map(|a| {
                                                    let ty = func.temp_types.get(a).cloned().unwrap_or(Type::Null);
                                                    self.llvm_param_type(&ty)
                                                })
                                                .collect();
                                            let fn_ty = if matches!(ret_ty, Type::Null | Type::Never) {
                                                void_ty.fn_type(&param_types, false)
                                            } else {
                                                self.llvm_type(ret_ty).fn_type(&param_types, false)
                                            };
                                            self.module.add_function(name, fn_ty, None)
                                        }
                                    };
                                    if let Some(p) = partial_app(self, callee_fn) { p }
                                    else {
                                        let call = self.builder.build_call(callee_fn, &arg_vals, "call_n").unwrap();
                                        if matches!(ret_ty, Type::Null | Type::Never) { ptr_ty.const_null().into() }
                                        else { call.try_as_basic_value().unwrap_basic() }
                                    }
                                }
                                CallTarget::Indirect(fn_temp) => {
                                    if let Some(&cls_ptr) = temp_map.get(fn_temp) {
                                        if cls_ptr.is_pointer_value() {
                                            // A callee retrieved as Json (e.g. from `arr[0]`) is a
                                            // TaggedVal* wrapping the closure pointer — unbox it to
                                            // the closure struct first.
                                            let callee_ty = func.temp_types.get(fn_temp).cloned().unwrap_or(Type::Null);
                                            let cls_ptr = if Self::is_union_type(&callee_ty) {
                                                self.builder.build_call(self.rt_unbox_ptr, &[cls_ptr.into()], "ir_fn_unbox")
                                                    .unwrap().try_as_basic_value().unwrap_basic()
                                            } else { cls_ptr };
                                            // Under-application of a closure value: the result is
                                            // still a Function, so bundle the inner closure + the
                                            // supplied args into a new partial-application closure
                                            // taking the remaining params (no direct call yet).
                                            if let Type::Function { params: remaining, .. } = ret_ty {
                                                let partials: Vec<BasicValueEnum> = arg_vals.iter().map(|a| match a {
                                                    BasicMetadataValueEnum::IntValue(v) => (*v).into(),
                                                    BasicMetadataValueEnum::FloatValue(v) => (*v).into(),
                                                    BasicMetadataValueEnum::PointerValue(v) => (*v).into(),
                                                    _ => ptr_ty.const_null().into(),
                                                }).collect();
                                                let r = self.build_closure_partial_application_values(
                                                    cls_ptr.into_pointer_value(), &partials, remaining);
                                                temp_map.insert(*dst, r);
                                                continue;
                                            }
                                            // Build closure call: load fn_ptr from offset 2 of closure struct.
                                            let cls_ty = self.closure_struct_type();
                                            let cls_ptr_v = cls_ptr.into_pointer_value();
                                            let fn_gep = self.builder.build_struct_gep(cls_ty, cls_ptr_v, 2, "ir_fp").unwrap();
                                            let fn_ptr = self.builder.build_load(ptr_ty, fn_gep, "ir_fnp").unwrap().into_pointer_value();
                                            let env_gep = self.builder.build_struct_gep(cls_ty, cls_ptr_v, 3, "ir_ep").unwrap();
                                            let env_ptr = self.builder.build_load(ptr_ty, env_gep, "ir_envp").unwrap();

                                            // Build param types: env_ptr + arg types.
                                            // Recover arg types from the IR temp_types map.
                                            let mut fn_param_types: Vec<BasicMetadataTypeEnum> = vec![ptr_ty.into()];
                                            let mut call_args: Vec<BasicMetadataValueEnum> = vec![env_ptr.into()];
                                            for (av, a_temp) in arg_vals.iter().zip(args.iter()) {
                                                let arg_ty = func.temp_types.get(a_temp).cloned().unwrap_or(Type::Null);
                                                fn_param_types.push(self.llvm_param_type(&arg_ty));
                                                call_args.push((*av).into());
                                            }
                                            // Closures use the uniform boxed ABI (return ptr,
                                            // except void). Call with ptr return, then unbox to ret_ty.
                                            let returns_void = matches!(ret_ty, Type::Null | Type::Never);
                                            let fn_ty = if returns_void {
                                                void_ty.fn_type(&fn_param_types, false)
                                            } else {
                                                ptr_ty.fn_type(&fn_param_types, false)
                                            };
                                            let call = self.builder.build_indirect_call(fn_ty, fn_ptr, &call_args, "ir_ind").unwrap();
                                            if returns_void {
                                                ptr_ty.const_null().into()
                                            } else {
                                                let boxed = call.try_as_basic_value().unwrap_basic();
                                                if Self::is_union_type(ret_ty) { boxed }
                                                else { self.unbox_tagged_val_to_type(boxed, ret_ty) }
                                            }
                                        } else { ptr_ty.const_null().into() }
                                    } else { ptr_ty.const_null().into() }
                                }
                            };
                            temp_map.insert(*dst, result);
                        }
                        Instruction::CallIntrinsic { dst, intrinsic, args, ret_ty } => {
                            let arg_vals: Vec<BasicValueEnum> = args
                                .iter()
                                .filter_map(|a| temp_map.get(a).copied())
                                .collect();
                            // Recover each argument's static type so intrinsics can
                            // dispatch correctly (e.g. ToString of Str vs tagged ptr).
                            let arg_tys: Vec<Type> = args
                                .iter()
                                .map(|a| func.temp_types.get(a).cloned().unwrap_or(Type::Null))
                                .collect();
                            let result = self.compile_ir_intrinsic(intrinsic, &arg_vals, &arg_tys, ret_ty);
                            temp_map.insert(*dst, result);
                        }
                        Instruction::MakeObject { dst, fields, spreads, ty } => {
                            // Compile field values first (they're already Temps).
                            let cap = i32_ty.const_int((fields.len() + 4).max(4) as u64, false);
                            let obj_ptr = self.builder.build_call(self.rt_object_alloc, &[cap.into()], "ir_obj")
                                .unwrap().try_as_basic_value().unwrap_basic().into_pointer_value();
                            // Apply spreads. A spread source typed Json/union arrives boxed
                            // (a TaggedVal*) — unbox to the raw LinObject* before merging, or
                            // lin_object_merge reads the box as an object and crashes.
                            if !spreads.is_empty() {
                                let merge_fn = self.get_or_declare_fn("lin_object_merge",
                                    void_ty.fn_type(&[ptr_ty.into(), ptr_ty.into()], false));
                                for s in spreads {
                                    if let Some(&sv) = temp_map.get(s) {
                                        if sv.is_pointer_value() {
                                            let s_ty = func.temp_types.get(s).cloned().unwrap_or(Type::Null);
                                            let src = if Self::is_union_type(&s_ty) {
                                                self.builder.build_call(self.rt_unbox_ptr, &[sv.into()], "ir_spread_unbox")
                                                    .unwrap().try_as_basic_value().unwrap_basic()
                                            } else { sv };
                                            self.builder.build_call(merge_fn, &[obj_ptr.into(), src.into()], "").unwrap();
                                        }
                                    }
                                }
                            }
                            for (key, val_temp) in fields {
                                if let Some(&val) = temp_map.get(val_temp) {
                                    let key_str = self.compile_string_lit(key).into_pointer_value();
                                    let val_ty = func.temp_types.get(val_temp).cloned().unwrap_or(Type::Null);
                                    // A union/Json-typed field value is ALREADY a boxed TaggedVal*
                                    // — pass it straight to lin_object_set. Re-wrapping it via
                                    // build_tagged_val_alloca would store the pointer under a
                                    // TAG_NULL tag (type_tag(TypeVar)=0), so later reads see null.
                                    let tagged = if Self::is_union_type(&val_ty) && val.is_pointer_value() {
                                        val.into_pointer_value()
                                    } else {
                                        self.build_tagged_val_alloca(&val, &val_ty)
                                    };
                                    self.builder.build_call(self.rt_object_set, &[obj_ptr.into(), key_str.into(), tagged.into()], "").unwrap();
                                    self.builder.build_call(self.rt_string_release, &[key_str.into()], "").unwrap();
                                }
                            }
                            let _ = ty;
                            temp_map.insert(*dst, obj_ptr.into());
                        }
                        Instruction::MakeArray { dst, elements, elem_ty } => {
                            let cap = i64_ty.const_int(elements.len().max(4) as u64, false);
                            let arr = if Self::is_flat_scalar(elem_ty) {
                                let suffix = Self::flat_suffix(elem_ty);
                                let alloc_fn = self.get_or_declare_fn(
                                    &format!("lin_flat_array_alloc_{}", suffix),
                                    ptr_ty.fn_type(&[i64_ty.into()], false));
                                let arr_v = self.builder.build_call(alloc_fn, &[cap.into()], "ir_farr")
                                    .unwrap().try_as_basic_value().unwrap_basic();
                                for e_temp in elements {
                                    if let Some(&ev) = temp_map.get(e_temp) {
                                        self.flat_array_push(arr_v, ev, elem_ty);
                                    }
                                }
                                arr_v
                            } else {
                                let arr_v = self.builder.build_call(self.rt_array_alloc, &[cap.into()], "ir_arr")
                                    .unwrap().try_as_basic_value().unwrap_basic();
                                for e_temp in elements {
                                    if let Some(&ev) = temp_map.get(e_temp) {
                                        self.tagged_array_push_value(arr_v, ev, elem_ty);
                                    }
                                }
                                arr_v
                            };
                            temp_map.insert(*dst, arr);
                        }
                        Instruction::MakeClosure { dst, func: fid, captures, ret_ty: _ } => {
                            if let Some(&callee_fn) = ir_fn_to_llvm.get(fid) {
                                let cls = if captures.is_empty() {
                                    // The target was lowered as a non-closure (no env param 0),
                                    // but closure call sites invoke fn_ptr(env, args...) -> ptr.
                                    // Wrap it in an env-ignoring stub that also boxes the return,
                                    // matching the uniform boxed closure ABI.
                                    self.wrap_named_fn_as_closure_boxed(callee_fn)
                                } else {
                                    // Captures present ⇒ the function has an env param 0; build
                                    // the env struct and store the raw fn ptr.
                                    let fn_ptr = callee_fn.as_global_value().as_pointer_value();
                                    let capture_vals: Vec<BasicValueEnum> = captures
                                        .iter()
                                        .filter_map(|c| temp_map.get(c).copied())
                                        .collect();
                                    self.make_closure_struct(fn_ptr.into(), &capture_vals)
                                };
                                temp_map.insert(*dst, cls);
                            }
                        }
                        Instruction::Index { dst, object, key, obj_ty, key_ty, result_ty } => {
                            if let (Some(&obj_v), Some(&key_v)) = (temp_map.get(object), temp_map.get(key)) {
                                let result = self.compile_ir_index(obj_v, key_v, obj_ty, key_ty, result_ty);
                                temp_map.insert(*dst, result);
                            }
                        }
                        Instruction::IndexSet { object, key, value, obj_ty, key_ty, val_ty } => {
                            if let (Some(&obj_v), Some(&key_v), Some(&val_v)) =
                                (temp_map.get(object), temp_map.get(key), temp_map.get(value))
                            {
                                self.compile_ir_index_set(obj_v, key_v, val_v, obj_ty, key_ty, val_ty);
                            }
                        }
                        Instruction::FieldGet { dst, object, field, obj_ty, result_ty } => {
                            if let Some(&obj_v) = temp_map.get(object) {
                                let result = self.compile_ir_field_get(obj_v, field, obj_ty, result_ty);
                                temp_map.insert(*dst, result);
                            }
                        }
                        Instruction::ObjectRest { dst, src, src_ty, exclude } => {
                            if let Some(&src_v) = temp_map.get(src) {
                                // Unbox a boxed Json object to the raw LinObject*.
                                let src_obj = if Self::is_union_type(src_ty) && src_v.is_pointer_value() {
                                    self.builder.build_call(self.rt_unbox_ptr, &[src_v.into()], "orest_unbox")
                                        .unwrap().try_as_basic_value().unwrap_basic()
                                } else { src_v };
                                let rest_obj = self.builder.build_call(self.rt_object_alloc,
                                    &[i32_ty.const_int(4, false).into()], "orest")
                                    .unwrap().try_as_basic_value().unwrap_basic().into_pointer_value();
                                let exclude_fn = self.get_or_declare_fn("lin_object_copy_except",
                                    void_ty.fn_type(&[ptr_ty.into(), ptr_ty.into(), ptr_ty.into(), i32_ty.into()], false));
                                let n_exc = exclude.len() as u32;
                                let arr_ty = ptr_ty.array_type(n_exc.max(1));
                                let keys_arr = self.builder.build_alloca(arr_ty, "orest_keys").unwrap();
                                for (i, key) in exclude.iter().enumerate() {
                                    let key_str = self.compile_string_lit(key);
                                    let gep = unsafe { self.builder.build_gep(arr_ty, keys_arr,
                                        &[i32_ty.const_zero(), i32_ty.const_int(i as u64, false)], "orest_kp").unwrap() };
                                    self.builder.build_store(gep, key_str).unwrap();
                                }
                                let keys_ptr = self.builder.build_pointer_cast(keys_arr, ptr_ty, "orest_kps").unwrap();
                                self.builder.build_call(exclude_fn,
                                    &[rest_obj.into(), src_obj.into(), keys_ptr.into(), i32_ty.const_int(n_exc as u64, false).into()], "").unwrap();
                                let boxed = self.builder.build_call(self.rt_box_object, &[rest_obj.into()], "orest_boxed")
                                    .unwrap().try_as_basic_value().unwrap_basic();
                                temp_map.insert(*dst, boxed);
                            }
                        }
                        Instruction::ArrayLenCheck { dst, val, n, at_least } => {
                            if let Some(&v) = temp_map.get(val) {
                                let result = if v.is_pointer_value() {
                                    // BRANCHLESS via runtime helper (tag check + length test),
                                    // so this stays in one basic block (SSA dominance).
                                    let i8t = self.context.i8_type();
                                    let check_fn = self.get_or_declare_fn("lin_value_array_len_check",
                                        i8t.fn_type(&[ptr_ty.into(), i64_ty.into(), i8t.into()], false));
                                    let n_v = i64_ty.const_int(*n, false);
                                    let at_v = i8t.const_int(*at_least as u64, false);
                                    let r = self.builder.build_call(check_fn, &[v.into(), n_v.into(), at_v.into()], "alc")
                                        .unwrap().try_as_basic_value().unwrap_basic().into_int_value();
                                    self.builder.build_int_truncate_or_bit_cast(r, self.context.bool_type(), "alc_b").unwrap().into()
                                } else {
                                    self.context.bool_type().const_zero().into()
                                };
                                temp_map.insert(*dst, result);
                            }
                        }
                        Instruction::GlobalValSet { slot, value, ty } => {
                            if let Some(&v) = temp_map.get(value) {
                                let llvm_ty = self.llvm_type(ty);
                                let glob = *ir_global_vals.entry(*slot).or_insert_with(|| {
                                    let g = self.module.add_global(llvm_ty, None, &format!("_ir_gv_{}", slot));
                                    g.set_initializer(&llvm_ty.const_zero());
                                    g
                                });
                                // A top-level `var` global owns one reference to its current
                                // value. On reassignment its previous reference must be dropped,
                                // otherwise every reassignment leaks the old value. The lowerer
                                // pairs this with a Retain of the new value so the global holds
                                // an independent reference. Restricted to concrete reference-
                                // counted types: boxed (Json/union) globals keep the legacy
                                // borrow model, where the value's owner (not the global) frees
                                // it — releasing here would double-free borrowed values.
                                if Self::ty_is_concrete_rc(ty) {
                                    let old = self.builder
                                        .build_load(llvm_ty, glob.as_pointer_value(), "ir_gv_old")
                                        .unwrap();
                                    self.emit_release(old, ty);
                                }
                                self.builder.build_store(glob.as_pointer_value(), v).unwrap();
                            }
                        }
                        Instruction::GlobalValGet { dst, slot, ty } => {
                            let llvm_ty = self.llvm_type(ty);
                            let glob = *ir_global_vals.entry(*slot).or_insert_with(|| {
                                let g = self.module.add_global(llvm_ty, None, &format!("_ir_gv_{}", slot));
                                g.set_initializer(&llvm_ty.const_zero());
                                g
                            });
                            let v = self.builder.build_load(llvm_ty, glob.as_pointer_value(), "ir_gvget").unwrap();
                            temp_map.insert(*dst, v);
                        }
                        Instruction::MakeCell { dst, init, ty } => {
                            if let Some(&v) = temp_map.get(init) {
                                let llvm_ty = self.llvm_type(ty);
                                let size = llvm_ty.size_of().unwrap();
                                let size_i64 = self.builder.build_int_z_extend_or_bit_cast(size, i64_ty, "cell_sz").unwrap();
                                let cell = self.builder.build_call(self.rt_alloc, &[size_i64.into()], "ir_cell")
                                    .unwrap().try_as_basic_value().unwrap_basic().into_pointer_value();
                                self.builder.build_store(cell, v).unwrap();
                                temp_map.insert(*dst, cell.into());
                            }
                        }
                        Instruction::CellGet { dst, cell, ty } => {
                            if let Some(&c) = temp_map.get(cell) {
                                if c.is_pointer_value() {
                                    let llvm_ty = self.llvm_type(ty);
                                    let v = self.builder.build_load(llvm_ty, c.into_pointer_value(), "ir_cellget").unwrap();
                                    temp_map.insert(*dst, v);
                                } else {
                                    temp_map.insert(*dst, self.llvm_type(ty).const_zero());
                                }
                            }
                        }
                        Instruction::CellSet { cell, value, ty } => {
                            if let (Some(&c), Some(&v)) = (temp_map.get(cell), temp_map.get(value)) {
                                if c.is_pointer_value() {
                                    // A captured `var` cell owns one reference to its current
                                    // value. On reassignment its previous reference must be
                                    // dropped, otherwise every reassignment leaks the old value.
                                    // The lowerer pairs this with a Retain of the new value so
                                    // the cell holds an independent reference. Restricted to
                                    // concrete reference-counted types: boxed (Json/union) cells
                                    // keep the legacy borrow model (releasing a borrowed value
                                    // here would double-free). The release fns null-check the
                                    // cell's initial zero.
                                    if Self::ty_is_concrete_rc(ty) {
                                        let llvm_ty = self.llvm_type(ty);
                                        let old = self.builder
                                            .build_load(llvm_ty, c.into_pointer_value(), "ir_cell_old")
                                            .unwrap();
                                        self.emit_release(old, ty);
                                    }
                                    self.builder.build_store(c.into_pointer_value(), v).unwrap();
                                }
                            }
                        }
                        Instruction::EnvCapture { dst, env, index, ty } => {
                            if let Some(&env_v) = temp_map.get(env) {
                                if env_v.is_pointer_value() {
                                    // Captures live at byte offset 8 + index*8 in the env
                                    // allocation (offset 0 is the size header), matching
                                    // make_closure_struct's layout.
                                    let i8_ty = self.context.i8_type();
                                    let offset = i64_ty.const_int(8 + (*index as u64) * 8, false);
                                    let gep = unsafe {
                                        self.builder.build_gep(i8_ty, env_v.into_pointer_value(), &[offset], "ir_capgep").unwrap()
                                    };
                                    let load_ty = self.llvm_type(ty);
                                    let loaded = self.builder.build_load(load_ty, gep, "ir_cap").unwrap();
                                    temp_map.insert(*dst, loaded);
                                } else {
                                    temp_map.insert(*dst, self.llvm_type(ty).const_zero());
                                }
                            }
                        }
                        Instruction::IsType { dst, val, ty } => {
                            if let Some(&v) = temp_map.get(val) {
                                let result = self.compile_ir_is_type(v, ty);
                                temp_map.insert(*dst, result.into());
                            }
                        }
                        Instruction::HasPattern { dst, val, pattern } => {
                            if let Some(&v) = temp_map.get(val) {
                                let result = self.compile_ir_has_pattern(v, pattern);
                                temp_map.insert(*dst, result.into());
                            }
                        }
                        Instruction::Coerce { dst, src, from_ty, to_ty } => {
                            if let Some(&sv) = temp_map.get(src) {
                                let result = self.compile_ir_coerce(sv, from_ty, to_ty);
                                temp_map.insert(*dst, result);
                            }
                        }
                        Instruction::Bind { dst, src, .. } => {
                            if let Some(&sv) = temp_map.get(src) {
                                temp_map.insert(*dst, sv);
                            }
                        }
                        Instruction::Panic { msg } => {
                            if let Some(&msg_v) = temp_map.get(msg) {
                                if msg_v.is_pointer_value() {
                                    let zero = i32_ty.const_zero();
                                    self.builder.build_call(self.rt_panic, &[msg_v.into(), zero.into(), zero.into()], "").unwrap();
                                }
                            }
                            // Note: no terminator here — the block's IR Terminator (an
                            // Unreachable) is emitted after the instruction loop. Emitting
                            // build_unreachable here would double-terminate the block.
                        }
                        Instruction::Box { dst, val, ty } => {
                            if let Some(&v) = temp_map.get(val) {
                                let result = self.compile_ir_box(v, ty);
                                temp_map.insert(*dst, result);
                            }
                        }
                        Instruction::Unbox { dst, val, result_ty } => {
                            if let Some(&v) = temp_map.get(val) {
                                let result = self.compile_ir_unbox(v, result_ty);
                                temp_map.insert(*dst, result);
                            }
                        }
                        Instruction::Unary { dst, op, operand, ty } => {
                            if let Some(&v) = temp_map.get(operand) {
                                let result = self.compile_ir_unary(v, op, ty);
                                temp_map.insert(*dst, result);
                            }
                        }
                    }
                }

                // Record the block's actual exit LLVM block (may differ from its entry if
                // an instruction emitted internal branches). The terminator below is emitted
                // here, at the current position.
                ir_block_exit.insert(block.id, self.builder.get_insert_block().unwrap());

                // Emit terminator
                match &block.terminator {
                    Terminator::Return(Some(t)) => {
                        if let Some(&v) = temp_map.get(t) {
                            self.builder.build_return(Some(&v)).unwrap();
                        } else {
                            self.builder.build_return(None).unwrap();
                        }
                    }
                    Terminator::Return(None) => {
                        self.builder.build_return(None).unwrap();
                    }
                    Terminator::Jump(target) => {
                        let target_bb = ir_block_to_llvm[target];
                        self.builder.build_unconditional_branch(target_bb).unwrap();
                    }
                    Terminator::CondJump { cond, then_block, else_block } => {
                        // A missing condition temp means malformed IR — the old `const_zero`
                        // fallback silently took the else branch unconditionally. Fail loudly.
                        let cond_val = *temp_map.get(cond).unwrap_or_else(|| panic!("CondJump: undefined cond temp {cond:?}"));
                        let cond_i1 = if cond_val.get_type() == self.context.bool_type().into() {
                            cond_val.into_int_value()
                        } else {
                            self.context.bool_type().const_zero()
                        };
                        let then_bb = ir_block_to_llvm[then_block];
                        let else_bb = ir_block_to_llvm[else_block];
                        self.builder.build_conditional_branch(cond_i1, then_bb, else_bb).unwrap();
                    }
                    Terminator::TailCall { args } => {
                        // TCO: store the new argument values into the param allocas and
                        // branch back to the loop header (the function's first IR block).
                        for (i, arg_temp) in args.iter().enumerate() {
                            if let (Some(&v), Some(slot)) = (temp_map.get(arg_temp), param_allocs.get(i)) {
                                self.builder.build_store(*slot, v).unwrap();
                            }
                        }
                        if let Some(first_ir_bb) = func.blocks.first().and_then(|b| ir_block_to_llvm.get(&b.id)) {
                            self.builder.build_unconditional_branch(*first_ir_bb).unwrap();
                        } else {
                            self.builder.build_unreachable().unwrap();
                        }
                    }
                    Terminator::Switch { val, cases, default } => {
                        if let Some(&v) = temp_map.get(val) {
                            if v.is_int_value() {
                                let int_v = v.into_int_value();
                                let def_bb = ir_block_to_llvm[default];
                                let case_bbs: Vec<(inkwell::values::IntValue, inkwell::basic_block::BasicBlock)> = cases
                                    .iter()
                                    .filter_map(|(tag, bid)| {
                                        ir_block_to_llvm.get(bid).map(|bb| (self.context.i8_type().const_int(*tag as u64, false), *bb))
                                    })
                                    .collect();
                                self.builder.build_switch(int_v, def_bb, &case_bbs).unwrap();
                            } else {
                                let def_bb = ir_block_to_llvm[default];
                                self.builder.build_unconditional_branch(def_bb).unwrap();
                            }
                        } else {
                            self.builder.build_unreachable().unwrap();
                        }
                    }
                    Terminator::Unreachable => {
                        self.builder.build_unreachable().unwrap();
                    }
                }
            }

            // Backpatch phi incoming edges now that every block (including back-edge
            // sources) has been compiled and all temps are in temp_map.
            for (phi, incomings) in &pending_phis {
                for (val_temp, pred_block) in incomings {
                    // Use the predecessor's EXIT block (where its branch to the merge was
                    // actually emitted), not its entry block.
                    let pred_bb = ir_block_exit.get(pred_block).or_else(|| ir_block_to_llvm.get(pred_block));
                    if let (Some(&v), Some(&pred_bb)) = (temp_map.get(val_temp), pred_bb) {
                        phi.add_incoming(&[(&v, pred_bb)]);
                    }
                }
            }
        }
    }

    /// Narrow/widen an integer value to the integer width of `target_ty`. Non-integer
    /// values and non-integer targets are returned unchanged. Used to reconcile a runtime
    /// intrinsic that returns a fixed width (e.g. lin_array_length → i64) with a declared
    /// result type of a different width (e.g. Int32).
    fn coerce_int_width(&self, val: BasicValueEnum<'ctx>, target_ty: &Type) -> BasicValueEnum<'ctx> {
        if !val.is_int_value() || !target_ty.is_integer() {
            return val;
        }
        let iv = val.into_int_value();
        let target_llvm = self.llvm_type(target_ty).into_int_type();
        let iv_bits = iv.get_type().get_bit_width();
        let tgt_bits = target_llvm.get_bit_width();
        if tgt_bits == iv_bits {
            val
        } else if tgt_bits > iv_bits {
            if target_ty.is_signed() {
                self.builder.build_int_s_extend(iv, target_llvm, "ir_len_sext").unwrap().into()
            } else {
                self.builder.build_int_z_extend(iv, target_llvm, "ir_len_zext").unwrap().into()
            }
        } else {
            self.builder.build_int_truncate(iv, target_llvm, "ir_len_trunc").unwrap().into()
        }
    }

    fn compile_ir_intrinsic(&mut self, intrinsic: &lir::Intrinsic, args: &[BasicValueEnum<'ctx>], arg_tys: &[Type], ret_ty: &Type) -> BasicValueEnum<'ctx> {
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
                    self.builder.build_call(self.rt_print, &[str_val.into()], "").unwrap();
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
                        self.builder.build_call(self.rt_string_length, &[arg.into()], "ir_slen")
                            .unwrap().try_as_basic_value().unwrap_basic()
                    }
                    Type::Array(_) | Type::FixedArray(_) | Type::Iterator(_) => {
                        let len_fn = self.get_or_declare_fn("lin_array_length",
                            i64_ty.fn_type(&[ptr_ty.into()], false));
                        self.builder.build_call(len_fn, &[arg.into()], "ir_alen")
                            .unwrap().try_as_basic_value().unwrap_basic()
                    }
                    Type::Object(_) | Type::Named(_) => {
                        let obj_len_fn = self.get_or_declare_fn("lin_object_length",
                            i64_ty.fn_type(&[ptr_ty.into()], false));
                        self.builder.build_call(obj_len_fn, &[arg.into()], "ir_olen")
                            .unwrap().try_as_basic_value().unwrap_basic()
                    }
                    _ => {
                        // Json / TypeVar / Union — dynamic dispatch on the runtime tag.
                        let len_dyn_fn = self.get_or_declare_fn("lin_length_dyn",
                            i32_ty.fn_type(&[ptr_ty.into()], false));
                        self.builder.build_call(len_dyn_fn, &[arg.into()], "ir_dynlen")
                            .unwrap().try_as_basic_value().unwrap_basic()
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
                        let arr_raw = self.builder.build_call(self.rt_unbox_ptr, &[arr.into()], "ir_push_arr")
                            .unwrap().try_as_basic_value().unwrap_basic();
                        let elem_is_fresh_box = !Self::is_union_type(&elem_ty);
                        let elem_tagged = if elem_is_fresh_box {
                            self.box_value(elem, &elem_ty)
                        } else { elem };
                        let push_dyn_fn = self.get_or_declare_fn("lin_push_dyn",
                            self.context.void_type().fn_type(&[ptr_ty.into(), ptr_ty.into()], false));
                        self.builder.build_call(push_dyn_fn, &[arr_raw.into(), elem_tagged.into()], "").unwrap();
                        if elem_is_fresh_box && elem_tagged.is_pointer_value() {
                            self.builder.build_call(self.rt_tagged_release, &[elem_tagged.into()], "").unwrap();
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
                self.builder.build_call(self.rt_array_alloc, &[cap.into()], "ir_arr_alloc")
                    .unwrap().try_as_basic_value().unwrap_basic()
            }
            Intrinsic::FlatArrayAlloc(kind) => {
                let cap = self.context.i64_type().const_int(4, false);
                let alloc_fn = self.get_or_declare_fn(
                    &format!("lin_flat_array_alloc_{}", kind.suffix()),
                    ptr_ty.fn_type(&[self.context.i64_type().into()], false));
                self.builder.build_call(alloc_fn, &[cap.into()], "ir_falloc")
                    .unwrap().try_as_basic_value().unwrap_basic()
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
                    self.builder.build_call(rt_concat, &[a.into(), b.into()], "ir_cat")
                        .unwrap().try_as_basic_value().unwrap_basic()
                } else { ptr_ty.const_null().into() }
            }
            Intrinsic::Async => {
                // async(thunk): call the thunk closure synchronously (it returns a boxed
                // Json result), then wrap in a LinPromise*. The thunk may arrive boxed (a
                // Json-typed parameter, as in std/async's `async(f: Json)`) — unbox to the
                // raw closure struct before calling.
                let thunk = args.last().copied().unwrap_or_else(|| ptr_ty.const_null().into());
                let thunk_ty = arg_tys.last().cloned().unwrap_or(Type::Null);
                let thunk = if Self::is_union_type(&thunk_ty) && thunk.is_pointer_value() {
                    self.builder.build_call(self.rt_unbox_ptr, &[thunk.into()], "ir_async_cls")
                        .unwrap().try_as_basic_value().unwrap_basic()
                } else { thunk };
                let result = self.call_thunk_value(thunk);
                let make_promise = self.get_or_declare_fn("lin_make_promise",
                    ptr_ty.fn_type(&[ptr_ty.into()], false));
                self.builder.build_call(make_promise, &[result.into()], "ir_promise")
                    .unwrap().try_as_basic_value().unwrap_basic()
            }
            Intrinsic::Await => {
                let promise = args.first().copied().unwrap_or_else(|| ptr_ty.const_null().into());
                let await_fn = self.get_or_declare_fn("lin_await_promise",
                    ptr_ty.fn_type(&[ptr_ty.into()], false));
                let tagged = self.builder.build_call(await_fn, &[promise.into()], "ir_await")
                    .unwrap().try_as_basic_value().unwrap_basic();
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
                        self.builder.build_call(exit_fn, &[code.into_int_value().into()], "").unwrap();
                    }
                }
                ptr_ty.const_null().into()
            }
            // parallel(tasks): tasks is a boxed array of thunk closures. Run each synchronously
            // and collect the boxed results into a new tagged array. Mirrors the runtime-path
            // branch of the AST compile_async_intrinsic.
            Intrinsic::Parallel => {
                let i64_ty = self.context.i64_type();
                let tasks = args.first().copied().unwrap_or_else(|| ptr_ty.const_null().into());
                let arr_unboxed = if tasks.is_pointer_value() {
                    self.builder.build_call(self.rt_unbox_ptr, &[tasks.into()], "ir_par_arr")
                        .unwrap().try_as_basic_value().unwrap_basic()
                } else { ptr_ty.const_null().into() };
                let len_fn = self.get_or_declare_fn("lin_array_length",
                    i64_ty.fn_type(&[ptr_ty.into()], false));
                let len = self.builder.build_call(len_fn, &[arr_unboxed.into()], "ir_par_len")
                    .unwrap().try_as_basic_value().unwrap_basic().into_int_value();
                let out_arr = self.builder.build_call(self.rt_array_alloc, &[len.into()], "ir_par_out")
                    .unwrap().try_as_basic_value().unwrap_basic();
                let get_tagged_fn = self.get_or_declare_fn("lin_array_get_tagged",
                    ptr_ty.fn_type(&[ptr_ty.into(), i64_ty.into()], false));
                let push_tagged_fn = self.get_or_declare_fn("lin_array_push_tagged",
                    self.context.void_type().fn_type(&[ptr_ty.into(), ptr_ty.into()], false));
                let llvm_fn = self.builder.get_insert_block().unwrap().get_parent().unwrap();
                let check = self.context.append_basic_block(llvm_fn, "ir_par_check");
                let body = self.context.append_basic_block(llvm_fn, "ir_par_body");
                let exit = self.context.append_basic_block(llvm_fn, "ir_par_exit");
                let i_alloc = self.builder.build_alloca(i64_ty, "ir_par_i").unwrap();
                self.builder.build_store(i_alloc, i64_ty.const_zero()).unwrap();
                self.builder.build_unconditional_branch(check).unwrap();
                self.builder.position_at_end(check);
                let cur = self.builder.build_load(i64_ty, i_alloc, "ir_par_cur").unwrap().into_int_value();
                let cond = self.builder.build_int_compare(inkwell::IntPredicate::SLT, cur, len, "ir_par_cond").unwrap();
                self.builder.build_conditional_branch(cond, body, exit).unwrap();
                self.builder.position_at_end(body);
                let elem_tv = self.builder.build_call(get_tagged_fn, &[arr_unboxed.into(), cur.into()], "ir_par_elem")
                    .unwrap().try_as_basic_value().unwrap_basic();
                // Element is a boxed closure (TaggedVal*); unbox to the closure struct, then
                // call it via the uniform boxed thunk ABI.
                let cls = self.builder.build_call(self.rt_unbox_ptr, &[elem_tv.into()], "ir_par_cls")
                    .unwrap().try_as_basic_value().unwrap_basic();
                let res = self.call_thunk_value(cls);
                self.builder.build_call(push_tagged_fn, &[out_arr.into(), res.into()], "").unwrap();
                let next = self.builder.build_int_add(cur, i64_ty.const_int(1, false), "ir_par_next").unwrap();
                self.builder.build_store(i_alloc, next).unwrap();
                self.builder.build_unconditional_branch(check).unwrap();
                self.builder.position_at_end(exit);
                out_arr
            }
            // race/timeout/retry — simplified synchronous semantics: return the given
            // promise/first argument unchanged (matches the AST handler).
            Intrinsic::Race | Intrinsic::Timeout | Intrinsic::Retry => {
                args.first().copied().unwrap_or_else(|| ptr_ty.const_null().into())
            }
            // threadPool(n) → lin_thread_pool_new(n).
            Intrinsic::ThreadPool => {
                let i32_ty = self.context.i32_type();
                let n = args.first().copied().unwrap_or_else(|| i32_ty.const_int(2, false).into());
                let n_i32 = if n.is_int_value() { n.into_int_value() } else { i32_ty.const_int(2, false) };
                let pool_fn = self.get_or_declare_fn("lin_thread_pool_new",
                    ptr_ty.fn_type(&[i32_ty.into()], false));
                self.builder.build_call(pool_fn, &[n_i32.into()], "ir_pool")
                    .unwrap().try_as_basic_value().unwrap_basic()
            }
            // worker(handler, onClose) → lin_worker_new(fn_ptr, env_ptr, has_env). The handler
            // arrives as a (possibly boxed) closure value.
            Intrinsic::Worker => {
                let handler = args.first().copied().unwrap_or_else(|| ptr_ty.const_null().into());
                let handler_ty = arg_tys.first().cloned().unwrap_or(Type::Null);
                let i8_ty = self.context.i8_type();
                let (fn_ptr, env_ptr, has_env) = if handler.is_pointer_value() {
                    let cls_ptr = if Self::is_union_type(&handler_ty) {
                        self.builder.build_call(self.rt_unbox_ptr, &[handler.into()], "ir_w_cls")
                            .unwrap().try_as_basic_value().unwrap_basic().into_pointer_value()
                    } else { handler.into_pointer_value() };
                    let cls_ty = self.closure_struct_type();
                    let fn_f = self.builder.build_struct_gep(cls_ty, cls_ptr, 2, "ir_w_fn_f").unwrap();
                    let fp = self.builder.build_load(ptr_ty, fn_f, "ir_w_fn").unwrap();
                    let env_f = self.builder.build_struct_gep(cls_ty, cls_ptr, 3, "ir_w_env_f").unwrap();
                    let ep = self.builder.build_load(ptr_ty, env_f, "ir_w_env").unwrap();
                    (fp, ep, i8_ty.const_int(1, false))
                } else {
                    (ptr_ty.const_null().into(), ptr_ty.const_null().into(), i8_ty.const_int(0, false))
                };
                let worker_fn = self.get_or_declare_fn("lin_worker_new",
                    ptr_ty.fn_type(&[ptr_ty.into(), ptr_ty.into(), i8_ty.into()], false));
                self.builder.build_call(worker_fn, &[fn_ptr.into(), env_ptr.into(), has_env.into()], "ir_worker")
                    .unwrap().try_as_basic_value().unwrap_basic()
            }
            // w.request(msg) → lin_worker_request(w, boxed msg) → result (unboxed if concrete).
            Intrinsic::Request => {
                let worker = args.first().copied().unwrap_or_else(|| ptr_ty.const_null().into());
                let msg = args.get(1).copied().unwrap_or_else(|| ptr_ty.const_null().into());
                let msg_ty = arg_tys.get(1).cloned().unwrap_or(Type::Null);
                let msg_ptr = if Self::is_union_type(&msg_ty) || msg.is_pointer_value() { msg } else { self.box_value(msg, &msg_ty) };
                let req_fn = self.get_or_declare_fn("lin_worker_request",
                    ptr_ty.fn_type(&[ptr_ty.into(), ptr_ty.into()], false));
                let tagged = self.builder.build_call(req_fn, &[worker.into(), msg_ptr.into()], "ir_w_reply")
                    .unwrap().try_as_basic_value().unwrap_basic();
                if !Self::is_union_type(ret_ty) && *ret_ty != Type::Null {
                    self.unbox_tagged_val_to_type(tagged, ret_ty)
                } else { tagged }
            }
            // w.message(msg) → lin_worker_message(w, boxed msg) (void).
            Intrinsic::Message => {
                let worker = args.first().copied().unwrap_or_else(|| ptr_ty.const_null().into());
                let msg = args.get(1).copied().unwrap_or_else(|| ptr_ty.const_null().into());
                let msg_ty = arg_tys.get(1).cloned().unwrap_or(Type::Null);
                let msg_ptr = if Self::is_union_type(&msg_ty) || msg.is_pointer_value() { msg } else { self.box_value(msg, &msg_ty) };
                let msg_fn = self.get_or_declare_fn("lin_worker_message",
                    self.context.void_type().fn_type(&[ptr_ty.into(), ptr_ty.into()], false));
                self.builder.build_call(msg_fn, &[worker.into(), msg_ptr.into()], "").unwrap();
                ptr_ty.const_null().into()
            }
            // w.close() → lin_worker_close(w) (void).
            Intrinsic::Close => {
                let worker = args.first().copied().unwrap_or_else(|| ptr_ty.const_null().into());
                let close_fn = self.get_or_declare_fn("lin_worker_close",
                    self.context.void_type().fn_type(&[ptr_ty.into()], false));
                self.builder.build_call(close_fn, &[worker.into()], "").unwrap();
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
                    self.builder.build_call(self.rt_object_set,
                        &[obj_ptr.into(), key_ptr.into(), val_tagged.into()], "").unwrap();
                    if val_is_fresh_box && val_tagged.is_pointer_value() {
                        self.builder.build_call(self.rt_tagged_release, &[val_tagged.into()], "").unwrap();
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
                        self.builder.build_call(self.rt_unbox_ptr, &[args[0].into()], "set_arr")
                            .unwrap().try_as_basic_value().unwrap_basic()
                    } else { args[0] };
                    let idx_i64 = self.index_value_to_i64(args[1]);
                    let elem_tagged = if Self::is_union_type(&val_ty) {
                        args[2]
                    } else {
                        self.box_value(args[2], &val_ty)
                    };
                    let set_fn = self.get_or_declare_fn("lin_array_set",
                        void_ty.fn_type(&[ptr_ty.into(), i64_ty.into(), ptr_ty.into()], false));
                    self.builder.build_call(set_fn, &[arr_ptr.into(), idx_i64.into(), elem_tagged.into()], "").unwrap();
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
                    self.builder.build_call(f, &[obj_ptr.into()], "ir_keys")
                        .unwrap().try_as_basic_value().unwrap_basic()
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
                    self.builder.build_call(vk_fn, &[tagged.into()], "ir_vkey")
                        .unwrap().try_as_basic_value().unwrap_basic()
                } else { ptr_ty.const_null().into() }
            }
            // lin_array_allocate(n) => Json[]  (null-filled tagged array of length n).
            Intrinsic::ArrayAllocate => {
                let i64_ty = self.context.i64_type();
                let n_i64 = self.ir_n_to_i64(args.first().copied(), arg_tys.first());
                let alloc_fn = self.get_or_declare_fn("lin_array_alloc_null",
                    ptr_ty.fn_type(&[i64_ty.into()], false));
                self.builder.build_call(alloc_fn, &[n_i64.into()], "ir_alloc_arr")
                    .unwrap().try_as_basic_value().unwrap_basic()
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
                    self.builder.build_call(alloc_fn, &[n_i64.into(), fill_val.into()], "ir_fillflat")
                        .unwrap().try_as_basic_value().unwrap_basic()
                } else {
                    let alloc_fn = self.get_or_declare_fn("lin_array_alloc_null",
                        ptr_ty.fn_type(&[i64_ty.into()], false));
                    let arr = self.builder.build_call(alloc_fn, &[n_i64.into()], "ir_fillgen")
                        .unwrap().try_as_basic_value().unwrap_basic();
                    let tagged = self.build_tagged_val_alloca(&fill_val, &fill_ty);
                    let set_fn = self.get_or_declare_fn("lin_array_set",
                        self.context.void_type().fn_type(&[ptr_ty.into(), i64_ty.into(), ptr_ty.into()], false));
                    let llvm_fn = self.builder.get_insert_block().unwrap().get_parent().unwrap();
                    let i_alloc = self.builder.build_alloca(i64_ty, "ir_fi").unwrap();
                    self.builder.build_store(i_alloc, i64_ty.const_zero()).unwrap();
                    let check = self.context.append_basic_block(llvm_fn, "ir_fill_check");
                    let body = self.context.append_basic_block(llvm_fn, "ir_fill_body");
                    let exit = self.context.append_basic_block(llvm_fn, "ir_fill_exit");
                    self.builder.build_unconditional_branch(check).unwrap();
                    self.builder.position_at_end(check);
                    let cur = self.builder.build_load(i64_ty, i_alloc, "ir_fi_v").unwrap().into_int_value();
                    let cond = self.builder.build_int_compare(inkwell::IntPredicate::SLT, cur, n_i64, "ir_fill_cond").unwrap();
                    self.builder.build_conditional_branch(cond, body, exit).unwrap();
                    self.builder.position_at_end(body);
                    self.builder.build_call(set_fn, &[arr.into(), cur.into(), tagged.into()], "").unwrap();
                    let next = self.builder.build_int_add(cur, i64_ty.const_int(1, false), "ir_fi_n").unwrap();
                    self.builder.build_store(i_alloc, next).unwrap();
                    self.builder.build_unconditional_branch(check).unwrap();
                    self.builder.position_at_end(exit);
                    arr
                }
            }
            _ => ptr_ty.const_null().into(),
        }
    }

    /// Coerce an IR value to a raw heap pointer (LinObject*/LinArray*/LinString*): if the
    /// static type is a union (boxed TaggedVal*) OR the value isn't already a pointer, unbox
    /// it; otherwise pass through. Used by the dynamic object/array helper intrinsics.
    fn ir_as_raw_ptr(&mut self, v: BasicValueEnum<'ctx>, ty: &Type) -> BasicValueEnum<'ctx> {
        if Self::is_union_type(ty) || !v.is_pointer_value() {
            self.builder.build_call(self.rt_unbox_ptr, &[v.into()], "ir_raw_ptr")
                .unwrap().try_as_basic_value().unwrap_basic()
        } else {
            v
        }
    }

    /// Normalise an array-length argument to i64: unbox a boxed Int32 if needed, then
    /// sign-extend. Used by the array-allocate helpers.
    fn ir_n_to_i64(&mut self, n: Option<BasicValueEnum<'ctx>>, n_ty: Option<&Type>) -> inkwell::values::IntValue<'ctx> {
        let i64_ty = self.context.i64_type();
        let Some(n) = n else { return i64_ty.const_zero() };
        if n.is_pointer_value() {
            let n_i32 = self.builder.build_call(self.rt_unbox_int32, &[n.into()], "ir_n_unbox")
                .unwrap().try_as_basic_value().unwrap_basic().into_int_value();
            return self.builder.build_int_s_extend(n_i32, i64_ty, "ir_n64").unwrap();
        }
        if n.is_int_value() {
            let _ = n_ty;
            self.builder.build_int_s_extend_or_bit_cast(n.into_int_value(), i64_ty, "ir_n64").unwrap()
        } else {
            i64_ty.const_zero()
        }
    }

    /// Call a thunk closure value `(env) -> ptr` (closures use the uniform boxed ABI).
    /// Returns the boxed Json result. Used by the async intrinsics on the IR path.
    fn call_thunk_value(&mut self, thunk: BasicValueEnum<'ctx>) -> BasicValueEnum<'ctx> {
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        if !thunk.is_pointer_value() { return ptr_ty.const_null().into(); }
        let cls_ptr = thunk.into_pointer_value();
        let cls_ty = self.closure_struct_type();
        let fn_field = self.builder.build_struct_gep(cls_ty, cls_ptr, 2, "thunk_fn_f").unwrap();
        let fn_ptr = self.builder.build_load(ptr_ty, fn_field, "thunk_fn").unwrap().into_pointer_value();
        let env_field = self.builder.build_struct_gep(cls_ty, cls_ptr, 3, "thunk_env_f").unwrap();
        let env_ptr = self.builder.build_load(ptr_ty, env_field, "thunk_env").unwrap();
        let fn_ty = ptr_ty.fn_type(&[ptr_ty.into()], false);
        self.builder.build_indirect_call(fn_ty, fn_ptr, &[env_ptr.into()], "thunk_res")
            .unwrap().try_as_basic_value().unwrap_basic()
    }

    fn compile_ir_index(&mut self, obj: BasicValueEnum<'ctx>, key: BasicValueEnum<'ctx>, obj_ty: &Type, key_ty: &Type, result_ty: &Type) -> BasicValueEnum<'ctx> {
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        if !obj.is_pointer_value() {
            return ptr_ty.const_null().into();
        }
        // When the object is statically Json/union, `obj` is a TaggedVal* wrapping the
        // real Array/Object pointer — unbox it to the raw container pointer before
        // calling the runtime accessors (which expect LinArray*/LinObject*).
        let container = if Self::is_union_type(obj_ty) {
            self.builder.build_call(self.rt_unbox_ptr, &[obj.into()], "ir_idx_unbox")
                .unwrap().try_as_basic_value().unwrap_basic()
        } else {
            obj
        };
        // When the object is Json/union AND the key is a runtime-boxed value whose kind isn't
        // statically known (e.g. `arr[j]` where j is a closure param typed Json — it could be
        // an int array-index or a string object-key at runtime), dispatch on the KEY's tag:
        // int → array get, otherwise → object get. The static `is_array_access` test below
        // would misclassify this as object access and a runtime array would return null.
        // Mirrors the AST compile_index pointer-key runtime dispatch.
        if Self::is_union_type(obj_ty)
            && Self::is_union_type(key_ty)
            && key.is_pointer_value()
        {
            let llvm_fn = self.builder.get_insert_block().unwrap().get_parent().unwrap();
            let k_tag = self.builder.build_call(self.rt_get_tag, &[key.into()], "ir_idxk_tag")
                .unwrap().try_as_basic_value().unwrap_basic().into_int_value();
            let i8t = self.context.i8_type();
            let is_i32 = self.builder.build_int_compare(IntPredicate::EQ, k_tag, i8t.const_int(2, false), "ir_k_i32").unwrap();
            let is_i64 = self.builder.build_int_compare(IntPredicate::EQ, k_tag, i8t.const_int(3, false), "ir_k_i64").unwrap();
            let is_int = self.builder.build_or(is_i32, is_i64, "ir_k_int").unwrap();
            let int_b = self.context.append_basic_block(llvm_fn, "ir_idx_intk");
            let str_b = self.context.append_basic_block(llvm_fn, "ir_idx_strk");
            let mrg = self.context.append_basic_block(llvm_fn, "ir_idx_kmrg");
            self.builder.build_conditional_branch(is_int, int_b, str_b).unwrap();
            // int key → array get (always returns a valid TaggedVal*).
            self.builder.position_at_end(int_b);
            let idx = self.unbox_value(key, &Type::Int64).into_int_value();
            let get_tagged_fn = self.get_or_declare_fn("lin_array_get_tagged",
                ptr_ty.fn_type(&[ptr_ty.into(), self.context.i64_type().into()], false));
            let arr_res = self.builder.build_call(get_tagged_fn, &[container.into(), idx.into()], "ir_idx_aget")
                .unwrap().try_as_basic_value().unwrap_basic();
            let int_exit = self.builder.get_insert_block().unwrap();
            self.builder.build_unconditional_branch(mrg).unwrap();
            // string key → object get, guarded by an object-tag check on the container source.
            self.builder.position_at_end(str_b);
            let key_raw = self.builder.build_call(self.rt_unbox_ptr, &[key.into()], "ir_idxk_str")
                .unwrap().try_as_basic_value().unwrap_basic();
            let obj_tag = self.builder.build_call(self.rt_get_tag, &[obj.into()], "ir_idx_otag")
                .unwrap().try_as_basic_value().unwrap_basic().into_int_value();
            let is_obj = self.builder.build_int_compare(IntPredicate::EQ, obj_tag, i8t.const_int(7, false), "ir_idx_isobj").unwrap();
            let oget_b = self.context.append_basic_block(llvm_fn, "ir_idx_oget");
            let onull_b = self.context.append_basic_block(llvm_fn, "ir_idx_onull");
            let omrg = self.context.append_basic_block(llvm_fn, "ir_idx_omrg");
            self.builder.build_conditional_branch(is_obj, oget_b, onull_b).unwrap();
            self.builder.position_at_end(oget_b);
            let oget = self.builder.build_call(self.rt_object_get, &[container.into(), key_raw.into()], "ir_idx_osget")
                .unwrap().try_as_basic_value().unwrap_basic();
            let oget_exit = self.builder.get_insert_block().unwrap();
            self.builder.build_unconditional_branch(omrg).unwrap();
            self.builder.position_at_end(onull_b);
            self.builder.build_unconditional_branch(omrg).unwrap();
            self.builder.position_at_end(omrg);
            let ophi = self.builder.build_phi(ptr_ty, "ir_idx_ophi").unwrap();
            ophi.add_incoming(&[(&oget, oget_exit), (&ptr_ty.const_null(), onull_b)]);
            let str_res = ophi.as_basic_value();
            let str_exit = self.builder.get_insert_block().unwrap();
            self.builder.build_unconditional_branch(mrg).unwrap();
            self.builder.position_at_end(mrg);
            let phi = self.builder.build_phi(ptr_ty, "ir_idx_kphi").unwrap();
            phi.add_incoming(&[(&arr_res, int_exit), (&str_res, str_exit)]);
            let res = phi.as_basic_value();
            return if Self::is_union_type(result_ty) { res } else { self.unbox_tagged_val_to_type(res, result_ty) };
        }
        // Array indexing when the object is an array type or the key is numeric (any int
        // width — e.g. an Int32 literal index like `lines[0]`, not just i64).
        let is_array_access = matches!(obj_ty, Type::Array(_) | Type::FixedArray(_))
            || key_ty.is_numeric()
            || (key.is_int_value() && key.get_type() != self.context.bool_type().into());
        if is_array_access {
            // Key may arrive as a raw int or a boxed TaggedVal* — unbox to i64.
            let idx = if key.is_int_value() {
                self.builder.build_int_s_extend_or_bit_cast(key.into_int_value(), self.context.i64_type(), "ir_idx").unwrap()
            } else if key.is_pointer_value() {
                let unboxed = self.unbox_value(key, &Type::Int64);
                unboxed.into_int_value()
            } else {
                return ptr_ty.const_null().into();
            };
            // Flat scalar element: read the unboxed scalar directly (mirrors AST `flat_array_get`).
            if Self::is_flat_scalar(result_ty) {
                return self.flat_array_get(container, idx, result_ty);
            }
            // For TypeVar/Union result, use lin_array_get_tagged so the result is always
            // a valid TaggedVal* regardless of whether the array is flat or tagged.
            if Self::is_union_type(result_ty) {
                let get_tagged_fn = self.get_or_declare_fn("lin_array_get_tagged",
                    ptr_ty.fn_type(&[ptr_ty.into(), self.context.i64_type().into()], false));
                return self.builder.build_call(get_tagged_fn, &[container.into(), idx.into()], "ir_aget_tv")
                    .unwrap().try_as_basic_value().unwrap_basic();
            }
            let tagged = self.builder.build_call(self.rt_array_get, &[container.into(), idx.into()], "ir_aget")
                .unwrap().try_as_basic_value().unwrap_basic();
            return self.unbox_tagged_val_to_type(tagged, result_ty);
        }
        // Object key access. lin_object_get expects a raw *LinString key; unbox a boxed key.
        let key_str = if matches!(key_ty, Type::Str) {
            key
        } else if Self::is_union_type(key_ty) && key.is_pointer_value() {
            self.builder.build_call(self.rt_unbox_ptr, &[key.into()], "ir_key_unbox")
                .unwrap().try_as_basic_value().unwrap_basic()
        } else {
            key
        };
        // When the object is statically Json/union, its runtime value may NOT be an object
        // (e.g. `results["type"]` where results is actually an array). Guard the lookup with
        // a tag check — TAG_OBJECT(7) → look up the key; otherwise return Null. Without this,
        // lin_object_get would read a LinArray*/scalar as a LinObject* and crash. Mirrors the
        // AST compile_index string-key-on-Json path.
        if Self::is_union_type(obj_ty) {
            let llvm_fn = self.builder.get_insert_block().unwrap().get_parent().unwrap();
            let obj_tag = self.builder.build_call(self.rt_get_tag, &[obj.into()], "ir_idx_tag")
                .unwrap().try_as_basic_value().unwrap_basic().into_int_value();
            let is_obj = self.builder.build_int_compare(
                IntPredicate::EQ, obj_tag, self.context.i8_type().const_int(7, false), "ir_idx_is_obj").unwrap();
            let ok = self.context.append_basic_block(llvm_fn, "ir_idx_obj_ok");
            let no = self.context.append_basic_block(llvm_fn, "ir_idx_obj_no");
            let mrg = self.context.append_basic_block(llvm_fn, "ir_idx_obj_mrg");
            self.builder.build_conditional_branch(is_obj, ok, no).unwrap();
            self.builder.position_at_end(ok);
            let entry = self.builder.build_call(self.rt_object_get, &[container.into(), key_str.into()], "ir_oget")
                .unwrap().try_as_basic_value().unwrap_basic();
            let ok_exit = self.builder.get_insert_block().unwrap();
            self.builder.build_unconditional_branch(mrg).unwrap();
            self.builder.position_at_end(no);
            let null_res = ptr_ty.const_null();
            self.builder.build_unconditional_branch(mrg).unwrap();
            self.builder.position_at_end(mrg);
            let phi = self.builder.build_phi(ptr_ty, "ir_idx_obj_phi").unwrap();
            phi.add_incoming(&[(&entry, ok_exit), (&null_res, no)]);
            let result_ptr = phi.as_basic_value();
            return self.unbox_tagged_val_to_type(result_ptr, result_ty);
        }
        let tagged = self.builder.build_call(self.rt_object_get, &[container.into(), key_str.into()], "ir_oget")
            .unwrap().try_as_basic_value().unwrap_basic();
        self.unbox_tagged_val_to_type(tagged, result_ty)
    }

    /// `object[key] = value` for the IR path. Mirrors the AST `compile_index_set`:
    /// dispatch on the object's static type; for Json/union objects, dispatch at
    /// runtime on the value's LLVM kind (pointer key ⇒ object set, int key ⇒ array set),
    /// unboxing the boxed container first.
    fn compile_ir_index_set(&mut self, obj: BasicValueEnum<'ctx>, key: BasicValueEnum<'ctx>, value: BasicValueEnum<'ctx>, obj_ty: &Type, _key_ty: &Type, val_ty: &Type) {
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i64_ty = self.context.i64_type();
        let void_ty = self.context.void_type();
        let tagged_val = self.build_tagged_val_alloca(&value, val_ty);
        match obj_ty {
            Type::Object(_) | Type::Named(_) => {
                if obj.is_pointer_value() && key.is_pointer_value() {
                    self.builder.build_call(self.rt_object_set,
                        &[obj.into(), key.into(), tagged_val.into()], "").unwrap();
                }
            }
            Type::Array(_) | Type::FixedArray(_) => {
                let idx = self.index_value_to_i64(key);
                let set_fn = self.get_or_declare_fn("lin_array_set",
                    void_ty.fn_type(&[ptr_ty.into(), i64_ty.into(), ptr_ty.into()], false));
                self.builder.build_call(set_fn, &[obj.into(), idx.into(), tagged_val.into()], "").unwrap();
            }
            Type::TypeVar(_) | Type::Union(_) => {
                if !obj.is_pointer_value() { return; }
                // Unbox the boxed container, then dispatch on the key's LLVM kind.
                let container = self.builder.build_call(self.rt_unbox_ptr, &[obj.into()], "iset_unbox")
                    .unwrap().try_as_basic_value().unwrap_basic();
                if key.is_pointer_value() {
                    // String (object) key.
                    self.builder.build_call(self.rt_object_set,
                        &[container.into(), key.into(), tagged_val.into()], "").unwrap();
                } else if key.is_int_value() {
                    let idx = self.index_value_to_i64(key);
                    let set_fn = self.get_or_declare_fn("lin_array_set",
                        void_ty.fn_type(&[ptr_ty.into(), i64_ty.into(), ptr_ty.into()], false));
                    self.builder.build_call(set_fn, &[container.into(), idx.into(), tagged_val.into()], "").unwrap();
                }
            }
            _ => {}
        }
    }

    /// Normalise an index value (raw int or boxed TaggedVal*) to an i64.
    fn index_value_to_i64(&mut self, key: BasicValueEnum<'ctx>) -> inkwell::values::IntValue<'ctx> {
        if key.is_int_value() {
            self.builder.build_int_s_extend_or_bit_cast(key.into_int_value(), self.context.i64_type(), "ir_idx64").unwrap()
        } else if key.is_pointer_value() {
            let i32_key = self.builder.build_call(self.rt_unbox_int32, &[key.into()], "ir_skey_i32")
                .unwrap().try_as_basic_value().unwrap_basic().into_int_value();
            self.builder.build_int_s_extend(i32_key, self.context.i64_type(), "ir_skey_i64").unwrap()
        } else {
            self.context.i64_type().const_zero()
        }
    }

    fn compile_ir_field_get(&mut self, obj: BasicValueEnum<'ctx>, field: &str, obj_ty: &Type, result_ty: &Type) -> BasicValueEnum<'ctx> {
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        if obj.is_pointer_value() {
            // A Json/union object arrives as a boxed TaggedVal*; unbox to the raw LinObject*.
            let container = if Self::is_union_type(obj_ty) {
                self.builder.build_call(self.rt_unbox_ptr, &[obj.into()], "ir_fget_unbox")
                    .unwrap().try_as_basic_value().unwrap_basic()
            } else {
                obj
            };
            let key_str = self.compile_string_lit(field).into_pointer_value();
            let tagged = self.builder.build_call(self.rt_object_get, &[container.into(), key_str.into()], "ir_fget")
                .unwrap().try_as_basic_value().unwrap_basic();
            self.builder.build_call(self.rt_string_release, &[key_str.into()], "").unwrap();
            self.unbox_tagged_val_to_type(tagged, result_ty)
        } else { ptr_ty.const_null().into() }
    }

    fn compile_ir_is_type(&mut self, val: BasicValueEnum<'ctx>, ty: &Type) -> inkwell::values::IntValue<'ctx> {
        // Use get_tag and compare.
        if val.is_pointer_value() {
            let tag = self.builder.build_call(self.rt_get_tag, &[val.into()], "ir_tag")
                .unwrap().try_as_basic_value().unwrap_basic().into_int_value();
            let expected = self.type_tag_const(ty);
            self.builder.build_int_compare(IntPredicate::EQ, tag, expected, "ir_is").unwrap()
        } else {
            self.context.bool_type().const_zero()
        }
    }

    fn compile_ir_has_pattern(&mut self, val: BasicValueEnum<'ctx>, pattern: &lir::HasDesc) -> inkwell::values::IntValue<'ctx> {
        let bool_ty = self.context.bool_type();
        if !val.is_pointer_value() { return bool_ty.const_zero(); }
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i8_ty = self.context.i8_type();
        // BRANCHLESS: lin_value_has_field does the tag check + unbox + presence test in the
        // runtime, returning 0 for null/non-object values. Emitting no LLVM branches keeps
        // this IR instruction within a single basic block (avoids out-of-order block
        // creation that breaks SSA dominance when used inside match arms).
        let has_fn = self.get_or_declare_fn("lin_value_has_field",
            i8_ty.fn_type(&[ptr_ty.into(), ptr_ty.into()], false));
        let mut all_present = bool_ty.const_int(1, false);
        for field in &pattern.required_fields {
            let key_str = self.compile_string_lit(field).into_pointer_value();
            let has_i8 = self.builder.build_call(has_fn, &[val.into(), key_str.into()], "ir_has")
                .unwrap().try_as_basic_value().unwrap_basic().into_int_value();
            self.builder.build_call(self.rt_string_release, &[key_str.into()], "").unwrap();
            let has_bool = self.builder.build_int_truncate_or_bit_cast(has_i8, bool_ty, "has_b").unwrap();
            all_present = self.builder.build_and(all_present, has_bool, "has_acc").unwrap();
        }
        all_present
    }

    fn compile_ir_coerce(&mut self, val: BasicValueEnum<'ctx>, from_ty: &Type, to_ty: &Type) -> BasicValueEnum<'ctx> {
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        // Numeric widening.
        if from_ty.is_numeric() && to_ty.is_numeric() {
            if val.is_int_value() && to_ty.is_float() {
                let iv = val.into_int_value();
                let ft = if matches!(to_ty, Type::Float32) { self.context.f32_type().into() } else { self.context.f64_type() };
                return self.builder.build_signed_int_to_float(iv, ft, "ir_i2f").unwrap().into();
            }
            if val.is_float_value() && to_ty.is_integer() {
                let fv = val.into_float_value();
                let it = self.llvm_type(to_ty).into_int_type();
                return self.builder.build_float_to_signed_int(fv, it, "ir_f2i").unwrap().into();
            }
            if val.is_int_value() && to_ty.is_integer() {
                let iv = val.into_int_value();
                let it = self.llvm_type(to_ty).into_int_type();
                let from_bits = iv.get_type().get_bit_width();
                let to_bits = it.get_bit_width();
                return if to_bits > from_bits {
                    self.builder.build_int_z_extend_or_bit_cast(iv, it, "ir_zext").unwrap().into()
                } else {
                    self.builder.build_int_truncate_or_bit_cast(iv, it, "ir_trunc").unwrap().into()
                };
            }
            return val;
        }
        // Box to union. Use heap boxing (lin_box_*) rather than a stack alloca, because
        // a coerced value may escape its defining function (returned, stored in an array,
        // captured) — a stack TaggedVal would dangle.
        if Self::is_union_type(to_ty) {
            return self.box_value(val, from_ty);
        }
        // Unbox from union.
        if Self::is_union_type(from_ty) && val.is_pointer_value() {
            return self.unbox_tagged_val_to_type(val, to_ty);
        }
        let _ = (from_ty, to_ty);
        if val.get_type() == self.llvm_type(to_ty) { val } else { ptr_ty.const_null().into() }
    }

    fn compile_ir_box(&mut self, val: BasicValueEnum<'ctx>, ty: &Type) -> BasicValueEnum<'ctx> {
        // Heap-box (see compile_ir_coerce) so the boxed value can safely escape.
        self.box_value(val, ty)
    }

    fn compile_ir_unbox(&mut self, val: BasicValueEnum<'ctx>, result_ty: &Type) -> BasicValueEnum<'ctx> {
        self.unbox_tagged_val_to_type(val, result_ty)
    }

    fn compile_ir_unary(&mut self, val: BasicValueEnum<'ctx>, op: &lir::UnaryOp, _ty: &Type) -> BasicValueEnum<'ctx> {
        match op {
            lir::UnaryOp::Neg => {
                if val.is_int_value() {
                    let iv = val.into_int_value();
                    self.builder.build_int_neg(iv, "ir_neg").unwrap().into()
                } else if val.is_float_value() {
                    let fv = val.into_float_value();
                    self.builder.build_float_neg(fv, "ir_fneg").unwrap().into()
                } else { val }
            }
            lir::UnaryOp::Not => {
                if val.is_int_value() {
                    let iv = val.into_int_value();
                    self.builder.build_not(iv, "ir_not").unwrap().into()
                } else { val }
            }
        }
    }

    /// Make a closure struct {i32 rc=1, i32 _pad, fn_ptr, env_ptr} with optional captured env.
    fn make_closure_struct(&mut self, fn_ptr: BasicValueEnum<'ctx>, captures: &[BasicValueEnum<'ctx>]) -> BasicValueEnum<'ctx> {
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i64_ty = self.context.i64_type();
        let cls_size = i64_ty.const_int(32, false);
        let cls_mem = self.builder.build_call(self.rt_alloc, &[cls_size.into()], "ir_cls")
            .unwrap().try_as_basic_value().unwrap_basic().into_pointer_value();
        let cls_ty = self.closure_struct_type();
        let rc_f = self.builder.build_struct_gep(cls_ty, cls_mem, 0, "ir_rc").unwrap();
        self.builder.build_store(rc_f, self.context.i32_type().const_int(1, false)).unwrap();
        let fp_f = self.builder.build_struct_gep(cls_ty, cls_mem, 2, "ir_fp").unwrap();
        self.builder.build_store(fp_f, fn_ptr).unwrap();

        if captures.is_empty() {
            let ep_f = self.builder.build_struct_gep(cls_ty, cls_mem, 3, "ir_ep").unwrap();
            self.builder.build_store(ep_f, ptr_ty.const_null()).unwrap();
        } else {
            // Build an env struct.
            // Layout: {u64 size, cap0, cap1, ...}
            let n = captures.len();
            let env_size_bytes = 8u64 + (n as u64 * 8); // size header + 8 bytes per capture (ptr/i64)
            let env_size_val = i64_ty.const_int(env_size_bytes, false);
            let env_mem = self.builder.build_call(self.rt_alloc, &[env_size_val.into()], "ir_env")
                .unwrap().try_as_basic_value().unwrap_basic().into_pointer_value();
            // Write size at offset 0.
            self.builder.build_store(env_mem, env_size_val).unwrap();
            // Write captures at offsets 8, 16, ...
            for (i, &cap) in captures.iter().enumerate() {
                let offset = 8u64 + (i as u64 * 8);
                let offset_v = i64_ty.const_int(offset, false);
                let cap_gep = unsafe { self.builder.build_gep(
                    self.context.i8_type(),
                    env_mem,
                    &[offset_v],
                    &format!("ir_cap{}", i)
                ).unwrap() };
                self.builder.build_store(cap_gep, cap).unwrap();
            }
            let ep_f = self.builder.build_struct_gep(cls_ty, cls_mem, 3, "ir_ep").unwrap();
            self.builder.build_store(ep_f, env_mem).unwrap();
            // env_size at offset 24.
            let env_size_gep = unsafe { self.builder.build_gep(
                self.context.i8_type(),
                cls_mem,
                &[i64_ty.const_int(24, false)],
                "ir_env_sz"
            ).unwrap() };
            self.builder.build_store(env_size_gep, env_size_val).unwrap();
        }
        cls_mem.into()
    }


    fn compile_binary_op_values(
        &mut self,
        lv: BasicValueEnum<'ctx>,
        rv: BasicValueEnum<'ctx>,
        op: &BinOp,
        lty: &Type,
        rty: &Type,
        result_ty: &Type,
    ) -> BasicValueEnum<'ctx> {
        // Box the rhs to a TaggedVal* when comparing against a boxed (union) lhs: a concrete
        // rhs value must be boxed by its STATIC type. A raw `LinString*` (a string literal)
        // is a pointer but NOT a TaggedVal — passing it to lin_tagged_eq/_cmp would read its
        // bytes as a tag/payload and overflow. `box_value` is a no-op when rty is already a
        // union, so this is safe to apply whenever rty is concrete.
        let box_rhs = |s: &mut Self, v: BasicValueEnum<'ctx>| -> BasicValueEnum<'ctx> {
            if Self::is_union_type(rty) { v } else { s.box_value(v, rty) }
        };
        // Mixed int/float arithmetic (e.g. `5 + 3.0`): widen the integer operand to float
        // so both sides agree, and dispatch on the float type. The checker permits these
        // numeric combinations without inserting explicit Coerce nodes on both operands.
        if matches!(op, BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div
            | BinOp::Lt | BinOp::LtEq | BinOp::Gt | BinOp::GtEq | BinOp::Eq | BinOp::NotEq)
            && lv.is_int_value() != rv.is_int_value()
            && (lv.is_float_value() || rv.is_float_value())
            && (lv.is_int_value() || lv.is_float_value())
            && (rv.is_int_value() || rv.is_float_value())
        {
            let f64_ty = self.context.f64_type();
            let to_f = |s: &Self, v: BasicValueEnum<'ctx>| -> BasicValueEnum<'ctx> {
                if v.is_int_value() {
                    s.builder.build_signed_int_to_float(v.into_int_value(), f64_ty, "ir_i2f").unwrap().into()
                } else {
                    s.builder.build_float_cast(v.into_float_value(), f64_ty, "ir_fwiden").unwrap().into()
                }
            };
            let lf = to_f(self, lv);
            let rf = to_f(self, rv);
            return self.compile_binary_op_values(lf, rf, op, &Type::Float64, &Type::Float64, result_ty);
        }
        // Mismatched integer widths (e.g. Int64 `n` vs an Int32 literal `0`): sign-extend
        // the narrower operand to the wider so the ICmp/arith operands agree.
        if lv.is_int_value() && rv.is_int_value() {
            let lw = lv.into_int_value().get_type().get_bit_width();
            let rw = rv.into_int_value().get_type().get_bit_width();
            if lw != rw && lw > 1 && rw > 1 {
                // Extend the narrower operand to the wider width. Choose sign- vs zero-extend
                // per the SOURCE operand's signedness so an unsigned small int (e.g. UInt8
                // 250) widens to 250, not -6. Result type is the wider operand's static type.
                let wide_is_left = lw > rw;
                let wide = if wide_is_left { lv.into_int_value().get_type() } else { rv.into_int_value().get_type() };
                let wide_ty = if wide_is_left { lty } else { rty };
                let ext = |s: &Self, v: BasicValueEnum<'ctx>, src_ty: &Type| -> BasicValueEnum<'ctx> {
                    if src_ty.is_signed() {
                        s.builder.build_int_s_extend(v.into_int_value(), wide, "ir_sext").unwrap().into()
                    } else {
                        s.builder.build_int_z_extend(v.into_int_value(), wide, "ir_zext").unwrap().into()
                    }
                };
                let lext = if lw < wide.get_bit_width() { ext(self, lv, lty) } else { lv };
                let rext = if rw < wide.get_bit_width() { ext(self, rv, rty) } else { rv };
                return self.compile_binary_op_values(lext, rext, op, wide_ty, wide_ty, result_ty);
            }
        }
        // When operands are boxed (Json/union), use tagged runtime ops for equality and
        // ordering (which tolerate mixed/null payloads), and unbox to a concrete numeric
        // type for arithmetic. Mirrors the AST path's TypeVar handling in compile_binary_op.
        if Self::is_union_type(lty) && lv.is_pointer_value() {
            match op {
                BinOp::Eq | BinOp::NotEq => {
                    // lin_tagged_eq returns u8 (i8), not i1 — declare it as i8 and
                    // truncate, else the call reads garbage bits and compares as always-true.
                    let i8_ty = self.context.i8_type();
                    let eq_fn = self.get_or_declare_fn("lin_tagged_eq",
                        i8_ty.fn_type(
                            &[self.context.ptr_type(AddressSpace::default()).into(),
                              self.context.ptr_type(AddressSpace::default()).into()], false));
                    // Box the rhs to a TaggedVal* by its STATIC type. A concrete rhs (incl. a
                    // raw LinString* from a string literal) must be boxed; a union rhs is
                    // already a TaggedVal*. `x == 3` boxes 3 as int; `t == "pass"` boxes the
                    // string. (box_rhs is a no-op for union rty.)
                    let rv_tagged = box_rhs(self, rv);
                    let eq_u8 = self.builder.build_call(eq_fn, &[lv.into(), rv_tagged.into()], "ir_teq")
                        .unwrap().try_as_basic_value().unwrap_basic().into_int_value();
                    let eq = self.builder.build_int_truncate(eq_u8, self.context.bool_type(), "ir_teq_b").unwrap();
                    return if matches!(op, BinOp::NotEq) {
                        self.builder.build_not(eq, "ir_tne").unwrap().into()
                    } else { eq.into() };
                }
                BinOp::Lt | BinOp::LtEq | BinOp::Gt | BinOp::GtEq => {
                    // Boxed operands may be strings or numbers — use lin_tagged_cmp (returns
                    // -1/0/1) which dispatches on the runtime tag, then compare to 0.
                    let i32_ty = self.context.i32_type();
                    let ptr_t = self.context.ptr_type(AddressSpace::default());
                    let cmp_fn = self.get_or_declare_fn("lin_tagged_cmp",
                        i32_ty.fn_type(&[ptr_t.into(), ptr_t.into()], false));
                    let rv_tagged = box_rhs(self, rv);
                    let ord = self.builder.build_call(cmp_fn, &[lv.into(), rv_tagged.into()], "ir_tcmp")
                        .unwrap().try_as_basic_value().unwrap_basic().into_int_value();
                    let zero = i32_ty.const_zero();
                    let pred = match op {
                        BinOp::Lt => IntPredicate::SLT, BinOp::LtEq => IntPredicate::SLE,
                        BinOp::Gt => IntPredicate::SGT, _ => IntPredicate::SGE,
                    };
                    return self.builder.build_int_compare(pred, ord, zero, "ir_tcmp_b").unwrap().into();
                }
                BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod
                | BinOp::BAnd | BinOp::BOr | BinOp::BXor | BinOp::Shl | BinOp::Shr => {
                    // Bitwise/shift ops are integer-only (checker-enforced); a boxed union
                    // operand (e.g. a TypeVar reduce-lambda param) must be unboxed to the
                    // concrete integer type before the LLVM int op, same as arithmetic.
                    let lconc = self.unbox_tagged_val_to_type(lv, &Type::Int32);
                    let rconc = if rv.is_pointer_value() { self.unbox_tagged_val_to_type(rv, &Type::Int32) } else { rv };
                    let concrete = self.compile_binary_op_values(lconc, rconc, op, &Type::Int32, &Type::Int32, &Type::Int32);
                    // If the surrounding context expects a union/Json value, re-box the
                    // concrete result (heap) so it can be stored/returned uniformly.
                    return if Self::is_union_type(result_ty) {
                        self.box_value(concrete, &Type::Int32)
                    } else {
                        concrete
                    };
                }
                _ => {}
            }
        }
        match op {
            BinOp::Add => self.compile_add(lv, rv, lty, lty, result_ty),
            BinOp::Sub => self.compile_arith_op(lv, rv, lty, "sub"),
            BinOp::Mul => self.compile_arith_op(lv, rv, lty, "mul"),
            BinOp::Div => self.compile_div(lv, rv, lty),
            BinOp::Mod => self.compile_mod(lv, rv, lty),
            BinOp::Eq => self.compile_eq(lv, rv, lty, false),
            BinOp::NotEq => self.compile_eq(lv, rv, lty, true),
            BinOp::Lt => self.compile_cmp(lv, rv, lty, IntPredicate::SLT, IntPredicate::ULT, FloatPredicate::OLT),
            BinOp::LtEq => self.compile_cmp(lv, rv, lty, IntPredicate::SLE, IntPredicate::ULE, FloatPredicate::OLE),
            BinOp::Gt => self.compile_cmp(lv, rv, lty, IntPredicate::SGT, IntPredicate::UGT, FloatPredicate::OGT),
            BinOp::GtEq => self.compile_cmp(lv, rv, lty, IntPredicate::SGE, IntPredicate::UGE, FloatPredicate::OGE),
            BinOp::And => self.builder.build_and(lv.into_int_value(), rv.into_int_value(), "ir_and").unwrap().into(),
            BinOp::Or => self.builder.build_or(lv.into_int_value(), rv.into_int_value(), "ir_or").unwrap().into(),
            // Bitwise integer operators (§35.2). Operands are integers (checker-enforced)
            // and widths have been reconciled above.
            BinOp::BAnd => self.builder.build_and(lv.into_int_value(), rv.into_int_value(), "ir_band").unwrap().into(),
            BinOp::BOr => self.builder.build_or(lv.into_int_value(), rv.into_int_value(), "ir_bor").unwrap().into(),
            BinOp::BXor => self.builder.build_xor(lv.into_int_value(), rv.into_int_value(), "ir_bxor").unwrap().into(),
            BinOp::Shl => self.builder.build_left_shift(lv.into_int_value(), rv.into_int_value(), "ir_shl").unwrap().into(),
            // `>>` is arithmetic for signed types and logical for unsigned types.
            BinOp::Shr => {
                let sign_extend = lty.is_signed();
                self.builder.build_right_shift(lv.into_int_value(), rv.into_int_value(), sign_extend, "ir_shr").unwrap().into()
            }
        }
    }

    /// Unbox a tagged union value to a concrete type.
    fn unbox_tagged_val_to_type(&mut self, tagged: BasicValueEnum<'ctx>, ty: &Type) -> BasicValueEnum<'ctx> {
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        if !tagged.is_pointer_value() { return tagged; }
        let ptr = tagged.into_pointer_value();
        match ty {
            Type::Int32 | Type::UInt32 => {
                self.builder.build_call(self.rt_unbox_int32, &[ptr.into()], "ir_u32")
                    .unwrap().try_as_basic_value().unwrap_basic()
            }
            Type::Int64 | Type::UInt64 => {
                self.builder.build_call(self.rt_unbox_int64, &[ptr.into()], "ir_u64")
                    .unwrap().try_as_basic_value().unwrap_basic()
            }
            Type::Float64 | Type::Float32 => {
                self.builder.build_call(self.rt_unbox_float64, &[ptr.into()], "ir_uf64")
                    .unwrap().try_as_basic_value().unwrap_basic()
            }
            Type::Bool => {
                let i8v = self.builder.build_call(self.rt_unbox_bool, &[ptr.into()], "ir_ubool")
                    .unwrap().try_as_basic_value().unwrap_basic().into_int_value();
                self.builder.build_int_truncate_or_bit_cast(i8v, self.context.bool_type(), "ub_bool").unwrap().into()
            }
            Type::Str => {
                self.builder.build_call(self.rt_unbox_ptr, &[ptr.into()], "ir_ustr")
                    .unwrap().try_as_basic_value().unwrap_basic()
            }
            Type::Array(_) | Type::FixedArray(_) | Type::Object(_) | Type::Function { .. } => {
                self.builder.build_call(self.rt_unbox_ptr, &[ptr.into()], "ir_uptr")
                    .unwrap().try_as_basic_value().unwrap_basic()
            }
            Type::Null => ptr_ty.const_null().into(),
            _ => tagged, // pass through for union/unknown
        }
    }

    /// Return the i8 constant for the runtime tag of a type.
    fn type_tag_const(&self, ty: &Type) -> inkwell::values::IntValue<'ctx> {
        let i8_ty = self.context.i8_type();
        let tag: u64 = match ty {
            Type::Null => 0,
            Type::Bool => 1,
            Type::Int32 | Type::UInt32 => 2,
            Type::Int64 | Type::UInt64 => 3,
            Type::Float32 | Type::Float64 => 4,
            Type::Str => 6,
            Type::Object(_) => 7,
            Type::Array(_) | Type::FixedArray(_) => 8,
            Type::Function { .. } => 9,
            _ => 0xFF,
        };
        i8_ty.const_int(tag, false)
    }

    /// Compile a `toString` call on a typed value (used by LinIR intrinsic path).
    /// Stringify a value for the IR path. Delegates to the type-driven
    /// `value_to_string_simple` so that Str returns as-is, numerics use the right
    /// width, and tagged/Array/Object values use the correct runtime dispatch.
    /// `ty` MUST be the input value's type, not the (always-Str) result type.
    fn compile_to_string_value(&mut self, val: BasicValueEnum<'ctx>, ty: &Type) -> BasicValueEnum<'ctx> {
        self.value_to_string_simple(val, ty)
    }


}
