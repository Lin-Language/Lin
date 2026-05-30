use inkwell::builder::Builder;
use inkwell::context::Context;
use inkwell::module::Module;
use inkwell::passes::PassBuilderOptions;
use inkwell::targets::{
    CodeModel, FileType, InitializationConfig, RelocMode, Target, TargetMachine,
};
use inkwell::types::{BasicMetadataTypeEnum, BasicType};
use inkwell::values::{
    BasicMetadataValueEnum, BasicValueEnum, FunctionValue, PointerValue,
};
use inkwell::{AddressSpace, OptimizationLevel};
use std::collections::HashMap;
use std::path::Path;

use lin_check::typed_ir::*;
use lin_check::types::Type;
use lin_ir::ir as lir;
use crate::coverage::{self, CoverageEmitter};
use runtime::RuntimeFns;

mod runtime;
mod types;
mod boxing;
mod literals;
mod arith;
mod call;
mod data;
mod intrinsics;
mod rc;
mod r#match;

pub struct Codegen<'ctx> {
    context: &'ctx Context,
    module: Module<'ctx>,
    builder: Builder<'ctx>,
    /// Process-wide `lin-runtime` C-ABI function declarations (see `runtime.rs`).
    rt: RuntimeFns<'ctx>,
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

        // Declare runtime functions (C ABI, defined in lin-runtime).
        let rt = RuntimeFns::new(context, &module);

        Self {
            context,
            module,
            builder,
            rt,
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




    // -------------------------------------------------------------------------
    // Variables
    // -------------------------------------------------------------------------



    // -------------------------------------------------------------------------
    // Binary operators
    // -------------------------------------------------------------------------









    // -------------------------------------------------------------------------
    // Numeric coercions (widening / narrowing)
    // -------------------------------------------------------------------------


    // -------------------------------------------------------------------------
    // Function calls
    // -------------------------------------------------------------------------













    // -------------------------------------------------------------------------
    // Intrinsic calls (runtime functions with known ABI)
    // -------------------------------------------------------------------------




    pub(crate) fn get_or_declare_fn(&self, name: &str, fn_type: inkwell::types::FunctionType<'ctx>) -> FunctionValue<'ctx> {
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







    // -------------------------------------------------------------------------
    // Arrays
    // -------------------------------------------------------------------------










    // -------------------------------------------------------------------------
    // Iteration
    // -------------------------------------------------------------------------













    // -------------------------------------------------------------------------
    // Index assignment (obj[key] = val, arr[i] = val)
    // -------------------------------------------------------------------------


    // -------------------------------------------------------------------------
    // Objects
    // -------------------------------------------------------------------------





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
                                        self.builder.build_call(self.rt.rc_retain, &[v.into()], "").unwrap();
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
                                                self.builder.build_call(self.rt.unbox_ptr, &[cls_ptr.into()], "ir_fn_unbox")
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
                            let obj_ptr = self.builder.build_call(self.rt.object_alloc, &[cap.into()], "ir_obj")
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
                                                self.builder.build_call(self.rt.unbox_ptr, &[sv.into()], "ir_spread_unbox")
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
                                    self.builder.build_call(self.rt.object_set, &[obj_ptr.into(), key_str.into(), tagged.into()], "").unwrap();
                                    self.builder.build_call(self.rt.string_release, &[key_str.into()], "").unwrap();
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
                                let arr_v = self.builder.build_call(self.rt.array_alloc, &[cap.into()], "ir_arr")
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
                                    self.builder.build_call(self.rt.unbox_ptr, &[src_v.into()], "orest_unbox")
                                        .unwrap().try_as_basic_value().unwrap_basic()
                                } else { src_v };
                                let rest_obj = self.builder.build_call(self.rt.object_alloc,
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
                                let boxed = self.builder.build_call(self.rt.box_object, &[rest_obj.into()], "orest_boxed")
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
                                let cell = self.builder.build_call(self.rt.alloc, &[size_i64.into()], "ir_cell")
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
                                    self.builder.build_call(self.rt.panic, &[msg_v.into(), zero.into(), zero.into()], "").unwrap();
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























}
