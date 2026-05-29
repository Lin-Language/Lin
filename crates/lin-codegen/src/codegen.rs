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
use crate::coverage::{CoverageEmitter, FnCovInfo};

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
    /// Module-level function slots (slot → FunctionValue). Checked as fallback when a slot
    /// is not found in `slots`. Kept separate so local param slots never shadow module slots
    /// (params and module-level functions share the same slot-number space within a module).
    module_fn_slots: HashMap<usize, FunctionValue<'ctx>>,
    llvm_fn: FunctionValue<'ctx>,
    /// Pointer to the environment struct for this closure (if any).
    env_ptr: Option<PointerValue<'ctx>>,
    /// For TCO: if this function is being compiled with the loop-transform,
    /// this holds the entry block to branch back to and the phi slots.
    tco: Option<TcoState<'ctx, 'a>>,
    /// Slots whose storage holds a TaggedVal* pointer (not a concrete scalar).
    /// When LocalGet requests a narrowed concrete type from these, we must unbox.
    pointer_slots: std::collections::HashSet<usize>,
    /// Slots for `var` bindings that are captured mutably by inner closures.
    /// These are heap-allocated (via lin_alloc) instead of stack-allocated (alloca),
    /// so the value survives the creating function's stack frame.
    heap_var_slots: std::collections::HashSet<usize>,
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
    rt_string_from_bytes: FunctionValue<'ctx>,
    rt_string_length: FunctionValue<'ctx>,
    rt_string_eq: FunctionValue<'ctx>,
    rt_print: FunctionValue<'ctx>,
    rt_panic: FunctionValue<'ctx>,
    rt_array_alloc: FunctionValue<'ctx>,
    rt_array_push: FunctionValue<'ctx>,
    rt_array_get: FunctionValue<'ctx>,
    rt_array_get_tagged: FunctionValue<'ctx>,
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
    // Single-allocation multi-part string build
    rt_string_build_n: FunctionValue<'ctx>,
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
    global_fn_slots: HashMap<usize, FunctionValue<'ctx>>,
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
    global_val_slots: HashMap<usize, inkwell::values::GlobalValue<'ctx>>,
    /// Module-level slot map active during register_import. Closures compiled inside
    /// imported module bodies use this to resolve sibling function calls.
    current_module_slots: HashMap<usize, FunctionValue<'ctx>>,
    /// Coverage emitter: Some if compiling with coverage instrumentation.
    pub coverage: Option<CoverageEmitter<'ctx>>,
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
        let rt_array_get_tagged = module.add_function(
            "lin_array_get_tagged",
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
        // lin_string_build_n(parts: ptr, n: i32) -> ptr — single-allocation multi-part concat
        let rt_string_build_n = module.add_function(
            "lin_string_build_n",
            string_ptr_type.fn_type(&[ptr_type.into(), i32_type.into()], false),
            None,
        );
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
            rt_array_get_tagged,
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
            rt_string_build_n,
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
            global_fn_slots: HashMap::new(),
            closure_count: 0,
            imported_fns: HashMap::new(),
            imported_val_wrappers: HashMap::new(),
            foreign_lib_paths: Vec::new(),
            global_val_slots: HashMap::new(),
            current_module_slots: HashMap::new(),
            coverage: if coverage_enabled {
                // Source path is set by compile_module; start with empty path.
                Some(CoverageEmitter::new(String::new()))
            } else {
                None
            },
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

        // Collect the module-local slot→fn mapping. This is passed to compile_function_body
        // so intra-module calls (e.g. clamp calling max/min) resolve without polluting
        // the global slot map.
        let mut module_slots: HashMap<usize, FunctionValue<'ctx>> = HashMap::new();
        // Also populate current_module_slots so closures compiled inside this module's
        // function bodies (via compile_closure) can resolve sibling function calls.
        self.current_module_slots.clear();

        // First pass: declare foreign (lin-runtime) bindings so module function bodies can call them.
        // These are stored in module_slots only (not global_fn_slots) because slot numbers are
        // module-local and would conflict with other modules' slot numbers in the global map.
        for stmt in &module.statements {
            if let TypedStmt::ForeignImport { bindings, .. } = stmt {
                for binding in bindings.iter() {
                    if !binding.valid { continue; }
                    if let Type::Function { params, ret } = &binding.ty {
                        let param_types: Vec<inkwell::types::BasicMetadataTypeEnum> = params.iter()
                            .map(|p| self.llvm_type(p).into())
                            .collect();
                        let fn_type = self.llvm_type(ret).fn_type(&param_types, false);
                        let llvm_fn = self.get_or_declare_fn(&binding.name, fn_type);
                        module_slots.insert(binding.slot, llvm_fn);
                        self.current_module_slots.insert(binding.slot, llvm_fn);
                    }
                }
            }
        }

        // Resolve cross-module imports (e.g. `import { map } from "std/array"` inside std/object).
        // These are already compiled; we just need their slot→fn mapping available inside
        // this module's function bodies.
        for stmt in &module.statements {
            if let TypedStmt::Import { path: import_path, bindings, .. } = stmt {
                let known_intrinsics = ["lin_print", "lin_to_string", "lin_length", "lin_push",
                    "lin_array_set", "lin_keys", "lin_object_set", "lin_for", "lin_while", "lin_iter", "lin_range", "lin_map",
                    "lin_filter", "lin_reduce",
                    "lin_async", "lin_await", "lin_parallel", "lin_race", "lin_timeout", "lin_retry",
                    "lin_thread_pool", "lin_worker", "lin_request", "lin_message", "lin_close",
                    "lin_exit", "lin_value_key", "lin_array_allocate", "lin_array_allocate_filled"];
                for binding in bindings.iter() {
                    let key = (import_path.clone(), binding.name.clone());
                    if let Some(&llvm_fn) = self.imported_fns.get(&key) {
                        module_slots.insert(binding.slot, llvm_fn);
                        self.current_module_slots.insert(binding.slot, llvm_fn);
                    } else if known_intrinsics.contains(&binding.name.as_str()) {
                        self.intrinsic_slots.insert(binding.slot, binding.name.clone());
                    }
                }
            }
        }

        // Use "module_path/name" as the LLVM symbol to avoid collisions between modules
        // that export functions with the same name (e.g. std/string and std/array both export indexOf).
        let module_key = path.replace("/", "_").replace("-", "_");
        for stmt in &module.statements {
            if let TypedStmt::Val {
                slot,
                value: TypedExpr::Function { name: Some(name), params, ret_type, .. },
                ..
            } = stmt {
                let llvm_name = format!("{}_{}", module_key, name);
                let llvm_fn = self.declare_function(&llvm_name, params, ret_type);
                // Use the unqualified name in named_fns for TCO detection (per-module scope is fine).
                self.named_fns.insert(name.clone(), llvm_fn);
                self.imported_fns.insert((path.to_string(), name.clone()), llvm_fn);
                module_slots.insert(*slot, llvm_fn);
                self.current_module_slots.insert(*slot, llvm_fn);
            }
        }
        // Compile the bodies of imported functions, passing the module-local slot map
        // so sibling calls resolve without touching global state.
        for stmt in &module.statements {
            if let TypedStmt::Val {
                value: TypedExpr::Function { name: Some(name), params, body, ret_type, captures, span: _, .. },
                ..
            } = stmt {
                if captures.is_empty() {
                    if let Some(&llvm_fn) = self.named_fns.get(name.as_str()) {
                        if llvm_fn.count_basic_blocks() == 0 {
                            self.compile_function_body(llvm_fn, params, body, ret_type, &[], name, &module_slots, false);
                        }
                    }
                }
            }
        }

        // Generate wrapper functions for non-function exported vals (e.g. `export val PI = lin_math_pi()`).
        // Each wrapper is a zero-arg LLVM function `{module_key}_{name}__val()` that computes and returns
        // the value. The caller (TypedStmt::Import handling) will call the wrapper to get the value.
        for stmt in &module.statements {
            if let TypedStmt::Val { slot: _, value, ty, name: Some(name), .. } = stmt {
                // Skip function vals — already handled above.
                if matches!(value, TypedExpr::Function { .. }) { continue; }
                let ret_llvm_ty = self.llvm_type(ty);
                let wrapper_name = format!("{}_{}__val", module_key, name);
                // Only generate once.
                if self.module.get_function(&wrapper_name).is_some() { continue; }
                let fn_ty = ret_llvm_ty.fn_type(&[], false);
                let wrapper_fn = self.module.add_function(&wrapper_name, fn_ty, None);
                let entry_bb = self.context.append_basic_block(wrapper_fn, "entry");
                let saved_pos = self.builder.get_insert_block();
                self.builder.position_at_end(entry_bb);
                // Build a temporary FnCtx for evaluating the val expression.
                let mut tmp_ctx = FnCtx {
                    slots: module_slots.iter().map(|(k, v)| (*k, SlotStorage::Value(v.as_global_value().as_pointer_value().into()))).collect(),
                    module_fn_slots: module_slots.clone(),
                    llvm_fn: wrapper_fn,
                    env_ptr: None,
                    tco: None,
                    pointer_slots: std::collections::HashSet::new(),
                    heap_var_slots: std::collections::HashSet::new(),
                };
                let result = self.compile_expr(value, &mut tmp_ctx);
                self.builder.build_return(Some(&result)).unwrap();
                if let Some(bb) = saved_pos {
                    self.builder.position_at_end(bb);
                }
                // Store the wrapper in imported_val_wrappers so TypedStmt::Import can call it.
                self.imported_val_wrappers.insert((path.to_string(), name.clone()), wrapper_fn);
            }
        }

        // Clear current_module_slots now that this module's compilation is done.
        self.current_module_slots.clear();
    }

    /// Register an imported module with its source for coverage instrumentation.
    /// Call this instead of register_import when coverage is enabled and the module is
    /// a user-defined (non-stdlib) file that should be tracked.
    pub fn register_import_with_source(
        &mut self,
        path: &str,
        module: &TypedModule,
        source_path: &str,
        source_text: &str,
    ) {
        // Add the source file to coverage emitter BEFORE compiling, so we have a file_idx.
        let file_idx = if let Some(cov) = &mut self.coverage {
            cov.add_source_file(source_path, source_text)
        } else {
            0
        };

        // Register the import normally (compiles function bodies).
        self.register_import(path, module);

        // Now emit coverage globals for each named function in this module.
        if self.coverage.is_some() {
            for stmt in &module.statements {
                if let lin_check::typed_ir::TypedStmt::Val {
                    value: lin_check::typed_ir::TypedExpr::Function { name: Some(name), params: _, ret_type: _, span, .. },
                    ..
                } = stmt {
                    let module_key = path.replace("/", "_").replace("-", "_");
                    let llvm_name = format!("{}_{}", module_key, name);
                    if let Some(&llvm_fn) = self.named_fns.get(name.as_str()) {
                        let cov = self.coverage.as_mut().unwrap();
                        let (start_line, start_col) = cov.offset_to_line_col_in(file_idx as usize, span.start);
                        let (end_line, end_col) = cov.offset_to_line_col_in(file_idx as usize, span.end);
                        let info = FnCovInfo {
                            name: llvm_name.clone(),
                            file_idx,
                            start_line,
                            start_col,
                            end_line,
                            end_col,
                        };
                        let profc = cov.emit_function_globals(self.context, &self.module, info);
                        // Insert counter increment into the compiled function.
                        let i64_type = self.context.i64_type();
                        let i32_type = self.context.i32_type();
                        if let Some(entry_bb) = llvm_fn.get_first_basic_block() {
                            let first_inst = entry_bb.get_first_instruction();
                            if let Some(inst) = first_inst {
                                self.builder.position_before(&inst);
                            } else {
                                self.builder.position_at_end(entry_bb);
                            }
                            let counter_type = i64_type.array_type(1);
                            let counter_ptr = profc.as_pointer_value();
                            let elem_ptr = unsafe {
                                self.builder.build_in_bounds_gep(
                                    counter_type,
                                    counter_ptr,
                                    &[i32_type.const_int(0, false).into(), i32_type.const_int(0, false).into()],
                                    "profc_ptr",
                                ).unwrap()
                            };
                            let old = self.builder.build_load(i64_type, elem_ptr, "profc_old").unwrap().into_int_value();
                            let new_val = self.builder.build_int_add(old, i64_type.const_int(1, false), "profc_inc").unwrap();
                            self.builder.build_store(elem_ptr, new_val).unwrap();
                        }
                    }
                }
            }
        }
    }

    /// Set source file path and text for coverage instrumentation.
    /// Must be called before compile_module if coverage is enabled.
    pub fn set_source(&mut self, source_path: &str, source_text: &str) {
        if let Some(cov) = &mut self.coverage {
            // Update the main source file (index 0).
            if cov.source_files.is_empty() {
                cov.source_files.push(source_path.to_string());
                cov.source_texts.push(source_text.to_string());
            } else {
                cov.source_files[0] = source_path.to_string();
                cov.source_texts[0] = source_text.to_string();
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
            module_fn_slots: HashMap::new(),
            llvm_fn: main_fn,
            env_ptr: None,
            tco: None,
            pointer_slots: std::collections::HashSet::new(),
            heap_var_slots: std::collections::HashSet::new(),
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
        // For coverage, also collect per-function span info and emit __profc/__profd globals.
        let mut fn_coverage_globals: HashMap<String, inkwell::values::GlobalValue<'ctx>> = HashMap::new();
        for stmt in &module.statements {
            if let TypedStmt::Val {
                value: TypedExpr::Function { name: Some(name), params, body, ret_type, captures, span, .. },
                ..
            } = stmt
            {
                if captures.is_empty() {
                    // Emit coverage globals for this function (if coverage enabled).
                    if self.coverage.is_some() {
                        let cov = self.coverage.as_mut().unwrap();
                        let file_idx = 0u32; // main module source is always index 0
                        let (start_line, start_col) = cov.offset_to_line_col_in(file_idx as usize, span.start);
                        let (end_line, end_col) = cov.offset_to_line_col_in(file_idx as usize, span.end);
                        let info = FnCovInfo {
                            name: name.clone(),
                            file_idx,
                            start_line,
                            start_col,
                            end_line,
                            end_col,
                        };
                        let profc = cov.emit_function_globals(self.context, &self.module, info);
                        fn_coverage_globals.insert(name.clone(), profc);
                    }

                    let llvm_fn = *self.named_fns.get(name.as_str()).unwrap();
                    let profc_global = fn_coverage_globals.get(name.as_str()).copied();
                    self.compile_function_body_with_coverage(
                        llvm_fn, params, body, ret_type, &[], name, &HashMap::new(), false,
                        profc_global,
                    );
                }
            }
        }

        // Finalize coverage globals (emit __covrec, __llvm_coverage_mapping, __llvm_prf_nm).
        if let Some(cov) = self.coverage.take() {
            cov.finalize(self.context, &self.module);
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

    /// True if the type is stored as a TaggedVal* pointer at runtime (union/dynamic).
    fn is_pointer_stored_type(ty: &Type) -> bool {
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

    /// Collect all slots captured mutably (`var` captures) within a typed expression tree.
    /// Stops at nested function boundaries (does not recurse into inner Function nodes
    /// after collecting their captures, because inner functions have their own scope).
    fn collect_mutable_capture_slots(expr: &TypedExpr, out: &mut std::collections::HashSet<usize>) {
        match expr {
            TypedExpr::Function { captures, body, .. } => {
                for cap in captures {
                    if cap.is_mutable {
                        out.insert(cap.outer_slot);
                    }
                }
                // Don't recurse deeper into the body — inner closures have their own capture lists.
            }
            TypedExpr::Block { stmts, expr, .. } => {
                for s in stmts {
                    match s {
                        TypedStmt::Val { value, .. } | TypedStmt::Var { value, .. } => {
                            Self::collect_mutable_capture_slots(value, out);
                        }
                        TypedStmt::Expr(e) => Self::collect_mutable_capture_slots(e, out),
                        TypedStmt::Destructure { value, .. } => Self::collect_mutable_capture_slots(value, out),
                        TypedStmt::ArrayDestructure { value, .. } => Self::collect_mutable_capture_slots(value, out),
                        _ => {}
                    }
                }
                Self::collect_mutable_capture_slots(expr, out);
            }
            TypedExpr::If { cond, then_br, else_br, .. } => {
                Self::collect_mutable_capture_slots(cond, out);
                Self::collect_mutable_capture_slots(then_br, out);
                Self::collect_mutable_capture_slots(else_br, out);
            }
            TypedExpr::Call { func, args, .. } => {
                Self::collect_mutable_capture_slots(func, out);
                for a in args { Self::collect_mutable_capture_slots(a, out); }
            }
            TypedExpr::BinaryOp { left, right, .. } => {
                Self::collect_mutable_capture_slots(left, out);
                Self::collect_mutable_capture_slots(right, out);
            }
            TypedExpr::MakeArray { elements, .. } => {
                for e in elements { Self::collect_mutable_capture_slots(e, out); }
            }
            TypedExpr::MakeObject { fields, spreads, .. } => {
                for (_, v) in fields { Self::collect_mutable_capture_slots(v, out); }
                for s in spreads { Self::collect_mutable_capture_slots(s, out); }
            }
            TypedExpr::Match { scrutinee, arms, .. } => {
                Self::collect_mutable_capture_slots(scrutinee, out);
                for arm in arms { Self::collect_mutable_capture_slots(&arm.body, out); }
            }
            TypedExpr::Index { object, key, .. } | TypedExpr::IndexSet { object, key, .. } => {
                Self::collect_mutable_capture_slots(object, out);
                Self::collect_mutable_capture_slots(key, out);
            }
            TypedExpr::FieldGet { object, .. } => Self::collect_mutable_capture_slots(object, out),
            TypedExpr::LocalSet { value, .. } => Self::collect_mutable_capture_slots(value, out),
            TypedExpr::StringInterp { parts, .. } => {
                for p in parts {
                    if let TypedStringPart::Expr(e) = p { Self::collect_mutable_capture_slots(e, out); }
                }
            }
            TypedExpr::Is { expr, .. } | TypedExpr::Has { expr, .. } | TypedExpr::Coerce { expr, .. } => {
                Self::collect_mutable_capture_slots(expr, out);
            }
            _ => {}
        }
    }

    /// True if the expression produces a freshly heap-allocated value that the caller owns.
    /// Used to decide whether to release the value after consuming it.
    fn expr_is_owned_alloc(expr: &TypedExpr) -> bool {
        match expr {
            TypedExpr::Call { .. }
            | TypedExpr::MakeArray { .. }
            | TypedExpr::MakeObject { .. }
            | TypedExpr::StringLit { .. }
            | TypedExpr::StringInterp { .. }
            | TypedExpr::Function { .. } => true,
            // If all branches produce owned values, the if result is also owned.
            // (Only one branch runs per execution, so there is exactly one owned value at the merge.)
            TypedExpr::If { then_br, else_br, .. } => {
                Self::expr_is_owned_alloc(then_br) && Self::expr_is_owned_alloc(else_br)
            }
            // Match: owned iff every arm body produces an owned value.
            TypedExpr::Match { arms, .. } => {
                !arms.is_empty() && arms.iter().all(|a| Self::expr_is_owned_alloc(&a.body))
            }
            // Block: owned iff the final expression is owned (internal releases already handled).
            TypedExpr::Block { expr, .. } => Self::expr_is_owned_alloc(expr),
            // Coerce is a numeric-only wrapper (no heap involvement).
            TypedExpr::Coerce { expr, .. } => Self::expr_is_owned_alloc(expr),
            _ => false,
        }
    }

    /// Returns the slot number if the expression's "final value" comes directly from a LocalGet.
    /// Used to identify when a function param is being returned (to skip releasing it at exit).
    /// Recurses into Block to find the final expression.
    fn body_return_slot(expr: &TypedExpr) -> Option<usize> {
        match expr {
            TypedExpr::LocalGet { slot, .. } => Some(*slot),
            TypedExpr::Block { expr, .. } => Self::body_return_slot(expr),
            _ => None,
        }
    }

    /// True if a type is heap-allocated (has a refcount) and needs a release call.
    fn ty_is_heap(ty: &Type) -> bool {
        matches!(ty, Type::Str | Type::Array(_) | Type::FixedArray(_) | Type::Object(_) | Type::Function { .. })
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

    /// Wrap a bare function pointer in a {fn_ptr, null_env} closure struct on the heap.
    /// Used when boxing non-capturing functions so call_body can uniformly call them
    /// as closures (fn_ptr(env_ptr, args...)).
    fn wrap_fn_ptr_as_closure(&mut self, fn_ptr: BasicValueEnum<'ctx>) -> BasicValueEnum<'ctx> {
        let ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
        let lin_alloc_fn = self.get_or_declare_fn("lin_alloc",
            ptr_ty.fn_type(&[self.context.i64_type().into()], false));
        let cls_size = self.context.i64_type().const_int(32, false); // {i32,i32,ptr,ptr} + u64 env_size
        let cls_mem = self.builder.build_call(lin_alloc_fn, &[cls_size.into()], "wfn_cls")
            .unwrap().try_as_basic_value().unwrap_basic().into_pointer_value();
        let cls_ty = self.closure_struct_type();
        let rc_field = self.builder.build_struct_gep(cls_ty, cls_mem, 0, "wfn_rc").unwrap();
        self.builder.build_store(rc_field, self.context.i32_type().const_int(1, false)).unwrap();
        let fn_field = self.builder.build_struct_gep(cls_ty, cls_mem, 2, "wfn_fp").unwrap();
        self.builder.build_store(fn_field, fn_ptr).unwrap();
        let env_field = self.builder.build_struct_gep(cls_ty, cls_mem, 3, "wfn_ep").unwrap();
        self.builder.build_store(env_field, ptr_ty.const_null()).unwrap();
        // env_size = 0 at offset 24 (already zeroed by lin_alloc).
        cls_mem.into()
    }

    /// Wrap a named (top-level) LLVM function in a closure struct with a thin adapter.
    /// Named functions have signature `(T1, T2, ...) -> R` (no env_ptr).
    /// The closure ABI expects `(ptr env, T1, T2, ...) -> R`.
    /// We generate a wrapper `__cls_wrap_N(ptr _env, T1, T2, ...) -> R` that forwards the call.
    fn wrap_named_fn_as_closure(&mut self, named_fn: FunctionValue<'ctx>, _param_ty: &Type) -> BasicValueEnum<'ctx> {
        let ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());

        // Build wrapper function type: (ptr env, ...original params) -> original ret
        let named_ret_ty = named_fn.get_type().get_return_type();
        let mut wrapper_param_types: Vec<BasicMetadataTypeEnum> = vec![ptr_ty.into()]; // env_ptr
        for i in 0..named_fn.count_params() {
            wrapper_param_types.push(named_fn.get_nth_param(i).unwrap().get_type().into());
        }
        let wrapper_fn_ty = if let Some(ret_ty) = named_ret_ty {
            ret_ty.fn_type(&wrapper_param_types, false)
        } else {
            self.context.void_type().fn_type(&wrapper_param_types, false)
        };

        // Emit or find an existing wrapper for this function.
        let wrapper_name = format!("__cls_wrap_{}", named_fn.get_name().to_str().unwrap_or("fn"));
        let wrapper_fn = if let Some(existing) = self.module.get_function(&wrapper_name) {
            existing
        } else {
            let wf = self.module.add_function(&wrapper_name, wrapper_fn_ty, None);
            let saved_block = self.builder.get_insert_block().unwrap();
            let entry = self.context.append_basic_block(wf, "entry");
            self.builder.position_at_end(entry);
            // Forward all params (skip env_ptr at index 0).
            let fwd_args: Vec<BasicMetadataValueEnum> = (1..wf.count_params())
                .map(|i| wf.get_nth_param(i).unwrap().into())
                .collect();
            let call = self.builder.build_call(named_fn, &fwd_args, "wfwd").unwrap();
            if named_ret_ty.is_some() {
                let ret_val = call.try_as_basic_value().basic().unwrap();
                self.builder.build_return(Some(&ret_val)).unwrap();
            } else {
                self.builder.build_return(None).unwrap();
            }
            self.builder.position_at_end(saved_block);
            wf
        };

        // Build {rc, _pad, fn_ptr, null_env} closure struct.
        let lin_alloc_fn = self.get_or_declare_fn("lin_alloc",
            ptr_ty.fn_type(&[self.context.i64_type().into()], false));
        let cls_mem = self.builder.build_call(lin_alloc_fn,
            &[self.context.i64_type().const_int(32, false).into()], "wnfn_cls")
            .unwrap().try_as_basic_value().unwrap_basic().into_pointer_value();
        let cls_ty = self.closure_struct_type();
        let rc_field = self.builder.build_struct_gep(cls_ty, cls_mem, 0, "wnfn_rc").unwrap();
        self.builder.build_store(rc_field, self.context.i32_type().const_int(1, false)).unwrap();
        let fn_field = self.builder.build_struct_gep(cls_ty, cls_mem, 2, "wnfn_fp").unwrap();
        self.builder.build_store(fn_field, wrapper_fn.as_global_value().as_pointer_value()).unwrap();
        let env_field = self.builder.build_struct_gep(cls_ty, cls_mem, 3, "wnfn_ep").unwrap();
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

    fn compile_function_body_with_coverage(
        &mut self,
        llvm_fn: FunctionValue<'ctx>,
        params: &[TypedParam],
        body: &TypedExpr,
        ret_type: &Type,
        captures: &[Capture],
        fn_name: &str,
        module_slots: &HashMap<usize, FunctionValue<'ctx>>,
        is_closure: bool,
        profc_global: Option<inkwell::values::GlobalValue<'ctx>>,
    ) {
        self.compile_function_body(llvm_fn, params, body, ret_type, captures, fn_name, module_slots, is_closure);
        // Emit the coverage counter increment at function entry (after building basic blocks).
        if let Some(profc) = profc_global {
            // Find the entry block and insert counter increment at the very beginning.
            if let Some(entry_bb) = llvm_fn.get_first_basic_block() {
                let i64_type = self.context.i64_type();
                // Position before the first instruction of the entry block.
                if let Some(first_instr) = entry_bb.get_first_instruction() {
                    self.builder.position_before(&first_instr);
                } else {
                    self.builder.position_at_end(entry_bb);
                }
                // Load, increment, store: __profc_fn[0]++
                let counter_ptr = profc.as_pointer_value();
                let counter_type = i64_type.array_type(1);
                let elem_ptr = unsafe {
                    self.builder
                        .build_in_bounds_gep(
                            counter_type,
                            counter_ptr,
                            &[
                                self.context.i32_type().const_int(0, false).into(),
                                self.context.i32_type().const_int(0, false).into(),
                            ],
                            "profc_ptr",
                        )
                        .unwrap()
                };
                let old_val = self.builder
                    .build_load(i64_type, elem_ptr, "profc_old")
                    .unwrap()
                    .into_int_value();
                let new_val = self.builder
                    .build_int_add(old_val, i64_type.const_int(1, false), "profc_inc")
                    .unwrap();
                self.builder.build_store(elem_ptr, new_val).unwrap();
            }
        }
    }

    fn compile_function_body(
        &mut self,
        llvm_fn: FunctionValue<'ctx>,
        params: &[TypedParam],
        body: &TypedExpr,
        ret_type: &Type,
        captures: &[Capture],
        fn_name: &str,
        // Extra slot→fn bindings visible during compilation (for intra-module calls in imported modules).
        module_slots: &HashMap<usize, FunctionValue<'ctx>>,
        // True when this is an anonymous closure (env_ptr is always first param).
        is_closure: bool,
    ) {
        // Check if this function can use the TCO loop transform.
        // Condition: name is known, and fn_name matches the function being compiled.
        let use_tco = self.named_fns.contains_key(fn_name);

        let entry_block = self.context.append_basic_block(llvm_fn, "entry");
        self.builder.position_at_end(entry_block);

        // Pre-scan to find slots for `var` bindings that are captured mutably by inner closures.
        // These must be heap-allocated so they outlive the creating function's stack frame.
        let mut heap_var_slots = std::collections::HashSet::new();
        Self::collect_mutable_capture_slots(body, &mut heap_var_slots);

        // Module-level function slots are kept separate from local param/val slots.
        // This prevents local slots (which share the same slot-number space) from shadowing
        // module-level function references when a param happens to have the same slot number.
        let module_fn_slots: HashMap<usize, FunctionValue<'ctx>> = module_slots.clone();

        let mut fn_ctx = FnCtx {
            slots: HashMap::new(),
            module_fn_slots,
            llvm_fn,
            env_ptr: None,
            tco: None,
            pointer_slots: std::collections::HashSet::new(),
            heap_var_slots,
        };

        // First parameter is env_ptr for any closure (even non-capturing ones, since
        // compile_closure always adds env_ptr to maintain uniform calling convention).
        let param_offset = if is_closure || !captures.is_empty() { 1 } else { 0 };

        if !captures.is_empty() {
            let env_ptr = llvm_fn
                .get_nth_param(0)
                .unwrap()
                .into_pointer_value();
            fn_ctx.env_ptr = Some(env_ptr);

            // Build the env struct type and load each captured field into slots.
            // Env layout: { u64 size_header, cap_0, cap_1, ... } — captures start at field index 1.
            let mut cap_types: Vec<inkwell::types::BasicTypeEnum> = vec![self.context.i64_type().into()];
            cap_types.extend(captures.iter().map(|c| {
                if c.is_mutable {
                    self.context.ptr_type(AddressSpace::default()).into()
                } else {
                    self.llvm_type(&c.ty)
                }
            }));
            let env_struct_type = self.context.struct_type(&cap_types, false);

            for (i, cap) in captures.iter().enumerate() {
                let field_ptr = self.builder
                    .build_struct_gep(env_struct_type, env_ptr, (i + 1) as u32, &format!("cap_{}_ptr", cap.name))
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
                    // Function-typed captures hold {fn_ptr, env_ptr} closure struct pointers.
                    // Store them as Closure slots so compile_call dispatches correctly.
                    if matches!(cap.ty, Type::Function { .. }) && val.is_pointer_value() {
                        fn_ctx.slots.insert(cap.outer_slot, SlotStorage::Closure(val.into_pointer_value()));
                    } else {
                        fn_ctx.slots.insert(cap.outer_slot, SlotStorage::Value(val));
                    }
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
                if Self::is_pointer_stored_type(&param.ty) {
                    fn_ctx.pointer_slots.insert(param.slot);
                }
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
                if Self::is_pointer_stored_type(&param.ty) {
                    fn_ctx.pointer_slots.insert(param.slot);
                }
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
                if Self::is_pointer_stored_type(&param.ty) {
                    fn_ctx.pointer_slots.insert(param.slot);
                }
            }
            self.builder.build_unconditional_branch(loop_block).unwrap();
            self.builder.position_at_end(loop_block);
            fn_ctx.tco = Some(TcoState {
                loop_block,
                param_allocs,
                _marker: std::marker::PhantomData,
            });
        }

        // Collect Function-typed params that need to be released at function return.
        // A fresh lambda (owned alloc) arrives with rc=1; we release it here so the caller
        // doesn't leak it. A non-owned closure was retained at the call site (rc incremented),
        // so releasing here restores the original rc.
        let fn_params_to_release: Vec<(usize, Type)> = params.iter()
            .filter(|p| matches!(p.ty, Type::Function { .. }))
            .map(|p| (p.slot, p.ty.clone()))
            .collect();
        // Find which slot (if any) is directly returned by the body — that param must not be
        // released here because the return value ownership transfers to the caller.
        let body_return_slot = Self::body_return_slot(body);

        let result = self.compile_expr(body, &mut fn_ctx);

        // Emit releases for Function-typed params (except the one being returned).
        // Helper closure to load param value from current slot storage.
        let emit_fn_param_releases = |codegen: &mut Self, fn_ctx: &FnCtx<'ctx, '_>| {
            let ptr_ty = codegen.context.ptr_type(AddressSpace::default());
            for (slot, ty) in &fn_params_to_release {
                if Some(*slot) == body_return_slot { continue; }
                let val_opt: Option<BasicValueEnum<'ctx>> = match fn_ctx.slots.get(slot) {
                    Some(SlotStorage::Value(v)) => Some(*v),
                    Some(SlotStorage::Closure(p)) => Some((*p).into()),
                    Some(SlotStorage::Alloca(alloca)) => {
                        Some(codegen.builder.build_load(ptr_ty, *alloca, "fn_param_rel")
                            .unwrap())
                    }
                    _ => None,
                };
                if let Some(v) = val_opt {
                    codegen.emit_release(v, ty);
                }
            }
        };

        match ret_type {
            Type::Never => {
                // Void-returning function: discard body result and return void.
                emit_fn_param_releases(self, &fn_ctx);
                self.builder.build_return(None).unwrap();
            }
            _ => {
                // Coerce body result to declared return type.
                let body_ty = body.ty();
                let final_result = if Self::is_union_type(ret_type) {
                    if matches!(body_ty, Type::TypeVar(_) | Type::Union(_) | Type::Null) {
                        // Already a TaggedVal* (or will produce one) — pass through if pointer.
                        if result.is_pointer_value() {
                            result
                        } else {
                            // Scalar from TypeVar expression (e.g. int from TypeVar + TypeVar)
                            self.box_value(result, &body_ty)
                        }
                    } else {
                        // Concrete body type (Array, Str, Int32, Object, etc.) — box it
                        self.box_value(result, &body_ty)
                    }
                } else if !Self::is_union_type(ret_type) && Self::is_union_type(&body_ty) && !matches!(body_ty, Type::Never) {
                    // Concrete return type but body produced TypeVar — unbox/coerce it.
                    self.coerce_typevar(result, &body_ty, ret_type)
                } else {
                    result
                };
                emit_fn_param_releases(self, &fn_ctx);
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
                        let compiled_raw = self.compile_expr(value, fn_ctx);
                        let val_ty = value.ty();
                        // When the declared slot type is a union (TypeVar/Union) but the expression
                        // produces a concrete type, box the value so the slot always holds a TaggedVal*.
                        let compiled = if Self::is_union_type(ty) && !Self::is_union_type(&val_ty)
                            && !matches!(val_ty, Type::Never)
                        {
                            self.box_value(compiled_raw, &val_ty)
                        } else {
                            compiled_raw
                        };
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
                        fn_ctx.slots.insert(*slot, storage.clone());
                        // Register as LLVM global so closures without explicit captures can access it.
                        // This handles: val x = 10; async(() => x * 2) — x is not "captured" but must
                        // be accessible inside the deferred closure body.
                        if fn_ctx.llvm_fn.get_name().to_str() == Ok("main") {
                            let llvm_ty = self.llvm_type(ty);
                            let glob = self.module.add_global(llvm_ty, None, &format!("_gv_{}", slot));
                            glob.set_initializer(&llvm_ty.const_zero());
                            // Store the just-computed value into the global.
                            let compiled_val = match &storage {
                                SlotStorage::Value(v) => *v,
                                SlotStorage::Closure(p) => (*p).into(),
                                SlotStorage::Alloca(_) => compiled,
                            };
                            self.builder.build_store(glob.as_pointer_value(), compiled_val).unwrap();
                            self.global_val_slots.insert(*slot, glob);
                        }
                    }
                }
            }
            TypedStmt::Var { slot, value, ty, .. } => {
                let compiled = self.compile_expr(value, fn_ctx);
                let llvm_ty = self.llvm_type(ty);
                if fn_ctx.heap_var_slots.contains(slot) {
                    // This var is captured mutably by an inner closure — heap-allocate the cell
                    // so it outlives this function's stack frame. The Alloca pointer stored in
                    // the slot is now a heap pointer that closures can safely hold after we return.
                    let cell_size = llvm_ty.size_of().unwrap();
                    let cell_size_i64 = self.builder
                        .build_int_z_extend_or_bit_cast(cell_size, self.context.i64_type(), "cell_size")
                        .unwrap();
                    let cell_ptr = self.builder
                        .build_call(self.rt_alloc, &[cell_size_i64.into()], &format!("var_cell_{}", slot))
                        .unwrap()
                        .try_as_basic_value()
                        .unwrap_basic()
                        .into_pointer_value();
                    self.builder.build_store(cell_ptr, compiled).unwrap();
                    fn_ctx.slots.insert(*slot, SlotStorage::Alloca(cell_ptr));
                } else {
                    let alloc = self
                        .builder
                        .build_alloca(llvm_ty, &format!("var_{}", slot))
                        .unwrap();
                    self.builder.build_store(alloc, compiled).unwrap();
                    fn_ctx.slots.insert(*slot, SlotStorage::Alloca(alloc));
                }
            }
            TypedStmt::Destructure { obj_slot, value, obj_ty, fields, rest, .. } => {
                let compiled = self.compile_expr(value, fn_ctx);
                fn_ctx.slots.insert(*obj_slot, SlotStorage::Value(compiled));
                if compiled.is_pointer_value() {
                    // Unbox TaggedVal* to LinObject* for TypeVar/Union typed values.
                    let obj_ptr = if matches!(obj_ty, Type::TypeVar(_) | Type::Union(_)) {
                        self.builder.build_call(self.rt_unbox_ptr, &[compiled.into()], "destr_unbox")
                            .unwrap().try_as_basic_value().unwrap_basic().into_pointer_value()
                    } else {
                        compiled.into_pointer_value()
                    };
                    for (key, slot, field_ty) in fields {
                        let key_str = self.compile_string_lit(key).into_pointer_value();
                        let entry_ptr = self.builder
                            .build_call(self.rt_object_get, &[obj_ptr.into(), key_str.into()], "destr_p")
                            .unwrap()
                            .try_as_basic_value().unwrap_basic().into_pointer_value();
                        // For TypeVar fields: store the TaggedVal* directly so callers treat it as Json.
                        let val = if matches!(field_ty, Type::TypeVar(_)) {
                            entry_ptr.into()
                        } else {
                            self.load_tagged_val_payload(entry_ptr, field_ty, obj_ty)
                        };
                        fn_ctx.slots.insert(*slot, SlotStorage::Value(val));
                    }
                    // Build rest object: copy all fields except the bound ones into a new object.
                    if let Some(rest_slot) = rest {
                        let ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
                        let i32_ty = self.context.i32_type();
                        let rest_obj = self.builder
                            .build_call(self.rt_object_alloc, &[i32_ty.const_int(4, false).into()], "rest_obj")
                            .unwrap().try_as_basic_value().unwrap_basic().into_pointer_value();
                        let exclude_fn = self.get_or_declare_fn("lin_object_copy_except",
                            self.context.void_type().fn_type(&[ptr_ty.into(), ptr_ty.into(), ptr_ty.into(), i32_ty.into()], false));
                        // Build an array of excluded keys on the stack.
                        let n_excluded = fields.len() as u32;
                        let arr_ty = ptr_ty.array_type(n_excluded.max(1));
                        let keys_arr = self.builder.build_alloca(arr_ty, "exc_keys").unwrap();
                        for (i, (key, _, _)) in fields.iter().enumerate() {
                            let key_str = self.compile_string_lit(key);
                            let gep = unsafe {
                                self.builder.build_gep(
                                    arr_ty,
                                    keys_arr,
                                    &[self.context.i32_type().const_zero(), i32_ty.const_int(i as u64, false)],
                                    "exc_key_p",
                                ).unwrap()
                            };
                            self.builder.build_store(gep, key_str).unwrap();
                        }
                        let keys_ptr = self.builder.build_pointer_cast(keys_arr, ptr_ty, "exc_keys_p").unwrap();
                        self.builder.build_call(exclude_fn, &[
                            rest_obj.into(),
                            obj_ptr.into(),
                            keys_ptr.into(),
                            i32_ty.const_int(n_excluded as u64, false).into(),
                        ], "").unwrap();
                        // Box as TaggedVal* so TypeVar-typed accesses (e.g. rest["key"]) work correctly.
                        let boxed_rest = self.builder
                            .build_call(self.rt_box_object, &[rest_obj.into()], "boxed_rest")
                            .unwrap().try_as_basic_value().unwrap_basic();
                        fn_ctx.slots.insert(*rest_slot, SlotStorage::Value(boxed_rest));
                    }
                } else {
                    for (_, slot, field_ty) in fields {
                        fn_ctx.slots.insert(*slot, SlotStorage::Value(self.llvm_type(field_ty).const_zero()));
                    }
                }
            }
            TypedStmt::ArrayDestructure { arr_slot, value, elem_ty, elements, rest, .. } => {
                let arr_val = self.compile_expr(value, fn_ctx);
                fn_ctx.slots.insert(*arr_slot, SlotStorage::Value(arr_val));
                let i64_ty = self.context.i64_type();
                for (index, slot, ty) in elements {
                    let idx = i64_ty.const_int(*index as u64, false);
                    let elem_val = if Self::is_flat_scalar(ty) {
                        self.flat_array_get(arr_val, idx, ty)
                    } else {
                        let elem_ptr = self.builder
                            .build_call(self.rt_array_get, &[arr_val.into(), idx.into()], "ad_elem")
                            .unwrap()
                            .try_as_basic_value()
                            .unwrap_basic()
                            .into_pointer_value();
                        self.load_array_element(elem_ptr, ty)
                    };
                    fn_ctx.slots.insert(*slot, SlotStorage::Value(elem_val));
                }
                if let Some((rest_slot, rest_ty)) = rest {
                    // Build a new array containing elements from elements.len() onward.
                    let start = elements.len();
                    let len_val = self.builder
                        .build_call(self.rt_array_length, &[arr_val.into()], "ad_len")
                        .unwrap()
                        .try_as_basic_value()
                        .unwrap_basic()
                        .into_int_value();
                    let inner_ty = if let Type::Array(inner) = rest_ty { inner.as_ref().clone() } else { elem_ty.clone() };
                    let cap = self.builder.build_int_sub(len_val, i64_ty.const_int(start as u64, false), "rest_cap").unwrap();
                    let rest_arr = if Self::is_flat_scalar(&inner_ty) {
                        let suffix = Self::flat_suffix(&inner_ty);
                        let alloc_name = format!("lin_flat_array_alloc_{}", suffix);
                        let alloc_fn_ty = self.context.ptr_type(inkwell::AddressSpace::default())
                            .fn_type(&[i64_ty.into()], false);
                        let alloc_fn = self.get_or_declare_fn(&alloc_name, alloc_fn_ty);
                        let arr = self.builder.build_call(alloc_fn, &[cap.into()], "rest_arr")
                            .unwrap().try_as_basic_value().unwrap_basic();
                        let llvm_fn = fn_ctx.llvm_fn;
                        let i_alloc = self.builder.build_alloca(i64_ty, "rest_i").unwrap();
                        self.builder.build_store(i_alloc, i64_ty.const_int(start as u64, false)).unwrap();
                        let check_bb = self.context.append_basic_block(llvm_fn, "rest_check");
                        let body_bb = self.context.append_basic_block(llvm_fn, "rest_body");
                        let after_bb = self.context.append_basic_block(llvm_fn, "rest_after");
                        self.builder.build_unconditional_branch(check_bb).unwrap();
                        self.builder.position_at_end(check_bb);
                        let cur_i = self.builder.build_load(i64_ty, i_alloc, "ri").unwrap().into_int_value();
                        let cond = self.builder.build_int_compare(inkwell::IntPredicate::SLT, cur_i, len_val, "rest_cond").unwrap();
                        self.builder.build_conditional_branch(cond, body_bb, after_bb).unwrap();
                        self.builder.position_at_end(body_bb);
                        let elem = self.flat_array_get(arr_val, cur_i, &inner_ty);
                        self.flat_array_push(arr, elem, &inner_ty);
                        let next_i = self.builder.build_int_add(cur_i, i64_ty.const_int(1, false), "ri_next").unwrap();
                        self.builder.build_store(i_alloc, next_i).unwrap();
                        self.builder.build_unconditional_branch(check_bb).unwrap();
                        self.builder.position_at_end(after_bb);
                        arr
                    } else {
                        let arr = self.builder
                            .build_call(self.rt_array_alloc, &[cap.into()], "rest_arr")
                            .unwrap().try_as_basic_value().unwrap_basic();
                        let llvm_fn = fn_ctx.llvm_fn;
                        let i_alloc = self.builder.build_alloca(i64_ty, "rest_i").unwrap();
                        self.builder.build_store(i_alloc, i64_ty.const_int(start as u64, false)).unwrap();
                        let check_bb = self.context.append_basic_block(llvm_fn, "rest_check");
                        let body_bb = self.context.append_basic_block(llvm_fn, "rest_body");
                        let after_bb = self.context.append_basic_block(llvm_fn, "rest_after");
                        self.builder.build_unconditional_branch(check_bb).unwrap();
                        self.builder.position_at_end(check_bb);
                        let cur_i = self.builder.build_load(i64_ty, i_alloc, "ri").unwrap().into_int_value();
                        let cond = self.builder.build_int_compare(inkwell::IntPredicate::SLT, cur_i, len_val, "rest_cond").unwrap();
                        self.builder.build_conditional_branch(cond, body_bb, after_bb).unwrap();
                        self.builder.position_at_end(body_bb);
                        let elem_ptr = self.builder
                            .build_call(self.rt_array_get, &[arr_val.into(), cur_i.into()], "rest_elem")
                            .unwrap().try_as_basic_value().unwrap_basic().into_pointer_value();
                        let elem = self.load_array_element(elem_ptr, &inner_ty);
                        self.array_push_value(arr, elem, &inner_ty);
                        let next_i = self.builder.build_int_add(cur_i, i64_ty.const_int(1, false), "ri_next").unwrap();
                        self.builder.build_store(i_alloc, next_i).unwrap();
                        self.builder.build_unconditional_branch(check_bb).unwrap();
                        self.builder.position_at_end(after_bb);
                        arr
                    };
                    fn_ctx.slots.insert(*rest_slot, SlotStorage::Value(rest_arr));
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
                        let known_intrinsics = ["lin_print", "lin_to_string", "lin_length", "lin_push",
                            "lin_array_set", "lin_keys", "lin_object_set", "lin_for", "lin_while", "lin_iter", "lin_range",
                            "lin_map", "lin_filter", "lin_reduce",
                            "lin_async", "lin_await", "lin_parallel", "lin_race", "lin_timeout", "lin_retry",
                            "lin_thread_pool", "lin_worker", "lin_request", "lin_message", "lin_close",
                            "lin_exit", "lin_value_key", "lin_array_allocate", "lin_array_allocate_filled"];
                        if known_intrinsics.contains(&binding.name.as_str()) {
                            self.intrinsic_slots.insert(binding.slot, binding.name.clone());
                        } else if let Some(&wrapper_fn) = self.imported_val_wrappers.get(&key) {
                            // Non-function exported val (e.g. PI, E from std/math) — call the wrapper to get the value.
                            let result = self.builder.build_call(wrapper_fn, &[], &format!("imp_{}", binding.name))
                                .unwrap().try_as_basic_value().unwrap_basic();
                            fn_ctx.slots.insert(binding.slot, SlotStorage::Value(result));
                            // Store in a module-level global so closures at scope depth 0 can load it.
                            let llvm_ty = self.llvm_type(&binding.ty);
                            let glob = self.module.add_global(llvm_ty, None, &format!("_gv_{}", binding.slot));
                            glob.set_initializer(&llvm_ty.const_zero());
                            self.builder.build_store(glob.as_pointer_value(), result).unwrap();
                            self.global_val_slots.insert(binding.slot, glob);
                        } else {
                            // Not found — store null pointer as fallback.
                            fn_ctx.slots.insert(binding.slot, SlotStorage::Value(ptr_ty.const_null().into()));
                        }
                    }
                }
            }
            TypedStmt::ForeignImport { path, bindings, .. } => {
                // Declare each foreign symbol as an LLVM external function and store
                // a pointer to it in the binding slot. The actual symbol is resolved
                // at link time by passing the library path to the linker.
                for binding in &*bindings {
                    if !binding.valid { continue; }
                    if let Type::Function { params, ret } = &binding.ty {
                        let param_types: Vec<inkwell::types::BasicMetadataTypeEnum> = params.iter()
                            .map(|p| self.llvm_type(p).into())
                            .collect();
                        let llvm_ret = self.llvm_type(ret);
                        let fn_type = llvm_ret.fn_type(&param_types, false);
                        let llvm_fn = self.module.add_function(
                            &binding.name,
                            fn_type,
                            Some(inkwell::module::Linkage::External),
                        );
                        let fn_ptr = llvm_fn.as_global_value().as_pointer_value();
                        fn_ctx.slots.insert(binding.slot, SlotStorage::Value(fn_ptr.into()));
                        self.global_fn_slots.insert(binding.slot, llvm_fn);
                    }
                }
                // "lin-runtime" is always linked unconditionally — don't add it again.
                if path != "lin-runtime" {
                    self.foreign_lib_paths.push(path.clone());
                }
            }
            TypedStmt::Expr(expr) => {
                let val = self.compile_expr(expr, fn_ctx);
                // Release owned heap results that are discarded by expression-statements.
                if Self::expr_is_owned_alloc(expr) && Self::ty_is_heap(&expr.ty()) {
                    self.emit_release(val, &expr.ty());
                }
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
            TypedExpr::NullLit(_) => self.context.ptr_type(AddressSpace::default()).const_null().into(),
            TypedExpr::StringLit(s, _) => self.compile_string_lit(s),

            TypedExpr::LocalGet { slot, ty, .. } => {
                let val = self.compile_local_get(*slot, ty, fn_ctx);
                // If this slot stores a TaggedVal* pointer and the requested type is a concrete
                // narrowed type, unbox the payload. This happens in match arm bodies after `is T`.
                if fn_ctx.pointer_slots.contains(slot) && !Self::is_union_type(ty) && *ty != Type::Null {
                    self.load_tagged_val_payload(val.into_pointer_value(), ty, &Type::TypeVar(0))
                } else {
                    val
                }
            }
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
                // Collect owned heap vals introduced in this block for scope-exit release.
                let mut owned_slots: Vec<(usize, Type)> = Vec::new();
                // Collect heap-typed var slots introduced in this block for scope-exit release.
                let mut var_slots: Vec<(usize, Type)> = Vec::new();
                for s in stmts {
                    if let TypedStmt::Val { slot, value, ty: val_ty, .. } = s {
                        if Self::expr_is_owned_alloc(value) && Self::ty_is_heap(val_ty) {
                            owned_slots.push((*slot, val_ty.clone()));
                        }
                    }
                    if let TypedStmt::Var { slot, ty: var_ty, .. } = s {
                        if Self::ty_is_heap(var_ty) {
                            var_slots.push((*slot, var_ty.clone()));
                        }
                    }
                    self.compile_stmt(s, fn_ctx);
                }
                let result_slot: Option<usize> = if let TypedExpr::LocalGet { slot, .. } = expr.as_ref() {
                    Some(*slot)
                } else {
                    None
                };
                let result = self.compile_expr(expr, fn_ctx);
                // Release owned heap values that are not being returned.
                // Safety: array/object construction already retained any shared references
                // into these values, so releasing the originals here is safe.
                for (slot, slot_ty) in &owned_slots {
                    if result_slot == Some(*slot) { continue; }
                    if let Some(storage) = fn_ctx.slots.get(slot) {
                        let val = match storage {
                            SlotStorage::Value(v) => Some(*v),
                            SlotStorage::Closure(p) => Some((*p).into()),
                            SlotStorage::Alloca(_) => None,
                        };
                        if let Some(v) = val {
                            self.emit_release(v, slot_ty);
                        }
                    }
                }
                // Release the current heap value of each var binding at scope exit.
                // Gap 1 releases intermediate old values on reassignment; here we release
                // the final value stored in the alloca when the block ends.
                for (slot, slot_ty) in &var_slots {
                    if result_slot == Some(*slot) { continue; }
                    // Skip heap-captured var slots — their lifecycle is managed by the closure.
                    if fn_ctx.heap_var_slots.contains(slot) { continue; }
                    if let Some(SlotStorage::Alloca(ptr)) = fn_ctx.slots.get(slot) {
                        let ptr = *ptr;
                        let llvm_ty = self.llvm_type(slot_ty);
                        let current_val = self.builder.build_load(llvm_ty, ptr, "var_scope_val").unwrap();
                        // Only release if non-null.
                        let current_ptr = current_val.into_pointer_value();
                        let i64_ty = self.context.i64_type();
                        let as_int = self.builder.build_ptr_to_int(current_ptr, i64_ty, "var_scope_pti").unwrap();
                        let is_nonnull = self.builder.build_int_compare(
                            inkwell::IntPredicate::NE, as_int, i64_ty.const_zero(), "var_scope_nonnull"
                        ).unwrap();
                        let release_block = self.context.append_basic_block(fn_ctx.llvm_fn, "var_scope_release");
                        let after_block = self.context.append_basic_block(fn_ctx.llvm_fn, "var_scope_after");
                        self.builder.build_conditional_branch(is_nonnull, release_block, after_block).unwrap();
                        self.builder.position_at_end(release_block);
                        self.emit_release(current_val, slot_ty);
                        self.builder.build_unconditional_branch(after_block).unwrap();
                        self.builder.position_at_end(after_block);
                    }
                }
                result
            }

            TypedExpr::Function { name, params, body, ret_type, captures, .. } => {
                self.compile_closure(name.as_deref(), params, body, ret_type, captures, fn_ctx)
            }

            TypedExpr::StringInterp { parts, .. } => self.compile_string_interp(parts, fn_ctx),

            TypedExpr::MakeArray { elements, ty, .. } => {
                self.compile_make_array(elements, ty, fn_ctx)
            }

            TypedExpr::MakeObject { fields, spreads, .. } => {
                self.compile_make_object(fields, spreads, fn_ctx)
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

            TypedExpr::IndexSet { object, key, value, obj_ty, .. } => {
                self.compile_index_set(object, key, value, obj_ty, fn_ctx)
            }
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
        let ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
        match fn_ctx.slots.get(&slot) {
            Some(SlotStorage::Value(v)) => {
                // If this slot stores a TaggedVal* pointer but ty is narrowed to concrete, return the pointer.
                // The LocalGet arm in compile_expr will unbox it.
                *v
            }
            Some(SlotStorage::Alloca(ptr)) => {
                // If slot was allocated for a pointer-typed var (TypeVar/Union),
                // always load as pointer even when ty is narrowed to a concrete type.
                let load_ty = if fn_ctx.pointer_slots.contains(&slot) {
                    ptr_ty.as_basic_type_enum()
                } else {
                    self.llvm_type(ty)
                };
                self.builder
                    .build_load(load_ty, *ptr, &format!("load_{}", slot))
                    .unwrap()
            }
            Some(SlotStorage::Closure(ptr)) => {
                // Closure pointer — return as-is (caller will unpack fn_ptr+env_ptr at call site).
                (*ptr).into()
            }
            None => {
                // Slot not found in local fn_ctx — check global val slots.
                // This happens for closures that reference top-level non-function vals
                // (e.g. `val x = 10; async(() => x * 2)`).
                if let Some(glob) = self.global_val_slots.get(&slot) {
                    let llvm_ty = self.llvm_type(ty);
                    let ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
                    // If the global stores a pointer (closure/tagged val), load as ptr.
                    let load_ty = if matches!(ty, Type::Function { .. } | Type::TypeVar(_) | Type::Union(_) | Type::Null) {
                        ptr_ty.as_basic_type_enum()
                    } else {
                        llvm_ty
                    };
                    return self.builder
                        .build_load(load_ty, glob.as_pointer_value(), &format!("gv_{}", slot))
                        .unwrap();
                }
                // Check module-local fn slots (intra-module calls, e.g. toBe calling _pass).
                // These are kept separate from fn_ctx.slots to prevent local param slots
                // (which reuse the same slot-number space) from shadowing them.
                if let Some(&mfn) = fn_ctx.module_fn_slots.get(&slot) {
                    return mfn.as_global_value().as_pointer_value().into();
                }
                // Check global_fn_slots, then current_module_slots (for stdlib imports).
                if let Some(&gfn) = self.global_fn_slots.get(&slot) {
                    return gfn.as_global_value().as_pointer_value().into();
                }
                if let Some(&mfn) = self.current_module_slots.get(&slot) {
                    return mfn.as_global_value().as_pointer_value().into();
                }
                // Truly not found — return a zero/poison value.
                self.llvm_type(ty).const_zero()
            }
        }
    }

    fn compile_local_set(
        &mut self,
        slot: usize,
        value: &TypedExpr,
        ty: &Type,
        fn_ctx: &mut FnCtx<'ctx, '_>,
    ) -> BasicValueEnum<'ctx> {
        let compiled = self.compile_expr(value, fn_ctx);
        match fn_ctx.slots.get(&slot).cloned() {
            Some(SlotStorage::Alloca(ptr)) => {
                // Release the old heap value before overwriting.
                if Self::ty_is_heap(ty) {
                    let llvm_ty = self.llvm_type(ty);
                    let old_val = self.builder.build_load(llvm_ty, ptr, "var_old").unwrap();
                    // Only release if the old pointer is non-null.
                    let old_ptr = old_val.into_pointer_value();
                    let i64_ty = self.context.i64_type();
                    let as_int = self.builder.build_ptr_to_int(old_ptr, i64_ty, "old_pti").unwrap();
                    let is_nonnull = self.builder.build_int_compare(
                        inkwell::IntPredicate::NE, as_int, i64_ty.const_zero(), "old_nonnull"
                    ).unwrap();
                    let release_block = self.context.append_basic_block(fn_ctx.llvm_fn, "var_release");
                    let after_block = self.context.append_basic_block(fn_ctx.llvm_fn, "var_after");
                    self.builder.build_conditional_branch(is_nonnull, release_block, after_block).unwrap();
                    self.builder.position_at_end(release_block);
                    self.emit_release(old_val, ty);
                    self.builder.build_unconditional_branch(after_block).unwrap();
                    self.builder.position_at_end(after_block);
                }
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

        // When either operand is TypeVar or Union, use lin_tagged_eq for equality rather than coercing.
        // Coercing a TypeVar pointer to a concrete type crashes when the value is null
        // (e.g. object_get returns null for a missing key, and null["type"] == "error" would
        // deref null at payload offset 8). lin_tagged_eq handles null on either side safely.
        // Union types also produce TaggedVal* (e.g. Object({})[k] returns Union([Union([]),Null])),
        // so they need the same treatment.
        if (matches!(lty_orig, Type::TypeVar(_) | Type::Union(_)) || matches!(rty_orig, Type::TypeVar(_) | Type::Union(_)))
            && matches!(op, BinOp::Eq | BinOp::NotEq)
        {
            let ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
            let i8_ty = self.context.i8_type();
            let tagged_eq_fn = self.get_or_declare_fn("lin_tagged_eq",
                i8_ty.fn_type(&[ptr_ty.into(), ptr_ty.into()], false));
            // Ensure both sides are TaggedVal* (box if concrete).
            let lv_tagged = if matches!(lty_orig, Type::TypeVar(_) | Type::Union(_)) {
                lv_raw
            } else {
                self.box_value(lv_raw, &lty_orig)
            };
            let rv_tagged = if matches!(rty_orig, Type::TypeVar(_) | Type::Union(_)) {
                rv_raw
            } else {
                self.box_value(rv_raw, &rty_orig)
            };
            let eq_i8 = self.builder
                .build_call(tagged_eq_fn, &[lv_tagged.into(), rv_tagged.into()], "teq")
                .unwrap()
                .try_as_basic_value().unwrap_basic().into_int_value();
            let eq_b = self.builder.build_int_truncate(eq_i8, self.context.bool_type(), "teq_b").unwrap();
            return if matches!(op, BinOp::NotEq) {
                self.builder.build_not(eq_b, "tneq").unwrap().into()
            } else {
                eq_b.into()
            };
        }

        // For ordering comparisons where either side is TypeVar, use lin_tagged_cmp which handles
        // strings lexicographically and numerics by value. This avoids comparing raw pointer payloads.
        let is_ordering_op = matches!(op, BinOp::Lt | BinOp::Gt | BinOp::LtEq | BinOp::GtEq);
        if (matches!(lty_orig, Type::TypeVar(_) | Type::Union(_)) || matches!(rty_orig, Type::TypeVar(_) | Type::Union(_)))
            && is_ordering_op
        {
            let ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
            let i32_ty = self.context.i32_type();
            let tagged_cmp_fn = self.get_or_declare_fn("lin_tagged_cmp",
                i32_ty.fn_type(&[ptr_ty.into(), ptr_ty.into()], false));
            let lv_tagged = if matches!(lty_orig, Type::TypeVar(_) | Type::Union(_)) {
                lv_raw
            } else {
                self.box_value(lv_raw, &lty_orig)
            };
            let rv_tagged = if matches!(rty_orig, Type::TypeVar(_) | Type::Union(_)) {
                rv_raw
            } else {
                self.box_value(rv_raw, &rty_orig)
            };
            let cmp_i32 = self.builder
                .build_call(tagged_cmp_fn, &[lv_tagged.into(), rv_tagged.into()], "tcmp")
                .unwrap()
                .try_as_basic_value().unwrap_basic().into_int_value();
            let zero = i32_ty.const_zero();
            let result = match op {
                BinOp::Lt    => self.builder.build_int_compare(inkwell::IntPredicate::SLT, cmp_i32, zero, "tclt").unwrap(),
                BinOp::Gt    => self.builder.build_int_compare(inkwell::IntPredicate::SGT, cmp_i32, zero, "tcgt").unwrap(),
                BinOp::LtEq  => self.builder.build_int_compare(inkwell::IntPredicate::SLE, cmp_i32, zero, "tcle").unwrap(),
                BinOp::GtEq  => self.builder.build_int_compare(inkwell::IntPredicate::SGE, cmp_i32, zero, "tcge").unwrap(),
                _ => unreachable!(),
            };
            return result.into();
        }

        // Coerce TypeVar operands to the concrete type of the other operand.
        // When both are TypeVar, use _result_type as the coercion target.
        // For ordering comparisons (< > <= >=) the result_type is Boolean but operands
        // are numeric — fall back to Int32 so we unbox the numeric payload, not the bool byte.
        let (lv, rv, lty, rty) = if matches!(lty_orig, Type::TypeVar(_)) && matches!(rty_orig, Type::TypeVar(_)) {
            // Both TypeVar: coerce both to result_type (or Int32 as fallback).
            let target = if is_ordering_op || matches!(_result_type, Type::TypeVar(_) | Type::Bool) {
                Type::Int32
            } else {
                _result_type.clone()
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
            // Numeric widening: if one is float and the other is integer, widen integer to float.
            let (lv2, rv2, effective_ty) = if lty_orig.is_float() && rty_orig.is_integer() {
                let rv_f = self.builder.build_signed_int_to_float(
                    rv_raw.into_int_value(),
                    self.llvm_type(&lty_orig).into_float_type(),
                    "itof"
                ).unwrap().into();
                (lv_raw, rv_f, lty_orig.clone())
            } else if rty_orig.is_float() && lty_orig.is_integer() {
                let lv_f = self.builder.build_signed_int_to_float(
                    lv_raw.into_int_value(),
                    self.llvm_type(&rty_orig).into_float_type(),
                    "itof"
                ).unwrap().into();
                (lv_f, rv_raw, rty_orig.clone())
            } else if lty_orig.is_integer() && rty_orig.is_integer() && lty_orig != rty_orig {
                // Integer width mismatch: widen the narrower one.
                use lin_check::widen::widen_numeric;
                let wide_ty = widen_numeric(&lty_orig, &rty_orig).unwrap_or(lty_orig.clone());
                let target_llvm = self.llvm_type(&wide_ty).into_int_type();
                let lv2 = if lty_orig != wide_ty {
                    let signed = lty_orig.is_signed();
                    if signed {
                        self.builder.build_int_s_extend(lv_raw.into_int_value(), target_llvm, "widen_l").unwrap().into()
                    } else {
                        self.builder.build_int_z_extend(lv_raw.into_int_value(), target_llvm, "widen_l").unwrap().into()
                    }
                } else { lv_raw };
                let rv2 = if rty_orig != wide_ty {
                    let signed = rty_orig.is_signed();
                    if signed {
                        self.builder.build_int_s_extend(rv_raw.into_int_value(), target_llvm, "widen_r").unwrap().into()
                    } else {
                        self.builder.build_int_z_extend(rv_raw.into_int_value(), target_llvm, "widen_r").unwrap().into()
                    }
                } else { rv_raw };
                (lv2, rv2, wide_ty.clone())
            } else {
                (lv_raw, rv_raw, lty_orig.clone())
            };
            (lv2, rv2, effective_ty.clone(), effective_ty)
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

    fn compile_call(
        &mut self,
        func: &TypedExpr,
        args: &[TypedExpr],
        result_type: &Type,
        is_tail: bool,
        fn_ctx: &mut FnCtx<'ctx, '_>,
    ) -> BasicValueEnum<'ctx> {
        // TCO: tail self-calls become a branch back to the loop header.
        if is_tail {
            if let TypedExpr::LocalGet { .. } = func {
                if let Some(result) = self.try_tco_tail_call(args, result_type, fn_ctx) {
                    return result;
                }
            }
        }

        match func {
            TypedExpr::LocalGet { slot, ty: func_ty, .. } => {
                if let Some(name) = self.intrinsic_slots.get(slot).cloned() {
                    return self.compile_intrinsic_call(&name, args, result_type, fn_ctx);
                }
                if let Some(llvm_fn) = self.global_fn_slots.get(slot).copied() {
                    return self.call_global_fn(llvm_fn, func, args, result_type, fn_ctx);
                }
                if let Some(llvm_fn) = fn_ctx.module_fn_slots.get(slot).copied() {
                    return self.call_global_fn(llvm_fn, func, args, result_type, fn_ctx);
                }
                if let Some(SlotStorage::Closure(cls)) = fn_ctx.slots.get(slot).cloned() {
                    return self.build_closure_call_typed(cls, args, result_type, func_ty, fn_ctx);
                }
                if let Some(SlotStorage::Value(fn_val)) = fn_ctx.slots.get(slot).cloned() {
                    if let BasicValueEnum::PointerValue(ptr) = fn_val {
                        // Opaque `Function` type: the param holds a closure struct pointer.
                        // Use closure call convention (fn_ptr + env_ptr) rather than a raw call.
                        let is_typed_fn = matches!(func_ty, Type::Function { .. });
                        if is_typed_fn {
                            return self.build_closure_call_typed(ptr, args, result_type, func_ty, fn_ctx);
                        }
                        return self.call_slot_fn(ptr, func, args, result_type, fn_ctx);
                    }
                }
                if let Some(SlotStorage::Alloca(alloc)) = fn_ctx.slots.get(slot).cloned() {
                    if matches!(func_ty, Type::Function { .. }) {
                        // Function param stored in alloca holds a {fn_ptr, env_ptr} closure struct.
                        // All Function-typed params arrive as closure structs (named fns are wrapped
                        // at call sites via wrap_named_fn_as_closure).
                        let ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
                        let cls_ptr = self.builder.build_load(ptr_ty, alloc, "fn_alloca_load")
                            .unwrap().into_pointer_value();
                        return self.build_closure_call_typed(cls_ptr, args, result_type, func_ty, fn_ctx);
                    }
                }
                let fn_val = self.compile_expr(func, fn_ctx);
                self.build_indirect_call(fn_val, args, result_type, fn_ctx)
            }
            _ => {
                let func_ty = func.ty();
                let fn_val = self.compile_expr(func, fn_ctx);
                // When the callee type is TypeVar/Union/Function-stored-in-Json, the value is a
                // TaggedVal* containing a boxed closure. Unbox it and dispatch via closure call.
                if matches!(func_ty, Type::TypeVar(_) | Type::Union(_)) {
                    if let BasicValueEnum::PointerValue(tagged_ptr) = fn_val {
                        let cls_ptr = self.builder
                            .build_call(self.rt_unbox_ptr, &[tagged_ptr.into()], "fn_unbox")
                            .unwrap().try_as_basic_value().unwrap_basic().into_pointer_value();
                        return self.build_closure_call(cls_ptr, args, result_type, fn_ctx);
                    }
                }
                self.build_indirect_call(fn_val, args, result_type, fn_ctx)
            }
        }
    }

    /// Attempt a TCO loop-back for a tail self-call. Returns Some if TCO was emitted.
    fn try_tco_tail_call(
        &mut self,
        args: &[TypedExpr],
        result_type: &Type,
        fn_ctx: &mut FnCtx<'ctx, '_>,
    ) -> Option<BasicValueEnum<'ctx>> {
        let tco_info = fn_ctx.tco.as_ref().map(|tco| {
            (tco.loop_block, tco.param_allocs.clone(), tco.param_allocs.len())
        });
        let (loop_block, param_allocs, param_count) = tco_info?;
        if args.len() != param_count {
            return None;
        }
        // Evaluate all args before any stores so we use the old values.
        let compiled_args: Vec<_> = args.iter().map(|a| self.compile_expr(a, fn_ctx)).collect();
        for (alloc, val) in param_allocs.iter().zip(compiled_args.iter()) {
            self.builder.build_store(*alloc, *val).unwrap();
        }
        self.builder.build_unconditional_branch(loop_block).unwrap();
        // Unreachable block to keep IR well-formed after the branch.
        let post = self.context.append_basic_block(fn_ctx.llvm_fn, "tco_post");
        self.builder.position_at_end(post);
        Some(self.llvm_type(result_type).const_zero())
    }

    /// Call a known global LLVM function, boxing args where the Lin param type is Union/TypeVar.
    fn call_global_fn(
        &mut self,
        llvm_fn: FunctionValue<'ctx>,
        func: &TypedExpr,
        args: &[TypedExpr],
        result_type: &Type,
        fn_ctx: &mut FnCtx<'ctx, '_>,
    ) -> BasicValueEnum<'ctx> {
        let total_params = llvm_fn.count_params() as usize;
        if args.len() < total_params {
            if let Type::Function { params: remaining, ret: final_ret } = result_type {
                return self.build_partial_application(llvm_fn, args, remaining, final_ret, fn_ctx);
            }
        }
        let lin_param_types = Self::fn_param_types(func);
        // Pre-compute which Function-typed args are "owned" (fresh alloc, no retain needed).
        // An arg is owned if it is a fresh alloc expression OR if it is a named global function
        // that will be wrapped in a new closure struct by wrap_named_fn_as_closure.
        let arg_is_fn_owned: Vec<bool> = args.iter().enumerate().map(|(i, a)| {
            let param_ty = lin_param_types.get(i).cloned().unwrap_or_else(|| a.ty());
            if !matches!(param_ty, Type::Function { .. }) {
                return true; // Not a Function param — retain not needed here.
            }
            // Function literal → fresh closure alloc.
            if Self::expr_is_owned_alloc(a) { return true; }
            // Named global function → wrap_named_fn_as_closure creates a fresh alloc.
            if let TypedExpr::LocalGet { slot, .. } = a {
                if self.global_fn_slots.contains_key(slot) { return true; }
            }
            false // Non-owned local closure slot — needs retain.
        }).collect();
        let compiled_args: Vec<BasicMetadataValueEnum> = args
            .iter()
            .enumerate()
            .map(|(i, a)| {
                let param_ty = lin_param_types.get(i).cloned().unwrap_or_else(|| a.ty());
                let arg_ty = a.ty();
                // When passing a Function as Json, compile it as a closure with uniform
                // env_ptr convention and TypeVar(MAX) return so it boxes its return value.
                // This ensures call_body can uniformly call it as fn(env_ptr, args...) -> ptr.
                if Self::is_union_type(&param_ty) && matches!(arg_ty, Type::Function { .. }) {
                    if let TypedExpr::Function { params, body, captures, .. } = a {
                        // Compile with TypeVar(MAX) so the closure boxes its return value.
                        // When called through a TypeVar/Json slot, call_body calls the function
                        // via an indirect call with ptr return type — the closure must return ptr.
                        let json_ret = Type::TypeVar(u32::MAX);
                        let cls = self.compile_closure(None, params, body, &json_ret, captures, fn_ctx);
                        return self.box_value(cls, &arg_ty).into();
                    }
                }
                // When passing an Array of Functions as Json (e.g. parallel([() => 1, ...])),
                // compile each Function element with TypeVar(MAX) return so runtime callers
                // can uniformly call them via ptr return type.
                if Self::is_union_type(&param_ty) {
                    if let TypedExpr::MakeArray { elements, ty: arr_ty, .. } = a {
                        if let Type::Array(inner) = arr_ty {
                            if matches!(**inner, Type::Function { .. }) {
                                let ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
                                let i64_ty = self.context.i64_type();
                                let n = elements.len();
                                let arr = self.builder.build_call(self.rt_array_alloc,
                                    &[i64_ty.const_int(n.max(4) as u64, false).into()], "fn_arr")
                                    .unwrap().try_as_basic_value().unwrap_basic();
                                let elems: Vec<TypedExpr> = elements.iter().cloned().collect();
                                for elem in &elems {
                                    let elem_ty = elem.ty();
                                    let val = if let TypedExpr::Function { params, body, captures, .. } = elem {
                                        let json_ret = Type::TypeVar(u32::MAX);
                                        self.compile_closure(None, params, body, &json_ret, captures, fn_ctx)
                                    } else {
                                        self.compile_expr(elem, fn_ctx)
                                    };
                                    self.tagged_array_push_value(arr, val, &elem_ty);
                                }
                                let boxed = self.box_value(arr, arr_ty);
                                return boxed.into();
                            }
                        }
                    }
                }
                // When passing a named (global) function as a concrete Function-typed param,
                // wrap it in a closure stub {wrapper_fn, null_env} using the closure ABI.
                if matches!(param_ty, Type::Function { .. }) && matches!(arg_ty, Type::Function { .. }) {
                    if let TypedExpr::LocalGet { slot, .. } = a {
                        if let Some(&named_fn) = self.global_fn_slots.get(slot) {
                            return self.wrap_named_fn_as_closure(named_fn, &param_ty).into();
                        }
                    }
                    if let TypedExpr::Function { params, body, captures, ret_type, .. } = a {
                        // If param expects TypeVar args, compile closure with TypeVar return so the
                        // call site can safely unbox ptr to the declared concrete return type.
                        let has_typevar_params = if let Type::Function { params: fn_params, .. } = &param_ty {
                            fn_params.iter().any(|p| matches!(p, Type::TypeVar(_)))
                        } else { false };
                        let effective_ret = if has_typevar_params && !matches!(ret_type, Type::TypeVar(_) | Type::Never) {
                            &Type::TypeVar(u32::MAX)
                        } else {
                            ret_type
                        };
                        let cls = self.compile_closure(None, params, body, effective_ret, captures, fn_ctx);
                        return cls.into();
                    }
                }
                let val = self.compile_expr(a, fn_ctx);
                if Self::is_union_type(&param_ty) && !Self::is_union_type(&arg_ty) {
                    // Arg is concrete, param expects tagged — box it.
                    self.box_value(val, &arg_ty).into()
                } else if Self::is_union_type(&param_ty) && Self::is_union_type(&arg_ty) && !val.is_pointer_value() {
                    // arg_ty is TypeVar but the actual LLVM value is a scalar (e.g. result of
                    // TypeVar+TypeVar arithmetic was coerced to i32). Must box before passing.
                    let concrete_ty = if val.is_int_value() {
                        let bits = val.into_int_value().get_type().get_bit_width();
                        match bits { 8 => Type::Int8, 16 => Type::Int16, 32 => Type::Int32, 64 => Type::Int64, _ => Type::Int32 }
                    } else if val.is_float_value() {
                        let bits = val.into_float_value().get_type().get_bit_width();
                        if bits <= 32 { Type::Float32 } else { Type::Float64 }
                    } else { arg_ty.clone() };
                    self.box_value(val, &concrete_ty).into()
                } else if Self::is_union_type(&arg_ty) && !Self::is_union_type(&param_ty) {
                    // Arg is tagged/union, param expects concrete — unbox/coerce it.
                    self.coerce_typevar(val, &arg_ty, &param_ty).into()
                } else if arg_ty.is_integer() && param_ty.is_integer() && arg_ty != param_ty {
                    // Integer width mismatch — widen or truncate to match param type.
                    let target_llvm = self.llvm_type(&param_ty).into_int_type();
                    let iv = val.into_int_value();
                    let iv_bits = iv.get_type().get_bit_width();
                    let tgt_bits = target_llvm.get_bit_width();
                    if tgt_bits > iv_bits {
                        if arg_ty.is_signed() {
                            self.builder.build_int_s_extend(iv, target_llvm, "sext").unwrap().into()
                        } else {
                            self.builder.build_int_z_extend(iv, target_llvm, "zext").unwrap().into()
                        }
                    } else {
                        self.builder.build_int_truncate(iv, target_llvm, "trunc").unwrap().into()
                    }
                } else {
                    val.into()
                }
            })
            .collect();
        // Retain non-owned Function-typed args so the callee can safely release them at return.
        // Without this, the callee's release would decrement rc to 0 on a value the caller
        // still holds (use-after-free on subsequent calls with the same closure).
        for (i, is_owned) in arg_is_fn_owned.iter().enumerate() {
            if !is_owned {
                if let Some(BasicMetadataValueEnum::PointerValue(ptr)) = compiled_args.get(i) {
                    self.builder.build_call(self.rt_rc_retain, &[(*ptr).into()], "").unwrap();
                }
            }
        }
        self.builder.build_call(llvm_fn, &compiled_args, "call")
            .unwrap()
            .try_as_basic_value()
            .basic()
            .unwrap_or_else(|| self.llvm_type(result_type).const_zero())
    }

    /// Call a function via a slot-stored pointer (closure or bare fn ptr), boxing args where needed.
    fn call_slot_fn(
        &mut self,
        ptr: PointerValue<'ctx>,
        func: &TypedExpr,
        args: &[TypedExpr],
        result_type: &Type,
        fn_ctx: &mut FnCtx<'ctx, '_>,
    ) -> BasicValueEnum<'ctx> {
        let fn_param_types = Self::fn_param_types(func);
        let compiled_args: Vec<BasicMetadataValueEnum> = args
            .iter()
            .enumerate()
            .map(|(i, a)| {
                let param_ty = fn_param_types.get(i).cloned().unwrap_or_else(|| a.ty());
                let val = self.compile_expr(a, fn_ctx);
                let arg_ty = a.ty();
                if Self::is_union_type(&param_ty) && !Self::is_union_type(&arg_ty) {
                    self.box_value(val, &arg_ty).into()
                } else if arg_ty.is_integer() && param_ty.is_integer() && arg_ty != param_ty {
                    let target_llvm = self.llvm_type(&param_ty).into_int_type();
                    let iv = val.into_int_value();
                    let iv_bits = iv.get_type().get_bit_width();
                    let tgt_bits = target_llvm.get_bit_width();
                    if tgt_bits > iv_bits {
                        if arg_ty.is_signed() {
                            self.builder.build_int_s_extend(iv, target_llvm, "sext").unwrap().into()
                        } else {
                            self.builder.build_int_z_extend(iv, target_llvm, "zext").unwrap().into()
                        }
                    } else {
                        self.builder.build_int_truncate(iv, target_llvm, "trunc").unwrap().into()
                    }
                } else {
                    val.into()
                }
            })
            .collect();
        let ret_llvm = self.llvm_type(result_type);
        let param_meta_types: Vec<BasicMetadataTypeEnum> = fn_param_types
            .iter()
            .map(|pt| self.llvm_type(pt).into())
            .chain(args.iter().skip(fn_param_types.len()).map(|a| self.llvm_type(&a.ty()).into()))
            .collect();
        let fn_ty = ret_llvm.fn_type(&param_meta_types, false);
        self.builder.build_indirect_call(fn_ty, ptr, &compiled_args, "call")
            .unwrap()
            .try_as_basic_value()
            .basic()
            .unwrap_or_else(|| self.llvm_type(result_type).const_zero())
    }

    /// Extract the Lin parameter types from a function expression's type annotation.
    fn fn_param_types(func: &TypedExpr) -> Vec<Type> {
        if let Type::Function { params, .. } = func.ty() { params } else { vec![] }
    }

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
        let ret_llvm = self.llvm_type(final_ret);
        let wrapper_fn_ty = ret_llvm.fn_type(&wrapper_param_tys, false);
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
            match call.try_as_basic_value().basic() {
                Some(v) => { self.builder.build_return(Some(&v)).unwrap(); }
                None => { self.builder.build_return(None).unwrap(); }
            }
        }
        self.builder.position_at_end(current_block);
        cls_ptr.into()
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

        // Build closure struct: { rc, _pad, fn_ptr, env_ptr } + u64 env_size
        let cls_struct_ty = self.closure_struct_type();
        let cls_ptr = self.builder
            .build_call(self.rt_alloc, &[self.context.i64_type().const_int(32, false).into()], "papp_cls")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();
        let rc_field = self.builder.build_struct_gep(cls_struct_ty, cls_ptr, 0, "papp_cls_rc").unwrap();
        self.builder.build_store(rc_field, self.context.i32_type().const_int(1, false)).unwrap();
        let fn_field = self.builder.build_struct_gep(cls_struct_ty, cls_ptr, 2, "papp_cls_fn").unwrap();
        self.builder.build_store(fn_field, wrapper_fn.as_global_value().as_pointer_value()).unwrap();
        let env_field = self.builder.build_struct_gep(cls_struct_ty, cls_ptr, 3, "papp_cls_env").unwrap();
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

        // If the result is still a function, this is a partial application of a closure:
        // bundle the current closure + new args into a new wrapper closure.
        if let Type::Function { params: remaining_params, ret: final_ret } = result_type {
            let compiled_args: Vec<BasicValueEnum> = args.iter()
                .map(|a| self.compile_expr(a, fn_ctx))
                .collect();
            let arg_types: Vec<BasicTypeEnum> = compiled_args.iter()
                .map(|v| v.get_type())
                .collect();

            // Env struct: { ptr (closure_ptr), arg0, arg1, ... }
            let mut env_field_types: Vec<BasicTypeEnum> = vec![ptr_ty.into()];
            env_field_types.extend_from_slice(&arg_types);
            let env_struct_ty = self.context.struct_type(&env_field_types, false);
            let env_size = env_struct_ty.size_of().unwrap();
            let env_size_i64 = self.builder.build_int_z_extend_or_bit_cast(env_size, self.context.i64_type(), "papp_env_sz").unwrap();
            let env_ptr2 = self.builder.build_call(self.rt_alloc, &[env_size_i64.into()], "papp_env").unwrap()
                .try_as_basic_value().unwrap_basic().into_pointer_value();

            // Store closure_ptr as first field.
            let cls_field = self.builder.build_struct_gep(env_struct_ty, env_ptr2, 0, "papp_cls_f").unwrap();
            self.builder.build_store(cls_field, closure_ptr).unwrap();
            // Store each new arg.
            for (i, val) in compiled_args.iter().enumerate() {
                let f = self.builder.build_struct_gep(env_struct_ty, env_ptr2, (i + 1) as u32, "papp_f").unwrap();
                self.builder.build_store(f, *val).unwrap();
            }

            // Build wrapper: (env_ptr, ...remaining_params) -> final_ret
            let wrapper_name = format!("__papp_cls_{}", self.closure_count);
            self.closure_count += 1;
            let remaining_llvm: Vec<BasicMetadataTypeEnum> = remaining_params.iter()
                .map(|t| self.llvm_param_type(t))
                .collect();
            let final_ret_llvm = self.llvm_type(final_ret);
            let mut wrapper_param_types: Vec<BasicMetadataTypeEnum> = vec![ptr_ty.into()];
            wrapper_param_types.extend_from_slice(&remaining_llvm);
            let wrapper_fn_ty = final_ret_llvm.fn_type(&wrapper_param_types, false);
            let wrapper_fn = self.module.add_function(&wrapper_name, wrapper_fn_ty, None);

            let saved_block = self.builder.get_insert_block().unwrap();
            let wrapper_entry = self.context.append_basic_block(wrapper_fn, "entry");
            self.builder.position_at_end(wrapper_entry);

            let w_env_ptr = wrapper_fn.get_nth_param(0).unwrap().into_pointer_value();
            // Load closure_ptr from env.
            let cls_fp = self.builder.build_struct_gep(env_struct_ty, w_env_ptr, 0, "wcls_p").unwrap();
            let inner_cls_ptr = self.builder.build_load(ptr_ty, cls_fp, "inner_cls").unwrap().into_pointer_value();

            // Load stored args from env.
            let mut all_call_args: Vec<BasicMetadataValueEnum> = Vec::new();
            for (i, ty) in arg_types.iter().enumerate() {
                let fp = self.builder.build_struct_gep(env_struct_ty, w_env_ptr, (i + 1) as u32, "warg_p").unwrap();
                let v = self.builder.build_load(*ty, fp, "warg").unwrap();
                all_call_args.push(v.into());
            }
            // Append remaining params.
            for i in 0..remaining_params.len() {
                all_call_args.push(wrapper_fn.get_nth_param((i + 1) as u32).unwrap().into());
            }

            // Call the inner closure with all combined args.
            // The inner closure call goes through build_closure_call_ptr.
            let inner_result = self.call_closure_ptr_with_args(inner_cls_ptr, &all_call_args, &all_call_args.iter().map(|a| {
                match a {
                    BasicMetadataValueEnum::IntValue(v) => BasicValueEnum::IntValue(*v),
                    BasicMetadataValueEnum::FloatValue(v) => BasicValueEnum::FloatValue(*v),
                    BasicMetadataValueEnum::PointerValue(v) => BasicValueEnum::PointerValue(*v),
                    _ => self.context.i8_type().const_zero().into(),
                }
            }).collect::<Vec<_>>(), final_ret);
            self.builder.build_return(Some(&inner_result)).unwrap();
            self.builder.position_at_end(saved_block);

            // Build the outer closure struct { rc, _pad, fn_ptr, env_ptr } + u64 env_size
            let cls_struct_ty = self.closure_struct_type();
            let cls_ptr = self.builder.build_call(self.rt_alloc, &[self.context.i64_type().const_int(32, false).into()], "papp_cls").unwrap()
                .try_as_basic_value().unwrap_basic().into_pointer_value();
            let rc_field = self.builder.build_struct_gep(cls_struct_ty, cls_ptr, 0, "papp_cls_rc").unwrap();
            self.builder.build_store(rc_field, self.context.i32_type().const_int(1, false)).unwrap();
            let fn_field = self.builder.build_struct_gep(cls_struct_ty, cls_ptr, 2, "papp_cls_fn").unwrap();
            self.builder.build_store(fn_field, wrapper_fn.as_global_value().as_pointer_value()).unwrap();
            let env_field = self.builder.build_struct_gep(cls_struct_ty, cls_ptr, 3, "papp_cls_env").unwrap();
            self.builder.build_store(env_field, env_ptr2).unwrap();
            return cls_ptr.into();
        }

        let closure_struct_type = self.closure_struct_type();
        let fn_field_ptr = self.builder
            .build_struct_gep(closure_struct_type, closure_ptr, 2, "cls_fn_ptr")
            .unwrap();
        let fn_ptr = self.builder
            .build_load(ptr_ty, fn_field_ptr, "cls_fn")
            .unwrap()
            .into_pointer_value();
        let env_field_ptr = self.builder
            .build_struct_gep(closure_struct_type, closure_ptr, 3, "cls_env_ptr")
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

    /// Call a closure through a statically-typed function parameter.
    /// Handles type marshalling: boxes concrete args to TaggedVal* if the declared param type is
    /// TypeVar (the closure was compiled expecting ptr args), and unboxes the return value if the
    /// declared return type is concrete but the closure returns TaggedVal*.
    fn build_closure_call_typed(
        &mut self,
        closure_ptr: PointerValue<'ctx>,
        args: &[TypedExpr],
        result_type: &Type,
        func_ty: &Type,
        fn_ctx: &mut FnCtx<'ctx, '_>,
    ) -> BasicValueEnum<'ctx> {
        let ptr_ty = self.context.ptr_type(AddressSpace::default());

        let (declared_params, declared_ret): (Vec<Type>, &Type) = if let Type::Function { params, ret } = func_ty {
            (params.clone(), ret.as_ref())
        } else {
            return self.build_closure_call(closure_ptr, args, result_type, fn_ctx);
        };

        let has_typevar_params = declared_params.iter().any(|p| matches!(p, Type::TypeVar(_)));

        if !has_typevar_params {
            // No TypeVar params — call normally without boxing.
            return self.build_closure_call(closure_ptr, args, result_type, fn_ctx);
        }

        // TypeVar params: closure expects all args as TaggedVal*.
        // Box each arg, call with ptr return, then unbox the result.
        let closure_struct_type = self.closure_struct_type();
        let fn_field_ptr = self.builder.build_struct_gep(closure_struct_type, closure_ptr, 2, "tcls_fn_ptr").unwrap();
        let fn_ptr = self.builder.build_load(ptr_ty, fn_field_ptr, "tcls_fn").unwrap().into_pointer_value();
        let env_field_ptr = self.builder.build_struct_gep(closure_struct_type, closure_ptr, 3, "tcls_env_ptr").unwrap();
        let env_ptr = self.builder.build_load(ptr_ty, env_field_ptr, "tcls_env").unwrap();

        let mut compiled_args: Vec<BasicMetadataValueEnum> = vec![env_ptr.into()];
        for (i, a) in args.iter().enumerate() {
            let val = self.compile_expr(a, fn_ctx);
            let arg_ty = a.ty();
            // If declared param is TypeVar, box arg to TaggedVal*.
            let boxed = if i < declared_params.len() && matches!(declared_params[i], Type::TypeVar(_)) {
                if matches!(arg_ty, Type::TypeVar(_) | Type::Union(_)) && val.is_pointer_value() {
                    val  // already TaggedVal*
                } else {
                    self.box_value(val, &arg_ty)
                }
            } else {
                val
            };
            compiled_args.push(boxed.into());
        }

        // Declare call as returning ptr (the closure always returns TaggedVal* for TypeVar ret).
        let mut param_types: Vec<BasicMetadataTypeEnum> = vec![ptr_ty.into()];
        for _ in args {
            param_types.push(ptr_ty.into());  // all TypeVar params → ptr
        }
        let call_fn_ty = ptr_ty.fn_type(&param_types, false);
        let raw_result = self.builder.build_indirect_call(call_fn_ty, fn_ptr, &compiled_args, "tcls_call")
            .unwrap().try_as_basic_value().basic()
            .unwrap_or_else(|| ptr_ty.const_null().into());

        // The closure was compiled with TypeVar params → it returns TaggedVal* (ptr).
        // Unbox the result to the declared return type if it's concrete.
        if !Self::is_union_type(result_type) && raw_result.is_pointer_value() {
            let tv_ptr = raw_result.into_pointer_value();
            if !tv_ptr.is_null() {
                self.load_tagged_val_payload(tv_ptr, result_type, &Type::TypeVar(0))
            } else {
                self.llvm_type(result_type).const_zero()
            }
        } else {
            raw_result
        }
    }

    /// Call a closure pointer with pre-compiled args and a known result type.
    fn call_closure_ptr_with_args(
        &mut self,
        closure_ptr: PointerValue<'ctx>,
        _args_meta: &[BasicMetadataValueEnum<'ctx>],
        args_vals: &[BasicValueEnum<'ctx>],
        result_type: &Type,
    ) -> BasicValueEnum<'ctx> {
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let closure_struct_type = self.closure_struct_type();
        let fn_field_ptr = self.builder
            .build_struct_gep(closure_struct_type, closure_ptr, 2, "cls_fn_ptr")
            .unwrap();
        let fn_ptr = self.builder
            .build_load(ptr_ty, fn_field_ptr, "cls_fn")
            .unwrap()
            .into_pointer_value();
        let env_field_ptr = self.builder
            .build_struct_gep(closure_struct_type, closure_ptr, 3, "cls_env_ptr")
            .unwrap();
        let env_ptr = self.builder
            .build_load(ptr_ty, env_field_ptr, "cls_env")
            .unwrap();

        let mut compiled_args: Vec<BasicMetadataValueEnum> = vec![env_ptr.into()];
        for v in args_vals {
            compiled_args.push((*v).into());
        }

        let ret_llvm = self.llvm_type(result_type);
        let mut param_types: Vec<BasicMetadataTypeEnum> = vec![ptr_ty.into()];
        for v in args_vals {
            param_types.push(v.get_type().into());
        }
        let fn_ty = ret_llvm.fn_type(&param_types, false);

        self.builder
            .build_indirect_call(fn_ty, fn_ptr, &compiled_args, "cls_call")
            .unwrap()
            .try_as_basic_value()
            .basic()
            .unwrap_or_else(|| self.llvm_type(result_type).const_zero())
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
        let null_val = || -> BasicValueEnum<'ctx> { self.context.ptr_type(AddressSpace::default()).const_null().into() };

        match name {
            "lin_print" => {
                // print: (value: any) => Null
                // Coerce arg to string, print it, release the temp string if we created one.
                let arg_val = self.compile_expr(&args[0], fn_ctx);
                let arg_ty = args[0].ty();
                let str_val = self.value_to_string(arg_val, &arg_ty, fn_ctx);
                self.builder.build_call(self.rt_print, &[str_val.into()], "").unwrap();
                // Release the string if it was a guaranteed-fresh allocation (numeric/bool/null conversion).
                // Don't release Str (borrowed from slot) or TypeVar (lin_tagged_to_string may not allocate).
                let is_fresh_alloc = matches!(arg_ty,
                    Type::Int8 | Type::Int16 | Type::Int32 | Type::Int64 |
                    Type::UInt8 | Type::UInt16 | Type::UInt32 | Type::UInt64 |
                    Type::Float32 | Type::Float64 | Type::Bool | Type::Null);
                if is_fresh_alloc {
                    self.builder.build_call(self.rt_string_release, &[str_val.into()], "").unwrap();
                }
                null_val()
            }
            "lin_to_string" => {
                let arg_val = self.compile_expr(&args[0], fn_ctx);
                let arg_ty = args[0].ty();
                self.value_to_string(arg_val, &arg_ty, fn_ctx)
            }
            "lin_value_key" => {
                let arg_val = self.compile_expr(&args[0], fn_ctx);
                let arg_ty = args[0].ty();
                // Box the value to a TaggedVal* then call lin_value_key(tagged) -> LinString*.
                let tagged = self.box_value(arg_val, &arg_ty);
                let ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
                let str_ty = self.context.ptr_type(inkwell::AddressSpace::default());
                let vk_fn = self.get_or_declare_fn(
                    "lin_value_key",
                    str_ty.fn_type(&[ptr_ty.into()], false),
                );
                self.builder
                    .build_call(vk_fn, &[tagged.into()], "vkey")
                    .unwrap()
                    .try_as_basic_value()
                    .unwrap_basic()
            }
            "lin_array_allocate" => {
                // arrayAllocate(n: Int32) => Json[] — null-filled tagged array of length n
                let n_val_raw = self.compile_expr(&args[0], fn_ctx);
                // When n comes from a TypeVar/Union context (e.g. chained index access), it
                // arrives as a TaggedVal* pointer. Unbox it to a concrete Int32 first.
                let n_val = if n_val_raw.is_pointer_value() {
                    self.unbox_value(n_val_raw, &Type::Int32)
                } else {
                    n_val_raw
                };
                let n_i64 = self.builder.build_int_s_extend(
                    n_val.into_int_value(),
                    self.context.i64_type(),
                    "alloc_n",
                ).unwrap();
                let ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
                let alloc_fn = self.get_or_declare_fn(
                    "lin_array_alloc_null",
                    ptr_ty.fn_type(&[self.context.i64_type().into()], false),
                );
                self.builder
                    .build_call(alloc_fn, &[n_i64.into()], "alloc_arr")
                    .unwrap()
                    .try_as_basic_value()
                    .unwrap_basic()
            }
            "lin_array_allocate_filled" => {
                // arrayAllocateFilled(n: Int32, val: T) => T[]
                // Dispatches to the appropriate flat or tagged runtime function based on val type.
                let n_val_raw = self.compile_expr(&args[0], fn_ctx);
                // When n comes from a TypeVar/Union context (e.g. chained index access), it
                // arrives as a TaggedVal* pointer. Unbox it to a concrete Int32 first.
                let n_val = if n_val_raw.is_pointer_value() {
                    self.unbox_value(n_val_raw, &Type::Int32)
                } else {
                    n_val_raw
                };
                let n_i64 = self.builder.build_int_s_extend(
                    n_val.into_int_value(),
                    self.context.i64_type(),
                    "fillalloc_n",
                ).unwrap();
                let fill_val = self.compile_expr(&args[1], fn_ctx);
                let fill_ty = args[1].ty();
                let ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
                if Self::is_flat_scalar(&fill_ty) {
                    let suffix = Self::flat_suffix(&fill_ty);
                    let fn_name = format!("lin_flat_array_alloc_filled_{}", suffix);
                    let llvm_elem_ty = self.llvm_type(&fill_ty);
                    let alloc_fn = self.get_or_declare_fn(
                        &fn_name,
                        ptr_ty.fn_type(&[self.context.i64_type().into(), llvm_elem_ty.into()], false),
                    );
                    self.builder
                        .build_call(alloc_fn, &[n_i64.into(), fill_val.into()], "fillflat_arr")
                        .unwrap()
                        .try_as_basic_value()
                        .unwrap_basic()
                } else {
                    // Non-scalar fill: allocate null-filled tagged array then set each slot.
                    // This is correct but not as fast as the scalar path.
                    let alloc_fn = self.get_or_declare_fn(
                        "lin_array_alloc_null",
                        ptr_ty.fn_type(&[self.context.i64_type().into()], false),
                    );
                    let arr = self.builder
                        .build_call(alloc_fn, &[n_i64.into()], "fillgen_arr")
                        .unwrap()
                        .try_as_basic_value()
                        .unwrap_basic();
                    // Overwrite every slot with the fill value.
                    let i_alloc = self.builder.build_alloca(self.context.i64_type(), "fi").unwrap();
                    self.builder.build_store(i_alloc, self.context.i64_type().const_zero()).unwrap();
                    let llvm_fn = fn_ctx.llvm_fn;
                    let fill_check = self.context.append_basic_block(llvm_fn, "fill_check");
                    let fill_body = self.context.append_basic_block(llvm_fn, "fill_body");
                    let fill_exit = self.context.append_basic_block(llvm_fn, "fill_exit");
                    self.builder.build_unconditional_branch(fill_check).unwrap();
                    self.builder.position_at_end(fill_check);
                    let cur = self.builder.build_load(self.context.i64_type(), i_alloc, "fi").unwrap().into_int_value();
                    let cond = self.builder.build_int_compare(inkwell::IntPredicate::SLT, cur, n_i64, "fill_cond").unwrap();
                    self.builder.build_conditional_branch(cond, fill_body, fill_exit).unwrap();
                    self.builder.position_at_end(fill_body);
                    let tagged_fill = self.box_value(fill_val, &fill_ty);
                    let set_fn = self.get_or_declare_fn("lin_array_set",
                        self.context.void_type().fn_type(&[ptr_ty.into(), self.context.i64_type().into(), ptr_ty.into()], false));
                    self.builder.build_call(set_fn, &[arr.into(), cur.into(), tagged_fill.into()], "").unwrap();
                    let next = self.builder.build_int_add(cur, self.context.i64_type().const_int(1, false), "fi_next").unwrap();
                    self.builder.build_store(i_alloc, next).unwrap();
                    self.builder.build_unconditional_branch(fill_check).unwrap();
                    self.builder.position_at_end(fill_exit);
                    arr
                }
            }
            "lin_length" => {
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
                        // Dispatch dynamically based on tag (string/array/object).
                        let ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
                        let i32_ty = self.context.i32_type();
                        let len_dyn_fn = self.get_or_declare_fn("lin_length_dyn",
                            i32_ty.fn_type(&[ptr_ty.into()], false));
                        self.builder
                            .build_call(len_dyn_fn, &[arg_val.into()], "dyn_len")
                            .unwrap()
                            .try_as_basic_value()
                            .unwrap_basic()
                    }
                    Type::Object(_) => {
                        let ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
                        let i64_ty = self.context.i64_type();
                        let obj_len_fn = self.get_or_declare_fn("lin_object_length",
                            i64_ty.fn_type(&[ptr_ty.into()], false));
                        let len_i64 = self.builder
                            .build_call(obj_len_fn, &[arg_val.into()], "olen")
                            .unwrap()
                            .try_as_basic_value()
                            .unwrap_basic()
                            .into_int_value();
                        self.builder
                            .build_int_truncate(len_i64, self.context.i32_type(), "olen32")
                            .unwrap()
                            .into()
                    }
                    _ => self.context.i32_type().const_zero().into(),
                }
            }
            "lin_push" => {
                let arr_raw = self.compile_expr(&args[0], fn_ctx);
                let arr_ty = args[0].ty();
                let elem_raw = self.compile_expr(&args[1], fn_ctx);
                let elem_ty = args[1].ty();
                if matches!(arr_ty, Type::TypeVar(_) | Type::Union(_)) {
                    // arr is a TaggedVal* containing a LinArray* (flat or tagged).
                    // Unbox to get the LinArray*, then use lin_push_dyn which dispatches
                    // based on the array's elem_tag (flat or tagged format).
                    let ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
                    let arr_val = self.builder
                        .build_call(self.rt_unbox_ptr, &[arr_raw.into()], "push_arr")
                        .unwrap().try_as_basic_value().unwrap_basic();
                    // Box the element into a TaggedVal* for lin_push_dyn.
                    let elem_is_fresh_box = !matches!(elem_ty, Type::TypeVar(_) | Type::Union(_));
                    let elem_tagged = if elem_is_fresh_box {
                        self.box_value(elem_raw, &elem_ty)
                    } else {
                        elem_raw
                    };
                    let push_dyn_fn = self.get_or_declare_fn("lin_push_dyn",
                        self.context.void_type().fn_type(&[ptr_ty.into(), ptr_ty.into()], false));
                    self.builder.build_call(push_dyn_fn, &[arr_val.into(), elem_tagged.into()], "").unwrap();
                    // Release the TaggedVal* box we created — lin_push_dyn copies the value.
                    if elem_is_fresh_box {
                        self.builder.build_call(self.rt_tagged_release, &[elem_tagged.into()], "").unwrap();
                    }
                } else {
                    self.tagged_array_push_value(arr_raw, elem_raw, &elem_ty);
                }
                null_val()
            }
            "lin_array_set" => {
                // set(arr: T[], idx: Int32, val: T) => Null
                // Unbox the array arg to LinArray*, box the element to TaggedVal*, call lin_array_set.
                let arr_raw = self.compile_expr(&args[0], fn_ctx);
                let arr_ty = args[0].ty();
                let idx_raw = self.compile_expr(&args[1], fn_ctx);
                let idx_ty = args[1].ty();
                let elem_raw = self.compile_expr(&args[2], fn_ctx);
                let elem_ty = args[2].ty();

                let ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
                let i32_ty = self.context.i32_type();
                let i64_ty = self.context.i64_type();

                // Get the LinArray* — either unbox from TaggedVal* or use directly.
                let arr_ptr = if matches!(arr_ty, Type::TypeVar(_) | Type::Union(_)) {
                    self.builder.build_call(self.rt_unbox_ptr, &[arr_raw.into()], "set_arr")
                        .unwrap().try_as_basic_value().unwrap_basic()
                } else {
                    arr_raw
                };

                // Get the index as an i32 integer — unbox from TaggedVal* if needed.
                let idx_i32 = if matches!(idx_ty, Type::TypeVar(_) | Type::Union(_)) {
                    self.builder.build_call(self.rt_unbox_int32, &[idx_raw.into()], "set_idx_unbox")
                        .unwrap().try_as_basic_value().unwrap_basic().into_int_value()
                } else {
                    idx_raw.into_int_value()
                };

                // Sign-extend index to i64.
                let idx_i64 = self.builder
                    .build_int_s_extend_or_bit_cast(idx_i32, i64_ty, "set_idx")
                    .unwrap();

                // Box the element into a TaggedVal*.
                let elem_tagged = if matches!(elem_ty, Type::TypeVar(_) | Type::Union(_)) {
                    elem_raw
                } else {
                    self.box_value(elem_raw, &elem_ty)
                };

                let set_fn = self.get_or_declare_fn("lin_array_set",
                    self.context.void_type().fn_type(&[ptr_ty.into(), i64_ty.into(), ptr_ty.into()], false));
                self.builder.build_call(set_fn, &[arr_ptr.into(), idx_i64.into(), elem_tagged.into()], "").unwrap();
                null_val()
            }
            "lin_range" => {
                // range(start: Int32, end: Int32) => Iterator<Int32>
                // Eagerly create a flat i32 LinArray so the `for` loop can use
                // lin_flat_array_get_i32 — consistent with how Array(Int32) is read.
                let start_val = self.compile_expr(&args[0], fn_ctx);
                let end_val = self.compile_expr(&args[1], fn_ctx);
                let start_i32 = start_val.into_int_value();
                let end_i32 = end_val.into_int_value();

                let init_cap = self.context.i64_type().const_int(4, false);
                let arr_ptr = {
                    let ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
                    let fn_ty = ptr_ty.fn_type(&[self.context.i64_type().into()], false);
                    let alloc_fn = self.get_or_declare_fn("lin_flat_array_alloc_i32", fn_ty);
                    self.builder.build_call(alloc_fn, &[init_cap.into()], "rng_arr").unwrap()
                        .try_as_basic_value().unwrap_basic().into_pointer_value()
                };

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
                self.flat_array_push(arr_ptr.into(), cur.into(), &Type::Int32);
                let next = self.builder.build_int_add(cur, self.context.i32_type().const_int(1, false), "rng_next").unwrap();
                self.builder.build_store(i_alloc, next).unwrap();
                self.builder.build_unconditional_branch(rng_fill_check).unwrap();
                self.builder.position_at_end(rng_fill_exit);
                arr_ptr.into()
            }
            "lin_for" => {
                // for(iterable, body) — generate an inline loop.
                let owns_iterable = Self::expr_is_owned_alloc(&args[0]);
                let iterable_ty = args[0].ty();
                let iterable_val = self.compile_expr(&args[0], fn_ctx);
                let body_expr = &args[1];
                let result = self.compile_for_loop(iterable_val, &iterable_ty, body_expr, fn_ctx);
                if owns_iterable {
                    self.builder.build_call(self.rt_array_release, &[iterable_val.into()], "").unwrap();
                }
                result
            }
            "lin_while" => {
                // while(iterable, body) — like for but stops when body returns false.
                let iterable_ty = args[0].ty();
                let iterable_val = self.compile_expr(&args[0], fn_ctx);
                let body_expr = &args[1];
                self.compile_while_loop(iterable_val, &iterable_ty, body_expr, fn_ctx)
            }
            "lin_iter" => {
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

                // Allocate output array. Use flat array for scalar element types (matches range).
                let out_arr = if Self::is_flat_scalar(&elem_ty) {
                    let suffix = Self::flat_suffix(&elem_ty);
                    let alloc_name = format!("lin_flat_array_alloc_{}", suffix);
                    let ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
                    let fn_ty = ptr_ty.fn_type(&[i64_ty.into()], false);
                    let alloc_fn = self.get_or_declare_fn(&alloc_name, fn_ty);
                    self.builder.build_call(alloc_fn, &[i64_ty.const_int(4, false).into()], "iter_out")
                        .unwrap().try_as_basic_value().unwrap_basic()
                } else {
                    self.builder
                        .build_call(self.rt_array_alloc, &[i64_ty.const_int(4, false).into()], "iter_out")
                        .unwrap()
                        .try_as_basic_value().unwrap_basic()
                };

                // Call init() to get initial state first (before alloca so we know the actual LLVM type).
                let init_state = self.call_body(&args[0], &[], &state_ty, fn_ctx);

                // Determine actual storage type from the concrete value returned by init.
                // When state_ty is TypeVar, init may return a concrete scalar (e.g. i32) —
                // alloca must match the actual type, not the TypeVar (which maps to ptr).
                let actual_state_llvm_ty = init_state.get_type();
                let actual_state_ty = if matches!(state_ty, Type::TypeVar(_)) {
                    // Infer concrete type from the LLVM type of the init result.
                    if init_state.is_int_value() {
                        match init_state.into_int_value().get_type().get_bit_width() {
                            1 => Type::Bool,
                            8 => Type::Int8,
                            16 => Type::Int16,
                            32 => Type::Int32,
                            _ => Type::Int64,
                        }
                    } else if init_state.is_float_value() {
                        Type::Float64
                    } else {
                        state_ty.clone()
                    }
                } else {
                    state_ty.clone()
                };

                // State alloca to hold current state across iterations.
                let state_alloc = self.builder.build_alloca(actual_state_llvm_ty, "iter_state").unwrap();
                self.builder.build_store(state_alloc, init_state).unwrap();

                // Loop blocks.
                let check_b = self.context.append_basic_block(llvm_fn_val, "iter_check");
                let body_b = self.context.append_basic_block(llvm_fn_val, "iter_body");
                let exit_b = self.context.append_basic_block(llvm_fn_val, "iter_exit");

                self.builder.build_unconditional_branch(check_b).unwrap();
                self.builder.position_at_end(check_b);
                let state_val = self.builder.build_load(actual_state_llvm_ty, state_alloc, "state").unwrap();
                // Call cond(state) -> Bool.
                let cond_result = self.call_body(&args[1], &[state_val], &Type::Bool, fn_ctx);
                let cond_bool = if cond_result.is_int_value() {
                    self.builder.build_int_truncate(cond_result.into_int_value(), self.context.bool_type(), "cond_b").unwrap()
                } else {
                    self.context.bool_type().const_zero()
                };
                self.builder.build_conditional_branch(cond_bool, body_b, exit_b).unwrap();

                self.builder.position_at_end(body_b);
                let state_val2 = self.builder.build_load(actual_state_llvm_ty, state_alloc, "state2").unwrap();
                // Call current(state) -> T (use args[3] = current).
                let elem_val = self.call_body(&args[3], &[state_val2], &elem_ty, fn_ctx);
                // Push elem_val: use flat push for scalar types (matches flat alloc above).
                if Self::is_flat_scalar(&elem_ty) {
                    self.flat_array_push(out_arr, elem_val, &elem_ty);
                } else {
                    self.tagged_array_push_value(out_arr, elem_val, &elem_ty);
                }
                // Need state for next call after getting current.
                let state_val3 = self.builder.build_load(actual_state_llvm_ty, state_alloc, "state3").unwrap();
                // Call next(state) -> state (use args[2] = next).
                let next_state_raw = self.call_body(&args[2], &[state_val3], &actual_state_ty, fn_ctx);
                // Coerce back to state storage type when there's a mismatch. An inline lambda that
                // receives a boxed TaggedVal* and does arithmetic extracts the scalar (i32), but
                // the state alloca holds a ptr — rebox the scalar before storing.
                let next_state = if next_state_raw.get_type() != actual_state_llvm_ty
                    && actual_state_llvm_ty.is_pointer_type()
                    && !next_state_raw.is_pointer_value()
                {
                    let scalar_ty = if next_state_raw.is_int_value() {
                        match next_state_raw.into_int_value().get_type().get_bit_width() {
                            1 => Type::Bool, 8 => Type::Int8, 16 => Type::Int16,
                            32 => Type::Int32, _ => Type::Int64,
                        }
                    } else { Type::Float64 };
                    self.box_value(next_state_raw, &scalar_ty)
                } else {
                    next_state_raw
                };
                self.builder.build_store(state_alloc, next_state).unwrap();
                self.builder.build_unconditional_branch(check_b).unwrap();

                self.builder.position_at_end(exit_b);
                out_arr
            }
            "lin_map" => {
                // map(iterable, fn) => Iterator<U>
                let owns_iterable = Self::expr_is_owned_alloc(&args[0]);
                let iterable_val = self.compile_expr(&args[0], fn_ctx);
                let iterable_ty = args[0].ty();
                let body_expr = &args[1];
                // Derive output element type from the explicit result type if possible,
                // otherwise fall back to the lambda's declared return type.
                let out_elem_ty = match result_type {
                    Type::Iterator(t) if !matches!(**t, Type::TypeVar(_) | Type::Null) => *t.clone(),
                    Type::Array(t) if !matches!(**t, Type::TypeVar(_) | Type::Null) => *t.clone(),
                    _ => match &args[1].ty() {
                        Type::Function { ret, .. } if !matches!(**ret, Type::TypeVar(_) | Type::Null) => *ret.clone(),
                        _ => Type::TypeVar(u32::MAX),
                    },
                };
                let result = self.compile_map_loop(iterable_val, &iterable_ty, body_expr, &out_elem_ty, fn_ctx);
                if owns_iterable {
                    self.builder.build_call(self.rt_array_release, &[iterable_val.into()], "").unwrap();
                }
                result
            }
            "lin_filter" => {
                // filter(iterable, pred) => Iterator<T>
                let owns_iterable = Self::expr_is_owned_alloc(&args[0]);
                let iterable_val = self.compile_expr(&args[0], fn_ctx);
                let iterable_ty = args[0].ty();
                let body_expr = &args[1];
                let elem_ty = match &iterable_ty {
                    Type::Array(t) => *t.clone(),
                    Type::Iterator(t) => *t.clone(),
                    _ => Type::Null,
                };
                let result = self.compile_filter_loop(iterable_val, &iterable_ty, body_expr, &elem_ty, fn_ctx);
                if owns_iterable {
                    self.builder.build_call(self.rt_array_release, &[iterable_val.into()], "").unwrap();
                }
                result
            }
            "lin_reduce" => {
                // reduce(iterable, initial, fn) => U
                let owns_iterable = Self::expr_is_owned_alloc(&args[0]);
                let iterable_val = self.compile_expr(&args[0], fn_ctx);
                let iterable_ty = args[0].ty();
                let init_val = self.compile_expr(&args[1], fn_ctx);
                let init_ty = args[1].ty();
                let body_expr = &args[2];
                let result = self.compile_reduce_loop(iterable_val, &iterable_ty, init_val, &init_ty, body_expr, result_type, fn_ctx);
                if owns_iterable {
                    self.builder.build_call(self.rt_array_release, &[iterable_val.into()], "").unwrap();
                }
                result
            }
            "lin_keys" => {
                let ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
                let f = self.get_or_declare_fn("lin_object_keys",
                    ptr_ty.fn_type(&[ptr_ty.into()], false));
                let obj_val = self.compile_expr(&args[0], fn_ctx);
                let arg_ty = args[0].ty();
                // If the arg is a TaggedVal* (Json/TypeVar/Union), unbox to get LinObject*.
                let obj_ptr = if matches!(arg_ty, Type::TypeVar(_) | Type::Union(_)) {
                    self.builder.build_call(self.rt_unbox_ptr, &[obj_val.into()], "keys_unbox")
                        .unwrap().try_as_basic_value().unwrap_basic().into_pointer_value()
                } else if obj_val.is_pointer_value() {
                    obj_val.into_pointer_value()
                } else {
                    self.builder.build_call(self.rt_unbox_ptr, &[obj_val.into()], "keys_unbox")
                        .unwrap().try_as_basic_value().unwrap_basic().into_pointer_value()
                };
                self.builder.build_call(f, &[obj_ptr.into()], "keys")
                    .unwrap().try_as_basic_value().unwrap_basic()
            }

            "lin_object_set" => {
                // lin_object_set(obj: Object, key: String, val: Json) => Null
                // Unbox obj to LinObject*, unbox key to LinString*, box val to TaggedVal*, call rt.
                let obj_raw = self.compile_expr(&args[0], fn_ctx);
                let key_raw = self.compile_expr(&args[1], fn_ctx);
                let val_raw = self.compile_expr(&args[2], fn_ctx);
                let obj_ty = args[0].ty();
                let key_ty = args[1].ty();
                let val_ty = args[2].ty();

                // obj must be a raw *LinObject. If obj_ty is TypeVar/Union, unbox from TaggedVal*.
                let obj_ptr = if obj_raw.is_pointer_value() {
                    let op = obj_raw.into_pointer_value();
                    if matches!(obj_ty, Type::TypeVar(_) | Type::Union(_)) {
                        self.builder.build_call(self.rt_unbox_ptr, &[op.into()], "oset_obj_ub")
                            .unwrap().try_as_basic_value().unwrap_basic().into_pointer_value()
                    } else {
                        op
                    }
                } else {
                    self.builder.build_call(self.rt_unbox_ptr, &[obj_raw.into()], "oset_obj")
                        .unwrap().try_as_basic_value().unwrap_basic().into_pointer_value()
                };
                // Key must be a raw LinString*. If key is a tagged pointer (TypeVar/Union),
                // unbox it to extract the LinString* from the payload.
                let key_ptr = if key_raw.is_pointer_value() {
                    let kp = key_raw.into_pointer_value();
                    if matches!(key_ty, Type::TypeVar(_) | Type::Union(_)) {
                        // Tagged pointer — unbox to get raw LinString*.
                        self.builder.build_call(self.rt_unbox_ptr, &[kp.into()], "oset_key_ub")
                            .unwrap().try_as_basic_value().unwrap_basic().into_pointer_value()
                    } else {
                        kp
                    }
                } else {
                    self.builder.build_call(self.rt_unbox_ptr, &[key_raw.into()], "oset_key")
                        .unwrap().try_as_basic_value().unwrap_basic().into_pointer_value()
                };
                let val_is_fresh_box = !matches!(val_ty, Type::TypeVar(_) | Type::Union(_));
                let val_tagged = if val_is_fresh_box {
                    self.box_value(val_raw, &val_ty).into_pointer_value()
                } else {
                    val_raw.into_pointer_value()
                };
                self.builder.build_call(self.rt_object_set, &[obj_ptr.into(), key_ptr.into(), val_tagged.into()], "").unwrap();
                // Release the TaggedVal* box we created — lin_object_set copies the TaggedVal inline.
                if val_is_fresh_box {
                    self.builder.build_call(self.rt_tagged_release, &[val_tagged.into()], "").unwrap();
                }
                null_val()
            }

            "lin_exit" => {
                // exit(code: Int32) — terminate the process
                let code = self.compile_expr(&args[0], fn_ctx);
                let exit_fn = self.module.get_function("exit").unwrap_or_else(|| {
                    self.module.add_function(
                        "exit",
                        self.context.void_type().fn_type(&[self.context.i32_type().into()], false),
                        None,
                    )
                });
                self.builder.build_call(exit_fn, &[code.into_int_value().into()], "").unwrap();
                null_val()
            }

            // --- async/await/parallel/threadPool/worker ---
            "lin_async" | "lin_await" | "lin_parallel" | "lin_thread_pool" | "lin_worker" | "lin_race" | "lin_timeout" | "lin_retry"
            | "lin_request" | "lin_message" | "lin_close" => {
                self.compile_async_intrinsic(name, args, result_type, fn_ctx)
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
    /// Call a zero-arg thunk expression (Function { params: [], ret: T }) synchronously.
    /// Returns the boxed TaggedVal* result.
    fn call_thunk_and_box(
        &mut self,
        thunk_expr: &TypedExpr,
        fn_ctx: &mut FnCtx<'ctx, '_>,
    ) -> BasicValueEnum<'ctx> {
        let thunk_ty = thunk_expr.ty();
        let ret_type = if let Type::Function { ret, .. } = &thunk_ty { *ret.clone() } else { Type::Null };
        let thunk_val = self.compile_expr(thunk_expr, fn_ctx);
        let ptr_ty = self.context.ptr_type(AddressSpace::default());

        // When the thunk type is Json/TypeVar (opaque), it's a TaggedVal* holding a function.
        // Unbox it to get the closure struct pointer, then call via closure convention.
        if Self::is_union_type(&thunk_ty) || matches!(thunk_ty, Type::TypeVar(_)) {
            let tagged_ptr = thunk_val.into_pointer_value();
            let cls_ptr = self.builder.build_call(self.rt_unbox_ptr, &[tagged_ptr.into()], "thunk_cls").unwrap()
                .try_as_basic_value().unwrap_basic().into_pointer_value();
            let cls_ty = self.closure_struct_type();
            let fn_field = self.builder.build_struct_gep(cls_ty, cls_ptr, 2, "thunk_fn_f").unwrap();
            let fn_ptr = self.builder.build_load(ptr_ty, fn_field, "thunk_fn").unwrap().into_pointer_value();
            let env_field = self.builder.build_struct_gep(cls_ty, cls_ptr, 3, "thunk_env_f").unwrap();
            let env_ptr = self.builder.build_load(ptr_ty, env_field, "thunk_env").unwrap();
            // Call as closure: fn(env) → ptr (result is always a tagged/boxed value)
            let fn_ty = ptr_ty.fn_type(&[ptr_ty.into()], false);
            let result = self.builder.build_indirect_call(fn_ty, fn_ptr, &[env_ptr.into()], "thunk_res").unwrap()
                .try_as_basic_value().unwrap_basic();
            return result;
        }

        // All compiled Function expressions now return {fn_ptr, env_ptr} closure structs
        // (even non-capturing ones use {fn_ptr, null_env}). LocalGet slots may also hold structs.
        let is_closure = matches!(thunk_expr, TypedExpr::Function { .. })
            || matches!(thunk_expr, TypedExpr::LocalGet { .. });

        let result = if is_closure && thunk_val.is_pointer_value() {
            // Try closure call first — extract fn_ptr and env_ptr from closure struct.
            let closure_ptr = thunk_val.into_pointer_value();
            let closure_struct_ty = self.closure_struct_type();
            // Try to read fn_ptr field — if it fails (bare fn ptr), fall back.
            let fn_field = self.builder.build_struct_gep(closure_struct_ty, closure_ptr, 2, "cls_fn_ptr_a").unwrap();
            let fn_ptr = self.builder.build_load(ptr_ty, fn_field, "cls_fn_a").unwrap().into_pointer_value();
            let env_field = self.builder.build_struct_gep(closure_struct_ty, closure_ptr, 3, "cls_env_ptr_a").unwrap();
            let env_ptr = self.builder.build_load(ptr_ty, env_field, "cls_env_a").unwrap();
            let ret_llvm = self.llvm_type(&ret_type);
            let fn_ty = ret_llvm.fn_type(&[ptr_ty.into()], false);
            self.builder.build_indirect_call(fn_ty, fn_ptr, &[env_ptr.into()], "thunk_res").unwrap()
                .try_as_basic_value().basic()
                .unwrap_or_else(|| ret_llvm.const_zero())
        } else {
            // Plain function pointer.
            let fn_ptr = thunk_val.into_pointer_value();
            let ret_llvm = self.llvm_type(&ret_type);
            let fn_ty = ret_llvm.fn_type(&[], false);
            self.builder.build_indirect_call(fn_ty, fn_ptr, &[], "thunk_res").unwrap()
                .try_as_basic_value().basic()
                .unwrap_or_else(|| ret_llvm.const_zero())
        };
        self.box_value(result, &ret_type)
    }

    fn compile_async_intrinsic(
        &mut self,
        name: &str,
        args: &[TypedExpr],
        result_type: &Type,
        fn_ctx: &mut FnCtx<'ctx, '_>,
    ) -> BasicValueEnum<'ctx> {
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i32_ty = self.context.i32_type();

        match name {
            "lin_async" => {
                // 1-arg: async(thunk) → call thunk synchronously, wrap in LinPromise*
                // 2-arg: pool.async(thunk) → desugared as async(pool, thunk)
                let thunk_arg = if args.len() >= 2 { &args[1] } else { &args[0] };
                let pool_arg = if args.len() >= 2 { Some(&args[0]) } else { None };

                if let Some(pool_expr) = pool_arg {
                    // pool.async(thunk) — pass to pool (synchronous: call thunk directly)
                    let _pool_val = self.compile_expr(pool_expr, fn_ctx); // evaluate pool (unused in sync impl)
                    let thunk_val = self.compile_expr(thunk_arg, fn_ctx);
                    let is_closure = !matches!(thunk_arg, TypedExpr::Function { captures, .. } if captures.is_empty());
                    let boxed_result = if is_closure && thunk_val.is_pointer_value() {
                        let cls = thunk_val.into_pointer_value();
                        let cls_ty = self.closure_struct_type();
                        let fn_f = self.builder.build_struct_gep(cls_ty, cls, 2, "pa_fn_f").unwrap();
                        let fp = self.builder.build_load(ptr_ty, fn_f, "pa_fn").unwrap().into_pointer_value();
                        let env_f = self.builder.build_struct_gep(cls_ty, cls, 3, "pa_env_f").unwrap();
                        let ep = self.builder.build_load(ptr_ty, env_f, "pa_env").unwrap();
                        // Call fn(env) → ptr
                        let fn_ty = ptr_ty.fn_type(&[ptr_ty.into()], false);
                        self.builder.build_indirect_call(fn_ty, fp, &[ep.into()], "pa_res").unwrap()
                            .try_as_basic_value().unwrap_basic()
                    } else {
                        let fp = thunk_val.into_pointer_value();
                        // Use the actual return type of the thunk to match the LLVM function signature.
                        let thunk_ret = if let Type::Function { ret, .. } = thunk_arg.ty() { *ret } else { Type::Null };
                        let ret_llvm = self.llvm_type(&thunk_ret);
                        let fn_ty = ret_llvm.fn_type(&[], false);
                        self.builder.build_indirect_call(fn_ty, fp, &[], "pa_res").unwrap()
                            .try_as_basic_value().unwrap_basic()
                    };
                    let thunk_ret = if let Type::Function { ret, .. } = thunk_arg.ty() { *ret } else { Type::Null };
                    let make_promise = self.get_or_declare_fn("lin_make_promise",
                        ptr_ty.fn_type(&[ptr_ty.into()], false));
                    let result_ptr = if boxed_result.is_pointer_value() {
                        boxed_result.into_pointer_value()
                    } else {
                        // box the scalar result (e.g. Int32 from `() => 100`)
                        self.box_value(boxed_result, &thunk_ret).into_pointer_value()
                    };
                    self.builder.build_call(make_promise, &[result_ptr.into()], "pool_promise").unwrap()
                        .try_as_basic_value().unwrap_basic()
                } else {
                    // async(thunk) — plain async
                    let boxed_result = self.call_thunk_and_box(thunk_arg, fn_ctx);
                    let make_promise = self.get_or_declare_fn("lin_make_promise",
                        ptr_ty.fn_type(&[ptr_ty.into()], false));
                    let boxed_ptr = boxed_result.into_pointer_value();
                    self.builder.build_call(make_promise, &[boxed_ptr.into()], "promise").unwrap()
                        .try_as_basic_value().unwrap_basic()
                }
            }
            "lin_await" => {
                // await(promise) → unwrap LinPromise* to TaggedVal*
                let promise = self.compile_expr(&args[0], fn_ctx).into_pointer_value();
                let await_fn = self.get_or_declare_fn("lin_await_promise",
                    ptr_ty.fn_type(&[ptr_ty.into()], false));
                let tagged = self.builder.build_call(await_fn, &[promise.into()], "awaited").unwrap()
                    .try_as_basic_value().unwrap_basic();
                // Unbox if result is a concrete type.
                if !Self::is_union_type(result_type) && *result_type != Type::Null {
                    self.coerce_typevar(tagged, &Type::TypeVar(u32::MAX), result_type)
                } else {
                    tagged
                }
            }
            "lin_parallel" => {
                // parallel([thunk1, thunk2, ...]) → call each thunk, collect results
                // args[0] is a MakeArray of thunks.
                if let TypedExpr::MakeArray { elements, .. } = &args[0] {
                    let n = elements.len();
                    let arr = self.builder.build_call(
                        self.get_or_declare_fn("lin_array_alloc",
                            ptr_ty.fn_type(&[self.context.i64_type().into()], false)),
                        &[self.context.i64_type().const_int(n.max(4) as u64, false).into()],
                        "par_arr"
                    ).unwrap().try_as_basic_value().unwrap_basic().into_pointer_value();

                    // Use lin_array_push_tagged: copies full TaggedVal (16 bytes) from pointer into slot.
                    let push_tagged_fn = self.get_or_declare_fn("lin_array_push_tagged",
                        self.context.void_type().fn_type(&[ptr_ty.into(), ptr_ty.into()], false));

                    // Clone elements to avoid borrow issues.
                    let elems: Vec<TypedExpr> = elements.iter().cloned().collect();
                    for thunk_expr in &elems {
                        let boxed = self.call_thunk_and_box(thunk_expr, fn_ctx);
                        let boxed_ptr = if boxed.is_pointer_value() {
                            boxed.into_pointer_value()
                        } else {
                            let b = self.box_value(boxed, &thunk_expr.ty());
                            b.into_pointer_value()
                        };
                        self.builder.build_call(push_tagged_fn, &[arr.into(), boxed_ptr.into()], "par_push").unwrap();
                    }
                    arr.into()
                } else {
                    // Runtime path: args[0] is a TaggedVal* holding an array of function thunks.
                    // Iterate the array, call each thunk, collect boxed results.
                    let tasks_val = self.compile_expr(&args[0], fn_ctx);
                    let i64_ty = self.context.i64_type();
                    let i8_ty = self.context.i8_type();

                    // Unbox TaggedVal* → LinArray*
                    let arr_unboxed = if tasks_val.is_pointer_value() {
                        let tv_ptr = tasks_val.into_pointer_value();
                        // Check if it's already a LinArray* (not wrapped in TaggedVal) or TaggedVal*.
                        // Try unboxing — lin_unbox_ptr returns the inner ptr from a TaggedVal.
                        self.builder.build_call(self.rt_unbox_ptr, &[tv_ptr.into()], "par_arr_raw")
                            .unwrap().try_as_basic_value().unwrap_basic().into_pointer_value()
                    } else {
                        ptr_ty.const_null()
                    };

                    // Get array length.
                    let len_fn = self.get_or_declare_fn("lin_array_length",
                        i64_ty.fn_type(&[ptr_ty.into()], false));
                    let len = self.builder.build_call(len_fn, &[arr_unboxed.into()], "par_len")
                        .unwrap().try_as_basic_value().unwrap_basic().into_int_value();

                    // Allocate output array.
                    let alloc_fn = self.get_or_declare_fn("lin_array_alloc",
                        ptr_ty.fn_type(&[i64_ty.into()], false));
                    let out_arr = self.builder.build_call(alloc_fn, &[len.into()], "par_out")
                        .unwrap().try_as_basic_value().unwrap_basic().into_pointer_value();

                    let push_tagged_fn = self.get_or_declare_fn("lin_array_push_tagged",
                        self.context.void_type().fn_type(&[ptr_ty.into(), ptr_ty.into()], false));
                    let get_tagged_fn = self.get_or_declare_fn("lin_array_get_tagged",
                        ptr_ty.fn_type(&[ptr_ty.into(), i64_ty.into()], false));

                    // Build loop blocks.
                    let llvm_fn = fn_ctx.llvm_fn;
                    let loop_check = self.context.append_basic_block(llvm_fn, "par_check");
                    let loop_body  = self.context.append_basic_block(llvm_fn, "par_body");
                    let loop_exit  = self.context.append_basic_block(llvm_fn, "par_exit");

                    let i_alloc = self.builder.build_alloca(i64_ty, "par_i").unwrap();
                    self.builder.build_store(i_alloc, i64_ty.const_int(0, false)).unwrap();
                    self.builder.build_unconditional_branch(loop_check).unwrap();

                    // Loop check: i < len
                    self.builder.position_at_end(loop_check);
                    let cur_i = self.builder.build_load(i64_ty, i_alloc, "par_cur_i")
                        .unwrap().into_int_value();
                    let cond = self.builder.build_int_compare(
                        inkwell::IntPredicate::SLT, cur_i, len, "par_cond").unwrap();
                    self.builder.build_conditional_branch(cond, loop_body, loop_exit).unwrap();

                    // Loop body: get element (TaggedVal*), unbox → closure struct, call thunk.
                    self.builder.position_at_end(loop_body);
                    let cur_i2 = self.builder.build_load(i64_ty, i_alloc, "par_i2")
                        .unwrap().into_int_value();
                    let elem_tv = self.builder.build_call(get_tagged_fn,
                        &[arr_unboxed.into(), cur_i2.into()], "par_elem")
                        .unwrap().try_as_basic_value().unwrap_basic().into_pointer_value();
                    // Unbox TaggedVal* → closure struct ptr.
                    let cls_ptr = self.builder.build_call(self.rt_unbox_ptr, &[elem_tv.into()], "par_cls")
                        .unwrap().try_as_basic_value().unwrap_basic().into_pointer_value();
                    let cls_ty = self.closure_struct_type();
                    let fn_field = self.builder.build_struct_gep(cls_ty, cls_ptr, 2, "par_fn_f").unwrap();
                    let fn_ptr = self.builder.build_load(ptr_ty, fn_field, "par_fn").unwrap().into_pointer_value();
                    let env_field = self.builder.build_struct_gep(cls_ty, cls_ptr, 3, "par_env_f").unwrap();
                    let env_ptr = self.builder.build_load(ptr_ty, env_field, "par_env").unwrap();
                    // Call thunk: fn(env) → TaggedVal*
                    let thunk_fn_ty = ptr_ty.fn_type(&[ptr_ty.into()], false);
                    let result = self.builder.build_indirect_call(thunk_fn_ty, fn_ptr,
                        &[env_ptr.into()], "par_res")
                        .unwrap().try_as_basic_value().unwrap_basic().into_pointer_value();
                    self.builder.build_call(push_tagged_fn,
                        &[out_arr.into(), result.into()], "par_push").unwrap();

                    // Increment i.
                    let next_i = self.builder.build_int_add(cur_i2, i64_ty.const_int(1, false), "par_next_i").unwrap();
                    self.builder.build_store(i_alloc, next_i).unwrap();
                    self.builder.build_unconditional_branch(loop_check).unwrap();

                    self.builder.position_at_end(loop_exit);
                    out_arr.into()
                }
            }
            "lin_thread_pool" => {
                // threadPool(n) → allocate LinThreadPool*
                let n = self.compile_expr(&args[0], fn_ctx);
                let n_i32 = if n.is_int_value() {
                    n.into_int_value()
                } else {
                    i32_ty.const_int(2, false)
                };
                let pool_fn = self.get_or_declare_fn("lin_thread_pool_new",
                    ptr_ty.fn_type(&[i32_ty.into()], false));
                self.builder.build_call(pool_fn, &[n_i32.into()], "pool").unwrap()
                    .try_as_basic_value().unwrap_basic()
            }
            "lin_worker" => {
                // worker(on_msg, on_shutdown) → allocate LinWorker*
                let thunk = &args[0];
                let thunk_ty = thunk.ty();
                let thunk_val = self.compile_expr(thunk, fn_ctx);

                let (fn_ptr, env_ptr, has_env) = if thunk_val.is_pointer_value() {
                    let raw_ptr = thunk_val.into_pointer_value();
                    // If the handler arrived as TaggedVal* (TypeVar/Json), unbox to closure struct.
                    let cls_ptr = if matches!(thunk_ty, Type::TypeVar(_) | Type::Union(_)) {
                        self.builder.build_call(self.rt_unbox_ptr, &[raw_ptr.into()], "w_cls")
                            .unwrap().try_as_basic_value().unwrap_basic().into_pointer_value()
                    } else {
                        raw_ptr
                    };
                    let cls_ty = self.closure_struct_type();
                    let fn_f = self.builder.build_struct_gep(cls_ty, cls_ptr, 2, "w_fn_f").unwrap();
                    let fp = self.builder.build_load(ptr_ty, fn_f, "w_fn").unwrap().into_pointer_value();
                    let env_f = self.builder.build_struct_gep(cls_ty, cls_ptr, 3, "w_env_f").unwrap();
                    let ep = self.builder.build_load(ptr_ty, env_f, "w_env").unwrap().into_pointer_value();
                    (fp, ep, self.context.i8_type().const_int(1, false))
                } else {
                    let fp = ptr_ty.const_null();
                    let null_ptr = ptr_ty.const_null();
                    (fp, null_ptr, self.context.i8_type().const_int(0, false))
                };

                let worker_fn = self.get_or_declare_fn("lin_worker_new",
                    ptr_ty.fn_type(&[ptr_ty.into(), ptr_ty.into(), self.context.i8_type().into()], false));
                self.builder.build_call(worker_fn, &[fn_ptr.into(), env_ptr.into(), has_env.into()], "worker").unwrap()
                    .try_as_basic_value().unwrap_basic()
            }
            "lin_race" | "lin_timeout" | "lin_retry" => {
                // Simplified: race returns first arg promise, timeout/retry just call thunk.
                if args.is_empty() {
                    ptr_ty.const_null().into()
                } else {
                    self.compile_expr(&args[0], fn_ctx)
                }
            }
            "lin_request" => {
                // w.request(msg) → lin_worker_request(w, msg) → TaggedVal*
                let worker_val = self.compile_expr(&args[0], fn_ctx);
                let msg_val = self.compile_expr(&args[1], fn_ctx);
                // Ensure msg is boxed to TaggedVal*
                let msg_ptr = if msg_val.is_pointer_value() {
                    msg_val.into_pointer_value()
                } else {
                    self.box_value(msg_val, &args[1].ty()).into_pointer_value()
                };
                let worker_ptr = if worker_val.is_pointer_value() {
                    worker_val.into_pointer_value()
                } else {
                    ptr_ty.const_null()
                };
                let req_fn = self.get_or_declare_fn("lin_worker_request",
                    ptr_ty.fn_type(&[ptr_ty.into(), ptr_ty.into()], false));
                let tagged = self.builder.build_call(req_fn, &[worker_ptr.into(), msg_ptr.into()], "w_reply").unwrap()
                    .try_as_basic_value().unwrap_basic();
                // Unbox result if we know the concrete return type
                if !Self::is_union_type(result_type) && *result_type != Type::Null {
                    self.coerce_typevar(tagged, &Type::TypeVar(u32::MAX), result_type)
                } else {
                    tagged
                }
            }
            "lin_message" => {
                // w.message(msg) → lin_worker_message(w, msg) → void / null
                let worker_val = self.compile_expr(&args[0], fn_ctx);
                let msg_val = self.compile_expr(&args[1], fn_ctx);
                let msg_ptr = if msg_val.is_pointer_value() {
                    msg_val.into_pointer_value()
                } else {
                    self.box_value(msg_val, &args[1].ty()).into_pointer_value()
                };
                let worker_ptr = if worker_val.is_pointer_value() {
                    worker_val.into_pointer_value()
                } else {
                    ptr_ty.const_null()
                };
                let msg_fn = self.get_or_declare_fn("lin_worker_message",
                    self.context.void_type().fn_type(&[ptr_ty.into(), ptr_ty.into()], false));
                self.builder.build_call(msg_fn, &[worker_ptr.into(), msg_ptr.into()], "").unwrap();
                ptr_ty.const_null().into()
            }
            "lin_close" => {
                // w.close() → lin_worker_close(w)
                let worker_val = self.compile_expr(&args[0], fn_ctx);
                let worker_ptr = if worker_val.is_pointer_value() {
                    worker_val.into_pointer_value()
                } else {
                    ptr_ty.const_null()
                };
                let close_fn = self.get_or_declare_fn("lin_worker_close",
                    self.context.void_type().fn_type(&[ptr_ty.into()], false));
                self.builder.build_call(close_fn, &[worker_ptr.into()], "").unwrap();
                ptr_ty.const_null().into()
            }
            _ => ptr_ty.const_null().into(),
        }
    }

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
        let cond_compiled = self.compile_expr(cond, fn_ctx);
        let cond_val = if cond_compiled.is_pointer_value() {
            // TypeVar/union condition: check if it's a Bool-tagged value; if so extract bool.
            // Non-Bool tagged values (e.g. null) are treated as false; everything else true.
            let ptr = cond_compiled.into_pointer_value();
            let tag = self.builder.build_call(self.rt_get_tag, &[ptr.into()], "cond_tag")
                .unwrap().try_as_basic_value().unwrap_basic().into_int_value();
            let tag_bool = self.context.i8_type().const_int(1u64, false); // TAG_BOOL = 1
            let is_bool = self.builder.build_int_compare(inkwell::IntPredicate::EQ, tag, tag_bool, "is_bool").unwrap();
            let bool_payload = self.builder.build_call(self.rt_unbox_bool, &[ptr.into()], "cond_ubool")
                .unwrap().try_as_basic_value().unwrap_basic().into_int_value();
            let bool_flag = self.builder.build_int_truncate(bool_payload, self.context.bool_type(), "bool_flag").unwrap();
            // If tag == Bool, use bool_flag; else treat as truthy (non-null = true, null = false).
            let i64_ty = self.context.i64_type();
            let as_int = self.builder.build_ptr_to_int(ptr, i64_ty, "cond_pti").unwrap();
            let ptr_nonzero = self.builder.build_int_compare(inkwell::IntPredicate::NE, as_int, i64_ty.const_zero(), "ptr_nonnull").unwrap();
            self.builder.build_select(is_bool, bool_flag, ptr_nonzero, "cond_bool").unwrap().into_int_value()
        } else {
            cond_compiled.into_int_value()
        };

        let then_block = self.context.append_basic_block(fn_ctx.llvm_fn, "then");
        let else_block = self.context.append_basic_block(fn_ctx.llvm_fn, "else");
        let merge_block = self.context.append_basic_block(fn_ctx.llvm_fn, "merge");

        self.builder
            .build_conditional_branch(cond_val, then_block, else_block)
            .unwrap();

        // Coerce branch values to the declared result type.
        // Must happen inside branch blocks (before br to merge) because PHI nodes must be
        // the first instructions in the merge block.
        let result_llvm_ty = self.llvm_type(result_type);
        let result_is_union = Self::is_union_type(result_type);

        // Pre-compute ownership of each branch so we can normalise at the merge point.
        let then_owned = Self::expr_is_owned_alloc(then_br);
        let else_owned = Self::expr_is_owned_alloc(else_br);
        // Normalise ownership: if one branch is fresh (rc=1) and the other is borrowed (rc≥1),
        // retain the borrowed branch so both arrive at the PHI with an extra rc reference.
        // Skip union/TypeVar results — box_value already allocates a new TaggedVal box.
        let needs_normalize = Self::ty_is_heap(result_type)
            && !result_is_union
            && !matches!(result_type, Type::Never)
            && (then_owned != else_owned);

        // Then branch
        self.builder.position_at_end(then_block);
        let then_val_raw = self.compile_expr(then_br, fn_ctx);
        let then_ty = then_br.ty();
        let then_val = if result_is_union && !Self::is_union_type(&then_ty) && !matches!(then_ty, Type::Never) {
            // Result is TaggedVal* but branch produced a concrete type — box it.
            self.box_value(then_val_raw, &then_ty)
        } else if !result_is_union && Self::is_union_type(&then_ty) && !matches!(then_ty, Type::Never) {
            // Result is concrete but branch produced TaggedVal* — unbox/coerce it.
            self.coerce_typevar(then_val_raw, &then_ty, result_type)
        } else if then_val_raw.get_type() == result_llvm_ty { then_val_raw } else { result_llvm_ty.const_zero() };
        let then_end = self.builder.get_insert_block().unwrap();
        if !then_end.get_terminator().is_some() {
            // Retain borrowed branch value so both branches own a reference at the merge.
            if needs_normalize && !then_owned && then_val.is_pointer_value() {
                self.builder.build_call(self.rt_rc_retain, &[then_val.into()], "").unwrap();
            }
            self.builder.build_unconditional_branch(merge_block).unwrap();
        }

        // Else branch
        self.builder.position_at_end(else_block);
        let else_val_raw = self.compile_expr(else_br, fn_ctx);
        let else_ty = else_br.ty();
        let else_val = if result_is_union && !Self::is_union_type(&else_ty) && !matches!(else_ty, Type::Never) {
            // Result is TaggedVal* but branch produced a concrete type — box it.
            self.box_value(else_val_raw, &else_ty)
        } else if !result_is_union && Self::is_union_type(&else_ty) && !matches!(else_ty, Type::Never) {
            // Result is concrete but branch produced TaggedVal* — unbox/coerce it.
            self.coerce_typevar(else_val_raw, &else_ty, result_type)
        } else if else_val_raw.get_type() == result_llvm_ty { else_val_raw } else { result_llvm_ty.const_zero() };
        let else_end = self.builder.get_insert_block().unwrap();
        if !else_end.get_terminator().is_some() {
            // Retain borrowed branch value so both branches own a reference at the merge.
            if needs_normalize && !else_owned && else_val.is_pointer_value() {
                self.builder.build_call(self.rt_rc_retain, &[else_val.into()], "").unwrap();
            }
            self.builder.build_unconditional_branch(merge_block).unwrap();
        }

        // Merge with phi.
        self.builder.position_at_end(merge_block);
        let phi = self
            .builder
            .build_phi(result_llvm_ty, "iftmp")
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
        // Always include env_ptr (even for non-capturing closures) so all closures share
        // the same calling convention: fn(env_ptr, args...) -> ret. This allows non-capturing
        // closures to be stored in {fn_ptr, null_env} structs and called uniformly.
        let mut llvm_param_types: Vec<BasicMetadataTypeEnum> = Vec::new();
        let ptr_type: BasicMetadataTypeEnum = self.context.ptr_type(AddressSpace::default()).into();
        llvm_param_types.push(ptr_type); // env_ptr (may be null for non-capturing)
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
            // Env layout: { u64 size_header, cap_0, cap_1, ... }
            // The leading u64 stores the total env allocation size so lin_closure_release
            // can free the env without needing a separate size argument.
            let mut cap_types: Vec<BasicTypeEnum> = vec![self.context.i64_type().into()]; // size header
            cap_types.extend(captures.iter().map(|c| {
                if c.is_mutable {
                    self.context.ptr_type(AddressSpace::default()).into()
                } else {
                    self.llvm_type(&c.ty)
                }
            }));
            let env_struct_type = self.context.struct_type(&cap_types, false);
            // Heap-allocate the env so captured values survive the creating function's frame.
            let env_size = env_struct_type.size_of().unwrap();
            let env_size_i64 = self.builder
                .build_int_z_extend_or_bit_cast(env_size, self.context.i64_type(), "env_size")
                .unwrap();
            let env_alloc = self.builder
                .build_call(self.rt_alloc, &[env_size_i64.into()], "env_raw")
                .unwrap()
                .try_as_basic_value()
                .unwrap_basic()
                .into_pointer_value();
            // Write the size header at field 0 so lin_closure_release can free the env.
            let size_field = self.builder.build_struct_gep(env_struct_type, env_alloc, 0, "env_sz_f").unwrap();
            self.builder.build_store(size_field, env_size_i64).unwrap();

            // Store each captured value into the env struct (fields 1..=n).
            for (i, cap) in captures.iter().enumerate() {
                let field_ptr = self
                    .builder
                    .build_struct_gep(env_struct_type, env_alloc, (i + 1) as u32, "cap_field")
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
                    None => {
                        // Check module_fn_slots first, then global_fn_slots and current_module_slots.
                        if let Some(&mfn) = fn_ctx.module_fn_slots.get(&cap.outer_slot) {
                            mfn.as_global_value().as_pointer_value().into()
                        } else if let Some(&gfn) = self.global_fn_slots.get(&cap.outer_slot) {
                            gfn.as_global_value().as_pointer_value().into()
                        } else if let Some(&mfn) = self.current_module_slots.get(&cap.outer_slot) {
                            mfn.as_global_value().as_pointer_value().into()
                        } else if let Some(glob) = self.global_val_slots.get(&cap.outer_slot) {
                            // Module-level val (non-function) — load current value from global.
                            let load_ty = if matches!(cap.ty, Type::Function { .. } | Type::TypeVar(_) | Type::Union(_) | Type::Null) {
                                self.context.ptr_type(AddressSpace::default()).as_basic_type_enum()
                            } else {
                                self.llvm_type(&cap.ty)
                            };
                            self.builder
                                .build_load(load_ty, glob.as_pointer_value(), &format!("cap_gv_{}", cap.outer_slot))
                                .unwrap()
                        } else {
                            self.llvm_type(&cap.ty).const_zero()
                        }
                    }
                };
                self.builder.build_store(field_ptr, cap_val).unwrap();
            }

            // Build the closure struct: heap-allocated { rc, _pad, fn_ptr, env_ptr } + u64 env_size
            let fn_ptr = llvm_fn.as_global_value().as_pointer_value();
            let closure_struct_type = self.closure_struct_type();
            let closure_alloc = self.builder
                .build_call(self.rt_alloc, &[self.context.i64_type().const_int(32, false).into()], "closure")
                .unwrap()
                .try_as_basic_value()
                .unwrap_basic()
                .into_pointer_value();
            // Write refcount = 1.
            let rc_field = self.builder
                .build_struct_gep(closure_struct_type, closure_alloc, 0, "closure_rc").unwrap();
            self.builder.build_store(rc_field, self.context.i32_type().const_int(1, false)).unwrap();
            let fn_field = self
                .builder
                .build_struct_gep(closure_struct_type, closure_alloc, 2, "closure_fn")
                .unwrap();
            self.builder.build_store(fn_field, fn_ptr).unwrap();
            let env_field = self
                .builder
                .build_struct_gep(closure_struct_type, closure_alloc, 3, "closure_env")
                .unwrap();
            self.builder.build_store(env_field, env_alloc).unwrap();
            // Write env_size at raw offset 24 so lin_closure_release knows how much to free.
            let env_size_field = unsafe {
                self.builder.build_gep(
                    self.context.i64_type(),
                    closure_alloc,
                    &[self.context.i64_type().const_int(3, false)], // byte offset 24 / 8 = 3 i64s from start
                    "closure_env_sz_ptr",
                ).unwrap()
            };
            self.builder.build_store(env_size_field, env_size_i64).unwrap();

            // Compile the function body (deferred — save/restore builder position).
            // Propagate module_fn_slots so sibling module functions resolve inside inner closures.
            let current_block = self.builder.get_insert_block().unwrap();
            let mod_slots: HashMap<usize, FunctionValue<'ctx>> = fn_ctx.module_fn_slots.clone();
            // Adjust compile_function_body: captures now start at env field index 1 (after size header).
            self.compile_function_body(llvm_fn, params, body, ret_type, captures, &closure_name, &mod_slots, true);
            self.builder.position_at_end(current_block);

            closure_alloc.into()
        } else {
            // Non-capturing closure — compile function body (env_ptr is first param but ignored).
            let current_block = self.builder.get_insert_block().unwrap();
            let mod_slots: HashMap<usize, FunctionValue<'ctx>> = fn_ctx.module_fn_slots.clone();
            self.compile_function_body(llvm_fn, params, body, ret_type, &[], &closure_name, &mod_slots, true);
            self.builder.position_at_end(current_block);

            // Wrap in {rc, _pad, fn_ptr, null_env} closure struct so callers can use uniform convention.
            let fn_ptr = llvm_fn.as_global_value().as_pointer_value();
            let ptr_ty = self.context.ptr_type(AddressSpace::default());
            let closure_struct_type = self.closure_struct_type();
            let closure_alloc = self.builder
                .build_call(self.rt_alloc, &[self.context.i64_type().const_int(32, false).into()], "closure")
                .unwrap().try_as_basic_value().unwrap_basic().into_pointer_value();
            let rc_field = self.builder
                .build_struct_gep(closure_struct_type, closure_alloc, 0, "closure_rc").unwrap();
            self.builder.build_store(rc_field, self.context.i32_type().const_int(1, false)).unwrap();
            let fn_field = self.builder
                .build_struct_gep(closure_struct_type, closure_alloc, 2, "closure_fn").unwrap();
            self.builder.build_store(fn_field, fn_ptr).unwrap();
            let env_field = self.builder
                .build_struct_gep(closure_struct_type, closure_alloc, 3, "closure_env").unwrap();
            self.builder.build_store(env_field, ptr_ty.const_null()).unwrap();
            // env_size (offset 24) left as zero — lin_alloc zeroes memory.
            closure_alloc.into()
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
        if parts.len() == 1 {
            let (s, _owned) = self.compile_string_part_owned(&parts[0], fn_ctx);
            return s;
        }

        // Compile all parts, collecting (ptr, is_owned) pairs.
        let compiled: Vec<(BasicValueEnum<'ctx>, bool)> = parts
            .iter()
            .map(|p| self.compile_string_part_owned(p, fn_ctx))
            .collect();

        let n = compiled.len();
        let ptr_type = self.context.ptr_type(AddressSpace::default());
        let i32_type = self.context.i32_type();

        // Stack-allocate an array of n string pointers.
        let arr_alloca = self.builder
            .build_array_alloca(ptr_type, i32_type.const_int(n as u64, false), "interp_parts")
            .unwrap();

        // Store each part pointer into the array.
        for (i, (ptr, _)) in compiled.iter().enumerate() {
            let gep = unsafe {
                self.builder.build_gep(
                    ptr_type,
                    arr_alloca,
                    &[i32_type.const_int(i as u64, false)],
                    "part_slot",
                ).unwrap()
            };
            self.builder.build_store(gep, *ptr).unwrap();
        }

        // Single call: allocate result + copy all parts in one shot.
        let result = self.builder
            .build_call(
                self.rt_string_build_n,
                &[arr_alloca.into(), i32_type.const_int(n as u64, false).into()],
                "interp",
            )
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic();

        // Release owned temporaries (numeric/bool/null conversions).
        for (ptr, is_owned) in &compiled {
            if *is_owned {
                self.builder.build_call(self.rt_string_release, &[(*ptr).into()], "").unwrap();
            }
        }

        result
    }

    /// Compile a string interpolation part to a LinString* and return whether the caller owns it.
    /// Owned = freshly allocated (numeric/bool/null conversion). Not owned = Str slot or literal.
    fn compile_string_part_owned(
        &mut self,
        part: &TypedStringPart,
        fn_ctx: &mut FnCtx<'ctx, '_>,
    ) -> (BasicValueEnum<'ctx>, bool) {
        match part {
            TypedStringPart::Literal(s) => (self.compile_string_lit(s), false),
            TypedStringPart::Expr(e) => {
                let val = self.compile_expr(e, fn_ctx);
                let ty = e.ty();
                // Numeric/bool/null conversions produce fresh allocations.
                // TypeVar/Union also produce fresh allocations: value_to_string calls
                // lin_tagged_to_string which always allocates a new LinString*.
                // Str values are borrowed from slots (not fresh).
                let is_fresh = matches!(ty,
                    Type::Int8 | Type::Int16 | Type::Int32 | Type::Int64 |
                    Type::UInt8 | Type::UInt16 | Type::UInt32 | Type::UInt64 |
                    Type::Float32 | Type::Float64 | Type::Bool | Type::Null |
                    Type::TypeVar(_) | Type::Union(_));
                let str_val = self.value_to_string(val, &ty, fn_ctx);
                (str_val, is_fresh)
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

    /// Returns true when the element type maps to a flat unboxed scalar array.
    /// Only concrete fixed-width numeric scalars qualify — not Bool (stored as i1,
    /// awkward to pack densely), not pointers, not unions.
    fn is_flat_scalar(ty: &Type) -> bool {
        matches!(ty,
            Type::Int32 | Type::UInt32 |
            Type::Int64 | Type::UInt64 |
            Type::Float32 | Type::Float64
        )
    }

    /// Suffix used in runtime function names for flat array variants.
    fn flat_suffix(ty: &Type) -> &'static str {
        match ty {
            Type::Int32 | Type::UInt32 => "i32",
            Type::Int64 | Type::UInt64 => "i64",
            Type::Float32 => "f32",
            Type::Float64 => "f64",
            _ => unreachable!("flat_suffix called with non-scalar type"),
        }
    }

    fn compile_make_array(
        &mut self,
        elements: &[TypedExpr],
        ty: &Type,
        fn_ctx: &mut FnCtx<'ctx, '_>,
    ) -> BasicValueEnum<'ctx> {
        let cap = self.context.i64_type().const_int(elements.len().max(4) as u64, false);

        // If all elements have the same flat scalar type, use a flat (unboxed) array.
        let elem_ty = match ty {
            Type::Array(inner) => (**inner).clone(),
            _ => elements.first().map(|e| e.ty()).unwrap_or(Type::Null),
        };

        if Self::is_flat_scalar(&elem_ty) {
            let suffix = Self::flat_suffix(&elem_ty);
            let alloc_name = format!("lin_flat_array_alloc_{}", suffix);
            let ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
            let i64_ty = self.context.i64_type();
            let alloc_fn = self.get_or_declare_fn(&alloc_name,
                ptr_ty.fn_type(&[i64_ty.into()], false));
            let arr = self.builder
                .build_call(alloc_fn, &[cap.into()], "flat_arr")
                .unwrap()
                .try_as_basic_value()
                .unwrap_basic();
            for elem in elements {
                let val = self.compile_expr(elem, fn_ctx);
                self.flat_array_push(arr, val, &elem_ty);
            }
            return arr;
        }

        let arr = self
            .builder
            .build_call(self.rt_array_alloc, &[cap.into()], "arr")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic();

        // When array element type is TypeVar/Union, closures must return TaggedVal* (ptr)
        // so runtime callers (e.g. parallel) can call them uniformly via ptr return type.
        let typevar_context = matches!(elem_ty, Type::TypeVar(_) | Type::Union(_));
        for elem in elements {
            let (val, elem_lin_ty) = if typevar_context {
                if let TypedExpr::Function { params, body, captures, .. } = elem {
                    let json_ret = Type::TypeVar(u32::MAX);
                    let cls = self.compile_closure(None, params, body, &json_ret, captures, fn_ctx);
                    let et = elem.ty();
                    (cls, et)
                } else {
                    let val = self.compile_expr(elem, fn_ctx);
                    let et = elem.ty();
                    (val, et)
                }
            } else {
                let val = self.compile_expr(elem, fn_ctx);
                let et = elem.ty();
                (val, et)
            };
            // Retain shared heap values: LocalGet borrows an existing value — retain it so
            // the array's copy and the original slot can both be released independently.
            if !Self::expr_is_owned_alloc(elem) && Self::ty_is_heap(&elem_lin_ty) && val.is_pointer_value() {
                self.builder.build_call(self.rt_rc_retain, &[val.into()], "").unwrap();
            }
            self.tagged_array_push_value(arr, val, &elem_lin_ty);
        }

        arr
    }

    /// Allocate either a flat or tagged array depending on element type.
    fn alloc_array(&mut self, cap: inkwell::values::IntValue<'ctx>, elem_ty: &Type) -> BasicValueEnum<'ctx> {
        if Self::is_flat_scalar(elem_ty) {
            let suffix = Self::flat_suffix(elem_ty);
            let alloc_name = format!("lin_flat_array_alloc_{}", suffix);
            let ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
            let i64_ty = self.context.i64_type();
            let alloc_fn = self.get_or_declare_fn(&alloc_name,
                ptr_ty.fn_type(&[i64_ty.into()], false));
            self.builder.build_call(alloc_fn, &[cap.into()], "flat_arr")
                .unwrap().try_as_basic_value().unwrap_basic()
        } else {
            self.builder.build_call(self.rt_array_alloc, &[cap.into()], "arr")
                .unwrap().try_as_basic_value().unwrap_basic()
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

    /// Push a value into a LinArray* using the correct storage format.
    /// For flat scalar types (Int32, Int64, Float32, Float64), uses flat push.
    /// For tagged/pointer types, uses tagged push.
    fn array_push_value(&mut self, arr: BasicValueEnum<'ctx>, val: BasicValueEnum<'ctx>, val_ty: &Type) {
        if Self::is_flat_scalar(val_ty) {
            self.flat_array_push(arr, val, val_ty);
            return;
        }
        let i8_ty = self.context.i8_type();
        match val_ty {
            Type::TypeVar(_) | Type::Union(_) => self.push_tagged_val(arr, val, val_ty),
            _ => {
                // Concrete pointer type: store with correct tag.
                let tag_val = Self::type_tag(val_ty);
                let tag = i8_ty.const_int(tag_val as u64, false);
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

    /// Push a value into a tagged LinArray* always using tagged format (never flat).
    /// Use this when the array was allocated with rt_array_alloc (tagged format).
    fn tagged_array_push_value(&mut self, arr: BasicValueEnum<'ctx>, val: BasicValueEnum<'ctx>, val_ty: &Type) {
        let i8_ty = self.context.i8_type();
        match val_ty {
            Type::TypeVar(_) | Type::Union(_) => self.push_tagged_val(arr, val, val_ty),
            _ => {
                let tag_val = Self::type_tag(val_ty);
                let tag = i8_ty.const_int(tag_val as u64, false);
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

        let null_val = || -> BasicValueEnum<'ctx> { self.context.ptr_type(AddressSpace::default()).const_null().into() };
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
                // Load element: flat path for known scalars, tagged path otherwise.
                let elem_val = if Self::is_flat_scalar(&elem_ty) {
                    self.flat_array_get(iterable_val, cur_i, &elem_ty)
                } else if matches!(elem_ty, Type::TypeVar(_) | Type::Union(_)) {
                    // Unknown element type (TypeVar/Union): use lin_array_get_tagged which
                    // correctly handles both flat and tagged arrays, returning a TaggedVal*.
                    let ptr_ty = self.context.ptr_type(AddressSpace::default());
                    let i64_ty = self.context.i64_type();
                    let get_tagged_fn = self.get_or_declare_fn("lin_array_get_tagged",
                        ptr_ty.fn_type(&[ptr_ty.into(), i64_ty.into()], false));
                    self.builder
                        .build_call(get_tagged_fn, &[iterable_val.into(), cur_i.into()], "elem_tv")
                        .unwrap()
                        .try_as_basic_value()
                        .unwrap_basic()
                } else {
                    let elem_ptr_val = self.builder
                        .build_call(self.rt_array_get, &[iterable_val.into(), cur_i.into()], "elem_ptr")
                        .unwrap()
                        .try_as_basic_value()
                        .unwrap_basic()
                        .into_pointer_value();
                    self.load_array_element(elem_ptr_val, &elem_ty)
                };
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

    /// while(iterable, body) — like for but stops when body returns false.
    fn compile_while_loop(
        &mut self,
        iterable_val: BasicValueEnum<'ctx>,
        iterable_ty: &Type,
        body_expr: &TypedExpr,
        fn_ctx: &mut FnCtx<'ctx, '_>,
    ) -> BasicValueEnum<'ctx> {
        let (iterable_val, iterable_ty) = if matches!(iterable_ty, Type::TypeVar(_) | Type::Union(_)) {
            let unboxed = self.builder
                .build_call(self.rt_unbox_ptr, &[iterable_val.into()], "whl_unbox")
                .unwrap().try_as_basic_value().unwrap_basic();
            (unboxed, Type::Array(Box::new(Type::TypeVar(0))))
        } else {
            (iterable_val, iterable_ty.clone())
        };
        let iterable_ty = &iterable_ty;

        let null_val = || -> BasicValueEnum<'ctx> { self.context.ptr_type(AddressSpace::default()).const_null().into() };
        let llvm_fn = fn_ctx.llvm_fn;

        let elem_ty = match iterable_ty {
            Type::Array(t) => (**t).clone(),
            Type::Iterator(t) => (**t).clone(),
            Type::FixedArray(ts) => ts.first().cloned().unwrap_or(Type::Null),
            _ => {
                let msg = self.compile_string_lit("while: unsupported iterable type");
                let zero = self.context.i32_type().const_zero();
                self.builder.build_call(self.rt_panic, &[msg.into(), zero.into(), zero.into()], "").unwrap();
                return null_val();
            }
        };

        let i_alloc = self.builder.build_alloca(self.context.i64_type(), "whl_i").unwrap();
        self.builder.build_store(i_alloc, self.context.i64_type().const_zero()).unwrap();
        let len = self.builder
            .build_call(self.rt_array_length, &[iterable_val.into()], "whl_len")
            .unwrap().try_as_basic_value().unwrap_basic().into_int_value();

        let check_block = self.context.append_basic_block(llvm_fn, "whl_check");
        let body_block  = self.context.append_basic_block(llvm_fn, "whl_body");
        let exit_block  = self.context.append_basic_block(llvm_fn, "whl_exit");

        self.builder.build_unconditional_branch(check_block).unwrap();
        self.builder.position_at_end(check_block);
        let cur_i = self.builder.build_load(self.context.i64_type(), i_alloc, "whl_cur").unwrap().into_int_value();
        let bounds_ok = self.builder.build_int_compare(IntPredicate::SLT, cur_i, len, "whl_bounds").unwrap();
        self.builder.build_conditional_branch(bounds_ok, body_block, exit_block).unwrap();

        self.builder.position_at_end(body_block);
        let elem_val = if Self::is_flat_scalar(&elem_ty) {
            self.flat_array_get(iterable_val, cur_i, &elem_ty)
        } else if matches!(elem_ty, Type::TypeVar(_) | Type::Union(_)) {
            let ptr_ty = self.context.ptr_type(AddressSpace::default());
            let i64_ty = self.context.i64_type();
            let get_tagged_fn = self.get_or_declare_fn("lin_array_get_tagged",
                ptr_ty.fn_type(&[ptr_ty.into(), i64_ty.into()], false));
            self.builder
                .build_call(get_tagged_fn, &[iterable_val.into(), cur_i.into()], "whl_elem_tv")
                .unwrap().try_as_basic_value().unwrap_basic()
        } else {
            let elem_ptr_val = self.builder
                .build_call(self.rt_array_get, &[iterable_val.into(), cur_i.into()], "whl_elem_ptr")
                .unwrap().try_as_basic_value().unwrap_basic().into_pointer_value();
            self.load_array_element(elem_ptr_val, &elem_ty)
        };

        // Call body; returns Boolean (or a TypeVar/Json wrapping a bool) — false means stop.
        // Use the body's actual return type to avoid ABI mismatch between i1 and ptr.
        let body_ret_ty = match body_expr.ty() {
            Type::Function { ret, .. } => *ret,
            other => other,
        };
        let cont_raw = self.call_body(body_expr, &[elem_val], &body_ret_ty, fn_ctx);
        // Coerce result to i1: if it's a pointer (TypeVar), extract the Bool payload.
        let cont_bool = if cont_raw.is_pointer_value() {
            let ptr = cont_raw.into_pointer_value();
            let tag = self.builder.build_call(self.rt_get_tag, &[ptr.into()], "whl_ctag")
                .unwrap().try_as_basic_value().unwrap_basic().into_int_value();
            let tag_bool = self.context.i8_type().const_int(1u64, false); // TAG_BOOL = 1
            let is_bool = self.builder.build_int_compare(inkwell::IntPredicate::EQ, tag, tag_bool, "whl_is_bool").unwrap();
            let bool_payload = self.builder.build_call(self.rt_unbox_bool, &[ptr.into()], "whl_ubool")
                .unwrap().try_as_basic_value().unwrap_basic().into_int_value();
            let bool_flag = self.builder.build_int_truncate(bool_payload, self.context.bool_type(), "whl_bflag").unwrap();
            let i64_ty = self.context.i64_type();
            let as_int = self.builder.build_ptr_to_int(ptr, i64_ty, "whl_cpti").unwrap();
            let ptr_nonzero = self.builder.build_int_compare(inkwell::IntPredicate::NE, as_int, i64_ty.const_zero(), "whl_pnn").unwrap();
            self.builder.build_select(is_bool, bool_flag, ptr_nonzero, "whl_cbool").unwrap().into_int_value()
        } else {
            cont_raw.into_int_value()
        };
        let next_i = self.builder
            .build_int_add(cur_i, self.context.i64_type().const_int(1, false), "whl_next")
            .unwrap();
        self.builder.build_store(i_alloc, next_i).unwrap();
        self.builder.build_conditional_branch(cont_bool, check_block, exit_block).unwrap();

        self.builder.position_at_end(exit_block);
        null_val()
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

        // Allocate output array (flat if output element type is a known scalar).
        let cap = i64_ty.const_int(4, false);
        let out_arr = self.alloc_array(cap, out_elem_ty);

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
        let out_val = self.call_body(body_expr, &[elem_val], out_elem_ty, fn_ctx);
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

        let (i_alloc, len, inferred_elem_ty) = self.setup_iteration(iterable_val, iterable_ty, fn_ctx);
        let actual_elem_ty = if matches!(elem_ty, Type::Null) { inferred_elem_ty } else { elem_ty.clone() };

        let cap = i64_ty.const_int(4, false);
        let out_arr = self.alloc_array(cap, &actual_elem_ty);

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
        let keep = self.call_body(body_expr, &[elem_val], &Type::Bool, fn_ctx);
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
        let new_acc = self.call_body(body_expr, &[acc_val, elem_val], result_type, fn_ctx);
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
                // Use flat path for known scalar element types.
                if Self::is_flat_scalar(elem_ty) {
                    return self.flat_array_get(arr_val, cur_i, elem_ty);
                }
                // When element type is TypeVar (array may be flat), use lin_array_get_tagged
                // which properly boxes flat scalar elements to TaggedVal* before returning.
                if matches!(elem_ty, Type::TypeVar(_) | Type::Union(_)) {
                    let elem_ptr = self.builder
                        .build_call(self.rt_array_get_tagged, &[arr_val.into(), cur_i.into()], "elem_ptr")
                        .unwrap()
                        .try_as_basic_value().unwrap_basic().into_pointer_value();
                    return elem_ptr.into();
                }
                let elem_ptr = self.builder
                    .build_call(self.rt_array_get, &[arr_val.into(), cur_i.into()], "elem_ptr")
                    .unwrap()
                    .try_as_basic_value().unwrap_basic().into_pointer_value();
                self.load_array_element(elem_ptr, elem_ty)
            }
            _ => self.llvm_type(elem_ty).const_zero(),
        }
    }

    /// Call a body expression (function or closure) with the given arguments, return result_ty.
    /// Handles: LocalGet (closure/value/global), inline Function (with or without captures), fallback.
    fn call_body(
        &mut self,
        body_expr: &TypedExpr,
        args: &[BasicValueEnum<'ctx>],
        result_ty: &Type,
        fn_ctx: &mut FnCtx<'ctx, '_>,
    ) -> BasicValueEnum<'ctx> {
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let ret_llvm = self.llvm_type(result_ty);
        let arg_metas: Vec<BasicMetadataTypeEnum> = args.iter().map(|a| a.get_type().into()).collect();
        let arg_vals: Vec<BasicMetadataValueEnum> = args.iter().map(|a| (*a).into()).collect();

        match body_expr {
            TypedExpr::LocalGet { slot, .. } => {
                if let Some(SlotStorage::Closure(cls_ptr)) = fn_ctx.slots.get(slot).cloned() {
                    let cls_ty = self.closure_struct_type();
                    let fn_ptr = self.builder.build_load(ptr_ty,
                        self.builder.build_struct_gep(cls_ty, cls_ptr, 2, "cb_fp").unwrap(), "cb_fn").unwrap().into_pointer_value();
                    let env_ptr = self.builder.build_load(ptr_ty,
                        self.builder.build_struct_gep(cls_ty, cls_ptr, 3, "cb_ep").unwrap(), "cb_env").unwrap();
                    let mut param_types: Vec<BasicMetadataTypeEnum> = vec![ptr_ty.into()];
                    param_types.extend_from_slice(&arg_metas);
                    let mut call_args: Vec<BasicMetadataValueEnum> = vec![env_ptr.into()];
                    call_args.extend_from_slice(&arg_vals);
                    let fn_type = ret_llvm.fn_type(&param_types, false);
                    return self.builder.build_indirect_call(fn_type, fn_ptr, &call_args, "cb_call")
                        .unwrap().try_as_basic_value().basic().unwrap_or_else(|| ret_llvm.const_zero());
                }
                // For Alloca slots, load the value first then fall through to pointer dispatch.
                let raw_ptr_opt: Option<PointerValue<'ctx>> = match fn_ctx.slots.get(slot).cloned() {
                    Some(SlotStorage::Value(BasicValueEnum::PointerValue(p))) => Some(p),
                    Some(SlotStorage::Alloca(alloc)) => {
                        Some(self.builder.build_load(ptr_ty, alloc, "cb_load")
                            .unwrap().into_pointer_value())
                    }
                    _ => None,
                };
                if let Some(raw_ptr) = raw_ptr_opt {
                    let expr_ty = body_expr.ty();
                    // If the expr is TypeVar/Union (Json-typed function parameter), it is a
                    // TaggedVal* wrapping a closure {fn_ptr, env_ptr}. Unbox it and call through.
                    if matches!(expr_ty, Type::TypeVar(_) | Type::Union(_)) {
                        let cls_raw = self.builder
                            .build_call(self.rt_unbox_ptr, &[raw_ptr.into()], "jcls_raw")
                            .unwrap().try_as_basic_value().unwrap_basic().into_pointer_value();
                        let cls_ty = self.closure_struct_type();
                        let fn_ptr = self.builder.build_load(ptr_ty,
                            self.builder.build_struct_gep(cls_ty, cls_raw, 2, "jcls_fp").unwrap(),
                            "jcls_fn").unwrap().into_pointer_value();
                        let env_ptr = self.builder.build_load(ptr_ty,
                            self.builder.build_struct_gep(cls_ty, cls_raw, 3, "jcls_ep").unwrap(),
                            "jcls_env").unwrap();
                        let mut param_types: Vec<BasicMetadataTypeEnum> = vec![ptr_ty.into()];
                        param_types.extend_from_slice(&arg_metas);
                        let mut call_args: Vec<BasicMetadataValueEnum> = vec![env_ptr.into()];
                        call_args.extend_from_slice(&arg_vals);
                        // Closures passed as Json always return ptr (boxed TaggedVal*).
                        // Call with ptr return type, then coerce to the expected result type.
                        let ptr_fn_type = ptr_ty.fn_type(&param_types, false);
                        let raw_result = self.builder.build_indirect_call(ptr_fn_type, fn_ptr, &call_args, "jcls_call")
                            .unwrap().try_as_basic_value().basic().unwrap_or_else(|| ptr_ty.const_null().into());
                        // Adapt raw ptr result to the expected result type.
                        let result = if ret_llvm == ptr_ty.as_basic_type_enum() {
                            raw_result // caller expects ptr — no conversion needed
                        } else {
                            // Unbox: load the payload from the TaggedVal* as result_ty.
                            self.coerce_typevar(raw_result, &Type::TypeVar(u32::MAX), result_ty)
                        };
                        return result;
                    }
                    // Concrete function type: call the pointer directly.
                    let fn_type = ret_llvm.fn_type(&arg_metas, false);
                    return self.builder.build_indirect_call(fn_type, raw_ptr, &arg_vals, "cb_call")
                        .unwrap().try_as_basic_value().basic().unwrap_or_else(|| ret_llvm.const_zero());
                }
                if let Some(llvm_fn) = self.global_fn_slots.get(slot).copied() {
                    return self.builder.build_call(llvm_fn, &arg_vals, "cb_call")
                        .unwrap().try_as_basic_value().basic().unwrap_or_else(|| ret_llvm.const_zero());
                }
                ret_llvm.const_zero()
            }
            TypedExpr::Function { params, body, captures, .. } if captures.is_empty() => {
                // Inline lambda: bind params to args directly, compile body.
                let saved: Vec<(usize, Option<SlotStorage<'ctx>>)> = params.iter().zip(args.iter())
                    .map(|(p, &a)| {
                        let old = fn_ctx.slots.insert(p.slot, SlotStorage::Value(a));
                        (p.slot, old)
                    }).collect();
                let result = self.compile_expr(body, fn_ctx);
                for (slot, old) in saved {
                    match old {
                        Some(prev) => { fn_ctx.slots.insert(slot, prev); }
                        None => { fn_ctx.slots.remove(&slot); }
                    }
                }
                result
            }
            TypedExpr::Function { params, body, captures, ret_type, .. } => {
                // Capturing lambda — compile closure, call immediately, then release (refcount=1, single use).
                let cls_ptr = self.compile_closure(None, params, body, ret_type, captures, fn_ctx).into_pointer_value();
                let cls_ty = self.closure_struct_type();
                let fn_ptr = self.builder.build_load(ptr_ty,
                    self.builder.build_struct_gep(cls_ty, cls_ptr, 2, "cbc_fp").unwrap(), "cbc_fn").unwrap().into_pointer_value();
                let env_ptr = self.builder.build_load(ptr_ty,
                    self.builder.build_struct_gep(cls_ty, cls_ptr, 3, "cbc_ep").unwrap(), "cbc_env").unwrap();
                let mut param_types: Vec<BasicMetadataTypeEnum> = vec![ptr_ty.into()];
                param_types.extend_from_slice(&arg_metas);
                let mut call_args: Vec<BasicMetadataValueEnum> = vec![env_ptr.into()];
                call_args.extend_from_slice(&arg_vals);
                let fn_type = ret_llvm.fn_type(&param_types, false);
                let result = self.builder.build_indirect_call(fn_type, fn_ptr, &call_args, "cbc_call")
                    .unwrap().try_as_basic_value().basic().unwrap_or_else(|| ret_llvm.const_zero());
                self.builder.build_call(self.rt_closure_release, &[cls_ptr.into()], "").unwrap();
                result
            }
            _ => {
                let fn_val = self.compile_expr(body_expr, fn_ctx);
                if let BasicValueEnum::PointerValue(fn_ptr) = fn_val {
                    let fn_type = ret_llvm.fn_type(&arg_metas, false);
                    self.builder.build_indirect_call(fn_type, fn_ptr, &arg_vals, "cb_call")
                        .unwrap().try_as_basic_value().basic().unwrap_or_else(|| ret_llvm.const_zero())
                } else {
                    ret_llvm.const_zero()
                }
            }
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
            TypedExpr::LocalGet { slot, ty: body_ty, .. } => {
                let cls_struct_type = self.closure_struct_type();
                // Resolve the closure struct pointer based on where the function is stored.
                let cls_ptr_opt: Option<PointerValue<'ctx>> = match fn_ctx.slots.get(slot).cloned() {
                    Some(SlotStorage::Closure(p)) => Some(p),
                    Some(SlotStorage::Value(BasicValueEnum::PointerValue(p))) => {
                        if matches!(body_ty, Type::Function { .. }) {
                            // Value slot holds {fn_ptr, env_ptr} struct directly.
                            Some(p)
                        } else {
                            None
                        }
                    }
                    Some(SlotStorage::Alloca(alloc)) => {
                        // Alloca holds a ptr to {fn_ptr, env_ptr} struct.
                        Some(self.builder.build_load(ptr_ty, alloc, "cbody_cls")
                            .unwrap().into_pointer_value())
                    }
                    _ => None,
                };
                if let Some(cls_ptr) = cls_ptr_opt {
                    if matches!(body_ty, Type::Function { .. }) {
                        // Concrete function: dispatch through {fn_ptr, env_ptr}.
                        let fn_ptr = self.builder
                            .build_load(ptr_ty,
                                self.builder.build_struct_gep(cls_struct_type, cls_ptr, 2, "cbody_fn_p").unwrap(),
                                "cbody_fn").unwrap().into_pointer_value();
                        let env_ptr = self.builder
                            .build_load(ptr_ty,
                                self.builder.build_struct_gep(cls_struct_type, cls_ptr, 3, "cbody_env_p").unwrap(),
                                "cbody_env").unwrap();
                        let fn_type = self.context.void_type().fn_type(&[ptr_ty.into(), arg_meta], false);
                        self.builder.build_indirect_call(fn_type, fn_ptr, &[env_ptr.into(), arg.into()], "for_body").unwrap();
                        return;
                    }
                    if matches!(body_ty, Type::TypeVar(_) | Type::Union(_)) {
                        // Tagged value (TaggedVal*): unbox to get closure struct, then dispatch.
                        let cls_raw = self.builder
                            .build_call(self.rt_unbox_ptr, &[cls_ptr.into()], "cbody_jcls")
                            .unwrap().try_as_basic_value().unwrap_basic().into_pointer_value();
                        let fn_ptr = self.builder
                            .build_load(ptr_ty,
                                self.builder.build_struct_gep(cls_struct_type, cls_raw, 2, "cbody_jfp").unwrap(),
                                "cbody_jfn").unwrap().into_pointer_value();
                        let env_ptr = self.builder
                            .build_load(ptr_ty,
                                self.builder.build_struct_gep(cls_struct_type, cls_raw, 3, "cbody_jep").unwrap(),
                                "cbody_jenv").unwrap();
                        let fn_type = self.context.void_type().fn_type(&[ptr_ty.into(), arg_meta], false);
                        self.builder.build_indirect_call(fn_type, fn_ptr, &[env_ptr.into(), arg.into()], "for_body_j").unwrap();
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
                // Use Never (→ void) as return type since for-loop bodies are side-effect only.
                let val = self.compile_closure(None, params, body, &Type::Never, captures, fn_ctx);
                let cls_ptr = val.into_pointer_value();
                let cls_struct_type = self.closure_struct_type();
                let fn_field = self.builder
                    .build_struct_gep(cls_struct_type, cls_ptr, 2, "ibody_fn_p")
                    .unwrap();
                let fn_ptr = self.builder
                    .build_load(ptr_ty, fn_field, "ibody_fn")
                    .unwrap()
                    .into_pointer_value();
                let env_field = self.builder
                    .build_struct_gep(cls_struct_type, cls_ptr, 3, "ibody_env_p")
                    .unwrap();
                let env_ptr = self.builder
                    .build_load(ptr_ty, env_field, "ibody_env")
                    .unwrap();
                let fn_type = self.context.void_type().fn_type(&[ptr_ty.into(), arg_meta], false);
                self.builder
                    .build_indirect_call(fn_type, fn_ptr, &[env_ptr.into(), arg.into()], "ibody_call")
                    .unwrap();
                // Release the closure after the single call (refcount=1, inline use).
                self.builder.build_call(self.rt_closure_release, &[cls_ptr.into()], "").unwrap();
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

    // -------------------------------------------------------------------------
    // Index assignment (obj[key] = val, arr[i] = val)
    // -------------------------------------------------------------------------

    fn compile_index_set(
        &mut self,
        object: &TypedExpr,
        key: &TypedExpr,
        value: &TypedExpr,
        obj_ty: &Type,
        fn_ctx: &mut FnCtx<'ctx, '_>,
    ) -> BasicValueEnum<'ctx> {
        let obj_val = self.compile_expr(object, fn_ctx);
        let key_val = self.compile_expr(key, fn_ctx);
        let val_val = self.compile_expr(value, fn_ctx);
        let val_ty = value.ty();
        let null_ptr = self.context.ptr_type(AddressSpace::default()).const_null();

        match obj_ty {
            Type::Object(_) | Type::Named(_) => {
                let obj_ptr = if obj_val.is_pointer_value() { obj_val.into_pointer_value() } else { return null_ptr.into(); };
                let key_ptr = if key_val.is_pointer_value() { key_val.into_pointer_value() } else { return null_ptr.into(); };
                let tagged = self.build_tagged_val_alloca(&val_val, &val_ty);
                self.builder.build_call(self.rt_object_set, &[obj_ptr.into(), key_ptr.into(), tagged.into()], "").unwrap();
            }
            Type::Array(_) | Type::FixedArray(_) => {
                let arr_ptr = obj_val;
                let idx = if key_val.is_int_value() {
                    self.builder.build_int_s_extend_or_bit_cast(key_val.into_int_value(), self.context.i64_type(), "idx").unwrap()
                } else if key_val.is_pointer_value() {
                    // TaggedVal* key — unbox to i32 then extend to i64.
                    let i32_key = self.builder.build_call(self.rt_unbox_int32, &[key_val.into()], "skey_i32")
                        .unwrap().try_as_basic_value().unwrap_basic().into_int_value();
                    self.builder.build_int_s_extend(i32_key, self.context.i64_type(), "skey_i64").unwrap()
                } else { self.context.i64_type().const_int(0, false) };
                let tagged = self.build_tagged_val_alloca(&val_val, &val_ty);
                let ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
                let set_fn = self.get_or_declare_fn("lin_array_set",
                    self.context.void_type().fn_type(&[ptr_ty.into(), self.context.i64_type().into(), ptr_ty.into()], false));
                self.builder.build_call(set_fn, &[arr_ptr.into(), idx.into(), tagged.into()], "").unwrap();
            }
            Type::TypeVar(_) | Type::Union(_) => {
                // Dynamic dispatch: check if it's an object (string key) or array (int key).
                if key_val.is_pointer_value() {
                    // String key → object set
                    let tagged_ptr = if obj_val.is_pointer_value() { obj_val.into_pointer_value() } else { return null_ptr.into(); };
                    let key_ptr = key_val.into_pointer_value();
                    let tag = self.builder.build_call(self.rt_get_tag, &[tagged_ptr.into()], "tv_tag")
                        .unwrap().try_as_basic_value().unwrap_basic().into_int_value();
                    let is_obj = self.builder.build_int_compare(IntPredicate::EQ, tag,
                        self.context.i8_type().const_int(7, false), "is_obj").unwrap();
                    let llvm_fn = fn_ctx.llvm_fn;
                    let obj_block = self.context.append_basic_block(llvm_fn, "iset_obj");
                    let skip_block = self.context.append_basic_block(llvm_fn, "iset_skip");
                    self.builder.build_conditional_branch(is_obj, obj_block, skip_block).unwrap();
                    self.builder.position_at_end(obj_block);
                    let obj_ptr = self.builder.build_call(self.rt_unbox_ptr, &[tagged_ptr.into()], "obj_ptr")
                        .unwrap().try_as_basic_value().unwrap_basic().into_pointer_value();
                    let tagged = self.build_tagged_val_alloca(&val_val, &val_ty);
                    self.builder.build_call(self.rt_object_set, &[obj_ptr.into(), key_ptr.into(), tagged.into()], "").unwrap();
                    self.builder.build_unconditional_branch(skip_block).unwrap();
                    self.builder.position_at_end(skip_block);
                } else if key_val.is_int_value() {
                    // Int key → array set
                    let tagged_ptr = if obj_val.is_pointer_value() { obj_val.into_pointer_value() } else { return null_ptr.into(); };
                    let arr_ptr = self.builder.build_call(self.rt_unbox_ptr, &[tagged_ptr.into()], "arr_ptr")
                        .unwrap().try_as_basic_value().unwrap_basic();
                    let idx = self.builder.build_int_s_extend_or_bit_cast(key_val.into_int_value(), self.context.i64_type(), "idx").unwrap();
                    let tagged = self.build_tagged_val_alloca(&val_val, &val_ty);
                    let ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
                    let set_fn = self.get_or_declare_fn("lin_array_set",
                        self.context.void_type().fn_type(&[ptr_ty.into(), self.context.i64_type().into(), ptr_ty.into()], false));
                    self.builder.build_call(set_fn, &[arr_ptr.into(), idx.into(), tagged.into()], "").unwrap();
                }
            }
            _ => {}
        }
        null_ptr.into()
    }

    // -------------------------------------------------------------------------
    // Objects
    // -------------------------------------------------------------------------

    fn compile_make_object(
        &mut self,
        fields: &[(String, TypedExpr)],
        spreads: &[TypedExpr],
        fn_ctx: &mut FnCtx<'ctx, '_>,
    ) -> BasicValueEnum<'ctx> {
        // Compile as a dynamic LinObject (key-value array).
        let i32_ty = self.context.i32_type();
        let cap = i32_ty.const_int((fields.len() + 4).max(4) as u64, false);
        let obj_ptr = self.builder
            .build_call(self.rt_object_alloc, &[cap.into()], "obj")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();

        // First apply all spreads (so explicit fields override them).
        if !spreads.is_empty() {
            let ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
            let merge_fn = self.get_or_declare_fn("lin_object_merge",
                self.context.void_type().fn_type(&[ptr_ty.into(), ptr_ty.into()], false));
            for spread_expr in spreads {
                let spread_val = self.compile_expr(spread_expr, fn_ctx);
                let spread_ty = spread_expr.ty();
                let spread_ptr = if spread_val.is_pointer_value() {
                    let sp = spread_val.into_pointer_value();
                    if matches!(spread_ty, Type::TypeVar(_) | Type::Union(_)) {
                        // Boxed TaggedVal* — unbox to get LinObject*.
                        self.builder
                            .build_call(self.rt_unbox_ptr, &[sp.into()], "spread_unbox")
                            .unwrap().try_as_basic_value().unwrap_basic().into_pointer_value()
                    } else {
                        sp
                    }
                } else {
                    // Non-pointer scalar: unbox to get LinObject*.
                    self.builder
                        .build_call(self.rt_unbox_ptr, &[spread_val.into()], "spread_unbox")
                        .unwrap().try_as_basic_value().unwrap_basic().into_pointer_value()
                };
                self.builder.build_call(merge_fn, &[obj_ptr.into(), spread_ptr.into()], "").unwrap();
            }
        }

        for (key, val_expr) in fields.iter() {
            let val = self.compile_expr(val_expr, fn_ctx);
            let val_ty = val_expr.ty();
            // Retain shared heap values: the object will release them in lin_object_release,
            // so we need a matching retain for each non-fresh (LocalGet) heap value stored.
            if !Self::expr_is_owned_alloc(val_expr) && Self::ty_is_heap(&val_ty) && val.is_pointer_value() {
                self.builder.build_call(self.rt_rc_retain, &[val.into()], "").unwrap();
            }
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
            // lin_object_set retained the key; release our reference from compile_string_lit.
            self.builder.build_call(self.rt_string_release, &[key_str.into()], "").unwrap();
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
        self.builder.build_call(self.rt_string_release, &[key_str.into()], "").unwrap();

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
        let key_ty = key.ty();

        match &obj_ty {
            Type::Array(_) | Type::FixedArray(_) => {
                // Array index: key must be integer, bounds-checked at runtime.
                let idx = if key_val.is_int_value() {
                    self.builder
                        .build_int_s_extend_or_bit_cast(key_val.into_int_value(), self.context.i64_type(), "idx")
                        .unwrap()
                } else if key_val.is_pointer_value() {
                    // Key is a TaggedVal* (e.g. when passed through a Json/Function callback).
                    // Unbox to i32 then sign-extend to i64 for array indexing.
                    let i32_key = self.builder
                        .build_call(self.rt_unbox_int32, &[key_val.into()], "key_i32")
                        .unwrap()
                        .try_as_basic_value()
                        .unwrap_basic()
                        .into_int_value();
                    self.builder.build_int_s_extend(i32_key, self.context.i64_type(), "key_i64").unwrap()
                } else {
                    self.context.i64_type().const_int(0, false)
                };
                // Use flat path for known scalar element types.
                if Self::is_flat_scalar(result_type) {
                    return self.flat_array_get(obj_val, idx, result_type);
                }
                // For TypeVar/Union result, use lin_array_get_tagged so the result is always
                // a valid TaggedVal* regardless of whether the array is flat or tagged.
                if matches!(result_type, Type::TypeVar(_) | Type::Union(_)) {
                    let ptr_ty = self.context.ptr_type(AddressSpace::default());
                    let i64_ty = self.context.i64_type();
                    let get_tagged_fn = self.get_or_declare_fn("lin_array_get_tagged",
                        ptr_ty.fn_type(&[ptr_ty.into(), i64_ty.into()], false));
                    return self.builder
                        .build_call(get_tagged_fn, &[obj_val.into(), idx.into()], "aref_tv")
                        .unwrap().try_as_basic_value().unwrap_basic();
                }

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
                // Key might be a raw *LinString (key_ty=Str) or a TaggedVal* (key_ty=TypeVar/Union).
                // lin_object_get expects raw *LinString — unbox if needed.
                let key_ptr = if key_val.is_pointer_value() {
                    let kp = key_val.into_pointer_value();
                    if matches!(key_ty, Type::TypeVar(_) | Type::Union(_)) {
                        // TaggedVal* — unbox to get raw LinString*
                        self.builder.build_call(self.rt_unbox_ptr, &[kp.into()], "obj_key_ub")
                            .unwrap().try_as_basic_value().unwrap_basic().into_pointer_value()
                    } else {
                        kp
                    }
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
                // For TypeVar/Union/Null result, entry_ptr IS a TaggedVal*.
                // lin_object_get returns null for missing keys — box as TAG_NULL so callers
                // always receive a valid TaggedVal* (null raw ptr causes crashes downstream).
                // Null result_type means the key was not in the static schema (e.g. Object({})["x"]),
                // but the object may have been populated dynamically — do a runtime lookup.
                if matches!(result_type, Type::TypeVar(_) | Type::Union(_) | Type::Null) {
                    let llvm_fn_inner = fn_ctx.llvm_fn;
                    let is_null_e = self.builder.build_is_null(entry_ptr, "entry_is_null").unwrap();
                    let found_block = self.context.append_basic_block(llvm_fn_inner, "obj_load");
                    let null_block_e = self.context.append_basic_block(llvm_fn_inner, "obj_enull");
                    let merge_block_e = self.context.append_basic_block(llvm_fn_inner, "obj_emrg");
                    let ptr_ty_e = self.context.ptr_type(AddressSpace::default());
                    self.builder.build_conditional_branch(is_null_e, null_block_e, found_block).unwrap();
                    self.builder.position_at_end(found_block);
                    self.builder.build_unconditional_branch(merge_block_e).unwrap();
                    self.builder.position_at_end(null_block_e);
                    let boxnull = self.builder
                        .build_call(self.rt_box_null, &[], "boxnull_miss")
                        .unwrap().try_as_basic_value().unwrap_basic().into_pointer_value();
                    self.builder.build_unconditional_branch(merge_block_e).unwrap();
                    self.builder.position_at_end(merge_block_e);
                    let phi_e = self.builder.build_phi(ptr_ty_e, "obj_ephi_e").unwrap();
                    phi_e.add_incoming(&[(&entry_ptr, found_block), (&boxnull, null_block_e)]);
                    return phi_e.as_basic_value();
                }
                // For concrete types, guard against null before loading payload.
                let is_null = self.builder.build_is_null(entry_ptr, "entry_is_null").unwrap();
                let llvm_fn = fn_ctx.llvm_fn;
                let load_block = self.context.append_basic_block(llvm_fn, "obj_load");
                let null_block = self.context.append_basic_block(llvm_fn, "obj_enull");
                let merge_block = self.context.append_basic_block(llvm_fn, "obj_emrg");
                self.builder.build_conditional_branch(is_null, null_block, load_block).unwrap();
                self.builder.position_at_end(load_block);
                let loaded = self.load_tagged_val_payload(entry_ptr, result_type, &obj_ty);
                self.builder.build_unconditional_branch(merge_block).unwrap();
                self.builder.position_at_end(null_block);
                let zero = self.llvm_type(result_type).const_zero();
                self.builder.build_unconditional_branch(merge_block).unwrap();
                self.builder.position_at_end(merge_block);
                let phi = self.builder.build_phi(self.llvm_type(result_type), "obj_ephi").unwrap();
                phi.add_incoming(&[(&loaded, load_block), (&zero, null_block)]);
                phi.as_basic_value()
            }
            Type::TypeVar(_) | Type::Union(_) => {
                let tagged_ptr = if obj_val.is_pointer_value() {
                    obj_val.into_pointer_value()
                } else {
                    return self.llvm_type(result_type).const_zero();
                };
                // Integer key → array indexing. Check tag is TAG_ARRAY before unboxing.
                // Also handle TaggedVal* keys (passed through Json/Function callbacks) that
                // contain integer values (e.g. from for(range(...), i => obj[i])).
                let int_key_opt: Option<inkwell::values::IntValue<'ctx>> = if key_val.is_int_value() {
                    Some(self.builder
                        .build_int_s_extend_or_bit_cast(key_val.into_int_value(), self.context.i64_type(), "tv_idx_raw")
                        .unwrap())
                } else {
                    None
                };
                // When the key is statically typed as String, it's a raw *LinString (not TaggedVal).
                // Skip runtime tag dispatch and call lin_object_get directly.
                if matches!(key_ty, Type::Str) && key_val.is_pointer_value() {
                    let ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
                    let llvm_fn = fn_ctx.llvm_fn;
                    let obj_tag = self.builder
                        .build_call(self.rt_get_tag, &[tagged_ptr.into()], "tv_obj_tag_s")
                        .unwrap().try_as_basic_value().unwrap_basic().into_int_value();
                    let is_obj = self.builder
                        .build_int_compare(IntPredicate::EQ, obj_tag,
                            self.context.i8_type().const_int(7, false), "tv_is_obj_s").unwrap();
                    let obj_ok  = self.context.append_basic_block(llvm_fn, "tv_obj_ok_s");
                    let obj_no  = self.context.append_basic_block(llvm_fn, "tv_obj_no_s");
                    let obj_mrg = self.context.append_basic_block(llvm_fn, "tv_obj_mrg_s");
                    self.builder.build_conditional_branch(is_obj, obj_ok, obj_no).unwrap();
                    self.builder.position_at_end(obj_ok);
                    let obj_ptr = self.builder.build_call(self.rt_unbox_ptr, &[tagged_ptr.into()], "tv_obj_p_s")
                        .unwrap().try_as_basic_value().unwrap_basic().into_pointer_value();
                    let entry = self.builder
                        .build_call(self.rt_object_get, &[obj_ptr.into(), key_val.into()], "tv_sentry_s")
                        .unwrap().try_as_basic_value().unwrap_basic().into_pointer_value();
                    self.builder.build_unconditional_branch(obj_mrg).unwrap();
                    self.builder.position_at_end(obj_no);
                    let null_res = ptr_ty.const_null();
                    self.builder.build_unconditional_branch(obj_mrg).unwrap();
                    self.builder.position_at_end(obj_mrg);
                    let phi = self.builder.build_phi(ptr_ty, "tv_str_res_s").unwrap();
                    phi.add_incoming(&[(&entry, obj_ok), (&null_res, obj_no)]);
                    let result_ptr = phi.as_basic_value().into_pointer_value();
                    if matches!(result_type, Type::TypeVar(_) | Type::Union(_)) {
                        return result_ptr.into();
                    }
                    return self.load_tagged_val_payload(result_ptr, result_type, &obj_ty);
                }
                // Pointer-key case: check key tag at runtime to distinguish int from string.
                if key_val.is_pointer_value() && int_key_opt.is_none() {
                    let ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
                    let i64_ty = self.context.i64_type();
                    let llvm_fn = fn_ctx.llvm_fn;
                    // Read key tag to determine int vs string.
                    let k_tag = self.builder
                        .build_call(self.rt_get_tag, &[key_val.into()], "kv_tag")
                        .unwrap().try_as_basic_value().unwrap_basic().into_int_value();
                    let is_int32 = self.builder
                        .build_int_compare(IntPredicate::EQ, k_tag,
                            self.context.i8_type().const_int(2, false), "kv_is_i32").unwrap();
                    let is_int64 = self.builder
                        .build_int_compare(IntPredicate::EQ, k_tag,
                            self.context.i8_type().const_int(3, false), "kv_is_i64").unwrap();
                    let is_int_key = self.builder.build_or(is_int32, is_int64, "kv_is_int").unwrap();
                    let int_key_block  = self.context.append_basic_block(llvm_fn, "tv_int_key");
                    let str_key_block  = self.context.append_basic_block(llvm_fn, "tv_str_key");
                    let int_merge      = self.context.append_basic_block(llvm_fn, "tv_int_mrg");
                    let str_merge      = self.context.append_basic_block(llvm_fn, "tv_str_mrg");
                    let final_block    = self.context.append_basic_block(llvm_fn, "tv_final");
                    self.builder.build_conditional_branch(is_int_key, int_key_block, str_key_block).unwrap();

                    // ── int key branch ──────────────────────────────────────────
                    self.builder.position_at_end(int_key_block);
                    let raw_i32 = self.builder
                        .build_call(self.rt_unbox_int32, &[key_val.into()], "kv_i32")
                        .unwrap().try_as_basic_value().unwrap_basic().into_int_value();
                    let idx_i64 = self.builder.build_int_s_extend(raw_i32, i64_ty, "kv_i64").unwrap();
                    let arr_tag = self.builder
                        .build_call(self.rt_get_tag, &[tagged_ptr.into()], "tv_arr_tag")
                        .unwrap().try_as_basic_value().unwrap_basic().into_int_value();
                    let is_arr = self.builder
                        .build_int_compare(IntPredicate::EQ, arr_tag,
                            self.context.i8_type().const_int(8, false), "tv_is_arr").unwrap();
                    let arr_ok  = self.context.append_basic_block(llvm_fn, "tv_arr_ok");
                    let arr_no  = self.context.append_basic_block(llvm_fn, "tv_arr_no");
                    self.builder.build_conditional_branch(is_arr, arr_ok, arr_no).unwrap();

                    self.builder.position_at_end(arr_ok);
                    let arr_inner = self.builder
                        .build_call(self.rt_unbox_ptr, &[tagged_ptr.into()], "tv_arr_p")
                        .unwrap().try_as_basic_value().unwrap_basic();
                    let get_tagged_fn = self.get_or_declare_fn("lin_array_get_tagged",
                        ptr_ty.fn_type(&[ptr_ty.into(), i64_ty.into()], false));
                    let elem_tv = self.builder
                        .build_call(get_tagged_fn, &[arr_inner.into(), idx_i64.into()], "tv_elem")
                        .unwrap().try_as_basic_value().unwrap_basic();
                    self.builder.build_unconditional_branch(int_merge).unwrap();

                    self.builder.position_at_end(arr_no);
                    let null_int = ptr_ty.const_null();
                    self.builder.build_unconditional_branch(int_merge).unwrap();

                    self.builder.position_at_end(int_merge);
                    let int_phi = self.builder.build_phi(ptr_ty, "tv_int_res").unwrap();
                    int_phi.add_incoming(&[(&elem_tv, arr_ok), (&null_int, arr_no)]);
                    let int_res = int_phi.as_basic_value();
                    self.builder.build_unconditional_branch(final_block).unwrap();

                    // ── string key branch ───────────────────────────────────────
                    self.builder.position_at_end(str_key_block);
                    let obj_tag = self.builder
                        .build_call(self.rt_get_tag, &[tagged_ptr.into()], "tv_obj_tag")
                        .unwrap().try_as_basic_value().unwrap_basic().into_int_value();
                    let is_obj = self.builder
                        .build_int_compare(IntPredicate::EQ, obj_tag,
                            self.context.i8_type().const_int(7, false), "tv_is_obj").unwrap();
                    let obj_ok  = self.context.append_basic_block(llvm_fn, "tv_obj_ok");
                    let obj_no  = self.context.append_basic_block(llvm_fn, "tv_obj_no");
                    self.builder.build_conditional_branch(is_obj, obj_ok, obj_no).unwrap();

                    self.builder.position_at_end(obj_ok);
                    let obj_ptr = self.builder.build_call(self.rt_unbox_ptr, &[tagged_ptr.into()], "tv_obj_p")
                        .unwrap().try_as_basic_value().unwrap_basic().into_pointer_value();
                    let str_entry = self.builder
                        .build_call(self.rt_object_get, &[obj_ptr.into(), key_val.into()], "tv_sentry")
                        .unwrap().try_as_basic_value().unwrap_basic().into_pointer_value();
                    self.builder.build_unconditional_branch(str_merge).unwrap();

                    self.builder.position_at_end(obj_no);
                    let null_str = ptr_ty.const_null();
                    self.builder.build_unconditional_branch(str_merge).unwrap();

                    self.builder.position_at_end(str_merge);
                    let str_phi = self.builder.build_phi(ptr_ty, "tv_str_res").unwrap();
                    str_phi.add_incoming(&[(&str_entry, obj_ok), (&null_str, obj_no)]);
                    let str_res = str_phi.as_basic_value();
                    self.builder.build_unconditional_branch(final_block).unwrap();

                    // ── final merge ─────────────────────────────────────────────
                    self.builder.position_at_end(final_block);
                    let final_phi = self.builder.build_phi(ptr_ty, "tv_res").unwrap();
                    final_phi.add_incoming(&[(&int_res, int_merge), (&str_res, str_merge)]);
                    let result_ptr: PointerValue<'ctx> = final_phi.as_basic_value().into_pointer_value();
                    if matches!(result_type, Type::TypeVar(_) | Type::Union(_)) {
                        return result_ptr.into();
                    }
                    return self.load_tagged_val_payload(result_ptr, result_type, &obj_ty);
                }
                if let Some(int_key) = int_key_opt {
                    let ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
                    let i64_ty = self.context.i64_type();
                    // Guard: if not TAG_ARRAY (8), return null rather than UB.
                    let tag = self.builder
                        .build_call(self.rt_get_tag, &[tagged_ptr.into()], "arr_tag")
                        .unwrap().try_as_basic_value().unwrap_basic().into_int_value();
                    let is_arr = self.builder
                        .build_int_compare(IntPredicate::EQ, tag,
                            self.context.i8_type().const_int(8, false), "is_arr")
                        .unwrap();
                    let llvm_fn = fn_ctx.llvm_fn;
                    let arr_block = self.context.append_basic_block(llvm_fn, "arr_index");
                    let arr_null_block = self.context.append_basic_block(llvm_fn, "arr_null");
                    let arr_merge_block = self.context.append_basic_block(llvm_fn, "arr_merge");
                    self.builder.build_conditional_branch(is_arr, arr_block, arr_null_block).unwrap();
                    // arr_block: unbox and fetch element
                    self.builder.position_at_end(arr_block);
                    let arr_ptr = self.builder
                        .build_call(self.rt_unbox_ptr, &[tagged_ptr.into()], "tv_arr")
                        .unwrap().try_as_basic_value().unwrap_basic();
                    let idx = int_key;
                    let get_tagged_fn = self.get_or_declare_fn("lin_array_get_tagged",
                        ptr_ty.fn_type(&[ptr_ty.into(), i64_ty.into()], false));
                    let elem_tv = self.builder
                        .build_call(get_tagged_fn, &[arr_ptr.into(), idx.into()], "tv_elem")
                        .unwrap().try_as_basic_value().unwrap_basic();
                    self.builder.build_unconditional_branch(arr_merge_block).unwrap();
                    // arr_null_block: return null pointer
                    self.builder.position_at_end(arr_null_block);
                    let arr_null_ptr = ptr_ty.const_null();
                    self.builder.build_unconditional_branch(arr_merge_block).unwrap();
                    // arr_merge_block: phi
                    self.builder.position_at_end(arr_merge_block);
                    let phi = self.builder.build_phi(ptr_ty, "arr_res").unwrap();
                    phi.add_incoming(&[(&elem_tv, arr_block), (&arr_null_ptr, arr_null_block)]);
                    let elem_result = phi.as_basic_value();
                    if matches!(result_type, Type::TypeVar(_) | Type::Union(_)) {
                        return elem_result;
                    }
                    return self.load_tagged_val_payload(elem_result.into_pointer_value(), result_type, &obj_ty);
                }
                // String key → object lookup. Verify tag is TAG_OBJECT before unboxing.
                let key_ptr = if key_val.is_pointer_value() {
                    key_val.into_pointer_value()
                } else {
                    return self.llvm_type(result_type).const_zero();
                };
                // Check that the tagged value is an object (tag == TAG_OBJECT == 7).
                let tag = self.builder
                    .build_call(self.rt_get_tag, &[tagged_ptr.into()], "tv_tag")
                    .unwrap().try_as_basic_value().unwrap_basic().into_int_value();
                let is_obj = self.builder
                    .build_int_compare(IntPredicate::EQ, tag,
                        self.context.i8_type().const_int(7, false), "is_obj")
                    .unwrap();
                let llvm_fn = fn_ctx.llvm_fn;
                let obj_block = self.context.append_basic_block(llvm_fn, "obj_index");
                let null_block = self.context.append_basic_block(llvm_fn, "obj_null");
                let merge_block = self.context.append_basic_block(llvm_fn, "obj_merge");
                self.builder.build_conditional_branch(is_obj, obj_block, null_block).unwrap();
                // obj_block: perform the lookup
                self.builder.position_at_end(obj_block);
                let obj_ptr = self.builder
                    .build_call(self.rt_unbox_ptr, &[tagged_ptr.into()], "obj_ptr")
                    .unwrap().try_as_basic_value().unwrap_basic().into_pointer_value();
                let entry_tv = self.builder
                    .build_call(self.rt_object_get, &[obj_ptr.into(), key_ptr.into()], "oindex_p")
                    .unwrap().try_as_basic_value().unwrap_basic().into_pointer_value();
                self.builder.build_unconditional_branch(merge_block).unwrap();
                // null_block: return null pointer
                self.builder.position_at_end(null_block);
                let null_ptr = self.context.ptr_type(AddressSpace::default()).const_null();
                self.builder.build_unconditional_branch(merge_block).unwrap();
                // merge_block: phi of entry_tv and null_ptr
                self.builder.position_at_end(merge_block);
                let ptr_ty = self.context.ptr_type(AddressSpace::default());
                let phi = self.builder.build_phi(ptr_ty, "obj_res").unwrap();
                phi.add_incoming(&[(&entry_tv, obj_block), (&null_ptr, null_block)]);
                let entry_ptr: PointerValue<'ctx> = phi.as_basic_value().into_pointer_value();
                // For TypeVar/Union result: return the TaggedVal* directly.
                if matches!(result_type, Type::TypeVar(_) | Type::Union(_)) {
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

    /// Returns true if all arms are tag-dispatch (`Is(TypeCheck)`) with no guards,
    /// and the scrutinee is a tagged union/TypeVar — the precondition for LLVM `switch`.
    fn can_use_tag_switch(arms: &[TypedMatchArm], scrut_ty: &Type) -> bool {
        if !Self::is_union_type(scrut_ty) {
            return false;
        }
        arms.iter().all(|arm| {
            if arm.guard.is_some() {
                return false;
            }
            matches!(&arm.pattern,
                TypedMatchPattern::Is(TypedPattern::TypeCheck(_, _)) |
                TypedMatchPattern::Else
            )
        })
    }

    /// Compile a match where every arm is an `is Type` tag check using a single LLVM `switch`.
    /// Returns the result value; caller must be positioned after the merge block.
    fn compile_match_switch(
        &mut self,
        scrut_val: BasicValueEnum<'ctx>,
        arms: &[TypedMatchArm],
        result_type: &Type,
        fn_ctx: &mut FnCtx<'ctx, '_>,
    ) -> BasicValueEnum<'ctx> {
        let i8_ty = self.context.i8_type();
        let result_llvm_ty = self.llvm_type(result_type);
        let merge_block = self.context.append_basic_block(fn_ctx.llvm_fn, "sw_merge");
        let mut incoming: Vec<(BasicValueEnum<'ctx>, inkwell::basic_block::BasicBlock<'ctx>)> = Vec::new();

        // Get the tag from the tagged-value pointer.
        let tag = self.builder
            .build_call(self.rt_get_tag, &[scrut_val.into()], "sw_tag")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_int_value();

        // Locate else arm (if any) — becomes the switch default.
        let else_arm = arms.iter().find(|a| matches!(a.pattern, TypedMatchPattern::Else));

        // Build the default block (else arm or panic).
        let default_block = self.context.append_basic_block(fn_ctx.llvm_fn, "sw_default");

        // Collect (tag_value, arm_body_block) pairs.
        let mut cases: Vec<(u8, inkwell::basic_block::BasicBlock<'ctx>, &TypedMatchArm)> = Vec::new();
        for arm in arms.iter() {
            if let TypedMatchPattern::Is(TypedPattern::TypeCheck(ty, _)) = &arm.pattern {
                let arm_block = self.context.append_basic_block(fn_ctx.llvm_fn, "sw_arm");
                cases.push((Self::type_tag(ty), arm_block, arm));
            }
        }

        // Build switch instruction: collect (tag_constant, block) pairs and pass as slice.
        let case_pairs: Vec<(inkwell::values::IntValue<'ctx>, inkwell::basic_block::BasicBlock<'ctx>)> = cases
            .iter()
            .map(|(tag_val, arm_block, _)| (i8_ty.const_int(*tag_val as u64, false), *arm_block))
            .collect();
        self.builder
            .build_switch(tag, default_block, &case_pairs)
            .unwrap();

        // Compile each case arm body.
        for (_, arm_block, arm) in &cases {
            self.builder.position_at_end(*arm_block);
            let body_val = self.compile_expr(&arm.body, fn_ctx);
            let arm_end = self.builder.get_insert_block().unwrap();
            if arm_end.get_terminator().is_none() {
                self.builder.build_unconditional_branch(merge_block).unwrap();
                incoming.push((body_val, arm_end));
            }
        }

        // Compile the default block.
        self.builder.position_at_end(default_block);
        if let Some(arm) = else_arm {
            let body_val = self.compile_expr(&arm.body, fn_ctx);
            let end = self.builder.get_insert_block().unwrap();
            if end.get_terminator().is_none() {
                self.builder.build_unconditional_branch(merge_block).unwrap();
                incoming.push((body_val, end));
            }
        } else {
            let panic_msg = self.compile_string_lit("match: no arm matched");
            let zero = self.context.i32_type().const_zero();
            self.builder.build_call(self.rt_panic, &[panic_msg.into(), zero.into(), zero.into()], "").unwrap();
            self.builder.build_unreachable().unwrap();
        }

        // Phi merge.
        self.builder.position_at_end(merge_block);
        if incoming.is_empty() {
            return result_llvm_ty.const_zero();
        }
        if incoming.len() == 1 {
            return incoming[0].0;
        }
        let phi = self.builder.build_phi(result_llvm_ty, "sw_result").unwrap();
        for (val, block) in &incoming {
            phi.add_incoming(&[(val, *block)]);
        }
        phi.as_basic_value()
    }

    fn compile_match(
        &mut self,
        scrutinee: &TypedExpr,
        arms: &[TypedMatchArm],
        result_type: &Type,
        fn_ctx: &mut FnCtx<'ctx, '_>,
    ) -> BasicValueEnum<'ctx> {
        let scrut_val = self.compile_expr(scrutinee, fn_ctx);
        let scrut_ty = scrutinee.ty();

        // Fast path: all `is Type` arms with no guards → emit a single LLVM switch.
        if Self::can_use_tag_switch(arms, &scrut_ty) {
            return self.compile_match_switch(scrut_val, arms, result_type, fn_ctx);
        }

        let merge_block = self.context.append_basic_block(fn_ctx.llvm_fn, "match_merge");
        let result_llvm_ty = self.llvm_type(result_type);

        let mut incoming: Vec<(BasicValueEnum<'ctx>, inkwell::basic_block::BasicBlock<'ctx>)> =
            Vec::new();

        let mut next_check = self.context.append_basic_block(fn_ctx.llvm_fn, "arm_0_check");
        self.builder.build_unconditional_branch(next_check).unwrap();

        // Determine if ownership is mixed across arms so we can normalise at the merge point.
        // If any arm is non-owned and result is a concrete heap type, retain the non-owned arms.
        let result_is_union_match = Self::is_union_type(result_type);
        let all_arms_owned = arms.iter().all(|a| Self::expr_is_owned_alloc(&a.body));
        let any_arm_owned = arms.iter().any(|a| Self::expr_is_owned_alloc(&a.body));
        let match_needs_normalize = Self::ty_is_heap(result_type)
            && !result_is_union_match
            && !matches!(result_type, Type::Never)
            && any_arm_owned && !all_arms_owned;

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
            let arm_owned = Self::expr_is_owned_alloc(&arm.body);
            self.builder.position_at_end(body_block);
            let body_val_raw = self.compile_expr(&arm.body, fn_ctx);
            let arm_ty = arm.body.ty();
            let result_is_union = Self::is_union_type(result_type);
            let body_val = if result_is_union && !Self::is_union_type(&arm_ty) && !matches!(arm_ty, Type::Never) {
                // Result is TaggedVal* but arm produced a concrete type — box it.
                self.box_value(body_val_raw, &arm_ty)
            } else if !result_is_union && Self::is_union_type(&arm_ty) && !matches!(arm_ty, Type::Never) {
                // Result is concrete but arm produced TaggedVal* — unbox/coerce.
                self.coerce_typevar(body_val_raw, &arm_ty, result_type)
            } else if body_val_raw.get_type() == result_llvm_ty {
                body_val_raw
            } else {
                result_llvm_ty.const_zero()
            };
            let body_end = self.builder.get_insert_block().unwrap();
            if !body_end.get_terminator().is_some() {
                // Retain non-owned arm value to normalise ownership with owned arms at the merge.
                if match_needs_normalize && !arm_owned && body_val.is_pointer_value() {
                    self.builder.build_call(self.rt_rc_retain, &[body_val.into()], "").unwrap();
                }
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
        let match_result = if incoming.is_empty() {
            result_llvm_ty.const_zero()
        } else if incoming.len() == 1 {
            incoming[0].0
        } else {
            let phi = self
                .builder
                .build_phi(result_llvm_ty, "match_result")
                .unwrap();
            for (val, block) in &incoming {
                phi.add_incoming(&[(val, *block)]);
            }
            phi.as_basic_value()
        };

        // Release a fresh scrutinee allocation after all arms have used it.
        if Self::expr_is_owned_alloc(scrutinee)
            && Self::ty_is_heap(&scrut_ty)
            && !Self::is_union_type(&scrut_ty)
        {
            self.emit_release(scrut_val, &scrut_ty);
        }

        match_result
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
            TypedPattern::Binding(slot, _ty, _) => {
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
                    // Pre-allocate allocas for each binding slot BEFORE the branch so they
                    // dominate all successor blocks (including merge_block where guards run).
                    // Field values are stored as TaggedVal* (ptr) in the alloca.
                    let ptr_ty = self.context.ptr_type(AddressSpace::default());
                    let binding_allocas: std::collections::HashMap<usize, inkwell::values::PointerValue<'ctx>> = fields.iter()
                        .filter_map(|pf| pf.binding_slot.map(|slot| {
                            let alloc = self.builder.build_alloca(ptr_ty, "has_bind").unwrap();
                            self.builder.build_store(alloc, ptr_ty.const_null()).unwrap();
                            (slot, alloc)
                        }))
                        .collect();

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
                            self.builder.build_call(self.rt_string_release, &[key_str.into()], "").unwrap();
                        }
                    }

                    // Store field TaggedVal* values into pre-allocated allocas.
                    for pf in fields {
                        if let Some(binding_slot) = pf.binding_slot {
                            if let Some(&alloc) = binding_allocas.get(&binding_slot) {
                                let key_str = self.compile_string_lit(&pf.key).into_pointer_value();
                                let entry_ptr = self.builder
                                    .build_call(self.rt_object_get, &[unboxed.into(), key_str.into()], "fget_p")
                                    .unwrap()
                                    .try_as_basic_value().unwrap_basic();
                                self.builder.build_call(self.rt_string_release, &[key_str.into()], "").unwrap();
                                self.builder.build_store(alloc, entry_ptr).unwrap();
                            }
                        }
                    }

                    let then_end = self.builder.get_insert_block().unwrap();
                    self.builder.build_unconditional_branch(merge_block).unwrap();

                    self.builder.position_at_end(merge_block);
                    let phi = self.builder.build_phi(self.context.bool_type(), "obj_match").unwrap();
                    phi.add_incoming(&[(&all_ok, then_end), (&self.context.bool_type().const_int(0, false), else_block)]);
                    // Register binding slots as Value(loaded ptr) — load from alloca here so the
                    // loaded value dominates all uses in merge_block and beyond.
                    for pf in fields {
                        if let Some(binding_slot) = pf.binding_slot {
                            if let Some(&alloc) = binding_allocas.get(&binding_slot) {
                                let loaded = self.builder.build_load(ptr_ty, alloc, "has_fld").unwrap();
                                fn_ctx.slots.insert(binding_slot, SlotStorage::Value(loaded));
                            }
                        }
                    }
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
                        self.builder.build_call(self.rt_string_release, &[key_str.into()], "").unwrap();
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
                        self.builder.build_call(self.rt_string_release, &[key_str.into()], "").unwrap();
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
            TypedPattern::Array { elements, rest, .. } => {
                let ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
                let i64_ty = self.context.i64_type();
                let bool_ty = self.context.bool_type();
                let false_val = bool_ty.const_int(0, false);
                let true_val = bool_ty.const_int(1, false);

                // Unbox TaggedVal* -> LinArray* if scrutinee is union/TypeVar.
                let arr_ptr: PointerValue = if is_union_scrut {
                    let tag_arr = self.context.i8_type().const_int(8, false); // TAG_ARRAY
                    let actual_tag = self.builder
                        .build_call(self.rt_get_tag, &[scrut_val.into()], "arrtag")
                        .unwrap().try_as_basic_value().unwrap_basic().into_int_value();
                    let is_arr = self.builder.build_int_compare(inkwell::IntPredicate::EQ, actual_tag, tag_arr, "is_arr").unwrap();
                    let tag_ok_block = self.context.append_basic_block(fn_ctx.llvm_fn, "apt_tag_ok");
                    let fail_block = self.context.append_basic_block(fn_ctx.llvm_fn, "apt_fail");
                    let merge_block = self.context.append_basic_block(fn_ctx.llvm_fn, "apt_merge");
                    self.builder.build_conditional_branch(is_arr, tag_ok_block, fail_block).unwrap();
                    let tag_check_end = self.builder.get_insert_block().unwrap();

                    // fail path
                    self.builder.position_at_end(fail_block);
                    self.builder.build_unconditional_branch(merge_block).unwrap();

                    // tag ok path: unbox and check length
                    self.builder.position_at_end(tag_ok_block);
                    let unboxed = self.builder.build_call(self.rt_unbox_ptr, &[scrut_val.into()], "arrptr")
                        .unwrap().try_as_basic_value().unwrap_basic().into_pointer_value();
                    let len_fn = self.get_or_declare_fn("lin_array_length",
                        i64_ty.fn_type(&[ptr_ty.into()], false));
                    let arr_len = self.builder.build_call(len_fn, &[unboxed.into()], "arrlen")
                        .unwrap().try_as_basic_value().unwrap_basic().into_int_value();
                    let needed = i64_ty.const_int(elements.len() as u64, false);
                    let len_ok = if rest.is_some() {
                        self.builder.build_int_compare(inkwell::IntPredicate::SGE, arr_len, needed, "len_ok").unwrap()
                    } else {
                        self.builder.build_int_compare(inkwell::IntPredicate::EQ, arr_len, needed, "len_ok").unwrap()
                    };
                    let bind_block = self.context.append_basic_block(fn_ctx.llvm_fn, "apt_bind");
                    let len_fail_block = self.context.append_basic_block(fn_ctx.llvm_fn, "apt_len_fail");
                    self.builder.build_conditional_branch(len_ok, bind_block, len_fail_block).unwrap();
                    let len_check_end = self.builder.get_insert_block().unwrap();

                    // length fail path
                    self.builder.position_at_end(len_fail_block);
                    self.builder.build_unconditional_branch(merge_block).unwrap();

                    // bind path
                    self.builder.position_at_end(bind_block);
                    let get_tagged_fn = self.get_or_declare_fn("lin_array_get_tagged",
                        ptr_ty.fn_type(&[ptr_ty.into(), i64_ty.into()], false));
                    for (i, elem_pat) in elements.iter().enumerate() {
                        let idx = i64_ty.const_int(i as u64, false);
                        let tv_ptr = self.builder.build_call(get_tagged_fn, &[unboxed.into(), idx.into()],
                            &format!("elem{}", i))
                            .unwrap().try_as_basic_value().unwrap_basic();
                        if let TypedPattern::Binding(slot, _, _) = elem_pat {
                            fn_ctx.slots.insert(*slot, SlotStorage::Value(tv_ptr));
                            fn_ctx.pointer_slots.insert(*slot);
                        }
                    }
                    if let Some(rest_slot) = rest {
                        let slice_fn = self.get_or_declare_fn("lin_array_slice_tagged",
                            ptr_ty.fn_type(&[ptr_ty.into(), i64_ty.into(), i64_ty.into()], false));
                        let start = i64_ty.const_int(elements.len() as u64, false);
                        let rest_arr = self.builder.build_call(slice_fn,
                            &[unboxed.into(), start.into(), arr_len.into()], "rest_arr")
                            .unwrap().try_as_basic_value().unwrap_basic();
                        fn_ctx.slots.insert(*rest_slot, SlotStorage::Value(rest_arr));
                    }
                    self.builder.build_unconditional_branch(merge_block).unwrap();
                    let bind_end = self.builder.get_insert_block().unwrap();

                    self.builder.position_at_end(merge_block);
                    let phi = self.builder.build_phi(bool_ty, "arr_match").unwrap();
                    phi.add_incoming(&[
                        (&true_val, bind_end),
                        (&false_val, fail_block),
                        (&false_val, len_fail_block),
                    ]);
                    let _ = tag_check_end;
                    let _ = len_check_end;
                    return phi.as_basic_value().into_int_value();
                } else if scrut_val.is_pointer_value() {
                    scrut_val.into_pointer_value()
                } else {
                    return false_val;
                };

                // Non-union path: arr_ptr is directly a LinArray*.
                let len_fn = self.get_or_declare_fn("lin_array_length",
                    i64_ty.fn_type(&[ptr_ty.into()], false));
                let arr_len = self.builder.build_call(len_fn, &[arr_ptr.into()], "arrlen")
                    .unwrap().try_as_basic_value().unwrap_basic().into_int_value();
                let needed = i64_ty.const_int(elements.len() as u64, false);
                let len_ok = if rest.is_some() {
                    self.builder.build_int_compare(inkwell::IntPredicate::SGE, arr_len, needed, "len_ok").unwrap()
                } else {
                    self.builder.build_int_compare(inkwell::IntPredicate::EQ, arr_len, needed, "len_ok").unwrap()
                };
                let get_tagged_fn = self.get_or_declare_fn("lin_array_get_tagged",
                    ptr_ty.fn_type(&[ptr_ty.into(), i64_ty.into()], false));
                for (i, elem_pat) in elements.iter().enumerate() {
                    let idx = i64_ty.const_int(i as u64, false);
                    let tv_ptr = self.builder.build_call(get_tagged_fn, &[arr_ptr.into(), idx.into()],
                        &format!("elem{}", i))
                        .unwrap().try_as_basic_value().unwrap_basic();
                    if let TypedPattern::Binding(slot, _, _) = elem_pat {
                        fn_ctx.slots.insert(*slot, SlotStorage::Value(tv_ptr));
                        fn_ctx.pointer_slots.insert(*slot);
                    }
                }
                if let Some(rest_slot) = rest {
                    let slice_fn = self.get_or_declare_fn("lin_array_slice_tagged",
                        ptr_ty.fn_type(&[ptr_ty.into(), i64_ty.into(), i64_ty.into()], false));
                    let start = i64_ty.const_int(elements.len() as u64, false);
                    let rest_arr = self.builder.build_call(slice_fn,
                        &[arr_ptr.into(), start.into(), arr_len.into()], "rest_arr")
                        .unwrap().try_as_basic_value().unwrap_basic();
                    fn_ctx.slots.insert(*rest_slot, SlotStorage::Value(rest_arr));
                }
                len_ok
            }
        }
    }

    // =========================================================================
    // LinIR-consuming codegen (Phase 3)
    // =========================================================================

    /// Compile a `LinModule` (produced by `lin_ir::lower_module` + `elide_rc`) to LLVM IR.
    /// This is the LinIR pipeline path, gated behind `LIN_USE_IR=1`.
    pub fn compile_module_from_ir(&mut self, module: &lir::LinModule) {
        use lir::{Instruction, Const, CallTarget, Terminator};
        use std::collections::HashMap as StdMap;

        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i32_ty = self.context.i32_type();
        let i64_ty = self.context.i64_type();
        let void_ty = self.context.void_type();

        // ---- Pass 1: pre-declare all LLVM functions (so cross-calls work) ----
        let mut ir_fn_to_llvm: StdMap<lir::FuncId, FunctionValue<'ctx>> = StdMap::new();
        for func in &module.functions {
            // Build LLVM function type from params/ret.
            let ret_ty = &func.ret_ty;
            let mut param_types: Vec<BasicMetadataTypeEnum> = Vec::new();
            for (_, ty) in &func.params {
                param_types.push(self.llvm_param_type(ty));
            }
            let name = if func.name.as_deref() == Some("main") || func.name.is_none() {
                if func.id == lir::FuncId(0) { "main".to_string() }
                else { format!("__lin_fn_{}", func.id.0) }
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
        }

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

            // Compile each block
            for block in &func.blocks {
                let bb = ir_block_to_llvm[&block.id];
                self.builder.position_at_end(bb);

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
                            let lv = temp_map.get(lhs).copied().unwrap_or_else(|| ptr_ty.const_null().into());
                            let rv = temp_map.get(rhs).copied().unwrap_or_else(|| ptr_ty.const_null().into());
                            let result = self.compile_binary_op_values(lv, rv, op, operand_ty, ty);
                            temp_map.insert(*dst, result);
                        }
                        Instruction::Retain { val, .. } => {
                            if let Some(&v) = temp_map.get(val) {
                                if v.is_pointer_value() {
                                    self.builder.build_call(self.rt_rc_retain, &[v.into()], "").unwrap();
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
                                    if let Some(callee_fn) = self.module.get_function(name) {
                                        if let Some(p) = partial_app(self, callee_fn) { p }
                                        else {
                                            let call = self.builder.build_call(callee_fn, &arg_vals, "call_n").unwrap();
                                            if matches!(ret_ty, Type::Null | Type::Never) { ptr_ty.const_null().into() }
                                            else { call.try_as_basic_value().unwrap_basic() }
                                        }
                                    } else { ptr_ty.const_null().into() }
                                }
                                CallTarget::Indirect(fn_temp) => {
                                    if let Some(&cls_ptr) = temp_map.get(fn_temp) {
                                        if cls_ptr.is_pointer_value() {
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
                                            let fn_ty = if matches!(ret_ty, Type::Null | Type::Never) {
                                                void_ty.fn_type(&fn_param_types, false)
                                            } else {
                                                self.llvm_type(ret_ty).fn_type(&fn_param_types, false)
                                            };
                                            let call = self.builder.build_indirect_call(fn_ty, fn_ptr, &call_args, "ir_ind").unwrap();
                                            if matches!(ret_ty, Type::Null | Type::Never) { ptr_ty.const_null().into() }
                                            else { call.try_as_basic_value().unwrap_basic() }
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
                            // Apply spreads.
                            if !spreads.is_empty() {
                                let merge_fn = self.get_or_declare_fn("lin_object_merge",
                                    void_ty.fn_type(&[ptr_ty.into(), ptr_ty.into()], false));
                                for s in spreads {
                                    if let Some(&sv) = temp_map.get(s) {
                                        if sv.is_pointer_value() {
                                            self.builder.build_call(merge_fn, &[obj_ptr.into(), sv.into()], "").unwrap();
                                        }
                                    }
                                }
                            }
                            for (key, val_temp) in fields {
                                if let Some(&val) = temp_map.get(val_temp) {
                                    let key_str = self.compile_string_lit(key).into_pointer_value();
                                    let val_ty = func.temp_types.get(val_temp).cloned().unwrap_or(Type::Null);
                                    let tagged = self.build_tagged_val_alloca(&val, &val_ty);
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
                                    // but closure call sites invoke fn_ptr(env, args...). Wrap it
                                    // in an env-ignoring stub so the closure ABI lines up.
                                    self.wrap_named_fn_as_closure(callee_fn, &Type::Null)
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
                        let cond_val = temp_map.get(cond).copied().unwrap_or_else(|| self.context.bool_type().const_zero().into());
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
                    if let (Some(&v), Some(&pred_bb)) =
                        (temp_map.get(val_temp), ir_block_to_llvm.get(pred_block))
                    {
                        phi.add_incoming(&[(&v, pred_bb)]);
                    }
                }
            }
        }
    }

    fn compile_ir_intrinsic(&mut self, intrinsic: &lir::Intrinsic, args: &[BasicValueEnum<'ctx>], arg_tys: &[Type], ret_ty: &Type) -> BasicValueEnum<'ctx> {
        use lir::Intrinsic;
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        match intrinsic {
            Intrinsic::Print => {
                if let Some(&s) = args.first() {
                    self.builder.build_call(self.rt_print, &[s.into()], "").unwrap();
                }
                ptr_ty.const_null().into()
            }
            Intrinsic::ToString => {
                let arg = args.first().copied().unwrap_or_else(|| ptr_ty.const_null().into());
                let in_ty = arg_tys.first().cloned().unwrap_or(Type::Null);
                self.compile_to_string_value(arg, &in_ty)
            }
            Intrinsic::Length => {
                if let Some(&arr) = args.first() {
                    let i64_ty = self.context.i64_type();
                    let len_fn = self.get_or_declare_fn("lin_array_length",
                        i64_ty.fn_type(&[ptr_ty.into()], false));
                    self.builder.build_call(len_fn, &[arr.into()], "ir_len")
                        .unwrap().try_as_basic_value().unwrap_basic()
                } else { self.context.i64_type().const_zero().into() }
            }
            Intrinsic::Push => {
                if args.len() >= 2 {
                    let arr = args[0];
                    let elem = args[1];
                    // The element's static type (arg 1) decides flat-vs-tagged storage; the
                    // arrays we push into here (map/filter output, user push) are tagged.
                    let elem_ty = arg_tys.get(1).cloned().unwrap_or_else(|| Type::TypeVar(u32::MAX));
                    self.tagged_array_push_value(arr, elem, &elem_ty);
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
                    let elem_ty = match kind {
                        lir::FlatElemKind::I32 => Type::Int32,
                        lir::FlatElemKind::I64 => Type::Int64,
                        lir::FlatElemKind::F32 => Type::Float32,
                        lir::FlatElemKind::F64 => Type::Float64,
                    };
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
                // Json result), then wrap in a LinPromise*.
                let thunk = args.last().copied().unwrap_or_else(|| ptr_ty.const_null().into());
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
            _ => ptr_ty.const_null().into(),
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
        // Array indexing when the object is an array type or the key is a raw integer.
        let is_array_access = matches!(obj_ty, Type::Array(_) | Type::FixedArray(_))
            || (key.is_int_value() && key.get_type() == self.context.i64_type().into());
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
        if !val.is_pointer_value() { return self.context.bool_type().const_zero(); }
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i8_ty = self.context.i8_type();
        let obj_has_fn = self.get_or_declare_fn("lin_object_has",
            i8_ty.fn_type(&[ptr_ty.into(), ptr_ty.into()], false));
        let mut result = self.context.bool_type().const_int(1, false);
        for field in &pattern.required_fields {
            let key_str = self.compile_string_lit(field).into_pointer_value();
            let has_i8 = self.builder.build_call(obj_has_fn, &[val.into(), key_str.into()], "ir_has")
                .unwrap().try_as_basic_value().unwrap_basic().into_int_value();
            self.builder.build_call(self.rt_string_release, &[key_str.into()], "").unwrap();
            let has_bool = self.builder.build_int_truncate_or_bit_cast(has_i8, self.context.bool_type(), "has_b").unwrap();
            result = self.builder.build_and(result, has_bool, "has_acc").unwrap();
        }
        result
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

    /// Compile a binary operation given already-compiled LLVM values (used by LinIR path).
    fn compile_binary_op_values(
        &mut self,
        lv: BasicValueEnum<'ctx>,
        rv: BasicValueEnum<'ctx>,
        op: &BinOp,
        lty: &Type,
        result_ty: &Type,
    ) -> BasicValueEnum<'ctx> {
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
                    let rv_tagged = if rv.is_pointer_value() { rv } else { self.box_value(rv, result_ty) };
                    let eq_u8 = self.builder.build_call(eq_fn, &[lv.into(), rv_tagged.into()], "ir_teq")
                        .unwrap().try_as_basic_value().unwrap_basic().into_int_value();
                    let eq = self.builder.build_int_truncate(eq_u8, self.context.bool_type(), "ir_teq_b").unwrap();
                    return if matches!(op, BinOp::NotEq) {
                        self.builder.build_not(eq, "ir_tne").unwrap().into()
                    } else { eq.into() };
                }
                BinOp::Lt | BinOp::LtEq | BinOp::Gt | BinOp::GtEq => {
                    // Unbox both operands to the numeric type of the other side / Int32, then compare.
                    let lconc = self.unbox_tagged_val_to_type(lv, &Type::Int32);
                    let rconc = if rv.is_pointer_value() { self.unbox_tagged_val_to_type(rv, &Type::Int32) } else { rv };
                    return self.compile_binary_op_values(lconc, rconc, op, &Type::Int32, result_ty);
                }
                BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod => {
                    let lconc = self.unbox_tagged_val_to_type(lv, &Type::Int32);
                    let rconc = if rv.is_pointer_value() { self.unbox_tagged_val_to_type(rv, &Type::Int32) } else { rv };
                    let concrete = self.compile_binary_op_values(lconc, rconc, op, &Type::Int32, &Type::Int32);
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
