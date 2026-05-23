use inkwell::builder::Builder;
use inkwell::context::Context;
use inkwell::module::Module;
use inkwell::passes::PassBuilderOptions;
use inkwell::targets::{
    CodeModel, FileType, InitializationConfig, RelocMode, Target, TargetMachine,
};
use inkwell::types::{BasicMetadataTypeEnum, BasicType, BasicTypeEnum, StructType};
use inkwell::values::{
    BasicMetadataValueEnum, BasicValueEnum, FunctionValue, PointerValue,
};
use inkwell::{AddressSpace, IntPredicate, FloatPredicate, OptimizationLevel};
use std::collections::HashMap;
use std::path::Path;

use lin_check::typed_ir::*;
use lin_check::types::Type;
use lin_parse::ast::BinOp;

/// Tracks a slot: either a stack alloca (mutable) or an immutable SSA value.
#[derive(Clone, Debug)]
enum SlotStorage<'ctx> {
    /// Immutable val — just the SSA value.
    Value(BasicValueEnum<'ctx>),
    /// Mutable var — heap cell ptr (i8* to LinCell { refcount, value_tag, payload }).
    Alloca(PointerValue<'ctx>),
    /// A closure with environment: pointer to heap-allocated { fn_ptr: ptr, env_ptr: ptr }.
    Closure(PointerValue<'ctx>),
}

/// State carried while compiling a single Lin function.
struct FnCtx<'ctx, 'a> {
    slots: HashMap<usize, SlotStorage<'ctx>>,
    llvm_fn: FunctionValue<'ctx>,
    /// Pointer to the environment struct for this closure (if any).
    env_ptr: Option<PointerValue<'ctx>>,
    /// For TCO: if this function is being compiled with the loop-transform,
    /// this holds the entry block to branch back to and the phi slots.
    tco: Option<TcoState<'ctx, 'a>>,
}

struct TcoState<'ctx, 'a> {
    loop_block: inkwell::basic_block::BasicBlock<'ctx>,
    param_allocs: Vec<PointerValue<'ctx>>,
    _marker: std::marker::PhantomData<&'a ()>,
}

pub struct Codegen<'ctx> {
    context: &'ctx Context,
    module: Module<'ctx>,
    builder: Builder<'ctx>,
    // Runtime function declarations
    #[allow(dead_code)]
    rt_string_alloc: FunctionValue<'ctx>,
    rt_string_concat: FunctionValue<'ctx>,
    rt_string_from_bytes: FunctionValue<'ctx>,
    rt_string_length: FunctionValue<'ctx>,
    rt_string_eq: FunctionValue<'ctx>,
    rt_print: FunctionValue<'ctx>,
    rt_panic: FunctionValue<'ctx>,
    rt_array_alloc: FunctionValue<'ctx>,
    rt_array_push: FunctionValue<'ctx>,
    rt_array_get: FunctionValue<'ctx>,
    rt_array_length: FunctionValue<'ctx>,
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
    rt_object_has: FunctionValue<'ctx>,
    rt_object_eq: FunctionValue<'ctx>,
    rt_tagged_to_string: FunctionValue<'ctx>,
    // Cached LLVM types
    string_ptr_type: inkwell::types::PointerType<'ctx>,
    array_ptr_type: inkwell::types::PointerType<'ctx>,
    // Closure type cache: fn_sig -> { fn_ptr, env_ptr }
    #[allow(dead_code)]
    closure_types: HashMap<String, StructType<'ctx>>,
    // Named functions (for call resolution and TCO detection)
    named_fns: HashMap<String, FunctionValue<'ctx>>,
    // Intrinsic slot -> name map from type checker
    intrinsic_slots: HashMap<usize, String>,
    // Global function slots: slot -> FunctionValue (top-level named functions)
    global_fn_slots: HashMap<usize, FunctionValue<'ctx>>,
    // Counter for anonymous closures
    closure_count: usize,
    // Map from (module_path, export_name) -> FunctionValue for compiled imports
    imported_fns: HashMap<(String, String), FunctionValue<'ctx>>,
}

impl<'ctx> Codegen<'ctx> {
    pub fn new(context: &'ctx Context, module_name: &str) -> Self {
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

        let rt_string_alloc = module.add_function(
            "lin_string_alloc",
            string_ptr_type.fn_type(&[i32_type.into()], false),
            None,
        );
        let rt_string_concat = module.add_function(
            "lin_string_concat",
            string_ptr_type.fn_type(&[string_ptr_type.into(), string_ptr_type.into()], false),
            None,
        );
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
        // lin_array_length(arr: ptr) -> i64
        let rt_array_length = module.add_function(
            "lin_array_length",
            i64_type.fn_type(&[ptr_type.into()], false),
            None,
        );
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
        let rt_object_has = module.add_function("lin_object_has", i8_type.fn_type(&[ptr_type.into(), ptr_type.into()], false), None);
        // lin_object_eq(a: ptr, b: ptr) -> i8
        let rt_object_eq = module.add_function("lin_object_eq", i8_type.fn_type(&[ptr_type.into(), ptr_type.into()], false), None);

        Self {
            context,
            module,
            builder,
            rt_string_alloc,
            rt_string_concat,
            rt_string_from_bytes,
            rt_string_length,
            rt_string_eq,
            rt_print,
            rt_panic,
            rt_array_alloc,
            rt_array_push,
            rt_array_get,
            rt_array_length,
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
            rt_object_has,
            rt_object_eq,
            rt_tagged_to_string,
            string_ptr_type,
            array_ptr_type,
            closure_types: HashMap::new(),
            named_fns: HashMap::new(),
            intrinsic_slots: HashMap::new(),
            global_fn_slots: HashMap::new(),
            closure_count: 0,
            imported_fns: HashMap::new(),
        }
    }

    // -------------------------------------------------------------------------
    // Public entry points
    // -------------------------------------------------------------------------

    /// Register a pre-compiled imported module's exported functions.
    /// Call this before compile_module for each imported module.
    pub fn register_import(&mut self, path: &str, module: &TypedModule) {
        // Merge the imported module's intrinsic slot map so that calls to intrinsics
        // inside the imported module's function bodies resolve correctly.
        for (slot, name) in &module.intrinsics {
            self.intrinsic_slots.insert(*slot, name.clone());
        }

        // Collect the module-local slot→fn mapping so intra-module calls resolve.
        let mut module_slots: Vec<(usize, FunctionValue<'ctx>)> = Vec::new();
        for stmt in &module.statements {
            if let TypedStmt::Val {
                slot,
                value: TypedExpr::Function { name: Some(name), params, ret_type, .. },
                ..
            } = stmt {
                // Declare (or find) the function in the LLVM module.
                let llvm_fn = self.declare_function(name, params, ret_type);
                self.named_fns.insert(name.clone(), llvm_fn);
                self.imported_fns.insert((path.to_string(), name.clone()), llvm_fn);
                module_slots.push((*slot, llvm_fn));
            }
        }
        // Temporarily install the imported module's slot→fn mapping into global_fn_slots
        // so that intra-module calls (e.g. clamp calling max/min) resolve correctly.
        let mut displaced: Vec<(usize, Option<FunctionValue<'ctx>>)> = Vec::new();
        for (slot, llvm_fn) in &module_slots {
            let old = self.global_fn_slots.insert(*slot, *llvm_fn);
            displaced.push((*slot, old));
        }
        // Compile the bodies of imported functions.
        for stmt in &module.statements {
            if let TypedStmt::Val {
                value: TypedExpr::Function { name: Some(name), params, body, ret_type, captures, .. },
                ..
            } = stmt {
                if captures.is_empty() {
                    if let Some(&llvm_fn) = self.named_fns.get(name.as_str()) {
                        // Only compile if not already compiled (no basic blocks).
                        if llvm_fn.count_basic_blocks() == 0 {
                            self.compile_function_body(llvm_fn, params, body, ret_type, &[], name);
                        }
                    }
                }
            }
        }
        // Restore displaced global_fn_slots entries.
        for (slot, old) in displaced {
            if let Some(old_fn) = old {
                self.global_fn_slots.insert(slot, old_fn);
            } else {
                self.global_fn_slots.remove(&slot);
            }
        }
    }

    pub fn compile_module(&mut self, module: &TypedModule) {
        // Merge the main module's intrinsic slot map (register_import already added
        // intrinsic slots from imported modules; main module slots take precedence).
        for (slot, name) in &module.intrinsics {
            self.intrinsic_slots.insert(*slot, name.clone());
        }

        // Generate the top-level "main" function containing all module-level
        // statements, then emit it.
        let i32_type = self.context.i32_type();
        let main_fn_type = i32_type.fn_type(&[], false);
        let main_fn = self.module.add_function("main", main_fn_type, None);
        self.named_fns.insert("main".to_string(), main_fn);

        let entry_block = self.context.append_basic_block(main_fn, "entry");
        self.builder.position_at_end(entry_block);

        let mut fn_ctx = FnCtx {
            slots: HashMap::new(),
            llvm_fn: main_fn,
            env_ptr: None,
            tco: None,
        };

        // Pre-scan: forward-declare all top-level named functions so mutual
        // recursion works (matches ADR-015 interpreter behaviour).
        for stmt in &module.statements {
            if let TypedStmt::Val {
                slot,
                value: TypedExpr::Function { name: Some(name), params, ret_type, captures, .. },
                ..
            } = stmt
            {
                if captures.is_empty() {
                    // Pure function — emit as an LLVM function directly.
                    let llvm_fn = self.declare_function(name, params, ret_type);
                    self.named_fns.insert(name.clone(), llvm_fn);
                    // Store function pointer in slot as an immutable value.
                    let fn_ptr = llvm_fn.as_global_value().as_pointer_value();
                    fn_ctx.slots.insert(*slot, SlotStorage::Value(fn_ptr.into()));
                    // Also register in global_fn_slots so sibling functions can call each other.
                    self.global_fn_slots.insert(*slot, llvm_fn);
                }
            }
        }

        for stmt in &module.statements {
            self.compile_stmt(stmt, &mut fn_ctx);
        }

        // main returns 0
        self.builder
            .build_return(Some(&i32_type.const_int(0, false)))
            .unwrap();

        // Now compile the bodies of all top-level functions.
        // They were forward-declared above; we complete them here.
        for stmt in &module.statements {
            if let TypedStmt::Val {
                value: TypedExpr::Function { name: Some(name), params, body, ret_type, captures, .. },
                ..
            } = stmt
            {
                if captures.is_empty() {
                    let llvm_fn = *self.named_fns.get(name.as_str()).unwrap();
                    self.compile_function_body(llvm_fn, params, body, ret_type, &[], name);
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
                // Null is represented as an i8 (0 = null tag)
                self.context.i8_type().into()
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
        }
    }

    fn llvm_param_type(&self, ty: &Type) -> BasicMetadataTypeEnum<'ctx> {
        self.llvm_type(ty).into()
    }

    /// True if `ty` is a union or TypeVar (i.e., needs tagged representation).
    fn is_union_type(ty: &Type) -> bool {
        matches!(ty, Type::Union(_) | Type::TypeVar(_))
    }

    /// Box a value of known concrete type `val_ty` into a tagged union pointer.
    fn box_value(&mut self, val: BasicValueEnum<'ctx>, val_ty: &Type) -> BasicValueEnum<'ctx> {
        let ptr = match val_ty {
            Type::Null => self.builder.build_call(self.rt_box_null, &[], "boxnull").unwrap()
                .try_as_basic_value().unwrap_basic(),
            Type::Bool => {
                let i8v = if val.is_int_value() {
                    self.builder.build_int_truncate(val.into_int_value(), self.context.i8_type(), "btoi8").unwrap().into()
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
            Type::Array(_) | Type::FixedArray(_) => {
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
            Type::TypeVar(_) => val,
            _ => val,
        };
        ptr
    }

    /// Unbox a tagged union pointer to the concrete type `target_ty`.
    fn unbox_value(&mut self, ptr: BasicValueEnum<'ctx>, target_ty: &Type) -> BasicValueEnum<'ctx> {
        let ptr_val = ptr.into_pointer_value();
        match target_ty {
            Type::Null => self.context.i8_type().const_zero().into(),
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
            Type::Array(_) | Type::FixedArray(_) => 8,
            Type::Function { .. } => 9,
            _ => 255,
        }
    }

    // -------------------------------------------------------------------------
    // Function declaration (without body — used for forward refs)
    // -------------------------------------------------------------------------

    fn declare_function(
        &self,
        name: &str,
        params: &[TypedParam],
        ret_type: &Type,
    ) -> FunctionValue<'ctx> {
        let param_types: Vec<BasicMetadataTypeEnum> = params
            .iter()
            .map(|p| self.llvm_param_type(&p.ty))
            .collect();

        let fn_type = match ret_type {
            Type::Never => {
                let vt = self.context.void_type();
                vt.fn_type(&param_types, false)
            }
            _ => {
                let ret_llvm = self.llvm_type(ret_type);
                ret_llvm.fn_type(&param_types, false)
            }
        };

        if let Some(existing) = self.module.get_function(name) {
            existing
        } else {
            self.module.add_function(name, fn_type, None)
        }
    }

    // -------------------------------------------------------------------------
    // Function body compilation
    // -------------------------------------------------------------------------

    fn compile_function_body(
        &mut self,
        llvm_fn: FunctionValue<'ctx>,
        params: &[TypedParam],
        body: &TypedExpr,
        ret_type: &Type,
        captures: &[Capture],
        fn_name: &str,
    ) {
        // Check if this function can use the TCO loop transform.
        // Condition: name is known, and fn_name matches the function being compiled.
        let use_tco = self.named_fns.contains_key(fn_name);

        let entry_block = self.context.append_basic_block(llvm_fn, "entry");
        self.builder.position_at_end(entry_block);

        let mut fn_ctx = FnCtx {
            slots: HashMap::new(),
            llvm_fn,
            env_ptr: None,
            tco: None,
        };

        // If function captures variables, the first parameter is env_ptr.
        let param_offset = if !captures.is_empty() { 1 } else { 0 };

        if !captures.is_empty() {
            let env_ptr = llvm_fn
                .get_nth_param(0)
                .unwrap()
                .into_pointer_value();
            fn_ctx.env_ptr = Some(env_ptr);

            // Build the env struct type and load each captured field into slots.
            let cap_types: Vec<inkwell::types::BasicTypeEnum> = captures
                .iter()
                .map(|c| {
                    if c.is_mutable {
                        self.context.ptr_type(AddressSpace::default()).into()
                    } else {
                        self.llvm_type(&c.ty)
                    }
                })
                .collect();
            let env_struct_type = self.context.struct_type(&cap_types, false);

            for (i, cap) in captures.iter().enumerate() {
                let field_ptr = self.builder
                    .build_struct_gep(env_struct_type, env_ptr, i as u32, &format!("cap_{}_ptr", cap.name))
                    .unwrap();
                if cap.is_mutable {
                    // Mutable capture: the env holds a pointer to the LinCell, forward it directly.
                    let cell_ptr = self.builder
                        .build_load(self.context.ptr_type(AddressSpace::default()), field_ptr, &format!("cap_{}", cap.name))
                        .unwrap()
                        .into_pointer_value();
                    fn_ctx.slots.insert(cap.outer_slot, SlotStorage::Alloca(cell_ptr));
                } else {
                    // Immutable capture: load the value directly.
                    let val = self.builder
                        .build_load(self.llvm_type(&cap.ty), field_ptr, &format!("cap_{}", cap.name))
                        .unwrap();
                    fn_ctx.slots.insert(cap.outer_slot, SlotStorage::Value(val));
                }
            }
        }

        if use_tco && !captures.is_empty() {
            // TCO loop transform: allocate a slot for each param, then branch
            // to a loop header. Tail recursive calls store new values into the
            // slots and branch back to the loop header.
            let loop_block = self.context.append_basic_block(llvm_fn, "tco_loop");

            let mut param_allocs = Vec::new();
            for (i, param) in params.iter().enumerate() {
                let llvm_ty = self.llvm_type(&param.ty);
                let alloc = self.builder.build_alloca(llvm_ty, &param.name).unwrap();
                let val = llvm_fn.get_nth_param((i + param_offset) as u32).unwrap();
                self.builder.build_store(alloc, val).unwrap();
                param_allocs.push(alloc);
                fn_ctx.slots.insert(param.slot, SlotStorage::Alloca(alloc));
            }

            self.builder.build_unconditional_branch(loop_block).unwrap();
            self.builder.position_at_end(loop_block);

            fn_ctx.tco = Some(TcoState {
                loop_block,
                param_allocs,
                _marker: std::marker::PhantomData,
            });
        } else {
            // No TCO: bind params directly as immutable SSA values.
            for (i, param) in params.iter().enumerate() {
                let val = llvm_fn.get_nth_param((i + param_offset) as u32).unwrap();
                fn_ctx.slots.insert(param.slot, SlotStorage::Value(val));
            }
        }

        // Non-TCO pure functions: still use alloca-based TCO for direct recursion.
        if use_tco && captures.is_empty() {
            let loop_block = self.context.append_basic_block(llvm_fn, "tco_loop");
            let mut param_allocs = Vec::new();
            for (i, param) in params.iter().enumerate() {
                let llvm_ty = self.llvm_type(&param.ty);
                let alloc = self.builder.build_alloca(llvm_ty, &param.name).unwrap();
                let val = llvm_fn.get_nth_param(i as u32).unwrap();
                self.builder.build_store(alloc, val).unwrap();
                param_allocs.push(alloc);
                fn_ctx.slots.insert(param.slot, SlotStorage::Alloca(alloc));
            }
            self.builder.build_unconditional_branch(loop_block).unwrap();
            self.builder.position_at_end(loop_block);
            fn_ctx.tco = Some(TcoState {
                loop_block,
                param_allocs,
                _marker: std::marker::PhantomData,
            });
        }

        let result = self.compile_expr(body, &mut fn_ctx);

        match ret_type {
            Type::Never => {
                self.builder.build_unreachable().unwrap();
            }
            _ => {
                // If function returns Union/TypeVar, box concrete result values.
                // Note: Union-typed body values may still be unboxed (e.g. LinObject*) if they
                // came from a branch expression — box them too.
                let body_ty = body.ty();
                let needs_boxing = Self::is_union_type(ret_type)
                    && !matches!(body_ty, Type::TypeVar(_) | Type::Null);
                let final_result = if needs_boxing {
                    self.box_value(result, &body_ty)
                } else {
                    result
                };
                self.builder.build_return(Some(&final_result)).unwrap();
            }
        }
    }

    // -------------------------------------------------------------------------
    // Statement compilation
    // -------------------------------------------------------------------------

    fn compile_stmt(&mut self, stmt: &TypedStmt, fn_ctx: &mut FnCtx<'ctx, '_>) {
        match stmt {
            TypedStmt::Val { slot, value, ty, .. } => {
                match value {
                    // Top-level named functions are compiled separately.
                    TypedExpr::Function { name: Some(_), captures, .. } if captures.is_empty() => {
                        // Already forward-declared and will be compiled after main.
                        // The slot was set in the pre-scan.
                    }
                    TypedExpr::Function { captures, .. } if !captures.is_empty() => {
                        // Closure with captures: result is a pointer to heap {fn_ptr, env_ptr}.
                        let compiled = self.compile_expr(value, fn_ctx);
                        fn_ctx.slots.insert(*slot, SlotStorage::Closure(compiled.into_pointer_value()));
                    }
                    _ => {
                        let compiled = self.compile_expr(value, fn_ctx);
                        // If the type is a function, it may be a closure struct returned from a call.
                        // Check whether the compiled value is a pointer with function type.
                        let storage = if matches!(ty, Type::Function { .. })
                            && matches!(compiled, BasicValueEnum::PointerValue(_))
                            && !self.global_fn_slots.contains_key(slot)
                        {
                            SlotStorage::Closure(compiled.into_pointer_value())
                        } else {
                            SlotStorage::Value(compiled)
                        };
                        fn_ctx.slots.insert(*slot, storage);
                    }
                }
            }
            TypedStmt::Var { slot, value, ty, .. } => {
                let compiled = self.compile_expr(value, fn_ctx);
                let llvm_ty = self.llvm_type(ty);
                let alloc = self
                    .builder
                    .build_alloca(llvm_ty, &format!("var_{}", slot))
                    .unwrap();
                self.builder.build_store(alloc, compiled).unwrap();
                fn_ctx.slots.insert(*slot, SlotStorage::Alloca(alloc));
            }
            TypedStmt::Destructure { obj_slot, value, obj_ty, fields, .. } => {
                let compiled = self.compile_expr(value, fn_ctx);
                fn_ctx.slots.insert(*obj_slot, SlotStorage::Value(compiled));
                // All objects are now LinObject* — use lin_object_get for field binding.
                if compiled.is_pointer_value() {
                    let obj_ptr = compiled.into_pointer_value();
                    for (key, slot, field_ty) in fields {
                        let key_str = self.compile_string_lit(key).into_pointer_value();
                        let entry_ptr = self.builder
                            .build_call(self.rt_object_get, &[obj_ptr.into(), key_str.into()], "destr_p")
                            .unwrap()
                            .try_as_basic_value().unwrap_basic().into_pointer_value();
                        let val = self.load_tagged_val_payload(entry_ptr, field_ty, obj_ty);
                        fn_ctx.slots.insert(*slot, SlotStorage::Value(val));
                    }
                } else {
                    for (_, slot, field_ty) in fields {
                        fn_ctx.slots.insert(*slot, SlotStorage::Value(self.llvm_type(field_ty).const_zero()));
                    }
                }
            }
            TypedStmt::Import { path, bindings, .. } => {
                // Store pre-compiled imported function pointers into the binding slots.
                // Use the MAIN module's slot numbers (binding.slot), not the imported module's.
                let ptr_ty = self.context.ptr_type(AddressSpace::default());
                for binding in bindings {
                    let key = (path.clone(), binding.name.clone());
                    if let Some(&llvm_fn) = self.imported_fns.get(&key) {
                        let fn_ptr = llvm_fn.as_global_value().as_pointer_value();
                        fn_ctx.slots.insert(binding.slot, SlotStorage::Value(fn_ptr.into()));
                        // Register in global_fn_slots with the MAIN module's slot number.
                        self.global_fn_slots.insert(binding.slot, llvm_fn);
                    } else {
                        // Check if it's a known intrinsic by name (e.g. `print` re-exported from std/io).
                        let known_intrinsics = ["print", "toString", "length", "push", "concat",
                            "keys", "values", "for", "iter", "range", "map", "filter", "reduce"];
                        if known_intrinsics.contains(&binding.name.as_str()) {
                            self.intrinsic_slots.insert(binding.slot, binding.name.clone());
                        } else {
                            // Not found — store null pointer as fallback.
                            fn_ctx.slots.insert(binding.slot, SlotStorage::Value(ptr_ty.const_null().into()));
                        }
                    }
                }
            }
            TypedStmt::Expr(expr) => {
                self.compile_expr(expr, fn_ctx);
            }
        }
    }

    // -------------------------------------------------------------------------
    // Expression compilation
    // -------------------------------------------------------------------------

    fn compile_expr(
        &mut self,
        expr: &TypedExpr,
        fn_ctx: &mut FnCtx<'ctx, '_>,
    ) -> BasicValueEnum<'ctx> {
        match expr {
            TypedExpr::IntLit(v, ty, _) => self.compile_int_lit(*v, ty),
            TypedExpr::FloatLit(v, ty, _) => self.compile_float_lit(*v, ty),
            TypedExpr::BoolLit(b, _) => self
                .context
                .bool_type()
                .const_int(if *b { 1 } else { 0 }, false)
                .into(),
            TypedExpr::NullLit(_) => self.context.i8_type().const_zero().into(),
            TypedExpr::StringLit(s, _) => self.compile_string_lit(s),

            TypedExpr::LocalGet { slot, ty, .. } => self.compile_local_get(*slot, ty, fn_ctx),
            TypedExpr::LocalSet { slot, value, ty, .. } => {
                self.compile_local_set(*slot, value, ty, fn_ctx)
            }

            TypedExpr::BinaryOp { left, op, right, result_type, .. } => {
                self.compile_binary_op(left, *op, right, result_type, fn_ctx)
            }

            TypedExpr::Coerce { expr, from, to, .. } => {
                self.compile_coerce(expr, from, to, fn_ctx)
            }

            TypedExpr::Call { func, args, result_type, is_tail, .. } => {
                self.compile_call(func, args, result_type, *is_tail, fn_ctx)
            }

            TypedExpr::If { cond, then_br, else_br, result_type, .. } => {
                self.compile_if(cond, then_br, else_br, result_type, fn_ctx)
            }

            TypedExpr::Block { stmts, expr, .. } => {
                for s in stmts {
                    self.compile_stmt(s, fn_ctx);
                }
                self.compile_expr(expr, fn_ctx)
            }

            TypedExpr::Function { name, params, body, ret_type, captures, .. } => {
                self.compile_closure(name.as_deref(), params, body, ret_type, captures, fn_ctx)
            }

            TypedExpr::StringInterp { parts, .. } => self.compile_string_interp(parts, fn_ctx),

            TypedExpr::MakeArray { elements, ty, .. } => {
                self.compile_make_array(elements, ty, fn_ctx)
            }

            TypedExpr::MakeObject { fields, .. } => {
                self.compile_make_object(fields, fn_ctx)
            }

            TypedExpr::FieldGet { object, field, result_type, .. } => {
                self.compile_field_get(object, field, result_type, fn_ctx)
            }

            TypedExpr::Index { object, key, result_type, .. } => {
                self.compile_index(object, key, result_type, fn_ctx)
            }

            TypedExpr::Match { scrutinee, arms, result_type, .. } => {
                self.compile_match(scrutinee, arms, result_type, fn_ctx)
            }

            TypedExpr::Is { expr, pattern, .. } => self.compile_is_check(expr, pattern, fn_ctx),
            TypedExpr::Has { expr, pattern, .. } => self.compile_has_check(expr, pattern, fn_ctx),
        }
    }

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

    fn compile_local_get(
        &self,
        slot: usize,
        ty: &Type,
        fn_ctx: &FnCtx<'ctx, '_>,
    ) -> BasicValueEnum<'ctx> {
        match fn_ctx.slots.get(&slot) {
            Some(SlotStorage::Value(v)) => *v,
            Some(SlotStorage::Alloca(ptr)) => {
                let llvm_ty = self.llvm_type(ty);
                self.builder
                    .build_load(llvm_ty, *ptr, &format!("load_{}", slot))
                    .unwrap()
            }
            Some(SlotStorage::Closure(ptr)) => {
                // Closure pointer — return as-is (caller will unpack fn_ptr+env_ptr at call site).
                (*ptr).into()
            }
            None => {
                // Slot not found — this is a type checker bug. Return a poison value.
                self.llvm_type(ty).const_zero()
            }
        }
    }

    fn compile_local_set(
        &mut self,
        slot: usize,
        value: &TypedExpr,
        _ty: &Type,
        fn_ctx: &mut FnCtx<'ctx, '_>,
    ) -> BasicValueEnum<'ctx> {
        let compiled = self.compile_expr(value, fn_ctx);
        match fn_ctx.slots.get(&slot) {
            Some(SlotStorage::Alloca(ptr)) => {
                let ptr = *ptr;
                self.builder.build_store(ptr, compiled).unwrap();
            }
            _ => {
                // Var not found as alloca — shouldn't happen after type checking.
            }
        }
        compiled
    }

    // -------------------------------------------------------------------------
    // Binary operators
    // -------------------------------------------------------------------------

    fn compile_binary_op(
        &mut self,
        left: &TypedExpr,
        op: BinOp,
        right: &TypedExpr,
        _result_type: &Type,
        fn_ctx: &mut FnCtx<'ctx, '_>,
    ) -> BasicValueEnum<'ctx> {
        let lv_raw = self.compile_expr(left, fn_ctx);
        let rv_raw = self.compile_expr(right, fn_ctx);
        let lty_orig = left.ty();
        let rty_orig = right.ty();

        // Coerce TypeVar operands to the concrete type of the other operand.
        // When both are TypeVar, use _result_type as the coercion target.
        let (lv, rv, lty, rty) = if matches!(lty_orig, Type::TypeVar(_)) && matches!(rty_orig, Type::TypeVar(_)) {
            // Both TypeVar: coerce both to result_type (or Int32 as fallback).
            let target = if !matches!(_result_type, Type::TypeVar(_)) {
                _result_type.clone()
            } else {
                Type::Int32
            };
            let lv_c = self.coerce_typevar(lv_raw, &lty_orig, &target);
            let rv_c = self.coerce_typevar(rv_raw, &rty_orig, &target);
            (lv_c, rv_c, target.clone(), target)
        } else if matches!(lty_orig, Type::TypeVar(_)) && !matches!(rty_orig, Type::TypeVar(_)) {
            let lv_c = self.coerce_typevar(lv_raw, &lty_orig, &rty_orig);
            (lv_c, rv_raw, rty_orig.clone(), rty_orig)
        } else if matches!(rty_orig, Type::TypeVar(_)) && !matches!(lty_orig, Type::TypeVar(_)) {
            let rv_c = self.coerce_typevar(rv_raw, &rty_orig, &lty_orig);
            (lv_raw, rv_c, lty_orig.clone(), lty_orig)
        } else {
            (lv_raw, rv_raw, lty_orig.clone(), rty_orig)
        };

        match op {
            BinOp::Add => self.compile_add(lv, rv, &lty, &rty, _result_type),
            BinOp::Sub => self.compile_arith_op(lv, rv, &lty, "sub"),
            BinOp::Mul => self.compile_arith_op(lv, rv, &lty, "mul"),
            BinOp::Div => self.compile_div(lv, rv, &lty),
            BinOp::Mod => self.compile_mod(lv, rv, &lty),
            BinOp::Eq => self.compile_eq(lv, rv, &lty, false),
            BinOp::NotEq => self.compile_eq(lv, rv, &lty, true),
            BinOp::Lt => self.compile_cmp(lv, rv, &lty, IntPredicate::SLT, IntPredicate::ULT, FloatPredicate::OLT),
            BinOp::LtEq => self.compile_cmp(lv, rv, &lty, IntPredicate::SLE, IntPredicate::ULE, FloatPredicate::OLE),
            BinOp::Gt => self.compile_cmp(lv, rv, &lty, IntPredicate::SGT, IntPredicate::UGT, FloatPredicate::OGT),
            BinOp::GtEq => self.compile_cmp(lv, rv, &lty, IntPredicate::SGE, IntPredicate::UGE, FloatPredicate::OGE),
            BinOp::And => self
                .builder
                .build_and(lv.into_int_value(), rv.into_int_value(), "and")
                .unwrap()
                .into(),
            BinOp::Or => self
                .builder
                .build_or(lv.into_int_value(), rv.into_int_value(), "or")
                .unwrap()
                .into(),
        }
    }

    fn compile_add(
        &mut self,
        lv: BasicValueEnum<'ctx>,
        rv: BasicValueEnum<'ctx>,
        lty: &Type,
        rty: &Type,
        _result_type: &Type,
    ) -> BasicValueEnum<'ctx> {
        if *lty == Type::Str || *rty == Type::Str {
            // String concatenation: convert each operand to LinString* if needed.
            let lv_str = if matches!(lty, Type::TypeVar(_)) && lv.is_pointer_value() {
                self.builder
                    .build_call(self.rt_tagged_to_string, &[lv.into()], "lv_ts")
                    .unwrap()
                    .try_as_basic_value()
                    .unwrap_basic()
            } else {
                self.value_to_string_simple(lv, lty)
            };
            let rv_str = if matches!(rty, Type::TypeVar(_)) && rv.is_pointer_value() {
                self.builder
                    .build_call(self.rt_tagged_to_string, &[rv.into()], "rv_ts")
                    .unwrap()
                    .try_as_basic_value()
                    .unwrap_basic()
            } else {
                self.value_to_string_simple(rv, rty)
            };
            self.builder
                .build_call(
                    self.rt_string_concat,
                    &[lv_str.into(), rv_str.into()],
                    "strcat",
                )
                .unwrap()
                .try_as_basic_value()
                .unwrap_basic()
        } else if lty.is_float() {
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
        } else if ty.is_signed() {
            self.builder.build_int_signed_div(lv.into_int_value(), rv.into_int_value(), "sdiv").unwrap().into()
        } else {
            self.builder.build_int_unsigned_div(lv.into_int_value(), rv.into_int_value(), "udiv").unwrap().into()
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
        } else if ty.is_signed() {
            self.builder.build_int_signed_rem(lv.into_int_value(), rv.into_int_value(), "srem").unwrap().into()
        } else {
            self.builder.build_int_unsigned_rem(lv.into_int_value(), rv.into_int_value(), "urem").unwrap().into()
        }
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
        } else if ty.is_float() {
            self.builder
                .build_float_compare(FloatPredicate::OEQ, lv.into_float_value(), rv.into_float_value(), "feq")
                .unwrap()
        } else if lv.is_pointer_value() || rv.is_pointer_value() {
            // Pointer comparison (arrays, closures) — compare addresses.
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

    fn compile_coerce(
        &mut self,
        expr: &TypedExpr,
        from: &Type,
        to: &Type,
        fn_ctx: &mut FnCtx<'ctx, '_>,
    ) -> BasicValueEnum<'ctx> {
        let val = self.compile_expr(expr, fn_ctx);

        // Float <-> Float
        if from.is_float() && to.is_float() {
            let from_bits = from.bit_width().unwrap();
            let to_bits = to.bit_width().unwrap();
            return if from_bits < to_bits {
                self.builder.build_float_ext(val.into_float_value(), self.context.f64_type(), "fext").unwrap().into()
            } else {
                self.builder.build_float_trunc(val.into_float_value(), self.context.f32_type(), "ftrunc").unwrap().into()
            };
        }

        // Int -> Float
        if from.is_integer() && to.is_float() {
            let fty = self.llvm_type(to).into_float_type();
            return if from.is_signed() {
                self.builder.build_signed_int_to_float(val.into_int_value(), fty, "sitof").unwrap().into()
            } else {
                self.builder.build_unsigned_int_to_float(val.into_int_value(), fty, "uitof").unwrap().into()
            };
        }

        // Float -> Int
        if from.is_float() && to.is_integer() {
            let ity = self.llvm_type(to).into_int_type();
            return if to.is_signed() {
                self.builder.build_float_to_signed_int(val.into_float_value(), ity, "ftosi").unwrap().into()
            } else {
                self.builder.build_float_to_unsigned_int(val.into_float_value(), ity, "ftoui").unwrap().into()
            };
        }

        // Int -> Int
        if from.is_integer() && to.is_integer() {
            let from_bits = from.bit_width().unwrap();
            let to_bits = to.bit_width().unwrap();
            let ity = self.llvm_type(to).into_int_type();
            return if from_bits < to_bits {
                if from.is_signed() {
                    self.builder.build_int_s_extend(val.into_int_value(), ity, "sext").unwrap().into()
                } else {
                    self.builder.build_int_z_extend(val.into_int_value(), ity, "zext").unwrap().into()
                }
            } else {
                self.builder.build_int_truncate(val.into_int_value(), ity, "trunc").unwrap().into()
            };
        }

        // No coercion needed
        val
    }

    // -------------------------------------------------------------------------
    // Function calls
    // -------------------------------------------------------------------------

    /// Compile an argument, boxing it if the expected parameter type is Union/TypeVar.
    fn compile_arg_for_param(&mut self, arg: &TypedExpr, param_ty: &Type, fn_ctx: &mut FnCtx<'ctx, '_>) -> BasicValueEnum<'ctx> {
        let val = self.compile_expr(arg, fn_ctx);
        if Self::is_union_type(param_ty) && !Self::is_union_type(&arg.ty()) {
            self.box_value(val, &arg.ty())
        } else {
            val
        }
    }

    fn compile_call(
        &mut self,
        func: &TypedExpr,
        args: &[TypedExpr],
        result_type: &Type,
        is_tail: bool,
        fn_ctx: &mut FnCtx<'ctx, '_>,
    ) -> BasicValueEnum<'ctx> {
        // Check for direct tail self-call (TCO loop transform).
        if is_tail {
            if let TypedExpr::LocalGet { .. } = func {
                // Extract TCO state without holding a borrow across the args compilation.
                let tco_info = fn_ctx.tco.as_ref().map(|tco| {
                    (tco.loop_block, tco.param_allocs.clone(), tco.param_allocs.len())
                });
                if let Some((loop_block, param_allocs, param_count)) = tco_info {
                    if args.len() == param_count {
                        // Compile all args before storing (evaluates old values, not new).
                        let compiled_args: Vec<_> = args
                            .iter()
                            .map(|a| self.compile_expr(a, fn_ctx))
                            .collect();
                        for (alloc, val) in param_allocs.iter().zip(compiled_args.iter()) {
                            self.builder.build_store(*alloc, *val).unwrap();
                        }
                        self.builder.build_unconditional_branch(loop_block).unwrap();
                        // Unreachable continuation block to keep IR valid.
                        let post_block = self.context.append_basic_block(fn_ctx.llvm_fn, "tco_post");
                        self.builder.position_at_end(post_block);
                        return self.llvm_type(result_type).const_zero();
                    }
                }
            }
        }

        // Compile the function value, then call it.
        match func {
            TypedExpr::LocalGet { slot, .. } => {
                // Check if this is an intrinsic call (e.g. print, toString, length).
                let intrinsic_name = self.intrinsic_slots.get(slot).cloned();
                if let Some(name) = intrinsic_name {
                    return self.compile_intrinsic_call(&name, args, result_type, fn_ctx);
                }

                // Try to resolve to a global named function (including sibling top-level fns).
                if let Some(llvm_fn) = self.global_fn_slots.get(slot).copied() {
                    // Partial application: fewer args than the function has params.
                    let total_params = llvm_fn.count_params() as usize;
                    if args.len() < total_params {
                        if let Type::Function { params: remaining_params, ret: final_ret } = result_type {
                            return self.build_partial_application(llvm_fn, args, remaining_params, final_ret, fn_ctx);
                        }
                    }
                    // Get the Lin param types from the function's type signature to decide boxing.
                    let lin_param_types: Vec<Type> = if let Type::Function { params, .. } = func.ty() {
                        params
                    } else {
                        vec![]
                    };
                    let compiled_args: Vec<BasicMetadataValueEnum> = args
                        .iter()
                        .enumerate()
                        .map(|(i, a)| {
                            let val = self.compile_expr(a, fn_ctx);
                            let param_ty = lin_param_types.get(i).cloned().unwrap_or_else(|| a.ty());
                            // Box only if the Lin parameter type is a Union/TypeVar (needs tagged representation).
                            if Self::is_union_type(&param_ty) && !Self::is_union_type(&a.ty()) {
                                self.box_value(val, &a.ty()).into()
                            } else {
                                val.into()
                            }
                        })
                        .collect();
                    let call = self.builder.build_call(llvm_fn, &compiled_args, "call").unwrap();
                    return call.try_as_basic_value().basic().unwrap_or_else(|| {
                        self.llvm_type(result_type).const_zero()
                    });
                }

                // Check if this is a closure (has env_ptr) — unpack and call.
                if let Some(SlotStorage::Closure(closure_ptr)) = fn_ctx.slots.get(slot).cloned() {
                    return self.build_closure_call(closure_ptr, args, result_type, fn_ctx);
                }

                // Try to resolve to a known LLVM function from fn_ctx slot.
                if let Some(SlotStorage::Value(fn_val)) = fn_ctx.slots.get(slot) {
                    if let BasicValueEnum::PointerValue(ptr) = fn_val {
                        let ptr = *ptr;
                        // Get param types from func type for determining expected LLVM arg types.
                        let fn_param_types: Vec<Type> = if let Type::Function { params, .. } = func.ty() {
                            params
                        } else {
                            vec![]
                        };
                        let compiled_args: Vec<BasicMetadataValueEnum> = args
                            .iter()
                            .enumerate()
                            .map(|(i, a)| {
                                let param_ty = fn_param_types.get(i).cloned().unwrap_or_else(|| a.ty());
                                let val = self.compile_expr(a, fn_ctx);
                                // Box if function expects Union/TypeVar and arg is concrete type.
                                if Self::is_union_type(&param_ty) && !Self::is_union_type(&a.ty()) {
                                    self.box_value(val, &a.ty()).into()
                                } else {
                                    val.into()
                                }
                            })
                            .collect();

                        // Determine the function type from the return type and expected param types.
                        let ret_llvm = self.llvm_type(result_type);
                        let param_meta_types: Vec<BasicMetadataTypeEnum> = fn_param_types
                            .iter()
                            .map(|pt| self.llvm_type(pt).into())
                            .chain(args.iter().skip(fn_param_types.len()).map(|a| self.llvm_type(&a.ty()).into()))
                            .collect();
                        let fn_ty = ret_llvm.fn_type(&param_meta_types, false);

                        let call = self
                            .builder
                            .build_indirect_call(fn_ty, ptr, &compiled_args, "call")
                            .unwrap();

                        return call.try_as_basic_value().basic().unwrap_or_else(|| {
                            self.llvm_type(result_type).const_zero()
                        });
                    }
                }

                // Fallback: compile function expression, then indirect call.
                let fn_val = self.compile_expr(func, fn_ctx);
                self.build_indirect_call(fn_val, args, result_type, fn_ctx)
            }
            _ => {
                let fn_val = self.compile_expr(func, fn_ctx);
                self.build_indirect_call(fn_val, args, result_type, fn_ctx)
            }
        }
    }

    fn build_indirect_call(
        &mut self,
        fn_val: BasicValueEnum<'ctx>,
        args: &[TypedExpr],
        result_type: &Type,
        fn_ctx: &mut FnCtx<'ctx, '_>,
    ) -> BasicValueEnum<'ctx> {
        let ptr = match fn_val {
            BasicValueEnum::PointerValue(p) => p,
            // If we got a non-pointer (e.g. an int from a partial application result), return it as-is
            // or return zero — this handles erroneous call paths gracefully.
            _ => return self.llvm_type(result_type).const_zero(),
        };

        let compiled_args: Vec<BasicMetadataValueEnum> = args
            .iter()
            .map(|a| self.compile_expr(a, fn_ctx).into())
            .collect();

        let ret_llvm = self.llvm_type(result_type);
        let param_types: Vec<BasicMetadataTypeEnum> = args
            .iter()
            .map(|a| self.llvm_type(&a.ty()).into())
            .collect();
        let fn_ty = ret_llvm.fn_type(&param_types, false);

        let call = self
            .builder
            .build_indirect_call(fn_ty, ptr, &compiled_args, "icall")
            .unwrap();

        call.try_as_basic_value().basic().unwrap_or_else(|| {
            self.llvm_type(result_type).const_zero()
        })
    }

    /// Build a partial application closure: heap-allocates an env with the partial args,
    /// generates a wrapper function that completes the call, returns a closure struct.
    fn build_partial_application(
        &mut self,
        llvm_fn: FunctionValue<'ctx>,
        partial_args: &[TypedExpr],
        remaining_params: &[Type],
        final_ret: &Type,
        fn_ctx: &mut FnCtx<'ctx, '_>,
    ) -> BasicValueEnum<'ctx> {
        let ptr_ty = self.context.ptr_type(AddressSpace::default());

        // Compile the partial arguments.
        let compiled_partials: Vec<BasicValueEnum> = partial_args
            .iter()
            .map(|a| self.compile_expr(a, fn_ctx))
            .collect();

        // Build env struct type: one field per partial arg.
        let env_field_types: Vec<BasicTypeEnum> = compiled_partials
            .iter()
            .map(|v| v.get_type())
            .collect();
        let env_struct_ty = self.context.struct_type(&env_field_types, false);

        // Heap-allocate env.
        let env_size = env_struct_ty.size_of().unwrap();
        let env_size_i64 = self.builder
            .build_int_z_extend_or_bit_cast(env_size, self.context.i64_type(), "papp_env_sz")
            .unwrap();
        let env_ptr = self.builder
            .build_call(self.rt_alloc, &[env_size_i64.into()], "papp_env")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();

        // Store partial args into env.
        for (i, val) in compiled_partials.iter().enumerate() {
            let field = self.builder
                .build_struct_gep(env_struct_ty, env_ptr, i as u32, "papp_f")
                .unwrap();
            self.builder.build_store(field, *val).unwrap();
        }

        // Generate wrapper function: (env_ptr, remaining_params...) -> final_ret
        let wrapper_name = format!("__papp_{}", self.closure_count);
        self.closure_count += 1;

        // Wrapper params: env ptr + remaining params
        let mut wrapper_param_tys: Vec<BasicMetadataTypeEnum> = vec![ptr_ty.into()];
        for p in remaining_params {
            wrapper_param_tys.push(self.llvm_type(p).into());
        }
        let ret_llvm = self.llvm_type(final_ret);
        let wrapper_fn_ty = ret_llvm.fn_type(&wrapper_param_tys, false);
        let wrapper_fn = self.module.add_function(&wrapper_name, wrapper_fn_ty, None);

        // Build closure struct: { fn_ptr, env_ptr }
        let cls_struct_ty = self.context.struct_type(&[ptr_ty.into(), ptr_ty.into()], false);
        let cls_size = cls_struct_ty.size_of().unwrap();
        let cls_size_i64 = self.builder
            .build_int_z_extend_or_bit_cast(cls_size, self.context.i64_type(), "papp_cls_sz")
            .unwrap();
        let cls_ptr = self.builder
            .build_call(self.rt_alloc, &[cls_size_i64.into()], "papp_cls")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();
        let fn_field = self.builder.build_struct_gep(cls_struct_ty, cls_ptr, 0, "papp_cls_fn").unwrap();
        self.builder.build_store(fn_field, wrapper_fn.as_global_value().as_pointer_value()).unwrap();
        let env_field = self.builder.build_struct_gep(cls_struct_ty, cls_ptr, 1, "papp_cls_env").unwrap();
        self.builder.build_store(env_field, env_ptr).unwrap();

        // Now compile wrapper body (deferred — save/restore builder).
        let current_block = self.builder.get_insert_block().unwrap();
        {
            let entry = self.context.append_basic_block(wrapper_fn, "entry");
            self.builder.position_at_end(entry);

            // Load partial args from env.
            let env_arg = wrapper_fn.get_nth_param(0).unwrap().into_pointer_value();
            let mut call_args: Vec<BasicMetadataValueEnum> = Vec::new();
            for (i, field_ty) in env_field_types.iter().enumerate() {
                let fp = self.builder.build_struct_gep(env_struct_ty, env_arg, i as u32, "papp_load_f").unwrap();
                let v = self.builder.build_load(*field_ty, fp, "papp_v").unwrap();
                call_args.push(v.into());
            }
            // Add remaining params (starting at param index 1, since 0 is env_ptr).
            for i in 0..remaining_params.len() {
                let p = wrapper_fn.get_nth_param(1 + i as u32).unwrap();
                call_args.push(p.into());
            }

            let call = self.builder.build_call(llvm_fn, &call_args, "papp_call").unwrap();
            match call.try_as_basic_value().basic() {
                Some(v) => { self.builder.build_return(Some(&v)).unwrap(); }
                None => { self.builder.build_return(None).unwrap(); }
            }
        }
        self.builder.position_at_end(current_block);

        cls_ptr.into()
    }

    /// Call a closure value: extract fn_ptr and env_ptr from the struct, then call fn_ptr(env_ptr, ...args).
    fn build_closure_call(
        &mut self,
        closure_ptr: PointerValue<'ctx>,
        args: &[TypedExpr],
        result_type: &Type,
        fn_ctx: &mut FnCtx<'ctx, '_>,
    ) -> BasicValueEnum<'ctx> {
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let closure_struct_type = self.context.struct_type(
            &[ptr_ty.into(), ptr_ty.into()],
            false,
        );
        let fn_field_ptr = self.builder
            .build_struct_gep(closure_struct_type, closure_ptr, 0, "cls_fn_ptr")
            .unwrap();
        let fn_ptr = self.builder
            .build_load(ptr_ty, fn_field_ptr, "cls_fn")
            .unwrap()
            .into_pointer_value();
        let env_field_ptr = self.builder
            .build_struct_gep(closure_struct_type, closure_ptr, 1, "cls_env_ptr")
            .unwrap();
        let env_ptr = self.builder
            .build_load(ptr_ty, env_field_ptr, "cls_env")
            .unwrap();

        // Call: fn_ptr(env_ptr, ...args)
        let mut compiled_args: Vec<BasicMetadataValueEnum> = vec![env_ptr.into()];
        for a in args {
            compiled_args.push(self.compile_expr(a, fn_ctx).into());
        }

        let ret_llvm = self.llvm_type(result_type);
        let mut param_types: Vec<BasicMetadataTypeEnum> = vec![ptr_ty.into()];
        for a in args {
            param_types.push(self.llvm_param_type(&a.ty()));
        }
        let fn_ty = ret_llvm.fn_type(&param_types, false);

        let call = self.builder
            .build_indirect_call(fn_ty, fn_ptr, &compiled_args, "cls_call")
            .unwrap();

        call.try_as_basic_value().basic().unwrap_or_else(|| {
            self.llvm_type(result_type).const_zero()
        })
    }

    // -------------------------------------------------------------------------
    // Intrinsic calls (runtime functions with known ABI)
    // -------------------------------------------------------------------------

    fn compile_intrinsic_call(
        &mut self,
        name: &str,
        args: &[TypedExpr],
        result_type: &Type,
        fn_ctx: &mut FnCtx<'ctx, '_>,
    ) -> BasicValueEnum<'ctx> {
        let null_val = || -> BasicValueEnum<'ctx> { self.context.i8_type().const_zero().into() };

        match name {
            "print" => {
                // print: (value: any) => Null
                // Coerce arg to string then call lin_print.
                let arg_val = self.compile_expr(&args[0], fn_ctx);
                let arg_ty = args[0].ty();
                let str_val = self.value_to_string(arg_val, &arg_ty, fn_ctx);
                self.builder
                    .build_call(self.rt_print, &[str_val.into()], "")
                    .unwrap();
                null_val()
            }
            "toString" => {
                let arg_val = self.compile_expr(&args[0], fn_ctx);
                let arg_ty = args[0].ty();
                self.value_to_string(arg_val, &arg_ty, fn_ctx)
            }
            "length" => {
                let arg_val = self.compile_expr(&args[0], fn_ctx);
                let arg_ty = args[0].ty();
                match &arg_ty {
                    Type::Str => {
                        self.builder
                            .build_call(self.rt_string_length, &[arg_val.into()], "slen")
                            .unwrap()
                            .try_as_basic_value()
                            .unwrap_basic()
                    }
                    Type::Array(_) | Type::FixedArray(_) | Type::Iterator(_) => {
                        let len_i64 = self.builder
                            .build_call(self.rt_array_length, &[arg_val.into()], "alen")
                            .unwrap()
                            .try_as_basic_value()
                            .unwrap_basic()
                            .into_int_value();
                        // Truncate to i32 (spec says length returns Int32).
                        self.builder
                            .build_int_truncate(len_i64, self.context.i32_type(), "alen32")
                            .unwrap()
                            .into()
                    }
                    Type::TypeVar(_) | Type::Union(_) => {
                        // arg_val is a TaggedVal* — unbox to get LinArray*, then get length.
                        let arr_ptr = self.builder
                            .build_call(self.rt_unbox_ptr, &[arg_val.into()], "tv_arr")
                            .unwrap()
                            .try_as_basic_value()
                            .unwrap_basic();
                        let len_i64 = self.builder
                            .build_call(self.rt_array_length, &[arr_ptr.into()], "tv_alen")
                            .unwrap()
                            .try_as_basic_value()
                            .unwrap_basic()
                            .into_int_value();
                        self.builder
                            .build_int_truncate(len_i64, self.context.i32_type(), "tv_alen32")
                            .unwrap()
                            .into()
                    }
                    _ => self.context.i32_type().const_zero().into(),
                }
            }
            "push" => {
                let arr_val = self.compile_expr(&args[0], fn_ctx);
                let elem_val = self.compile_expr(&args[1], fn_ctx);
                let elem_ty = args[1].ty();
                let elem_llvm_ty = self.llvm_type(&elem_ty);
                let cell = self.builder.build_alloca(elem_llvm_ty, "push_elem").unwrap();
                self.builder.build_store(cell, elem_val).unwrap();
                let tag = self.context.i8_type().const_int(0, false);
                self.builder
                    .build_call(self.rt_array_push, &[arr_val.into(), cell.into(), tag.into()], "")
                    .unwrap();
                null_val()
            }
            "concat" => {
                // concat: (T[], T[]) => T[] — runtime array concatenation
                let a_val = self.compile_expr(&args[0], fn_ctx);
                let b_val = self.compile_expr(&args[1], fn_ctx);
                let b_len = self.builder
                    .build_call(self.rt_array_length, &[b_val.into()], "blen")
                    .unwrap()
                    .try_as_basic_value()
                    .unwrap_basic();
                // Alloc new array with combined size.
                let a_len = self.builder
                    .build_call(self.rt_array_length, &[a_val.into()], "alen")
                    .unwrap()
                    .try_as_basic_value()
                    .unwrap_basic()
                    .into_int_value();
                let b_len_int = b_len.into_int_value();
                let total = self.builder.build_int_add(a_len, b_len_int, "total").unwrap();
                let new_arr = self.builder
                    .build_call(self.rt_array_alloc, &[total.into()], "newarr")
                    .unwrap()
                    .try_as_basic_value()
                    .unwrap_basic();
                // TODO: implement element-wise copy via runtime helper.
                // For now, return the new (empty) array as a placeholder.
                new_arr
            }
            "range" => {
                // range(start: Int32, end: Int32) => Iterator<Int32>
                // Eagerly create a LinArray containing the range values.
                // This unifies all iterators (map/filter/range) as LinArray*.
                let start_val = self.compile_expr(&args[0], fn_ctx);
                let end_val = self.compile_expr(&args[1], fn_ctx);
                let start_i32 = start_val.into_int_value();
                let end_i32 = end_val.into_int_value();

                // cap = max(end - start, 4)
                let raw_cap = self.builder.build_int_sub(end_i32, start_i32, "rng_cap_raw").unwrap();
                let cap_i64 = self.builder.build_int_s_extend(raw_cap, self.context.i64_type(), "rng_cap").unwrap();
                let min_cap = self.context.i64_type().const_int(4, false);
                let cap_cond = self.builder.build_int_compare(IntPredicate::SGT, cap_i64, min_cap, "rng_cap_cond").unwrap();
                let final_cap = self.builder.build_select(cap_cond, cap_i64, min_cap, "rng_final_cap").unwrap().into_int_value();

                let arr_ptr = self.builder.build_call(self.rt_array_alloc, &[final_cap.into()], "rng_arr").unwrap()
                    .try_as_basic_value().unwrap_basic().into_pointer_value();

                // Fill the array: for i = start; i < end; i++
                let llvm_fn = fn_ctx.llvm_fn;
                let i_alloc = self.builder.build_alloca(self.context.i32_type(), "rng_i").unwrap();
                self.builder.build_store(i_alloc, start_i32).unwrap();
                let rng_fill_check = self.context.append_basic_block(llvm_fn, "rng_check");
                let rng_fill_body = self.context.append_basic_block(llvm_fn, "rng_body");
                let rng_fill_exit = self.context.append_basic_block(llvm_fn, "rng_exit");
                self.builder.build_unconditional_branch(rng_fill_check).unwrap();
                self.builder.position_at_end(rng_fill_check);
                let cur = self.builder.build_load(self.context.i32_type(), i_alloc, "ri").unwrap().into_int_value();
                let cond = self.builder.build_int_compare(IntPredicate::SLT, cur, end_i32, "rng_cond").unwrap();
                self.builder.build_conditional_branch(cond, rng_fill_body, rng_fill_exit).unwrap();
                self.builder.position_at_end(rng_fill_body);
                let cell = self.builder.build_alloca(self.context.i32_type(), "rng_cell").unwrap();
                self.builder.build_store(cell, cur).unwrap();
                let tag = self.context.i8_type().const_int(2, false); // TAG_INT32
                self.builder.build_call(self.rt_array_push, &[arr_ptr.into(), cell.into(), tag.into()], "").unwrap();
                let next = self.builder.build_int_add(cur, self.context.i32_type().const_int(1, false), "rng_next").unwrap();
                self.builder.build_store(i_alloc, next).unwrap();
                self.builder.build_unconditional_branch(rng_fill_check).unwrap();
                self.builder.position_at_end(rng_fill_exit);
                arr_ptr.into()
            }
            "for" => {
                // for(iterable, body) — generate an inline loop.
                let iterable_ty = args[0].ty();
                let iterable_val = self.compile_expr(&args[0], fn_ctx);
                let body_expr = &args[1];
                self.compile_for_loop(iterable_val, &iterable_ty, body_expr, fn_ctx)
            }
            "iter" => {
                // iter(init, cond, next, current) => Iterator<T>
                // Eagerly evaluate: call init(), loop while cond(state), collect current(state).
                // Result is a LinArray*.
                let elem_ty = match result_type {
                    Type::Iterator(t) => *t.clone(),
                    Type::Array(t) => *t.clone(),
                    _ => Type::TypeVar(0),
                };
                let llvm_fn_val = fn_ctx.llvm_fn;
                let i64_ty = self.context.i64_type();

                // Infer state type from init's return type.
                let state_ty = if let Type::Function { ret, .. } = args[0].ty() {
                    *ret
                } else { Type::TypeVar(0) };
                let state_llvm_ty = self.llvm_type(&state_ty);

                // Allocate output array.
                let out_arr = self.builder
                    .build_call(self.rt_array_alloc, &[i64_ty.const_int(4, false).into()], "iter_out")
                    .unwrap()
                    .try_as_basic_value().unwrap_basic();

                // State alloca to hold current state across iterations.
                let state_alloc = self.builder.build_alloca(state_llvm_ty, "iter_state").unwrap();

                // Call init() to get initial state — use call_body_noarg_ret.
                let init_state = self.call_body_noarg_ret(&args[0], &state_ty, fn_ctx);
                self.builder.build_store(state_alloc, init_state).unwrap();

                // Loop blocks.
                let check_b = self.context.append_basic_block(llvm_fn_val, "iter_check");
                let body_b = self.context.append_basic_block(llvm_fn_val, "iter_body");
                let exit_b = self.context.append_basic_block(llvm_fn_val, "iter_exit");

                self.builder.build_unconditional_branch(check_b).unwrap();
                self.builder.position_at_end(check_b);
                let state_val = self.builder.build_load(state_llvm_ty, state_alloc, "state").unwrap();
                // Call cond(state) -> Bool.
                let cond_result = self.call_body_with_arg_ret(&args[1], state_val, &Type::Bool, fn_ctx);
                let cond_bool = if cond_result.is_int_value() {
                    self.builder.build_int_truncate(cond_result.into_int_value(), self.context.bool_type(), "cond_b").unwrap()
                } else {
                    self.context.bool_type().const_zero()
                };
                self.builder.build_conditional_branch(cond_bool, body_b, exit_b).unwrap();

                self.builder.position_at_end(body_b);
                let state_val2 = self.builder.build_load(state_llvm_ty, state_alloc, "state2").unwrap();
                // Call current(state) -> T (use args[3] = current).
                let elem_val = self.call_body_with_arg_ret(&args[3], state_val2, &elem_ty, fn_ctx);
                // Push elem_val into out_arr.
                let elem_llvm_ty = self.llvm_type(&elem_ty);
                let cell = self.builder.build_alloca(elem_llvm_ty, "iter_cell").unwrap();
                self.builder.build_store(cell, elem_val).unwrap();
                let tag = self.context.i8_type().const_zero();
                // Need state for next call after getting current.
                let state_val3 = self.builder.build_load(state_llvm_ty, state_alloc, "state3").unwrap();
                self.builder.build_call(self.rt_array_push, &[out_arr.into(), cell.into(), tag.into()], "").unwrap();
                // Call next(state) -> state (use args[2] = next).
                let next_state = self.call_body_with_arg_ret(&args[2], state_val3, &state_ty, fn_ctx);
                self.builder.build_store(state_alloc, next_state).unwrap();
                self.builder.build_unconditional_branch(check_b).unwrap();

                self.builder.position_at_end(exit_b);
                out_arr
            }
            "map" => {
                // map(iterable, fn) => Iterator<U>
                // Compile: iterate the input, call fn on each element, push into a new array.
                let iterable_val = self.compile_expr(&args[0], fn_ctx);
                let iterable_ty = args[0].ty();
                let body_expr = &args[1];
                let out_elem_ty = match result_type {
                    Type::Iterator(t) => *t.clone(),
                    Type::Array(t) => *t.clone(),
                    _ => Type::Null,
                };
                self.compile_map_loop(iterable_val, &iterable_ty, body_expr, &out_elem_ty, fn_ctx)
            }
            "filter" => {
                // filter(iterable, pred) => Iterator<T>
                let iterable_val = self.compile_expr(&args[0], fn_ctx);
                let iterable_ty = args[0].ty();
                let body_expr = &args[1];
                let elem_ty = match &iterable_ty {
                    Type::Array(t) => *t.clone(),
                    Type::Iterator(t) => *t.clone(),
                    _ => Type::Null,
                };
                self.compile_filter_loop(iterable_val, &iterable_ty, body_expr, &elem_ty, fn_ctx)
            }
            "reduce" => {
                // reduce(iterable, initial, fn) => U
                let iterable_val = self.compile_expr(&args[0], fn_ctx);
                let iterable_ty = args[0].ty();
                let init_val = self.compile_expr(&args[1], fn_ctx);
                let init_ty = args[1].ty();
                let body_expr = &args[2];
                self.compile_reduce_loop(iterable_val, &iterable_ty, init_val, &init_ty, body_expr, result_type, fn_ctx)
            }
            "keys" | "values" => {
                // Not yet fully implemented.
                let msg = self.compile_string_lit(&format!("intrinsic '{}' not yet compiled", name));
                let zero = self.context.i32_type().const_zero();
                self.builder
                    .build_call(self.rt_panic, &[msg.into(), zero.into(), zero.into()], "")
                    .unwrap();
                self.llvm_type(result_type).const_zero()
            }
            // --- stdlib string intrinsics ---
            "__stringTrim" => {
                let s = self.compile_expr(&args[0], fn_ctx).into_pointer_value();
                let f = self.get_or_declare_fn("lin_string_trim",
                    self.string_ptr_type.fn_type(&[self.string_ptr_type.into()], false));
                self.builder.build_call(f, &[s.into()], "strim").unwrap()
                    .try_as_basic_value().unwrap_basic()
            }
            "__stringToUpper" => {
                let s = self.compile_expr(&args[0], fn_ctx).into_pointer_value();
                let f = self.get_or_declare_fn("lin_string_to_upper",
                    self.string_ptr_type.fn_type(&[self.string_ptr_type.into()], false));
                self.builder.build_call(f, &[s.into()], "supper").unwrap()
                    .try_as_basic_value().unwrap_basic()
            }
            "__stringToLower" => {
                let s = self.compile_expr(&args[0], fn_ctx).into_pointer_value();
                let f = self.get_or_declare_fn("lin_string_to_lower",
                    self.string_ptr_type.fn_type(&[self.string_ptr_type.into()], false));
                self.builder.build_call(f, &[s.into()], "slower").unwrap()
                    .try_as_basic_value().unwrap_basic()
            }
            "__stringLength" => {
                let s = self.compile_expr(&args[0], fn_ctx).into_pointer_value();
                self.builder.build_call(self.rt_string_length, &[s.into()], "slen").unwrap()
                    .try_as_basic_value().unwrap_basic()
            }
            "__stringSlice" => {
                let s = self.compile_expr(&args[0], fn_ctx).into_pointer_value();
                let start = self.compile_expr(&args[1], fn_ctx).into_int_value();
                let end = self.compile_expr(&args[2], fn_ctx).into_int_value();
                let f = self.get_or_declare_fn("lin_string_slice",
                    self.string_ptr_type.fn_type(&[self.string_ptr_type.into(),
                        self.context.i32_type().into(), self.context.i32_type().into()], false));
                self.builder.build_call(f, &[s.into(), start.into(), end.into()], "sslice").unwrap()
                    .try_as_basic_value().unwrap_basic()
            }
            "__stringIndexOf" => {
                let s = self.compile_expr(&args[0], fn_ctx).into_pointer_value();
                let needle = self.compile_expr(&args[1], fn_ctx).into_pointer_value();
                let f = self.get_or_declare_fn("lin_string_index_of",
                    self.context.i32_type().fn_type(&[self.string_ptr_type.into(), self.string_ptr_type.into()], false));
                self.builder.build_call(f, &[s.into(), needle.into()], "sidxof").unwrap()
                    .try_as_basic_value().unwrap_basic()
            }
            "__stringContains" => {
                let s = self.compile_expr(&args[0], fn_ctx).into_pointer_value();
                let needle = self.compile_expr(&args[1], fn_ctx).into_pointer_value();
                let f = self.get_or_declare_fn("lin_string_contains",
                    self.context.bool_type().fn_type(&[self.string_ptr_type.into(), self.string_ptr_type.into()], false));
                self.builder.build_call(f, &[s.into(), needle.into()], "scont").unwrap()
                    .try_as_basic_value().unwrap_basic()
            }
            "__stringStartsWith" => {
                let s = self.compile_expr(&args[0], fn_ctx).into_pointer_value();
                let prefix = self.compile_expr(&args[1], fn_ctx).into_pointer_value();
                let f = self.get_or_declare_fn("lin_string_starts_with",
                    self.context.bool_type().fn_type(&[self.string_ptr_type.into(), self.string_ptr_type.into()], false));
                self.builder.build_call(f, &[s.into(), prefix.into()], "ssw").unwrap()
                    .try_as_basic_value().unwrap_basic()
            }
            "__stringEndsWith" => {
                let s = self.compile_expr(&args[0], fn_ctx).into_pointer_value();
                let suffix = self.compile_expr(&args[1], fn_ctx).into_pointer_value();
                let f = self.get_or_declare_fn("lin_string_ends_with",
                    self.context.bool_type().fn_type(&[self.string_ptr_type.into(), self.string_ptr_type.into()], false));
                self.builder.build_call(f, &[s.into(), suffix.into()], "sew").unwrap()
                    .try_as_basic_value().unwrap_basic()
            }
            "__stringReplace" => {
                let s = self.compile_expr(&args[0], fn_ctx).into_pointer_value();
                let pat = self.compile_expr(&args[1], fn_ctx).into_pointer_value();
                let rep = self.compile_expr(&args[2], fn_ctx).into_pointer_value();
                let f = self.get_or_declare_fn("lin_string_replace",
                    self.string_ptr_type.fn_type(&[self.string_ptr_type.into(), self.string_ptr_type.into(), self.string_ptr_type.into()], false));
                self.builder.build_call(f, &[s.into(), pat.into(), rep.into()], "srep").unwrap()
                    .try_as_basic_value().unwrap_basic()
            }
            "__stringRepeat" => {
                let s = self.compile_expr(&args[0], fn_ctx).into_pointer_value();
                let count = self.compile_expr(&args[1], fn_ctx).into_int_value();
                let f = self.get_or_declare_fn("lin_string_repeat",
                    self.string_ptr_type.fn_type(&[self.string_ptr_type.into(), self.context.i32_type().into()], false));
                self.builder.build_call(f, &[s.into(), count.into()], "srep").unwrap()
                    .try_as_basic_value().unwrap_basic()
            }
            "__stringCharAt" => {
                let s = self.compile_expr(&args[0], fn_ctx).into_pointer_value();
                let idx = self.compile_expr(&args[1], fn_ctx).into_int_value();
                let f = self.get_or_declare_fn("lin_string_char_at",
                    self.string_ptr_type.fn_type(&[self.string_ptr_type.into(), self.context.i32_type().into()], false));
                self.builder.build_call(f, &[s.into(), idx.into()], "sca").unwrap()
                    .try_as_basic_value().unwrap_basic()
            }
            "__stringSplit" => {
                let s = self.compile_expr(&args[0], fn_ctx).into_pointer_value();
                let delim = self.compile_expr(&args[1], fn_ctx).into_pointer_value();
                let f = self.get_or_declare_fn("lin_string_split",
                    self.array_ptr_type.fn_type(&[self.string_ptr_type.into(), self.string_ptr_type.into()], false));
                self.builder.build_call(f, &[s.into(), delim.into()], "ssplit").unwrap()
                    .try_as_basic_value().unwrap_basic()
            }
            "__stringJoin" => {
                let arr = self.compile_expr(&args[0], fn_ctx).into_pointer_value();
                let sep = self.compile_expr(&args[1], fn_ctx).into_pointer_value();
                let f = self.get_or_declare_fn("lin_string_join",
                    self.string_ptr_type.fn_type(&[self.array_ptr_type.into(), self.string_ptr_type.into()], false));
                self.builder.build_call(f, &[arr.into(), sep.into()], "sjoin").unwrap()
                    .try_as_basic_value().unwrap_basic()
            }
            // --- stdlib number intrinsics ---
            "__parseInt32" => {
                let s = self.compile_expr(&args[0], fn_ctx).into_pointer_value();
                let f = self.get_or_declare_fn("lin_parse_int32",
                    self.context.i32_type().fn_type(&[self.string_ptr_type.into()], false));
                self.builder.build_call(f, &[s.into()], "pi32").unwrap()
                    .try_as_basic_value().unwrap_basic()
            }
            "__parseFloat64" => {
                let s = self.compile_expr(&args[0], fn_ctx).into_pointer_value();
                let f = self.get_or_declare_fn("lin_parse_float64",
                    self.context.f64_type().fn_type(&[self.string_ptr_type.into()], false));
                self.builder.build_call(f, &[s.into()], "pf64").unwrap()
                    .try_as_basic_value().unwrap_basic()
            }
            "__toInt32" => {
                let v = self.compile_expr(&args[0], fn_ctx).into_float_value();
                let f = self.get_or_declare_fn("lin_to_int32",
                    self.context.i32_type().fn_type(&[self.context.f64_type().into()], false));
                self.builder.build_call(f, &[v.into()], "toi32").unwrap()
                    .try_as_basic_value().unwrap_basic()
            }
            "__toFloat64" => {
                let v = self.compile_expr(&args[0], fn_ctx).into_int_value();
                let f = self.get_or_declare_fn("lin_to_float64",
                    self.context.f64_type().fn_type(&[self.context.i32_type().into()], false));
                self.builder.build_call(f, &[v.into()], "tof64").unwrap()
                    .try_as_basic_value().unwrap_basic()
            }
            "__isInt32" => {
                let s = self.compile_expr(&args[0], fn_ctx).into_pointer_value();
                let f = self.get_or_declare_fn("lin_is_int32",
                    self.context.bool_type().fn_type(&[self.string_ptr_type.into()], false));
                self.builder.build_call(f, &[s.into()], "isint32").unwrap()
                    .try_as_basic_value().unwrap_basic()
            }
            _ => {
                // Unknown intrinsic — panic.
                let msg = self.compile_string_lit(&format!("unknown intrinsic '{}'", name));
                let zero = self.context.i32_type().const_zero();
                self.builder
                    .build_call(self.rt_panic, &[msg.into(), zero.into(), zero.into()], "")
                    .unwrap();
                self.llvm_type(result_type).const_zero()
            }
        }
    }

    /// Get or declare a runtime function by name. Uses existing declaration if present.
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

    fn compile_if(
        &mut self,
        cond: &TypedExpr,
        then_br: &TypedExpr,
        else_br: &TypedExpr,
        result_type: &Type,
        fn_ctx: &mut FnCtx<'ctx, '_>,
    ) -> BasicValueEnum<'ctx> {
        let cond_val = self.compile_expr(cond, fn_ctx).into_int_value();

        let then_block = self.context.append_basic_block(fn_ctx.llvm_fn, "then");
        let else_block = self.context.append_basic_block(fn_ctx.llvm_fn, "else");
        let merge_block = self.context.append_basic_block(fn_ctx.llvm_fn, "merge");

        self.builder
            .build_conditional_branch(cond_val, then_block, else_block)
            .unwrap();

        // Then branch
        self.builder.position_at_end(then_block);
        let then_val = self.compile_expr(then_br, fn_ctx);
        let then_end = self.builder.get_insert_block().unwrap();
        if !then_end.get_terminator().is_some() {
            self.builder.build_unconditional_branch(merge_block).unwrap();
        }

        // Else branch
        self.builder.position_at_end(else_block);
        let else_val = self.compile_expr(else_br, fn_ctx);
        let else_end = self.builder.get_insert_block().unwrap();
        if !else_end.get_terminator().is_some() {
            self.builder.build_unconditional_branch(merge_block).unwrap();
        }

        // Merge with phi
        self.builder.position_at_end(merge_block);
        let phi = self
            .builder
            .build_phi(self.llvm_type(result_type), "iftmp")
            .unwrap();
        phi.add_incoming(&[(&then_val, then_end), (&else_val, else_end)]);
        phi.as_basic_value()
    }

    // -------------------------------------------------------------------------
    // Closures
    // -------------------------------------------------------------------------

    fn compile_closure(
        &mut self,
        name: Option<&str>,
        params: &[TypedParam],
        body: &TypedExpr,
        ret_type: &Type,
        captures: &[Capture],
        fn_ctx: &mut FnCtx<'ctx, '_>,
    ) -> BasicValueEnum<'ctx> {
        let closure_name = match name {
            Some(n) => n.to_string(),
            None => {
                let n = format!("__closure_{}", self.closure_count);
                self.closure_count += 1;
                n
            }
        };

        // Build LLVM function type: (env_ptr, ...params) -> ret
        let mut llvm_param_types: Vec<BasicMetadataTypeEnum> = Vec::new();
        let ptr_type: BasicMetadataTypeEnum = self.context.ptr_type(AddressSpace::default()).into();
        if !captures.is_empty() {
            llvm_param_types.push(ptr_type);
        }
        for param in params {
            llvm_param_types.push(self.llvm_param_type(&param.ty));
        }

        let fn_type = if matches!(ret_type, Type::Never) {
            self.context.void_type().fn_type(&llvm_param_types, false)
        } else {
            self.llvm_type(ret_type).fn_type(&llvm_param_types, false)
        };

        let llvm_fn = self.module.add_function(&closure_name, fn_type, None);

        // Build the environment struct if there are captures.
        if !captures.is_empty() {
            let cap_types: Vec<BasicTypeEnum> = captures
                .iter()
                .map(|c| {
                    if c.is_mutable {
                        self.context.ptr_type(AddressSpace::default()).into()
                    } else {
                        self.llvm_type(&c.ty)
                    }
                })
                .collect();
            let env_struct_type = self.context.struct_type(&cap_types, false);
            // Heap-allocate the env so captured values survive the creating function's frame.
            let env_size = env_struct_type.size_of().unwrap();
            let env_size_i64 = self.builder
                .build_int_z_extend_or_bit_cast(env_size, self.context.i64_type(), "env_size")
                .unwrap();
            let env_raw = self.builder
                .build_call(self.rt_alloc, &[env_size_i64.into()], "env_raw")
                .unwrap()
                .try_as_basic_value()
                .unwrap_basic()
                .into_pointer_value();
            // Cast raw ptr to env struct ptr (LLVM opaque pointers — just use env_raw directly).
            let env_alloc = env_raw;

            // Store each captured value into the env struct.
            for (i, cap) in captures.iter().enumerate() {
                let field_ptr = self
                    .builder
                    .build_struct_gep(env_struct_type, env_alloc, i as u32, "cap_field")
                    .unwrap();
                let cap_val = match fn_ctx.slots.get(&cap.outer_slot) {
                    Some(SlotStorage::Value(v)) => *v,
                    Some(SlotStorage::Alloca(ptr)) => {
                        if cap.is_mutable {
                            // Pass the alloca pointer itself (shared mutable cell).
                            (*ptr).into()
                        } else {
                            self.builder
                                .build_load(self.llvm_type(&cap.ty), *ptr, "cap_load")
                                .unwrap()
                        }
                    }
                    Some(SlotStorage::Closure(ptr)) => {
                        // Capturing another closure — store the closure struct pointer.
                        (*ptr).into()
                    }
                    None => self.llvm_type(&cap.ty).const_zero(),
                };
                self.builder.build_store(field_ptr, cap_val).unwrap();
            }

            // Build the closure struct: heap-allocated { fn_ptr, env_ptr }
            let fn_ptr = llvm_fn.as_global_value().as_pointer_value();
            let closure_struct_type = self.context.struct_type(
                &[
                    self.context.ptr_type(AddressSpace::default()).into(),
                    self.context.ptr_type(AddressSpace::default()).into(),
                ],
                false,
            );
            // Heap-allocate closure struct so it survives the creating function's stack frame.
            let cls_size = closure_struct_type.size_of().unwrap();
            let cls_size_i64 = self.builder
                .build_int_z_extend_or_bit_cast(cls_size, self.context.i64_type(), "cls_size")
                .unwrap();
            let closure_alloc = self.builder
                .build_call(self.rt_alloc, &[cls_size_i64.into()], "closure")
                .unwrap()
                .try_as_basic_value()
                .unwrap_basic()
                .into_pointer_value();
            let fn_field = self
                .builder
                .build_struct_gep(closure_struct_type, closure_alloc, 0, "closure_fn")
                .unwrap();
            self.builder.build_store(fn_field, fn_ptr).unwrap();
            let env_field = self
                .builder
                .build_struct_gep(closure_struct_type, closure_alloc, 1, "closure_env")
                .unwrap();
            self.builder.build_store(env_field, env_alloc).unwrap();

            // Compile the function body (deferred — save/restore builder position).
            let current_block = self.builder.get_insert_block().unwrap();
            self.compile_function_body(llvm_fn, params, body, ret_type, captures, &closure_name);
            self.builder.position_at_end(current_block);

            closure_alloc.into()
        } else {
            // Pure function (no captures) — just return its pointer.
            let current_block = self.builder.get_insert_block().unwrap();
            self.compile_function_body(llvm_fn, params, body, ret_type, &[], &closure_name);
            self.builder.position_at_end(current_block);

            llvm_fn.as_global_value().as_pointer_value().into()
        }
    }

    // -------------------------------------------------------------------------
    // String interpolation
    // -------------------------------------------------------------------------

    fn compile_string_interp(
        &mut self,
        parts: &[TypedStringPart],
        fn_ctx: &mut FnCtx<'ctx, '_>,
    ) -> BasicValueEnum<'ctx> {
        if parts.is_empty() {
            return self.compile_string_lit("");
        }

        let mut acc = self.compile_string_part_to_string(&parts[0], fn_ctx);
        for part in &parts[1..] {
            let s = self.compile_string_part_to_string(part, fn_ctx);
            acc = self
                .builder
                .build_call(self.rt_string_concat, &[acc.into(), s.into()], "interp")
                .unwrap()
                .try_as_basic_value()
                .unwrap_basic();
        }
        acc
    }

    fn compile_string_part_to_string(
        &mut self,
        part: &TypedStringPart,
        fn_ctx: &mut FnCtx<'ctx, '_>,
    ) -> BasicValueEnum<'ctx> {
        match part {
            TypedStringPart::Literal(s) => self.compile_string_lit(s),
            TypedStringPart::Expr(e) => {
                let val = self.compile_expr(e, fn_ctx);
                let ty = e.ty();
                self.value_to_string(val, &ty, fn_ctx)
            }
        }
    }

    fn value_to_string(
        &mut self,
        val: BasicValueEnum<'ctx>,
        ty: &Type,
        _fn_ctx: &mut FnCtx<'ctx, '_>,
    ) -> BasicValueEnum<'ctx> {
        self.value_to_string_simple(val, ty)
    }

    fn value_to_string_simple(
        &mut self,
        val: BasicValueEnum<'ctx>,
        ty: &Type,
    ) -> BasicValueEnum<'ctx> {
        // TypeVar values are TaggedVal* — use runtime dispatch.
        if matches!(ty, Type::TypeVar(_)) {
            if val.is_pointer_value() {
                return self.builder
                    .build_call(self.rt_tagged_to_string, &[val.into()], "ttos")
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
            _ => {
                // For complex types (objects, arrays, etc.) fall back to "[object]" placeholder.
                self.compile_string_lit("[object]")
            }
        }
    }

    /// If `val` is a TypeVar (TaggedVal*) pointer, extract its payload as `target_ty`.
    /// Otherwise return `val` unchanged.
    fn coerce_typevar(&mut self, val: BasicValueEnum<'ctx>, val_ty: &Type, target_ty: &Type) -> BasicValueEnum<'ctx> {
        if matches!(val_ty, Type::TypeVar(_)) && val.is_pointer_value() {
            let tagged_ptr = val.into_pointer_value();
            self.load_tagged_val_payload(tagged_ptr, target_ty, val_ty)
        } else {
            val
        }
    }

    // -------------------------------------------------------------------------
    // Arrays
    // -------------------------------------------------------------------------

    fn compile_make_array(
        &mut self,
        elements: &[TypedExpr],
        ty: &Type,
        fn_ctx: &mut FnCtx<'ctx, '_>,
    ) -> BasicValueEnum<'ctx> {
        let cap = self.context.i64_type().const_int(elements.len().max(4) as u64, false);
        let arr = self
            .builder
            .build_call(self.rt_array_alloc, &[cap.into()], "arr")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic();

        for elem in elements {
            let val = self.compile_expr(elem, fn_ctx);
            let elem_lin_ty = elem.ty();
            self.array_push_value(arr, val, &elem_lin_ty);
        }

        arr
    }

    /// Push a value into a LinArray*, using the correct tag for inline storage.
    /// All elements are stored inline as { tag, pad, payload } (same layout as TaggedVal).
    fn array_push_value(&mut self, arr: BasicValueEnum<'ctx>, val: BasicValueEnum<'ctx>, val_ty: &Type) {
        let i8_ty = self.context.i8_type();
        match val_ty {
            Type::TypeVar(_) | Type::Union(_) => {
                // val is already a TaggedVal* (ptr to inline {tag,pad,payload}).
                // Use lin_array_push_tagged to copy tag+payload inline.
                let rt_push_tagged = self.get_or_declare_fn("lin_array_push_tagged",
                    self.context.void_type().fn_type(&[
                        self.context.ptr_type(inkwell::AddressSpace::default()).into(),
                        self.context.ptr_type(inkwell::AddressSpace::default()).into(),
                    ], false));
                self.builder.build_call(rt_push_tagged, &[arr.into(), val.into()], "").unwrap();
            }
            _ => {
                // Concrete type: store with correct tag.
                let tag_val = Self::type_tag_byte(val_ty);
                let tag = i8_ty.const_int(tag_val as u64, false);
                // For pointer types, store the pointer in a ptr-sized cell; for scalars use the direct type.
                let (store_val, store_llvm_ty) = match val_ty {
                    Type::Str | Type::Array(_) | Type::Object(_) | Type::Iterator(_) | Type::Function { .. } => {
                        (val, self.context.ptr_type(inkwell::AddressSpace::default()).as_basic_type_enum())
                    }
                    _ => (val, self.llvm_type(val_ty)),
                };
                let cell = self.builder.build_alloca(store_llvm_ty, "arr_cell").unwrap();
                self.builder.build_store(cell, store_val).unwrap();
                self.builder.build_call(self.rt_array_push, &[arr.into(), cell.into(), tag.into()], "arr_push").unwrap();
            }
        }
    }

    /// Return the TAG_* byte for a Lin type (must match lin-runtime tagged.rs constants).
    fn type_tag_byte(ty: &Type) -> u8 {
        match ty {
            Type::Null => 0,
            Type::Bool => 1,
            Type::Int8 | Type::Int16 | Type::Int32 |
            Type::UInt8 | Type::UInt16 | Type::UInt32 => 2,  // TAG_INT32
            Type::Int64 | Type::UInt64 => 3,                  // TAG_INT64
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
    // Iteration
    // -------------------------------------------------------------------------

    fn compile_for_loop(
        &mut self,
        iterable_val: BasicValueEnum<'ctx>,
        iterable_ty: &Type,
        body_expr: &TypedExpr,
        fn_ctx: &mut FnCtx<'ctx, '_>,
    ) -> BasicValueEnum<'ctx> {
        // If the iterable is a TaggedVal* (TypeVar/Union), unbox it to get the LinArray*.
        let (iterable_val, iterable_ty) = if matches!(iterable_ty, Type::TypeVar(_) | Type::Union(_)) {
            let unboxed = self.builder
                .build_call(self.rt_unbox_ptr, &[iterable_val.into()], "for_unbox")
                .unwrap().try_as_basic_value().unwrap_basic();
            (unboxed, Type::Array(Box::new(Type::TypeVar(0))))
        } else {
            (iterable_val, iterable_ty.clone())
        };
        let iterable_ty = &iterable_ty;
        let iterable_val = iterable_val;

        let null_val = || -> BasicValueEnum<'ctx> { self.context.i8_type().const_zero().into() };
        let llvm_fn = fn_ctx.llvm_fn;

        match iterable_ty {
            Type::Array(_) | Type::FixedArray(_) | Type::Iterator(_) => {
                let elem_ty = match iterable_ty {
                    Type::Array(t) => (**t).clone(),
                    Type::Iterator(t) => (**t).clone(),
                    Type::FixedArray(ts) => ts.first().cloned().unwrap_or(Type::Null),
                    _ => unreachable!(),
                };
                // Array iteration: i64 index loop, load each element.
                let i_alloc = self.builder
                    .build_alloca(self.context.i64_type(), "for_i")
                    .unwrap();
                self.builder
                    .build_store(i_alloc, self.context.i64_type().const_zero())
                    .unwrap();
                let len = self.builder
                    .build_call(self.rt_array_length, &[iterable_val.into()], "for_len")
                    .unwrap()
                    .try_as_basic_value()
                    .unwrap_basic()
                    .into_int_value();

                let check_block = self.context.append_basic_block(llvm_fn, "for_check");
                let body_block = self.context.append_basic_block(llvm_fn, "for_body");
                let exit_block = self.context.append_basic_block(llvm_fn, "for_exit");

                self.builder.build_unconditional_branch(check_block).unwrap();
                self.builder.position_at_end(check_block);
                let cur_i = self.builder
                    .build_load(self.context.i64_type(), i_alloc, "cur_i")
                    .unwrap()
                    .into_int_value();
                let cond = self.builder
                    .build_int_compare(IntPredicate::SLT, cur_i, len, "for_cond")
                    .unwrap();
                self.builder
                    .build_conditional_branch(cond, body_block, exit_block)
                    .unwrap();

                self.builder.position_at_end(body_block);
                // Get element pointer and load the payload, then interpret as elem_ty.
                let elem_ptr_val = self.builder
                    .build_call(self.rt_array_get, &[iterable_val.into(), cur_i.into()], "elem_ptr")
                    .unwrap()
                    .try_as_basic_value()
                    .unwrap_basic()
                    .into_pointer_value();
                let elem_val = self.load_array_element(elem_ptr_val, &elem_ty);
                self.call_body_with_arg(body_expr, elem_val, fn_ctx);

                // Increment counter.
                let next_i = self.builder
                    .build_int_add(cur_i, self.context.i64_type().const_int(1, false), "next_i")
                    .unwrap();
                self.builder.build_store(i_alloc, next_i).unwrap();
                self.builder.build_unconditional_branch(check_block).unwrap();

                self.builder.position_at_end(exit_block);
                null_val()
            }

            _ => {
                let msg = self.compile_string_lit("for: unsupported iterable type");
                let zero = self.context.i32_type().const_zero();
                self.builder
                    .build_call(self.rt_panic, &[msg.into(), zero.into(), zero.into()], "")
                    .unwrap();
                null_val()
            }
        }
    }

    /// map(iterable, fn) — iterate, call fn on each element, push into a new array.
    fn compile_map_loop(
        &mut self,
        iterable_val: BasicValueEnum<'ctx>,
        iterable_ty: &Type,
        body_expr: &TypedExpr,
        out_elem_ty: &Type,
        fn_ctx: &mut FnCtx<'ctx, '_>,
    ) -> BasicValueEnum<'ctx> {
        let llvm_fn = fn_ctx.llvm_fn;
        let i64_ty = self.context.i64_type();

        // Allocate output array.
        let cap = i64_ty.const_int(4, false);
        let out_arr = self.builder
            .build_call(self.rt_array_alloc, &[cap.into()], "map_out")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic();

        let (i_alloc, len, elem_ty) = self.setup_iteration(iterable_val, iterable_ty, fn_ctx);
        let check_block = self.context.append_basic_block(llvm_fn, "map_check");
        let body_block = self.context.append_basic_block(llvm_fn, "map_body");
        let exit_block = self.context.append_basic_block(llvm_fn, "map_exit");

        self.builder.build_unconditional_branch(check_block).unwrap();
        self.builder.position_at_end(check_block);
        let cur_i = self.builder.build_load(i64_ty, i_alloc, "mi").unwrap().into_int_value();
        let cond = self.builder.build_int_compare(IntPredicate::SLT, cur_i, len, "mc").unwrap();
        self.builder.build_conditional_branch(cond, body_block, exit_block).unwrap();

        self.builder.position_at_end(body_block);
        let elem_val = self.load_iteration_element(iterable_val, iterable_ty, cur_i, &elem_ty, fn_ctx);
        // Call body: get mapped value.
        let out_val = self.call_body_with_arg_ret(body_expr, elem_val, out_elem_ty, fn_ctx);
        // Push out_val into out_arr with correct tag.
        let out_arr_clone = out_arr;
        self.array_push_value(out_arr_clone, out_val, out_elem_ty);

        let next_i = self.builder.build_int_add(cur_i, i64_ty.const_int(1, false), "mi_next").unwrap();
        self.builder.build_store(i_alloc, next_i).unwrap();
        self.builder.build_unconditional_branch(check_block).unwrap();
        self.builder.position_at_end(exit_block);
        out_arr
    }

    /// filter(iterable, pred) — iterate, keep elements where pred returns true.
    fn compile_filter_loop(
        &mut self,
        iterable_val: BasicValueEnum<'ctx>,
        iterable_ty: &Type,
        body_expr: &TypedExpr,
        elem_ty: &Type,
        fn_ctx: &mut FnCtx<'ctx, '_>,
    ) -> BasicValueEnum<'ctx> {
        let llvm_fn = fn_ctx.llvm_fn;
        let i64_ty = self.context.i64_type();

        let cap = i64_ty.const_int(4, false);
        let out_arr = self.builder
            .build_call(self.rt_array_alloc, &[cap.into()], "filt_out")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic();

        let (i_alloc, len, inferred_elem_ty) = self.setup_iteration(iterable_val, iterable_ty, fn_ctx);
        let actual_elem_ty = if matches!(elem_ty, Type::Null) { inferred_elem_ty } else { elem_ty.clone() };

        let check_block = self.context.append_basic_block(llvm_fn, "filt_check");
        let body_block = self.context.append_basic_block(llvm_fn, "filt_body");
        let push_block = self.context.append_basic_block(llvm_fn, "filt_push");
        let next_block = self.context.append_basic_block(llvm_fn, "filt_next");
        let exit_block = self.context.append_basic_block(llvm_fn, "filt_exit");

        self.builder.build_unconditional_branch(check_block).unwrap();
        self.builder.position_at_end(check_block);
        let cur_i = self.builder.build_load(i64_ty, i_alloc, "fi").unwrap().into_int_value();
        let cond = self.builder.build_int_compare(IntPredicate::SLT, cur_i, len, "fc").unwrap();
        self.builder.build_conditional_branch(cond, body_block, exit_block).unwrap();

        self.builder.position_at_end(body_block);
        let elem_val = self.load_iteration_element(iterable_val, iterable_ty, cur_i, &actual_elem_ty, fn_ctx);
        let keep = self.call_body_with_arg_ret(body_expr, elem_val, &Type::Bool, fn_ctx);
        self.builder.build_conditional_branch(keep.into_int_value(), push_block, next_block).unwrap();

        self.builder.position_at_end(push_block);
        // Re-load element (elem_val from body_block is not in this block)
        let elem_val2 = self.load_iteration_element(iterable_val, iterable_ty, cur_i, &actual_elem_ty, fn_ctx);
        let out_arr_f = out_arr;
        self.array_push_value(out_arr_f, elem_val2, &actual_elem_ty);
        self.builder.build_unconditional_branch(next_block).unwrap();

        self.builder.position_at_end(next_block);
        let next_i = self.builder.build_int_add(cur_i, i64_ty.const_int(1, false), "fi_next").unwrap();
        self.builder.build_store(i_alloc, next_i).unwrap();
        self.builder.build_unconditional_branch(check_block).unwrap();
        self.builder.position_at_end(exit_block);
        out_arr
    }

    /// reduce(iterable, initial, fn) — fold left.
    fn compile_reduce_loop(
        &mut self,
        iterable_val: BasicValueEnum<'ctx>,
        iterable_ty: &Type,
        init_val: BasicValueEnum<'ctx>,
        init_ty: &Type,
        body_expr: &TypedExpr,
        result_type: &Type,
        fn_ctx: &mut FnCtx<'ctx, '_>,
    ) -> BasicValueEnum<'ctx> {
        let llvm_fn = fn_ctx.llvm_fn;
        let i64_ty = self.context.i64_type();
        let acc_llvm_ty = self.llvm_type(init_ty);

        // Accumulator alloca.
        let acc_alloc = self.builder.build_alloca(acc_llvm_ty, "acc").unwrap();
        self.builder.build_store(acc_alloc, init_val).unwrap();

        let (i_alloc, len, elem_ty) = self.setup_iteration(iterable_val, iterable_ty, fn_ctx);
        let check_block = self.context.append_basic_block(llvm_fn, "red_check");
        let body_block = self.context.append_basic_block(llvm_fn, "red_body");
        let exit_block = self.context.append_basic_block(llvm_fn, "red_exit");

        self.builder.build_unconditional_branch(check_block).unwrap();
        self.builder.position_at_end(check_block);
        let cur_i = self.builder.build_load(i64_ty, i_alloc, "ri").unwrap().into_int_value();
        let cond = self.builder.build_int_compare(IntPredicate::SLT, cur_i, len, "rc").unwrap();
        self.builder.build_conditional_branch(cond, body_block, exit_block).unwrap();

        self.builder.position_at_end(body_block);
        let elem_val = self.load_iteration_element(iterable_val, iterable_ty, cur_i, &elem_ty, fn_ctx);
        let acc_val = self.builder.build_load(acc_llvm_ty, acc_alloc, "acc_v").unwrap();
        // body is (acc, elem) => new_acc — call with both args
        let new_acc = self.call_body_with_two_args(body_expr, acc_val, elem_val, init_ty, &elem_ty, result_type, fn_ctx);
        self.builder.build_store(acc_alloc, new_acc).unwrap();

        let next_i = self.builder.build_int_add(cur_i, i64_ty.const_int(1, false), "ri_next").unwrap();
        self.builder.build_store(i_alloc, next_i).unwrap();
        self.builder.build_unconditional_branch(check_block).unwrap();

        self.builder.position_at_end(exit_block);
        self.builder.build_load(acc_llvm_ty, acc_alloc, "red_result").unwrap()
    }

    /// Set up iteration state: returns (i_alloc, arr_ptr, len, elem_ty).
    /// For arrays: i is i64 index, len is array length.
    /// For TypeVar/Union: unbox the TaggedVal* to get LinArray* first.
    fn setup_iteration(
        &mut self,
        iterable_val: BasicValueEnum<'ctx>,
        iterable_ty: &Type,
        _fn_ctx: &mut FnCtx<'ctx, '_>,
    ) -> (PointerValue<'ctx>, inkwell::values::IntValue<'ctx>, Type) {
        let i64_ty = self.context.i64_type();
        // Unbox TaggedVal* to get the actual LinArray* for TypeVar/Union iterables.
        let arr_val = if matches!(iterable_ty, Type::TypeVar(_) | Type::Union(_)) {
            self.builder
                .build_call(self.rt_unbox_ptr, &[iterable_val.into()], "si_unbox")
                .unwrap().try_as_basic_value().unwrap_basic()
        } else {
            iterable_val
        };
        let actual_elem_ty = match iterable_ty {
            Type::Array(t) => *t.clone(),
            Type::Iterator(t) => *t.clone(),
            Type::FixedArray(ts) => ts.first().cloned().unwrap_or(Type::Null),
            Type::TypeVar(_) | Type::Union(_) => Type::TypeVar(0),
            _ => Type::Null,
        };
        match iterable_ty {
            Type::Array(_) | Type::FixedArray(_) | Type::Iterator(_) | Type::TypeVar(_) | Type::Union(_) => {
                let i_alloc = self.builder.build_alloca(i64_ty, "iter_i").unwrap();
                self.builder.build_store(i_alloc, i64_ty.const_zero()).unwrap();
                let len = self.builder
                    .build_call(self.rt_array_length, &[arr_val.into()], "iter_len")
                    .unwrap()
                    .try_as_basic_value().unwrap_basic().into_int_value();
                (i_alloc, len, actual_elem_ty)
            }
            _ => {
                // Unknown iterable — return dummy zero-length.
                let i_alloc = self.builder.build_alloca(i64_ty, "iter_i").unwrap();
                self.builder.build_store(i_alloc, i64_ty.const_zero()).unwrap();
                (i_alloc, i64_ty.const_zero(), Type::Null)
            }
        }
    }

    /// Load the current element from iteration state.
    fn load_iteration_element(
        &mut self,
        iterable_val: BasicValueEnum<'ctx>,
        iterable_ty: &Type,
        cur_i: inkwell::values::IntValue<'ctx>,
        elem_ty: &Type,
        _fn_ctx: &mut FnCtx<'ctx, '_>,
    ) -> BasicValueEnum<'ctx> {
        // Unbox TaggedVal* to get actual LinArray* for TypeVar/Union iterables.
        let arr_val = if matches!(iterable_ty, Type::TypeVar(_) | Type::Union(_)) {
            self.builder
                .build_call(self.rt_unbox_ptr, &[iterable_val.into()], "lie_unbox")
                .unwrap().try_as_basic_value().unwrap_basic()
        } else {
            iterable_val
        };
        match iterable_ty {
            Type::Array(_) | Type::FixedArray(_) | Type::Iterator(_) | Type::TypeVar(_) | Type::Union(_) => {
                let elem_ptr = self.builder
                    .build_call(self.rt_array_get, &[arr_val.into(), cur_i.into()], "elem_ptr")
                    .unwrap()
                    .try_as_basic_value().unwrap_basic().into_pointer_value();
                self.load_array_element(elem_ptr, elem_ty)
            }
            _ => self.llvm_type(elem_ty).const_zero(),
        }
    }

    /// Call body with one argument and return the result.
    /// Call a zero-arg body expression: `init => result_ty`.
    fn call_body_noarg_ret(
        &mut self,
        body_expr: &TypedExpr,
        result_ty: &Type,
        fn_ctx: &mut FnCtx<'ctx, '_>,
    ) -> BasicValueEnum<'ctx> {
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let ret_llvm = self.llvm_type(result_ty);
        match body_expr {
            TypedExpr::LocalGet { slot, .. } => {
                if let Some(SlotStorage::Closure(cls_ptr)) = fn_ctx.slots.get(slot).cloned() {
                    let cls_struct_type = self.context.struct_type(&[ptr_ty.into(), ptr_ty.into()], false);
                    let fn_field = self.builder.build_struct_gep(cls_struct_type, cls_ptr, 0, "ci_p").unwrap();
                    let fn_ptr = self.builder.build_load(ptr_ty, fn_field, "ci").unwrap().into_pointer_value();
                    let env_field = self.builder.build_struct_gep(cls_struct_type, cls_ptr, 1, "cie_p").unwrap();
                    let env_ptr = self.builder.build_load(ptr_ty, env_field, "cie").unwrap();
                    let fn_type = ret_llvm.fn_type(&[ptr_ty.into()], false);
                    return self.builder.build_indirect_call(fn_type, fn_ptr, &[env_ptr.into()], "ci_call")
                        .unwrap().try_as_basic_value().basic().unwrap_or_else(|| ret_llvm.const_zero());
                }
                if let Some(SlotStorage::Value(fn_val)) = fn_ctx.slots.get(slot).cloned() {
                    if let BasicValueEnum::PointerValue(fn_ptr) = fn_val {
                        let fn_type = ret_llvm.fn_type(&[], false);
                        return self.builder.build_indirect_call(fn_type, fn_ptr, &[], "ci_call")
                            .unwrap().try_as_basic_value().basic().unwrap_or_else(|| ret_llvm.const_zero());
                    }
                }
                if let Some(llvm_fn) = self.global_fn_slots.get(slot).copied() {
                    return self.builder.build_call(llvm_fn, &[], "ci_call")
                        .unwrap().try_as_basic_value().basic().unwrap_or_else(|| ret_llvm.const_zero());
                }
                ret_llvm.const_zero()
            }
            TypedExpr::Function { params, body, captures, .. } if captures.is_empty() && params.is_empty() => {
                // No-arg inline lambda — directly compile the body.
                self.compile_expr(body, fn_ctx)
            }
            TypedExpr::Function { params, body, captures, .. } => {
                // Capturing no-arg closure: compile and call via { fn_ptr, env_ptr }.
                let ptr_ty = self.context.ptr_type(AddressSpace::default());
                let cls_ptr = self.compile_closure(None, params, body, result_ty, captures, fn_ctx).into_pointer_value();
                let cls_struct = self.context.struct_type(&[ptr_ty.into(), ptr_ty.into()], false);
                let fn_field = self.builder.build_struct_gep(cls_struct, cls_ptr, 0, "cin_p").unwrap();
                let fn_ptr = self.builder.build_load(ptr_ty, fn_field, "cin").unwrap().into_pointer_value();
                let env_field = self.builder.build_struct_gep(cls_struct, cls_ptr, 1, "cine_p").unwrap();
                let env_ptr = self.builder.build_load(ptr_ty, env_field, "cine").unwrap();
                let fn_type = ret_llvm.fn_type(&[ptr_ty.into()], false);
                self.builder.build_indirect_call(fn_type, fn_ptr, &[env_ptr.into()], "ci_call")
                    .unwrap().try_as_basic_value().basic().unwrap_or_else(|| ret_llvm.const_zero())
            }
            _ => {
                let fn_val = self.compile_expr(body_expr, fn_ctx);
                if let BasicValueEnum::PointerValue(fn_ptr) = fn_val {
                    let fn_type = ret_llvm.fn_type(&[], false);
                    self.builder.build_indirect_call(fn_type, fn_ptr, &[], "ci_call")
                        .unwrap().try_as_basic_value().basic().unwrap_or_else(|| ret_llvm.const_zero())
                } else {
                    ret_llvm.const_zero()
                }
            }
        }
    }

    fn call_body_with_arg_ret(
        &mut self,
        body_expr: &TypedExpr,
        arg: BasicValueEnum<'ctx>,
        result_ty: &Type,
        fn_ctx: &mut FnCtx<'ctx, '_>,
    ) -> BasicValueEnum<'ctx> {
        match body_expr {
            TypedExpr::LocalGet { slot, .. } => {
                let ptr_ty = self.context.ptr_type(AddressSpace::default());
                let arg_meta: BasicMetadataTypeEnum = arg.get_type().into();
                let ret_llvm = self.llvm_type(result_ty);
                // Check for closure.
                if let Some(SlotStorage::Closure(cls_ptr)) = fn_ctx.slots.get(slot).cloned() {
                    let cls_struct_type = self.context.struct_type(&[ptr_ty.into(), ptr_ty.into()], false);
                    let fn_field = self.builder.build_struct_gep(cls_struct_type, cls_ptr, 0, "cbrf_p").unwrap();
                    let fn_ptr = self.builder.build_load(ptr_ty, fn_field, "cbrf").unwrap().into_pointer_value();
                    let env_field = self.builder.build_struct_gep(cls_struct_type, cls_ptr, 1, "cbre_p").unwrap();
                    let env_ptr = self.builder.build_load(ptr_ty, env_field, "cbre").unwrap();
                    let fn_type = ret_llvm.fn_type(&[ptr_ty.into(), arg_meta], false);
                    return self.builder.build_indirect_call(fn_type, fn_ptr, &[env_ptr.into(), arg.into()], "cbr_call")
                        .unwrap().try_as_basic_value().basic().unwrap_or_else(|| ret_llvm.const_zero());
                }
                if let Some(SlotStorage::Value(fn_val)) = fn_ctx.slots.get(slot).cloned() {
                    if let BasicValueEnum::PointerValue(fn_ptr) = fn_val {
                        let fn_type = ret_llvm.fn_type(&[arg_meta], false);
                        return self.builder.build_indirect_call(fn_type, fn_ptr, &[arg.into()], "cbr_call")
                            .unwrap().try_as_basic_value().basic().unwrap_or_else(|| ret_llvm.const_zero());
                    }
                }
                if let Some(llvm_fn) = self.global_fn_slots.get(slot).copied() {
                    return self.builder.build_call(llvm_fn, &[arg.into()], "cbr_call")
                        .unwrap().try_as_basic_value().basic().unwrap_or_else(|| ret_llvm.const_zero());
                }
                ret_llvm.const_zero()
            }
            TypedExpr::Function { params, body, captures, .. } if captures.is_empty() => {
                // Inline lambda.
                if let Some(param) = params.first() {
                    let old = fn_ctx.slots.insert(param.slot, SlotStorage::Value(arg));
                    let result = self.compile_expr(body, fn_ctx);
                    if let Some(prev) = old {
                        fn_ctx.slots.insert(param.slot, prev);
                    } else {
                        fn_ctx.slots.remove(&param.slot);
                    }
                    result
                } else {
                    self.llvm_type(result_ty).const_zero()
                }
            }
            TypedExpr::Function { params, body, captures, .. } => {
                // Capturing lambda — compile as closure and call via { fn_ptr, env_ptr }.
                let ptr_ty = self.context.ptr_type(AddressSpace::default());
                let ret_llvm = self.llvm_type(result_ty);
                let arg_meta: BasicMetadataTypeEnum = arg.get_type().into();
                let cls_ptr = self.compile_closure(None, params, body, result_ty, captures, fn_ctx).into_pointer_value();
                let cls_struct_type = self.context.struct_type(&[ptr_ty.into(), ptr_ty.into()], false);
                let fn_field = self.builder.build_struct_gep(cls_struct_type, cls_ptr, 0, "clf_p").unwrap();
                let fn_ptr = self.builder.build_load(ptr_ty, fn_field, "clf").unwrap().into_pointer_value();
                let env_field = self.builder.build_struct_gep(cls_struct_type, cls_ptr, 1, "cle_p").unwrap();
                let env_ptr = self.builder.build_load(ptr_ty, env_field, "cle").unwrap();
                let fn_type = ret_llvm.fn_type(&[ptr_ty.into(), arg_meta], false);
                self.builder.build_indirect_call(fn_type, fn_ptr, &[env_ptr.into(), arg.into()], "cbr_call")
                    .unwrap().try_as_basic_value().basic().unwrap_or_else(|| ret_llvm.const_zero())
            }
            _ => {
                let fn_val = self.compile_expr(body_expr, fn_ctx);
                if let BasicValueEnum::PointerValue(fn_ptr) = fn_val {
                    let ret_llvm = self.llvm_type(result_ty);
                    let arg_meta: BasicMetadataTypeEnum = arg.get_type().into();
                    let fn_type = ret_llvm.fn_type(&[arg_meta], false);
                    self.builder.build_indirect_call(fn_type, fn_ptr, &[arg.into()], "cbr_call")
                        .unwrap().try_as_basic_value().basic().unwrap_or_else(|| ret_llvm.const_zero())
                } else {
                    self.llvm_type(result_ty).const_zero()
                }
            }
        }
    }

    /// Call a 2-arg body (acc, elem) => new_acc for reduce.
    fn call_body_with_two_args(
        &mut self,
        body_expr: &TypedExpr,
        acc: BasicValueEnum<'ctx>,
        elem: BasicValueEnum<'ctx>,
        acc_ty: &Type,
        _elem_ty: &Type,
        result_ty: &Type,
        fn_ctx: &mut FnCtx<'ctx, '_>,
    ) -> BasicValueEnum<'ctx> {
        let ret_llvm = self.llvm_type(result_ty);
        match body_expr {
            TypedExpr::LocalGet { slot, .. } => {
                let ptr_ty = self.context.ptr_type(AddressSpace::default());
                let acc_meta: BasicMetadataTypeEnum = acc.get_type().into();
                let elem_meta: BasicMetadataTypeEnum = elem.get_type().into();
                if let Some(SlotStorage::Closure(cls_ptr)) = fn_ctx.slots.get(slot).cloned() {
                    let cls_struct_type = self.context.struct_type(&[ptr_ty.into(), ptr_ty.into()], false);
                    let fn_field = self.builder.build_struct_gep(cls_struct_type, cls_ptr, 0, "r2_fn_p").unwrap();
                    let fn_ptr = self.builder.build_load(ptr_ty, fn_field, "r2_fn").unwrap().into_pointer_value();
                    let env_field = self.builder.build_struct_gep(cls_struct_type, cls_ptr, 1, "r2_env_p").unwrap();
                    let env_ptr = self.builder.build_load(ptr_ty, env_field, "r2_env").unwrap();
                    let fn_type = ret_llvm.fn_type(&[ptr_ty.into(), acc_meta, elem_meta], false);
                    return self.builder.build_indirect_call(fn_type, fn_ptr, &[env_ptr.into(), acc.into(), elem.into()], "r2_call")
                        .unwrap().try_as_basic_value().basic().unwrap_or_else(|| ret_llvm.const_zero());
                }
                if let Some(llvm_fn) = self.global_fn_slots.get(slot).copied() {
                    return self.builder.build_call(llvm_fn, &[acc.into(), elem.into()], "r2_call")
                        .unwrap().try_as_basic_value().basic().unwrap_or_else(|| ret_llvm.const_zero());
                }
                ret_llvm.const_zero()
            }
            TypedExpr::Function { params, body, captures, .. } if captures.is_empty() => {
                // Inline 2-param lambda.
                let old0 = params.get(0).map(|p| {
                    let old = fn_ctx.slots.insert(p.slot, SlotStorage::Value(acc));
                    (p.slot, old)
                });
                let old1 = params.get(1).map(|p| {
                    let old = fn_ctx.slots.insert(p.slot, SlotStorage::Value(elem));
                    (p.slot, old)
                });
                let result = self.compile_expr(body, fn_ctx);
                // Restore.
                if let Some((slot, old)) = old0 {
                    if let Some(prev) = old { fn_ctx.slots.insert(slot, prev); } else { fn_ctx.slots.remove(&slot); }
                }
                if let Some((slot, old)) = old1 {
                    if let Some(prev) = old { fn_ctx.slots.insert(slot, prev); } else { fn_ctx.slots.remove(&slot); }
                }
                // Coerce result to acc_ty if needed.
                if result.get_type() != self.llvm_type(acc_ty) {
                    ret_llvm.const_zero()
                } else {
                    result
                }
            }
            _ => ret_llvm.const_zero(),
        }
    }

    /// Load an element value from a LinArrayElem pointer, interpreting it as `elem_ty`.
    fn load_array_element(
        &self,
        elem_ptr: PointerValue<'ctx>,
        elem_ty: &Type,
    ) -> BasicValueEnum<'ctx> {
        // LinArrayElem layout: { tag: u8, _pad: [7 x u8], payload: u64 }
        // payload is at offset 8. LinArrayElem has the same layout as TaggedVal.
        let payload_ptr = unsafe {
            self.builder
                .build_gep(
                    self.context.i8_type(),
                    elem_ptr,
                    &[self.context.i32_type().const_int(8, false)],
                    "payload_ptr",
                )
                .unwrap()
        };
        let llvm_ty = self.llvm_type(elem_ty);
        match elem_ty {
            Type::Bool => {
                let u8_val = self.builder
                    .build_load(self.context.i8_type(), payload_ptr, "bool_raw")
                    .unwrap()
                    .into_int_value();
                self.builder
                    .build_int_truncate(u8_val, self.context.bool_type(), "bool_val")
                    .unwrap()
                    .into()
            }
            Type::Str | Type::Array(_) => {
                // Stored as pointer in the u64 payload.
                let u64_val = self.builder
                    .build_load(self.context.i64_type(), payload_ptr, "ptr_raw")
                    .unwrap()
                    .into_int_value();
                self.builder
                    .build_int_to_ptr(u64_val, self.context.ptr_type(AddressSpace::default()), "ptr_val")
                    .unwrap()
                    .into()
            }
            Type::TypeVar(_) | Type::Union(_) => {
                // LinArrayElem has the same layout as TaggedVal: return elem_ptr itself as TaggedVal*.
                // The coerce_typevar / load_tagged_val_payload will extract the correct type.
                elem_ptr.into()
            }
            _ => {
                // Integer/float: load directly as the target type.
                self.builder
                    .build_load(llvm_ty, payload_ptr, "elem_val")
                    .unwrap()
            }
        }
    }

    /// Load a value from a TaggedVal* (offset 8 = payload). Handles null pointer → return zero.
    fn load_tagged_val_payload(
        &mut self,
        tagged_ptr: PointerValue<'ctx>,
        result_type: &Type,
        _obj_ty: &Type,
    ) -> BasicValueEnum<'ctx> {
        let llvm_ty = self.llvm_type(result_type);
        // TaggedVal layout: tag(u8) + pad([u8;7]) + payload(u64) → payload at offset 8
        let payload_ptr = unsafe {
            self.builder.build_gep(
                self.context.i8_type(),
                tagged_ptr,
                &[self.context.i32_type().const_int(8, false)],
                "tv_payload_p",
            ).unwrap()
        };
        match result_type {
            Type::Null => llvm_ty.const_zero(),
            Type::Bool => {
                let u8_val = self.builder.build_load(self.context.i8_type(), payload_ptr, "tv_bool_raw").unwrap().into_int_value();
                self.builder.build_int_truncate(u8_val, self.context.bool_type(), "tv_bool").unwrap().into()
            }
            Type::Str | Type::Array(_) | Type::Object(_) | Type::TypeVar(_) => {
                // Payload is a pointer stored as u64.
                let u64_val = self.builder.build_load(self.context.i64_type(), payload_ptr, "tv_ptr_raw").unwrap().into_int_value();
                self.builder.build_int_to_ptr(u64_val, self.context.ptr_type(AddressSpace::default()), "tv_ptr").unwrap().into()
            }
            Type::Float32 => {
                // Payload stored as f64 bits (extended from f32). Load i64, bitcast to f64, then truncate to f32.
                let u64_val = self.builder.build_load(self.context.i64_type(), payload_ptr, "tv_f64_raw").unwrap();
                let f64_val = self.builder.build_bit_cast(u64_val, self.context.f64_type(), "tv_f64").unwrap().into_float_value();
                self.builder.build_float_trunc(f64_val, self.context.f32_type(), "tv_f32").unwrap().into()
            }
            Type::Float64 => {
                // Payload stored as bitcast of f64 bits.
                let u64_val = self.builder.build_load(self.context.i64_type(), payload_ptr, "tv_f64_raw").unwrap();
                self.builder.build_bit_cast(u64_val, self.context.f64_type(), "tv_f64").unwrap()
            }
            _ => {
                // Integer types: load directly.
                self.builder.build_load(llvm_ty, payload_ptr, "tv_int").unwrap()
            }
        }
    }

    /// Call a function value (closure or bare fn ptr) with no arguments. Returns the result as `ret_ty`.
    fn call_fn_val_noarg(&mut self, fn_val: BasicValueEnum<'ctx>, ret_ty: &Type, fn_ctx: &mut FnCtx<'ctx, '_>) -> BasicValueEnum<'ctx> {
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let ret_llvm_ty = self.llvm_type(ret_ty);
        let ret_meta: BasicMetadataTypeEnum = ret_llvm_ty.into();
        // Closures: { fn_ptr, env_ptr } struct on heap.
        if fn_val.is_pointer_value() {
            let cls_ptr = fn_val.into_pointer_value();
            let cls_struct = self.context.struct_type(&[ptr_ty.into(), ptr_ty.into()], false);
            // Try loading as closure.
            let fn_field = self.builder.build_struct_gep(cls_struct, cls_ptr, 0, "cfn_p").unwrap();
            let fn_ptr = self.builder.build_load(ptr_ty, fn_field, "cfn").unwrap().into_pointer_value();
            let env_field = self.builder.build_struct_gep(cls_struct, cls_ptr, 1, "cenv_p").unwrap();
            let env_ptr = self.builder.build_load(ptr_ty, env_field, "cenv").unwrap();
            let fn_type = ret_llvm_ty.fn_type(&[ptr_ty.into()], false);
            return self.builder
                .build_indirect_call(fn_type, fn_ptr, &[env_ptr.into()], "cfn_call")
                .unwrap()
                .try_as_basic_value()
                .basic()
                .unwrap_or_else(|| ret_llvm_ty.const_zero());
        }
        ret_llvm_ty.const_zero()
    }

    /// Call a function value (closure or bare fn ptr) with one argument. Returns the result as `ret_ty`.
    fn call_fn_val_with_arg(&mut self, fn_val: BasicValueEnum<'ctx>, arg: BasicValueEnum<'ctx>, ret_ty: &Type, fn_ctx: &mut FnCtx<'ctx, '_>) -> BasicValueEnum<'ctx> {
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let ret_llvm_ty = self.llvm_type(ret_ty);
        let arg_meta: BasicMetadataTypeEnum = arg.get_type().into();
        if fn_val.is_pointer_value() {
            let cls_ptr = fn_val.into_pointer_value();
            let cls_struct = self.context.struct_type(&[ptr_ty.into(), ptr_ty.into()], false);
            let fn_field = self.builder.build_struct_gep(cls_struct, cls_ptr, 0, "cfn1_p").unwrap();
            let fn_ptr = self.builder.build_load(ptr_ty, fn_field, "cfn1").unwrap().into_pointer_value();
            let env_field = self.builder.build_struct_gep(cls_struct, cls_ptr, 1, "cenv1_p").unwrap();
            let env_ptr = self.builder.build_load(ptr_ty, env_field, "cenv1").unwrap();
            let fn_type = ret_llvm_ty.fn_type(&[ptr_ty.into(), arg_meta], false);
            return self.builder
                .build_indirect_call(fn_type, fn_ptr, &[env_ptr.into(), arg.into()], "cfn1_call")
                .unwrap()
                .try_as_basic_value()
                .basic()
                .unwrap_or_else(|| ret_llvm_ty.const_zero());
        }
        ret_llvm_ty.const_zero()
    }

    /// Call the body expression (a closure or function) with a single argument.
    /// Uses the actual LLVM type of `arg` rather than the body's declared param type,
    /// so that TypeVar params don't cause type mismatches.
    fn call_body_with_arg(
        &mut self,
        body_expr: &TypedExpr,
        arg: BasicValueEnum<'ctx>,
        fn_ctx: &mut FnCtx<'ctx, '_>,
    ) {
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let arg_meta: BasicMetadataTypeEnum = arg.get_type().into();

        match body_expr {
            TypedExpr::LocalGet { slot, .. } => {
                // Check if this is a closure.
                if let Some(SlotStorage::Closure(cls_ptr)) = fn_ctx.slots.get(slot).cloned() {
                    let cls_struct_type = self.context.struct_type(&[ptr_ty.into(), ptr_ty.into()], false);
                    let fn_field = self.builder
                        .build_struct_gep(cls_struct_type, cls_ptr, 0, "cbody_fn_p")
                        .unwrap();
                    let fn_ptr = self.builder
                        .build_load(ptr_ty, fn_field, "cbody_fn")
                        .unwrap()
                        .into_pointer_value();
                    let env_field = self.builder
                        .build_struct_gep(cls_struct_type, cls_ptr, 1, "cbody_env_p")
                        .unwrap();
                    let env_ptr = self.builder
                        .build_load(ptr_ty, env_field, "cbody_env")
                        .unwrap();
                    let fn_type = self.context.void_type().fn_type(&[ptr_ty.into(), arg_meta], false);
                    self.builder
                        .build_indirect_call(fn_type, fn_ptr, &[env_ptr.into(), arg.into()], "for_body")
                        .unwrap();
                    return;
                }
                if let Some(SlotStorage::Value(fn_val)) = fn_ctx.slots.get(slot) {
                    if let BasicValueEnum::PointerValue(fn_ptr) = *fn_val {
                        let fn_ptr = fn_ptr;
                        let fn_type = self.context.void_type().fn_type(&[arg_meta], false);
                        self.builder
                            .build_indirect_call(fn_type, fn_ptr, &[arg.into()], "for_body")
                            .unwrap();
                        return;
                    }
                }
                // Global function slot.
                if let Some(llvm_fn) = self.global_fn_slots.get(slot).copied() {
                    self.builder
                        .build_call(llvm_fn, &[arg.into()], "for_body")
                        .unwrap();
                    return;
                }
            }
            TypedExpr::Function { params, body, captures, .. } => {
                if captures.is_empty() {
                    // Inline the lambda body directly into the current function context.
                    // This gives the body full access to all outer locals (including mutable vars
                    // at global scope depth 0 which are excluded from capture analysis).
                    if let Some(param) = params.first() {
                        let old = fn_ctx.slots.insert(param.slot, SlotStorage::Value(arg));
                        self.compile_expr(body, fn_ctx);
                        // Restore previous slot state.
                        if let Some(prev) = old {
                            fn_ctx.slots.insert(param.slot, prev);
                        } else {
                            fn_ctx.slots.remove(&param.slot);
                        }
                    }
                    return;
                }
                // Has captures — compile as a closure and call indirectly.
                let val = self.compile_closure(None, params, body, &Type::Null, captures, fn_ctx);
                let cls_ptr = val.into_pointer_value();
                let cls_struct_type = self.context.struct_type(&[ptr_ty.into(), ptr_ty.into()], false);
                let fn_field = self.builder
                    .build_struct_gep(cls_struct_type, cls_ptr, 0, "ibody_fn_p")
                    .unwrap();
                let fn_ptr = self.builder
                    .build_load(ptr_ty, fn_field, "ibody_fn")
                    .unwrap()
                    .into_pointer_value();
                let env_field = self.builder
                    .build_struct_gep(cls_struct_type, cls_ptr, 1, "ibody_env_p")
                    .unwrap();
                let env_ptr = self.builder
                    .build_load(ptr_ty, env_field, "ibody_env")
                    .unwrap();
                let fn_type = self.context.void_type().fn_type(&[ptr_ty.into(), arg_meta], false);
                self.builder
                    .build_indirect_call(fn_type, fn_ptr, &[env_ptr.into(), arg.into()], "ibody_call")
                    .unwrap();
                return;
            }
            _ => {}
        }
        // Fallback: compile and call.
        let body_val = self.compile_expr(body_expr, fn_ctx);
        if let BasicValueEnum::PointerValue(fn_ptr) = body_val {
            let fn_type = self.context.void_type().fn_type(&[arg_meta], false);
            self.builder
                .build_indirect_call(fn_type, fn_ptr, &[arg.into()], "for_body")
                .unwrap();
        }
    }

    /// Infer a Lin type from the LLVM representation of a value.
    /// Used when we have a TypeVar-typed SSA value that has been concretely typed by coercion.
    fn infer_type_from_llvm_value(&self, val: &BasicValueEnum<'ctx>) -> Type {
        match val {
            BasicValueEnum::IntValue(iv) => match iv.get_type().get_bit_width() {
                1 => Type::Bool,
                8 => Type::Int8,
                16 => Type::Int16,
                32 => Type::Int32,
                64 => Type::Int64,
                _ => Type::Int64,
            },
            BasicValueEnum::FloatValue(fv) => {
                if fv.get_type() == self.context.f32_type() { Type::Float32 } else { Type::Float64 }
            }
            BasicValueEnum::PointerValue(_) => Type::TypeVar(0), // still unknown
            _ => Type::TypeVar(0),
        }
    }

    /// Reverse-map an LLVM BasicValueEnum to a Lin type (for TypeVar substitution).
    fn basic_value_to_type(&self, val: BasicValueEnum<'ctx>) -> Type {
        match val {
            BasicValueEnum::IntValue(iv) => {
                match iv.get_type().get_bit_width() {
                    1 => Type::Bool,
                    8 => Type::Int8,
                    16 => Type::Int16,
                    32 => Type::Int32,
                    64 => Type::Int64,
                    _ => Type::Int64,
                }
            }
            BasicValueEnum::FloatValue(fv) => {
                if fv.get_type() == self.context.f32_type() { Type::Float32 } else { Type::Float64 }
            }
            BasicValueEnum::PointerValue(_) => Type::Str, // approximation
            _ => Type::Null,
        }
    }

    // -------------------------------------------------------------------------
    // Objects
    // -------------------------------------------------------------------------

    fn compile_make_object(
        &mut self,
        fields: &[(String, TypedExpr)],
        fn_ctx: &mut FnCtx<'ctx, '_>,
    ) -> BasicValueEnum<'ctx> {
        // Compile as a dynamic LinObject (key-value array).
        let i32_ty = self.context.i32_type();
        let cap = i32_ty.const_int(fields.len().max(4) as u64, false);
        let obj_ptr = self.builder
            .build_call(self.rt_object_alloc, &[cap.into()], "obj")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();

        for (key, val_expr) in fields.iter() {
            let val = self.compile_expr(val_expr, fn_ctx);
            let val_ty = val_expr.ty();
            // Compile key as a LinString.
            let key_str = self.compile_string_lit(key).into_pointer_value();
            // For TypeVar values that are already TaggedVal* (pointer), pass directly.
            // For TypeVar values that are concrete (non-pointer), determine the actual type.
            let tagged: PointerValue = if matches!(val_ty, Type::TypeVar(_)) && val.is_pointer_value() {
                val.into_pointer_value()
            } else if matches!(val_ty, Type::TypeVar(_)) {
                // TypeVar but non-pointer: detect actual type from LLVM value.
                let effective_ty = self.infer_type_from_llvm_value(&val);
                self.build_tagged_val_alloca(&val, &effective_ty)
            } else {
                self.build_tagged_val_alloca(&val, &val_ty)
            };
            self.builder.build_call(
                self.rt_object_set,
                &[obj_ptr.into(), key_str.into(), tagged.into()],
                "",
            ).unwrap();
        }

        obj_ptr.into()
    }

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

    fn compile_field_get(
        &mut self,
        object: &TypedExpr,
        field: &str,
        result_type: &Type,
        fn_ctx: &mut FnCtx<'ctx, '_>,
    ) -> BasicValueEnum<'ctx> {
        let obj_val = self.compile_expr(object, fn_ctx);
        let obj_ty = object.ty();

        if !obj_val.is_pointer_value() {
            return self.llvm_type(result_type).const_zero();
        }
        let raw_ptr = obj_val.into_pointer_value();

        // For TypeVar/Union objects: obj_val is a TaggedVal*, unbox first.
        let obj_ptr = if matches!(obj_ty, Type::TypeVar(_) | Type::Union(_)) {
            self.builder
                .build_call(self.rt_unbox_ptr, &[raw_ptr.into()], "fget_obj")
                .unwrap()
                .try_as_basic_value().unwrap_basic().into_pointer_value()
        } else {
            raw_ptr
        };

        let key_str = self.compile_string_lit(field).into_pointer_value();
        let entry_ptr = self.builder
            .build_call(self.rt_object_get, &[obj_ptr.into(), key_str.into()], "fget_p")
            .unwrap()
            .try_as_basic_value().unwrap_basic().into_pointer_value();

        // entry_ptr points to a TaggedVal (or null if not found).
        // For TypeVar result: return the TaggedVal* directly (caller dispatches at runtime).
        if matches!(result_type, Type::TypeVar(_)) {
            return entry_ptr.into();
        }
        // Extract the payload as the result type.
        self.load_tagged_val_payload(entry_ptr, result_type, &obj_ty)
    }

    fn compile_index(
        &mut self,
        object: &TypedExpr,
        key: &TypedExpr,
        result_type: &Type,
        fn_ctx: &mut FnCtx<'ctx, '_>,
    ) -> BasicValueEnum<'ctx> {
        let obj_val = self.compile_expr(object, fn_ctx);
        let key_val = self.compile_expr(key, fn_ctx);
        let obj_ty = object.ty();

        match &obj_ty {
            Type::Array(_) | Type::FixedArray(_) => {
                // Array index: key must be integer, bounds-checked at runtime.
                let idx = if key_val.is_int_value() {
                    self.builder
                        .build_int_s_extend_or_bit_cast(key_val.into_int_value(), self.context.i64_type(), "idx")
                        .unwrap()
                } else {
                    self.context.i64_type().const_int(0, false)
                };
                let elem_ptr = self
                    .builder
                    .build_call(self.rt_array_get, &[obj_val.into(), idx.into()], "aref")
                    .unwrap()
                    .try_as_basic_value()
                    .unwrap_basic()
                    .into_pointer_value();

                self.load_array_element(elem_ptr, result_type)
            }
            Type::Object(_) => {
                // Known object type: obj_val is already a LinObject*.
                let key_ptr = if key_val.is_pointer_value() {
                    key_val.into_pointer_value()
                } else {
                    return self.llvm_type(result_type).const_zero();
                };
                let obj_ptr = if obj_val.is_pointer_value() {
                    obj_val.into_pointer_value()
                } else {
                    return self.llvm_type(result_type).const_zero();
                };
                let entry_ptr = self.builder
                    .build_call(self.rt_object_get, &[obj_ptr.into(), key_ptr.into()], "oindex_p")
                    .unwrap()
                    .try_as_basic_value().unwrap_basic().into_pointer_value();
                if matches!(result_type, Type::TypeVar(_)) {
                    return entry_ptr.into();
                }
                self.load_tagged_val_payload(entry_ptr, result_type, &obj_ty)
            }
            Type::TypeVar(_) | Type::Union(_) => {
                // Dynamic access: obj_val is a TaggedVal*. Unbox to get the actual LinObject*.
                let key_ptr = if key_val.is_pointer_value() {
                    key_val.into_pointer_value()
                } else {
                    return self.llvm_type(result_type).const_zero();
                };
                let tagged_ptr = if obj_val.is_pointer_value() {
                    obj_val.into_pointer_value()
                } else {
                    return self.llvm_type(result_type).const_zero();
                };
                // Unbox to get the actual object pointer.
                let obj_ptr = self.builder
                    .build_call(self.rt_unbox_ptr, &[tagged_ptr.into()], "obj_ptr")
                    .unwrap()
                    .try_as_basic_value().unwrap_basic().into_pointer_value();
                let entry_ptr = self.builder
                    .build_call(self.rt_object_get, &[obj_ptr.into(), key_ptr.into()], "oindex_p")
                    .unwrap()
                    .try_as_basic_value().unwrap_basic().into_pointer_value();
                // For TypeVar result: return the TaggedVal* directly.
                if matches!(result_type, Type::TypeVar(_)) {
                    return entry_ptr.into();
                }
                self.load_tagged_val_payload(entry_ptr, result_type, &obj_ty)
            }
            _ => {
                // Unknown type: return null/zero.
                self.llvm_type(result_type).const_zero()
            }
        }
    }

    // -------------------------------------------------------------------------
    // Match / pattern matching
    // -------------------------------------------------------------------------

    fn compile_match(
        &mut self,
        scrutinee: &TypedExpr,
        arms: &[TypedMatchArm],
        result_type: &Type,
        fn_ctx: &mut FnCtx<'ctx, '_>,
    ) -> BasicValueEnum<'ctx> {
        let scrut_val = self.compile_expr(scrutinee, fn_ctx);
        let scrut_ty = scrutinee.ty();

        let merge_block = self.context.append_basic_block(fn_ctx.llvm_fn, "match_merge");
        let result_llvm_ty = self.llvm_type(result_type);

        let mut incoming: Vec<(BasicValueEnum<'ctx>, inkwell::basic_block::BasicBlock<'ctx>)> =
            Vec::new();

        let mut next_check = self.context.append_basic_block(fn_ctx.llvm_fn, "arm_0_check");
        self.builder.build_unconditional_branch(next_check).unwrap();

        for (arm_idx, arm) in arms.iter().enumerate() {
            self.builder.position_at_end(next_check);

            let body_block = self
                .context
                .append_basic_block(fn_ctx.llvm_fn, &format!("arm_{}_body", arm_idx));
            next_check = self
                .context
                .append_basic_block(fn_ctx.llvm_fn, &format!("arm_{}_next", arm_idx + 1));

            let matched = match &arm.pattern {
                TypedMatchPattern::Else => {
                    self.context.bool_type().const_int(1, false)
                }
                TypedMatchPattern::Is(pat) => {
                    self.compile_pattern_match(scrut_val, pat, &scrut_ty, fn_ctx)
                }
                TypedMatchPattern::Has(pat) => {
                    self.compile_pattern_match(scrut_val, pat, &scrut_ty, fn_ctx)
                }
            };

            // Apply guard if present.
            let final_matched = if let Some(guard) = &arm.guard {
                let guard_val = self.compile_expr(guard, fn_ctx);
                self.builder
                    .build_and(matched, guard_val.into_int_value(), "guarded")
                    .unwrap()
            } else {
                matched
            };

            self.builder
                .build_conditional_branch(final_matched, body_block, next_check)
                .unwrap();

            // Arm body
            self.builder.position_at_end(body_block);
            let body_val = self.compile_expr(&arm.body, fn_ctx);
            let body_end = self.builder.get_insert_block().unwrap();
            if !body_end.get_terminator().is_some() {
                self.builder.build_unconditional_branch(merge_block).unwrap();
                incoming.push((body_val, body_end));
            }
        }

        // After last arm: if no arm matched, panic.
        self.builder.position_at_end(next_check);
        let panic_msg = self.compile_string_lit("match: no arm matched");
        let zero = self.context.i32_type().const_zero();
        self.builder
            .build_call(self.rt_panic, &[panic_msg.into(), zero.into(), zero.into()], "")
            .unwrap();
        self.builder.build_unreachable().unwrap();

        // Merge
        self.builder.position_at_end(merge_block);
        if incoming.is_empty() {
            return result_llvm_ty.const_zero();
        }
        if incoming.len() == 1 {
            return incoming[0].0;
        }

        let phi = self
            .builder
            .build_phi(result_llvm_ty, "match_result")
            .unwrap();
        for (val, block) in &incoming {
            phi.add_incoming(&[(val, *block)]);
        }
        phi.as_basic_value()
    }

    fn compile_pattern_match(
        &mut self,
        scrut_val: BasicValueEnum<'ctx>,
        pattern: &TypedPattern,
        scrut_ty: &Type,
        fn_ctx: &mut FnCtx<'ctx, '_>,
    ) -> inkwell::values::IntValue<'ctx> {
        let is_union_scrut = Self::is_union_type(scrut_ty);

        match pattern {
            TypedPattern::TypeCheck(ty, _) => {
                if is_union_scrut {
                    // Runtime type tag check.
                    if *ty == Type::Null {
                        // Null is represented as null pointer.
                        let ptr = scrut_val.into_pointer_value();
                        let i64_ty = self.context.i64_type();
                        let as_int = self.builder.build_ptr_to_int(ptr, i64_ty, "pti").unwrap();
                        let zero = i64_ty.const_zero();
                        self.builder.build_int_compare(inkwell::IntPredicate::EQ, as_int, zero, "isnull").unwrap()
                    } else {
                        let tag = Self::type_tag(ty);
                        let expected = self.context.i8_type().const_int(tag as u64, false);
                        let actual = self.builder
                            .build_call(self.rt_get_tag, &[scrut_val.into()], "tag")
                            .unwrap()
                            .try_as_basic_value()
                            .unwrap_basic()
                            .into_int_value();
                        self.builder.build_int_compare(inkwell::IntPredicate::EQ, actual, expected, "tagcmp").unwrap()
                    }
                } else if scrut_ty == ty {
                    self.context.bool_type().const_int(1, false)
                } else if *ty == Type::Null && *scrut_ty == Type::Null {
                    self.context.bool_type().const_int(1, false)
                } else {
                    self.context.bool_type().const_int(0, false)
                }
            }
            TypedPattern::Literal(lit_expr) => {
                // For union scrutinee with literal, unbox first if needed.
                let (effective_val, effective_ty) = if is_union_scrut {
                    let lit_ty = lit_expr.ty();
                    let unboxed = self.unbox_value(scrut_val, &lit_ty);
                    (unboxed, lit_ty)
                } else {
                    (scrut_val, scrut_ty.clone())
                };
                let lit_val = self.compile_expr(lit_expr, fn_ctx);
                let result = self.compile_eq(effective_val, lit_val, &effective_ty, false);
                result.into_int_value()
            }
            TypedPattern::Binding(slot, ty, _) => {
                // Bind the scrutinee to a new slot and return true.
                // For union scrutinees, store as-is (TypeVar typed slot).
                fn_ctx.slots.insert(*slot, SlotStorage::Value(scrut_val));
                self.context.bool_type().const_int(1, false)
            }
            TypedPattern::Wildcard(_) => {
                self.context.bool_type().const_int(1, false)
            }
            TypedPattern::Object { fields, .. } => {
                // Get the LinObject* — either unbox from tagged union or use directly.
                let obj_ptr = if is_union_scrut {
                    // For union/TypeVar, check tag first: must be TAG_OBJECT (7).
                    let tag_obj = self.context.i8_type().const_int(7, false);
                    let actual_tag = self.builder
                        .build_call(self.rt_get_tag, &[scrut_val.into()], "obj_tag")
                        .unwrap().try_as_basic_value().unwrap_basic().into_int_value();
                    let is_obj = self.builder.build_int_compare(inkwell::IntPredicate::EQ, actual_tag, tag_obj, "is_obj").unwrap();
                    // Only proceed if tag matches; otherwise return false immediately.
                    let then_block = self.context.append_basic_block(fn_ctx.llvm_fn, "obj_has_check");
                    let else_block = self.context.append_basic_block(fn_ctx.llvm_fn, "obj_not_obj");
                    let merge_block = self.context.append_basic_block(fn_ctx.llvm_fn, "obj_has_merge");
                    self.builder.build_conditional_branch(is_obj, then_block, else_block).unwrap();

                    self.builder.position_at_end(else_block);
                    self.builder.build_unconditional_branch(merge_block).unwrap();

                    self.builder.position_at_end(then_block);
                    let unboxed = self.builder.build_call(self.rt_unbox_ptr, &[scrut_val.into()], "objptr")
                        .unwrap().try_as_basic_value().unwrap_basic().into_pointer_value();

                    // Check all required keys and literal values.
                    let mut all_ok = self.context.bool_type().const_int(1, false);
                    for pf in fields {
                        if !pf.key.is_empty() {
                            let key_str = self.compile_string_lit(&pf.key).into_pointer_value();
                            let has = self.builder
                                .build_call(self.rt_object_has, &[unboxed.into(), key_str.into()], "has")
                                .unwrap()
                                .try_as_basic_value().unwrap_basic().into_int_value();
                            let has_bool = self.builder.build_int_truncate(has, self.context.bool_type(), "has_b").unwrap();
                            all_ok = self.builder.build_and(all_ok, has_bool, "all_ok").unwrap();

                            // If there's a literal value pattern, also check the value equals it.
                            if let Some(val_pat) = &pf.value_pattern {
                                let pat_ty = val_pat.ty();
                                let entry_ptr = self.builder
                                    .build_call(self.rt_object_get, &[unboxed.into(), key_str.into()], "vpat_p")
                                    .unwrap()
                                    .try_as_basic_value().unwrap_basic().into_pointer_value();
                                // Load field value using the actual pattern type (not pf.ty which may be TypeVar).
                                let field_val = self.load_tagged_val_payload(entry_ptr, &pat_ty, scrut_ty);
                                let expected_val = self.compile_expr(val_pat, fn_ctx);
                                let eq_result = self.compile_eq(field_val, expected_val, &pat_ty, false);
                                let eq_bool = eq_result.into_int_value();
                                all_ok = self.builder.build_and(all_ok, eq_bool, "all_ok_v").unwrap();
                            }
                        }
                    }

                    // Bind field values.
                    for pf in fields {
                        if let Some(binding_slot) = pf.binding_slot {
                            let key_str = self.compile_string_lit(&pf.key).into_pointer_value();
                            let entry_ptr = self.builder
                                .build_call(self.rt_object_get, &[unboxed.into(), key_str.into()], "fget_p")
                                .unwrap()
                                .try_as_basic_value().unwrap_basic().into_pointer_value();
                            // For TypeVar-typed bindings, bind the TaggedVal* directly
                            // so callers can read the tag and dispatch at runtime.
                            let field_val: BasicValueEnum = if matches!(pf.ty, Type::TypeVar(_)) {
                                entry_ptr.into()
                            } else {
                                self.load_tagged_val_payload(entry_ptr, &pf.ty, scrut_ty)
                            };
                            fn_ctx.slots.insert(binding_slot, SlotStorage::Value(field_val));
                        }
                    }

                    let then_end = self.builder.get_insert_block().unwrap();
                    self.builder.build_unconditional_branch(merge_block).unwrap();

                    self.builder.position_at_end(merge_block);
                    let phi = self.builder.build_phi(self.context.bool_type(), "obj_match").unwrap();
                    phi.add_incoming(&[(&all_ok, then_end), (&self.context.bool_type().const_int(0, false), else_block)]);
                    return phi.as_basic_value().into_int_value();
                } else if scrut_val.is_pointer_value() {
                    scrut_val.into_pointer_value()
                } else {
                    return self.context.bool_type().const_int(0, false);
                };

                // Check all required keys and literal values using lin_object_has.
                let mut all_ok = self.context.bool_type().const_int(1, false);
                for pf in fields {
                    if !pf.key.is_empty() {
                        let key_str = self.compile_string_lit(&pf.key).into_pointer_value();
                        let has = self.builder
                            .build_call(self.rt_object_has, &[obj_ptr.into(), key_str.into()], "has")
                            .unwrap()
                            .try_as_basic_value().unwrap_basic().into_int_value();
                        let has_bool = self.builder.build_int_truncate(has, self.context.bool_type(), "has_b").unwrap();
                        all_ok = self.builder.build_and(all_ok, has_bool, "all_ok").unwrap();

                        if let Some(val_pat) = &pf.value_pattern {
                            let pat_ty = val_pat.ty();
                            let entry_ptr = self.builder
                                .build_call(self.rt_object_get, &[obj_ptr.into(), key_str.into()], "vpat_p")
                                .unwrap()
                                .try_as_basic_value().unwrap_basic().into_pointer_value();
                            let field_val = self.load_tagged_val_payload(entry_ptr, &pat_ty, scrut_ty);
                            let expected_val = self.compile_expr(val_pat, fn_ctx);
                            let eq_result = self.compile_eq(field_val, expected_val, &pat_ty, false);
                            all_ok = self.builder.build_and(all_ok, eq_result.into_int_value(), "all_ok_v").unwrap();
                        }
                    }
                }

                // Bind field values using lin_object_get.
                for pf in fields {
                    if let Some(binding_slot) = pf.binding_slot {
                        let key_str = self.compile_string_lit(&pf.key).into_pointer_value();
                        let entry_ptr = self.builder
                            .build_call(self.rt_object_get, &[obj_ptr.into(), key_str.into()], "fget_p")
                            .unwrap()
                            .try_as_basic_value().unwrap_basic().into_pointer_value();
                        let field_val: BasicValueEnum = if matches!(pf.ty, Type::TypeVar(_)) {
                            entry_ptr.into()
                        } else {
                            self.load_tagged_val_payload(entry_ptr, &pf.ty, scrut_ty)
                        };
                        fn_ctx.slots.insert(binding_slot, SlotStorage::Value(field_val));
                    }
                }

                all_ok
            }
            TypedPattern::Array { .. } => {
                self.context.bool_type().const_int(1, false)
            }
        }
    }

    fn compile_is_check(
        &mut self,
        expr: &TypedExpr,
        pattern: &TypedPattern,
        fn_ctx: &mut FnCtx<'ctx, '_>,
    ) -> BasicValueEnum<'ctx> {
        let val = self.compile_expr(expr, fn_ctx);
        let ty = expr.ty();
        let result = self.compile_pattern_match(val, pattern, &ty, fn_ctx);
        result.into()
    }

    fn compile_has_check(
        &mut self,
        expr: &TypedExpr,
        pattern: &TypedPattern,
        fn_ctx: &mut FnCtx<'ctx, '_>,
    ) -> BasicValueEnum<'ctx> {
        let val = self.compile_expr(expr, fn_ctx);
        let ty = expr.ty();
        let result = self.compile_pattern_match(val, pattern, &ty, fn_ctx);
        result.into()
    }
}
