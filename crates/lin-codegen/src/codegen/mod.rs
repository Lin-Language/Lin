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
use inkwell::attributes::AttributeLoc;
use inkwell::{AddressSpace, OptimizationLevel};
use std::collections::HashMap;
use std::path::Path;

use lin_check::typed_ir::*;
use lin_check::types::Type;
use lin_ir::ir as lir;
use crate::coverage::{self, CoverageEmitter};
use runtime::RuntimeFns;
use builder_ext::BuilderExt;

mod builder_ext;
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
    /// Coverage: import module path (as the IR/monomorphizer names it, e.g. `./gen`) → its source
    /// file index in the coverage emitter. Populated when an instrumented import is compiled. Used
    /// to attribute a cross-module monomorphized specialization's regions (whose block spans index
    /// into the generic-definition source) to that origin file via `LinFunction.coverage_origin`.
    cov_import_file_idx: HashMap<String, u32>,
    /// Default-argument descriptor global per real FuncId, for the module currently being
    /// compiled. A closure value created from a default-bearing function points at this
    /// descriptor (closure offset 32) so an indirect under-arity call dispatches to the
    /// correct default-fill adapter. Repopulated per `compile_module_from_ir`.
    cls_descriptors: HashMap<lir::FuncId, inkwell::values::PointerValue<'ctx>>,
    /// True if the whole program may spawn an async boundary (it references any of the
    /// concurrency intrinsics). When set, user-emitted Lin functions are NOT marked
    /// `nounwind`: a runtime fault inside an async thunk unwinds through Lin frames to the
    /// thread boundary's `catch_unwind` (spec §24.2.2), so `nounwind` would be unsound on
    /// any function reachable from a thunk — and any function can be (ADR-027, doc §2.4.3
    /// option a). Conservatively program-wide; the non-async hot path keeps `nounwind`.
    uses_async: bool,
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
            uses_async: false,
            coverage: if coverage_enabled {
                // Source path is set by set_main_source; start with empty path.
                Some(CoverageEmitter::new(String::new()))
            } else {
                None
            },
            current_source: None,
            cls_descriptors: HashMap::new(),
            cov_import_file_idx: HashMap::new(),
        }
    }

    /// Attach a set of named enum function-level attributes to `fn_value`.
    ///
    /// Only attributes that are sound for *user-emitted Lin functions* should be
    /// passed here. Lin uses value-based error handling, so user functions never
    /// unwind — `nounwind` is safe. We deliberately do NOT mark runtime (`lin_*`)
    /// `extern "C"` declarations `nounwind`, because the Rust runtime is built with
    /// the default `panic = "unwind"`; a panic crossing a `nounwind` boundary is UB.
    pub(crate) fn add_fn_attrs(&self, fn_value: FunctionValue<'ctx>, names: &[&str]) {
        for name in names {
            let kind_id = inkwell::attributes::Attribute::get_named_enum_kind_id(name);
            // get_named_enum_kind_id returns 0 for an unknown attribute name; skip those
            // rather than create an invalid (string-less) attribute.
            if kind_id == 0 {
                continue;
            }
            let attr = self.context.create_enum_attribute(kind_id, 0);
            fn_value.add_attribute(AttributeLoc::Function, attr);
        }
    }

    /// Mark `f` `nounwind` UNLESS the program uses async. When async is in play a runtime
    /// fault inside a thunk unwinds through Lin frames to the thread boundary (spec §24.2.2),
    /// so `nounwind` would be unsound on any reachable function — and we can't cheaply prove a
    /// given function is unreachable from a thunk, so we conservatively drop it program-wide.
    /// The common non-async program keeps the attribute (and its optimisation value).
    pub(crate) fn mark_user_fn_nounwind(&self, f: FunctionValue<'ctx>) {
        if !self.uses_async {
            self.add_fn_attrs(f, &["nounwind"]);
        } else {
            // Async program: a thunk fault unwinds through Lin frames to the thread boundary.
            // The frame must therefore emit an unwind table (`uwtable`) so the unwinder can
            // walk through it; without it a plain `call` to a faulting runtime fn that unwinds
            // is treated as a non-unwinding panic and aborts the process.
            self.add_fn_attrs(f, &["uwtable"]);
        }
    }

    /// Set by the driver before any module is compiled, once it has scanned the whole program
    /// (main + all imports) for any concurrency intrinsic. See `uses_async`.
    pub fn set_uses_async(&mut self, v: bool) {
        self.uses_async = v;
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
        imports: &HashMap<String, TypedModule>,
        // ADR-046: export names of THIS module that a test `replace` overrides. Their bodies are
        // not emitted here; the main module supplies the canonical symbol instead.
        replaced_exports: &std::collections::HashSet<String>,
    ) {
        // Merge the imported module's intrinsic slot map (same as register_import) so the
        // importer's lowering still recognises re-exported intrinsics.
        for (slot, name) in &module.intrinsics {
            self.intrinsic_slots.insert(*slot, name.clone());
        }

        let module_key = lin_ir::mangle_module_key(path);
        // Pass the program's imports so this module's OWN cross-module generic calls (e.g.
        // `examples/report` → `std/array.reduce`) are specialized here, not left as a boxed
        // type-erased call that crashes a concrete use site.
        let mut ir_module =
            lin_ir::lower_import_module_with_imports(module, &module_key, imports, replaced_exports);
        // Representation-inference pass (repr.rs) — STAGE 3; runs before rc_elide on the same IR
        // shape as the main module. Stores the per-temp repr table on each `func.repr` for codegen
        // to consume at DECIDE / ASSUME sites, and (debug builds) asserts the oracle + verifier.
        lin_ir::repr::run(&mut ir_module);
        lin_ir::rc_elide::elide_rc(&mut ir_module);
        // Sealed-records Stage 4: stack-allocate non-escaping all-scalar sealed records and suppress
        // their Retain/Release emission (imports get the same analysis as the main module).
        lin_ir::escape::analyze(&mut ir_module);
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
                    // Record import-path → file index so a later cross-module specialization of THIS
                    // module's generics (compiled in the importer's context) can attribute its
                    // coverage regions back to this source file.
                    self.cov_import_file_idx.insert(path.to_string(), idx);
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
                // ADR-046: a replaced export's symbol is defined by the main module, not here;
                // it's registered when the main module compiles. Skip it.
                if replaced_exports.contains(name) {
                    continue;
                }
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
                RelocMode::PIC,
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
                RelocMode::PIC,
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

    /// Create the `_ir_gv_{slot}` LLVM global backing a top-level `val`/`var` slot.
    ///
    /// These globals are inherently translation-unit-local: nothing references them by name
    /// across modules. Cross-module reads of an exported `val` go through a `__val` wrapper
    /// FUNCTION (lower.rs), and an imported `var` is only read inside its own module's exported
    /// functions — never by the importer directly. Even though all modules share one LLVM module
    /// (and `_ir_gv_{slot}` names aren't module-prefixed, so two modules can both define slot N),
    /// each `compile_module_from_ir` call keeps its OWN `ir_global_vals` handle map and accesses
    /// the global by handle; LLVM auto-disambiguates the colliding names. So the slots are private
    /// to the defining TU regardless of linkage.
    ///
    /// `immutable` (a top-level `val`, single-store) → `Internal` linkage. Internal linkage lets
    /// LLVM GlobalOpt prove a single-store-of-a-constant global is itself constant and propagate
    /// it into readers (e.g. a literal divisor `val MOD = 2147483647i64` folds from a per-iteration
    /// `idiv` to a magic multiply-shift). A non-`immutable` top-level `var` keeps the previous
    /// (external/default) linkage: it is genuine mutable shared state, GlobalOpt would not fold a
    /// multi-store global anyway, and (crucially) the once-guarded var-init flag must NOT be folded
    /// to a constant or initialisers would never run. We therefore intern ONLY immutable vals.
    fn add_module_global(
        module: &inkwell::module::Module<'ctx>,
        llvm_ty: inkwell::types::BasicTypeEnum<'ctx>,
        slot: usize,
        immutable: bool,
    ) -> inkwell::values::GlobalValue<'ctx> {
        let g = module.add_global(llvm_ty, None, &format!("_ir_gv_{}", slot));
        g.set_initializer(&llvm_ty.const_zero());
        if immutable {
            g.set_linkage(inkwell::module::Linkage::Internal);
        }
        g
    }

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
                else {
                    let f = self.module.add_function(&name, fn_ty, None);
                    // User-emitted Lin functions use value-based errors and never
                    // unwind, so `nounwind` is sound — EXCEPT when the program uses async,
                    // where a thunk fault unwinds through Lin frames (see
                    // `mark_user_fn_nounwind`). (Runtime `lin_*` decls are not marked — the
                    // Rust runtime is `panic = "unwind"`.)
                    self.mark_user_fn_nounwind(f);
                    f
                }
            } else {
                let ret_llvm = self.llvm_type(ret_ty);
                let fn_ty = ret_llvm.fn_type(&param_types, false);
                if let Some(existing) = self.module.get_function(&name) { existing }
                else {
                    let f = self.module.add_function(&name, fn_ty, None);
                    self.mark_user_fn_nounwind(f);
                    f
                }
            };
            self.named_fns.insert(name.clone(), llvm_fn);
            ir_fn_to_llvm.insert(func.id, llvm_fn);
            ir_fn_symbol.insert(func.id, name.clone());
        }

        // ---- Pass 1b: build default-argument descriptor globals ----
        // For each function with optional parameters, emit a static descriptor
        //   { i32 total, i32 required, [ptr; n] entries }
        // whose entries are boxed-ABI wrappers (env_ptr, args...) -> ptr of each arity's
        // entry function (adapters + the real fn). A closure value made from this function
        // points at the descriptor (closure offset 32) so an indirect under-arity call
        // dispatches to the right adapter. Cleared per module.
        self.cls_descriptors.clear();
        {
            let ptr_ty = self.context.ptr_type(AddressSpace::default());
            let i32_ty = self.context.i32_type();
            for (real_fid, desc) in &module.default_descriptors {
                // The real function's declared Lin return type — used so each entry wrapper
                // boxes a raw Str/Array/Object return (otherwise the indirect caller unboxes a
                // raw pointer). All entries share the real function's return type.
                let real_ret_ty = module.function(*real_fid).map(|f| f.ret_ty.clone());
                let entry_ptrs: Vec<inkwell::values::BasicValueEnum<'ctx>> = desc.entries
                    .iter()
                    .filter_map(|fid| ir_fn_to_llvm.get(fid).copied().map(|f| (*fid, f)))
                    .map(|(fid, f)| {
                        // Each entry has its own arity/param types (the default-fill adapters take
                        // fewer params than the real fn). The boxed closure ABI passes every arg
                        // boxed, so the wrapper must unbox each to that entry's concrete param type.
                        let entry_param_tys: Option<Vec<Type>> = module
                            .function(fid)
                            .map(|ef| ef.params.iter().map(|(_, t)| t.clone()).collect());
                        self.boxed_abi_wrapper_ret(f, real_ret_ty.as_ref(), entry_param_tys.as_deref())
                            .as_global_value().as_pointer_value().into()
                    })
                    .collect();
                if entry_ptrs.len() != desc.entries.len() { continue; }
                let entries_arr = ptr_ty.const_array(
                    &entry_ptrs.iter().map(|v| v.into_pointer_value()).collect::<Vec<_>>()
                );
                let desc_struct_ty = self.context.struct_type(
                    &[i32_ty.into(), i32_ty.into(), ptr_ty.array_type(desc.entries.len() as u32).into()],
                    false,
                );
                let desc_val = self.context.const_struct(
                    &[
                        i32_ty.const_int(desc.total as u64, false).into(),
                        i32_ty.const_int(desc.required as u64, false).into(),
                        entries_arr.into(),
                    ],
                    false,
                );
                let g = self.module.add_global(desc_struct_ty, None, &format!("{}__lin_desc_{}", self.ir_anon_prefix, real_fid.0));
                g.set_constant(true);
                g.set_initializer(&desc_val);
                self.cls_descriptors.insert(*real_fid, g.as_pointer_value());
            }
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
            // Per owned param: a bool slot tracking whether the CURRENT value in `param_allocs[i]`
            // is owned by the TCO loop (i.e. it was produced and stored by a prior tail iteration)
            // rather than borrowed from the caller. Params are BORROWED under Lin's calling
            // convention (the lowerer never releases them — see lin_ir::lower `free_arg_box_shells`
            // doc), so the original ENTRY value must NOT be released here (the caller owns and frees
            // it; doing so is a use-after-free at the caller). Only values the loop itself stored on
            // a back-edge are loop-owned and must be released before the next overwrite. We start
            // each flag at 0 (entry value = borrowed) and set it to 1 after the first back-edge store.
            let mut tco_owns: Vec<Option<PointerValue<'ctx>>> = Vec::new();
            if has_tail_call {
                // Emit allocas + initial stores in a dedicated entry block that branches to
                // the first IR block (which becomes the loop header).
                let tco_entry = self.context.append_basic_block(llvm_fn, "tco_entry");
                // Move the new entry before the first IR block so it is the function entry.
                if let Some(first_ir_bb) = func.blocks.first().and_then(|b| ir_block_to_llvm.get(&b.id)) {
                    tco_entry.move_before(*first_ir_bb).ok();
                }
                self.builder.position_at_end(tco_entry);
                let bool_ty = self.context.bool_type();
                for (i, (_temp, ty)) in func.params.iter().enumerate() {
                    let llvm_ty = self.llvm_type(ty);
                    let slot = self.builder.alloca(llvm_ty, "tco_param");
                    if let Some(pv) = llvm_fn.get_nth_param(i as u32) {
                        self.builder.store(slot, pv);
                    }
                    param_allocs.push(slot);
                    // Only owned/refcounted (and non-sealed) params can leak / need release tracking.
                    if Self::tco_param_needs_release(ty) {
                        let owns = self.builder.alloca(bool_ty, "tco_owns");
                        self.builder.store(owns, bool_ty.const_zero());
                        tco_owns.push(Some(owns));
                    } else {
                        tco_owns.push(None);
                    }
                }
                if let Some(first_ir_bb) = func.blocks.first().and_then(|b| ir_block_to_llvm.get(&b.id)) {
                    self.builder.unconditional_branch(*first_ir_bb);
                }
            }

            // Pre-load params into temp_map. With TCO, params are loaded from their allocas
            // at the top of the loop-header block so each iteration sees the updated values.
            if has_tail_call {
                if let Some(first_ir_bb) = func.blocks.first().and_then(|b| ir_block_to_llvm.get(&b.id)) {
                    self.builder.position_at_end(*first_ir_bb);
                    for (i, (temp, ty)) in func.params.iter().enumerate() {
                        let llvm_ty = self.llvm_type(ty);
                        let loaded = self.builder.load(llvm_ty, param_allocs[i], "tco_pload");
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
                // A kept GENERIC ORIGINAL (`<T>(x:T):T`, signature still mentions a TypeVar) is a
                // type-erased shadow of the same source lines its monomorphized specializations
                // cover, and in the fully-specialized common case it is never called. Emitting its
                // (always-zero) regions would double-count the generic definition's lines and force
                // them to 0%. Skip it; the specialization (attributed via `coverage_origin`) carries
                // the real, executed coverage for those lines.
                let is_generic_original = func.coverage_origin.is_none()
                    && (type_mentions_typevar(&func.ret_ty)
                        || func.params.iter().any(|(_, t)| type_mentions_typevar(t)));
                // A cross-module specialization's block spans index into its ORIGIN module's source,
                // not the module currently being compiled. Attribute its regions to that file.
                let origin_file_idx = func
                    .coverage_origin
                    .as_ref()
                    .and_then(|p| self.cov_import_file_idx.get(p).copied());
                let region_file = origin_file_idx.or_else(|| self.current_source.as_ref().map(|(i, _)| *i));
                if let (false, Some(file_idx)) = (is_generic_original, region_file) {
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
                        self.builder.in_bounds_gep(
                            counter_arr_ty,
                            profc.as_pointer_value(),
                            &[i64_ty.const_zero(), i64_ty.const_int(k as u64, false)],
                            "covctr_ptr",
                        )
                    };
                    let cur = self.builder.load(i64_ty, gep, "covctr").into_int_value();
                    let inc = self.builder.int_add(cur, i64_ty.const_int(1, false), "covctr_inc");
                    self.builder.store(gep, inc);
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
                            let phi = self.builder.phi(phi_ty, "ir_phi");
                            temp_map.insert(*dst, phi.as_basic_value());
                            pending_phis.push((phi, incomings.clone()));
                        }
                        Instruction::Binary { dst, op, lhs, rhs, operand_ty, ty } => {
                            // A missing operand temp means malformed IR (an undefined SSA temp) —
                            // the old null-pointer fallback silently miscompiled to garbage
                            // arithmetic. Fail loudly with the offending temp instead.
                            let mut lv = *temp_map.get(lhs).unwrap_or_else(|| panic!("Binary: undefined lhs temp {lhs:?}"));
                            let mut rv = *temp_map.get(rhs).unwrap_or_else(|| panic!("Binary: undefined rhs temp {rhs:?}"));
                            let rty = func.temp_types.get(rhs).cloned().unwrap_or(Type::Null);
                            // UNBOXED SUM TYPE (unboxed-sumtype Stage 1): `==`/`!=` over SumNode
                            // operands MATERIALIZES each to a boxed `LinObject` (order-independent
                            // structural object equality via `lin_tagged_eq`), matching the boxed
                            // golden semantics. A raw SumNode-pointer compare would test identity, not
                            // value. Other ops never apply to a whole sum value (checker-rejected).
                            if matches!(op, lin_parse::ast::BinOp::Eq | lin_parse::ast::BinOp::NotEq) {
                                let lrepr = func.repr_of(*lhs);
                                let rrepr = func.repr_of(*rhs);
                                if let Some(sum_ty) = lrepr.sumnode_sum_ty() {
                                    let sum_ty = sum_ty.clone();
                                    let obj = self.sumnode_materialize_to_object(lv, &sum_ty, llvm_fn);
                                    lv = self.box_value(obj, &Self::sumnode_first_variant_obj_ty(&sum_ty));
                                }
                                if let Some(sum_ty) = rrepr.sumnode_sum_ty() {
                                    let sum_ty = sum_ty.clone();
                                    let obj = self.sumnode_materialize_to_object(rv, &sum_ty, llvm_fn);
                                    rv = self.box_value(obj, &Self::sumnode_first_variant_obj_ty(&sum_ty));
                                }
                            }
                            let result = self.compile_binary_op_values(lv, rv, op, operand_ty, &rty, ty);
                            temp_map.insert(*dst, result);
                        }
                        Instruction::Retain { val, ty } => {
                            if let Some(&v) = temp_map.get(val) {
                                if v.is_pointer_value() {
                                    // UNBOXED SUM TYPE: a SumNode's refcount is the offset-0 u32
                                    // (lin_rc_retain) — NOT a tagged inner-payload retain (which would
                                    // corrupt the header). Read the proven repr.
                                    if func.repr_of(*val).sumnode_sum_ty().is_some() {
                                        self.builder.call(self.rt.rc_retain, &[v.into()], "");
                                    } else if Self::is_union_type(ty) {
                                        // A boxed TaggedVal*: bump the INNER payload's rc
                                        // (tag-aware). lin_rc_retain would hit the tag byte at
                                        // offset 0 and corrupt it.
                                        let retain_fn = self.get_or_declare_fn("lin_tagged_retain",
                                            self.context.void_type().fn_type(&[ptr_ty.into()], false));
                                        self.builder.call(retain_fn, &[v.into()], "");
                                    } else {
                                        self.builder.call(self.rt.rc_retain, &[v.into()], "");
                                    }
                                }
                            }
                        }
                        Instruction::Release { val, ty } => {
                            if let Some(&v) = temp_map.get(val) {
                                // PART C: release shape from the pass-proven representation, not Type.
                                let repr = func.repr_of(*val).clone();
                                self.emit_release_repr(v, ty, &repr);
                            }
                        }
                        Instruction::CloneBox { dst, src, ty } => {
                            if let Some(&v) = temp_map.get(src) {
                                // UNBOXED SUM TYPE: a SumNode value (repr Packed(SumNode)) is NOT a
                                // boxed TaggedVal — an "owning read" of one bumps the SumNode's own
                                // refcount (offset-0 u32, via lin_rc_retain) and keeps the SAME
                                // pointer. `lin_tagged_clone` would read offset 0 as a tag byte and
                                // corrupt it (the recursive sum-param crash).
                                if func.repr_of(*src).sumnode_sum_ty().is_some() && v.is_pointer_value() {
                                    self.builder.call(self.rt.rc_retain, &[v.into()], "");
                                    temp_map.insert(*dst, v);
                                    continue;
                                }
                                let cloned = if Self::is_union_type(ty) && v.is_pointer_value() {
                                    // Allocate a fresh, independently-owned box copying the
                                    // tag+payload and retaining the inner heap payload. The
                                    // cell/global (or reader) then owns its own box; releasing
                                    // it never frees a borrowed caller's box.
                                    let clone_fn = self.get_or_declare_fn(
                                        "lin_tagged_clone",
                                        ptr_ty.fn_type(&[ptr_ty.into()], false),
                                    );
                                    self.builder
                                        .call(clone_fn, &[v.into()], "ir_tagged_clone")
                                        .try_as_basic_value()
                                        .unwrap_basic()
                                } else {
                                    // Non-union (concrete rc): a plain retain, value unchanged.
                                    if v.is_pointer_value() {
                                        self.builder.call(self.rt.rc_retain, &[v.into()], "");
                                    }
                                    v
                                };
                                temp_map.insert(*dst, cloned);
                            }
                        }
                        Instruction::FreeBoxShell { val } => {
                            if let Some(&v) = temp_map.get(val) {
                                if v.is_pointer_value() {
                                    let free_fn = self.get_or_declare_fn(
                                        "lin_tagged_free_box",
                                        self.context.void_type().fn_type(&[ptr_ty.into()], false),
                                    );
                                    self.builder.call(free_fn, &[v.into()], "");
                                }
                            }
                        }
                        Instruction::FreeBoxShellIfDistinct { val, other } => {
                            if let Some(&v) = temp_map.get(val) {
                                if v.is_pointer_value() {
                                    match temp_map.get(other) {
                                        // `other` is also a pointer: the call result may ALIAS this
                                        // shell (a callee returning its borrowed Json param), so guard.
                                        Some(&o) if o.is_pointer_value() => {
                                            let free_fn = self.get_or_declare_fn(
                                                "lin_tagged_free_box_if_distinct",
                                                self.context.void_type().fn_type(&[ptr_ty.into(), ptr_ty.into()], false),
                                            );
                                            self.builder.call(free_fn, &[v.into(), o.into()], "");
                                        }
                                        // `other` (the call result) is a SCALAR/Null/non-pointer: it
                                        // can never alias the box shell, so free unconditionally.
                                        // (Previously this silently skipped the free → shell leak when
                                        // a fresh heap literal was boxed into a Json param of a function
                                        // returning a scalar — e.g. `f([1,2,3]): Int32`.)
                                        _ => {
                                            let free_fn = self.get_or_declare_fn(
                                                "lin_tagged_free_box",
                                                self.context.void_type().fn_type(&[ptr_ty.into()], false),
                                            );
                                            self.builder.call(free_fn, &[v.into()], "");
                                        }
                                    }
                                }
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
                                    if let Type::Function { params: remaining, ret: final_ret, .. } = ret_ty {
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
                                        let call = self.builder.call(callee_fn, &arg_vals, "call");
                                        if matches!(ret_ty, Type::Null | Type::Never) { ptr_ty.const_null().into() }
                                        else { call.try_as_basic_value().unwrap_basic() }
                                    }
                                }
                                CallTarget::Named(name) if name == "lin_string_byte_at" && arg_vals.len() == 2 => {
                                    // INLINE the O(1) byte accessor (mirrors flat_array_get, ADR-044):
                                    // lin_string_byte_at is a hot per-byte call in Lin-side string
                                    // scanning. The runtime fn is a non-inlinable staticlib call; emit
                                    // the bounds-checked indexed load inline so LLVM keeps the string
                                    // pointer in a register and hoists the length load out of the loop.
                                    // LinString layout: refcount@0 (u32), len@4 (u32), data@8 ([u8]).
                                    let s = arg_vals[0];
                                    let idx = arg_vals[1];
                                    if s.is_pointer_value() && idx.is_int_value() {
                                        let i8_ty = self.context.i8_type();
                                        let i32_ty = self.context.i32_type();
                                        let i64_ty = self.context.i64_type();
                                        let sp = s.into_pointer_value();
                                        let index = idx.into_int_value();
                                        // len = *(u32*)(s + 4)
                                        let len_p = unsafe { self.builder.gep(i8_ty, sp, &[i64_ty.const_int(4, false)], "sba_len_p") };
                                        let len = self.builder.load(i32_ty, len_p, "sba_len").into_int_value();
                                        // OOB (index < 0 || index >= len) via one unsigned compare on i32.
                                        let oob = self.builder.int_compare(inkwell::IntPredicate::UGE, index, len, "sba_oob");
                                        let llvm_fn = self.builder.get_insert_block().unwrap().get_parent().unwrap();
                                        let oob_b = self.context.append_basic_block(llvm_fn, "sba_oob");
                                        let ok_b = self.context.append_basic_block(llvm_fn, "sba_ok");
                                        let mrg = self.context.append_basic_block(llvm_fn, "sba_mrg");
                                        self.builder.conditional_branch(oob, oob_b, ok_b);
                                        // OOB → -1
                                        self.builder.position_at_end(oob_b);
                                        self.builder.unconditional_branch(mrg);
                                        // OK → zero-extend the byte at data+index to i32. data@8.
                                        self.builder.position_at_end(ok_b);
                                        let idx64 = self.builder.int_z_extend_or_bit_cast(index, i64_ty, "sba_i64");
                                        let data_p = unsafe { self.builder.gep(i8_ty, sp, &[i64_ty.const_int(8, false)], "sba_data") };
                                        let byte_p = unsafe { self.builder.in_bounds_gep(i8_ty, data_p, &[idx64], "sba_byte_p") };
                                        let byte = self.builder.load(i8_ty, byte_p, "sba_byte").into_int_value();
                                        let byte_i32 = self.builder.int_z_extend(byte, i32_ty, "sba_zext");
                                        let ok_exit = self.builder.get_insert_block().unwrap();
                                        self.builder.unconditional_branch(mrg);
                                        self.builder.position_at_end(mrg);
                                        let phi = self.builder.phi(i32_ty, "sba_phi");
                                        let neg1 = i32_ty.const_int((-1i32) as u64, true);
                                        phi.add_incoming(&[(&neg1, oob_b), (&byte_i32, ok_exit)]);
                                        phi.as_basic_value()
                                    } else {
                                        // Fallback: emit the normal call (shouldn't happen — typed Str,Int32).
                                        let f = self.get_or_declare_fn("lin_string_byte_at",
                                            self.context.i32_type().fn_type(&[ptr_ty.into(), self.context.i32_type().into()], false));
                                        self.builder.call(f, &arg_vals, "call_n").try_as_basic_value().unwrap_basic()
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
                                        let call = self.builder.call(callee_fn, &arg_vals, "call_n");
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
                                                self.builder.call(self.rt.unbox_ptr, &[cls_ptr.into()], "ir_fn_unbox").try_as_basic_value().unwrap_basic()
                                            } else { cls_ptr };
                                            // Under-application of a closure value: FEWER args than
                                            // the callee's declared arity are supplied AND the result
                                            // is still a Function — bundle the inner closure + the
                                            // supplied args into a partial-application closure over
                                            // the remaining params. A CURRIED callee (full arity, but
                                            // it RETURNS a function — e.g. a `map` callback
                                            // `i => () => i`) is NOT under-application: it must be
                                            // CALLED. Disambiguated by arg-count vs callee arity
                                            // (`ret is Function` alone is ambiguous between the two).
                                            let callee_arity = match &callee_ty {
                                                Type::Function { params, .. } => params.len(),
                                                _ => args.len(),
                                            };
                                            let is_under_application = args.len() < callee_arity;
                                            if let Type::Function { params: remaining, .. } = ret_ty {
                                              if is_under_application {
                                                // Box each supplied partial into a TaggedVal* (ptr)
                                                // so the partial-application wrapper forwards it to
                                                // the inner closure under the uniform all-ptr boxed
                                                // ABI (the inner closure's stored fn_ptr is itself a
                                                // boxed-ABI wrapper expecting boxed args).
                                                let partials: Vec<BasicValueEnum> = arg_vals
                                                    .iter()
                                                    .zip(args.iter())
                                                    .map(|(a, a_temp)| {
                                                        let arg_ty = func.temp_types.get(a_temp).cloned().unwrap_or(Type::Null);
                                                        self.box_arg_for_closure_abi(*a, &arg_ty)
                                                    })
                                                    .collect();
                                                let r = self.build_closure_partial_application_values(
                                                    cls_ptr.into_pointer_value(), &partials, remaining);
                                                temp_map.insert(*dst, r);
                                                continue;
                                              }
                                            }
                                            let cls_ty = self.closure_struct_type();
                                            let cls_ptr_v = cls_ptr.into_pointer_value();
                                            // Default-fill through a function VALUE: the result type is concrete
                                            // (handled above if it were still a Function) but fewer args than the
                                            // value's declared arity are supplied. Dispatch through the closure's
                                            // descriptor (offset 32): entries[k - required] is a boxed-ABI adapter
                                            // that fills the omitted trailing defaults. The descriptor is null for
                                            // functions without defaults, so this only fires when one is present.
                                            let callee_total = match &callee_ty {
                                                Type::Function { params, .. } => params.len(),
                                                _ => args.len(),
                                            };
                                            let callee_required = match &callee_ty {
                                                Type::Function { required, .. } => *required,
                                                _ => args.len(),
                                            };
                                            if args.len() < callee_total && args.len() >= callee_required {
                                                let desc_gep = unsafe { self.builder.gep(
                                                    self.context.i8_type(), cls_ptr_v,
                                                    &[i64_ty.const_int(32, false)], "ir_desc_p"
                                                ) };
                                                let desc_ptr = self.builder.load(ptr_ty, desc_gep, "ir_desc").into_pointer_value();
                                                // entries array begins at descriptor offset 8 (after i32 total,
                                                // i32 required). Select entry index = k - required.
                                                let entry_idx = args.len() - callee_required;
                                                let entry_gep = unsafe { self.builder.gep(
                                                    self.context.i8_type(), desc_ptr,
                                                    &[i64_ty.const_int((8 + entry_idx * 8) as u64, false)], "ir_entry_p"
                                                ) };
                                                let entry_fn = self.builder.load(ptr_ty, entry_gep, "ir_entry").into_pointer_value();
                                                let env_gep = self.builder.struct_gep(cls_ty, cls_ptr_v, 3, "ir_ep");
                                                let env_ptr = self.builder.load(ptr_ty, env_gep, "ir_envp");
                                                // Adapter uses the uniform boxed ABI: (env, k boxed
                                                // TaggedVal* args...) -> ptr. Box each supplied arg
                                                // (already-boxed union/Json args pass through) so
                                                // the all-ptr adapter wrapper unboxes them correctly.
                                                let mut fn_param_types: Vec<BasicMetadataTypeEnum> = vec![ptr_ty.into()];
                                                let mut call_args: Vec<BasicMetadataValueEnum> = vec![env_ptr.into()];
                                                for (av, a_temp) in arg_vals.iter().zip(args.iter()) {
                                                    let arg_ty = func.temp_types.get(a_temp).cloned().unwrap_or(Type::Null);
                                                    let boxed = self.box_arg_for_closure_abi(*av, &arg_ty);
                                                    fn_param_types.push(ptr_ty.into());
                                                    call_args.push(boxed.into());
                                                }
                                                let returns_void = matches!(ret_ty, Type::Null | Type::Never);
                                                let fn_ty = if returns_void {
                                                    void_ty.fn_type(&fn_param_types, false)
                                                } else {
                                                    ptr_ty.fn_type(&fn_param_types, false)
                                                };
                                                let call = self.builder.indirect_call(fn_ty, entry_fn, &call_args, "ir_desc_call");
                                                let result = if returns_void {
                                                    ptr_ty.const_null().into()
                                                } else {
                                                    let boxed = call.try_as_basic_value().unwrap_basic();
                                                    if Self::is_union_type(ret_ty) { boxed }
                                                    else { self.unbox_tagged_val_to_type(boxed, ret_ty) }
                                                };
                                                temp_map.insert(*dst, result);
                                                continue;
                                            }
                                            // Build closure call: load fn_ptr from offset 2 of closure struct.
                                            let fn_gep = self.builder.struct_gep(cls_ty, cls_ptr_v, 2, "ir_fp");
                                            let fn_ptr = self.builder.load(ptr_ty, fn_gep, "ir_fnp").into_pointer_value();
                                            let env_gep = self.builder.struct_gep(cls_ty, cls_ptr_v, 3, "ir_ep");
                                            let env_ptr = self.builder.load(ptr_ty, env_gep, "ir_envp");

                                            // Uniform boxed closure-call ABI: env_ptr + one boxed
                                            // TaggedVal* (ptr) per argument. EVERY function value
                                            // (capture-less named fn, capturing closure, partial
                                            // application) is stored as a boxed-ABI wrapper that
                                            // declares all params `ptr` and unboxes them, so each
                                            // arg MUST arrive boxed. The IR only boxes args up to
                                            // the value's *declared* arity (an opaque `Function`
                                            // declares ONE param), so a multi-arg call through such
                                            // a value reaches here with later args still concrete —
                                            // box them so the all-ptr wrapper ABI agrees (otherwise
                                            // raw bits are reinterpreted as a ptr → garbage /
                                            // misaligned deref — the wrapper-ABI bug). Already-boxed
                                            // union/Json args pass through (no double-box).
                                            let mut fn_param_types: Vec<BasicMetadataTypeEnum> = vec![ptr_ty.into()];
                                            let mut call_args: Vec<BasicMetadataValueEnum> = vec![env_ptr.into()];
                                            for (av, a_temp) in arg_vals.iter().zip(args.iter()) {
                                                let arg_ty = func.temp_types.get(a_temp).cloned().unwrap_or(Type::Null);
                                                let boxed = self.box_arg_for_closure_abi(*av, &arg_ty);
                                                fn_param_types.push(ptr_ty.into());
                                                call_args.push(boxed.into());
                                            }
                                            // Closures use the uniform boxed ABI (return ptr,
                                            // except void). Call with ptr return, then unbox to ret_ty.
                                            let returns_void = matches!(ret_ty, Type::Null | Type::Never);
                                            let fn_ty = if returns_void {
                                                void_ty.fn_type(&fn_param_types, false)
                                            } else {
                                                ptr_ty.fn_type(&fn_param_types, false)
                                            };
                                            let call = self.builder.indirect_call(fn_ty, fn_ptr, &call_args, "ir_ind");
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
                            // STAGE 3: the per-operand physical representation (from `func.repr`) so
                            // repr-deciding intrinsics (Push) dispatch on the proven repr instead of
                            // re-deriving from the static type.
                            let arg_reprs: Vec<lin_ir::repr::Repr> =
                                args.iter().map(|a| func.repr_of(*a)).collect();
                            let result = self.compile_ir_intrinsic(intrinsic, &arg_vals, &arg_tys, &arg_reprs, ret_ty);
                            temp_map.insert(*dst, result);
                        }
                        Instruction::MakeObject { dst, fields, spreads, ty, stack } => {
                            // Typed index-signature map `{ String: T }` (ADR-055): allocate a hashed
                            // `LinMap` and set each literal field via `lin_map_set` (key = interned
                            // LinString, value = boxed TaggedVal). The checker only produces a
                            // `Type::Map` MakeObject for spread-free string-keyed literals (incl. the
                            // common empty `{}`), so there are no spreads to merge here.
                            if let Type::Map(elem_ty) = ty {
                                let cap = i32_ty.const_int(fields.len().max(1) as u64, false);
                                let map_ptr = self.builder
                                    .call(self.rt.map_alloc, &[cap.into()], "ir_map")
                                    .try_as_basic_value().unwrap_basic().into_pointer_value();
                                for (key, val_temp) in fields.iter() {
                                    if let Some(&val) = temp_map.get(val_temp) {
                                        let key_str = self.compile_string_lit(key).into_pointer_value();
                                        let val_ty = func.temp_types.get(val_temp).cloned().unwrap_or(Type::Null);
                                        // Flat-scalar `T` (ADR-055 follow-up): store the scalar UNBOXED
                                        // via a stack TaggedVal carrying `T`'s tag/payload, widening a
                                        // narrower literal value to `T` first. No heap box, no RC.
                                        let tagged = if Self::is_flat_scalar(elem_ty.as_ref()) {
                                            let coerced = if &val_ty == elem_ty.as_ref() {
                                                val
                                            } else {
                                                self.compile_ir_coerce(val, &val_ty, elem_ty.as_ref())
                                            };
                                            self.build_tagged_val_alloca(&coerced, elem_ty.as_ref())
                                        } else if Self::is_union_type(&val_ty) && val.is_pointer_value() {
                                            // A union/Json value is already a TaggedVal* — pass through
                                            // (lin_map_set retains its inner).
                                            val.into_pointer_value()
                                        } else {
                                            // A heap `T` (String/Array/Object/Map): a STACK TaggedVal,
                                            // exactly as before. lin_map_set retains the inner so the
                                            // slot owns its own reference; the literal temp keeps its
                                            // own +1 (released at scope exit). UNCHANGED RC behaviour.
                                            self.build_tagged_val_alloca(&val, &val_ty)
                                        };
                                        self.builder.call(self.rt.map_set, &[map_ptr.into(), key_str.into(), tagged.into()], "");
                                    }
                                }
                                temp_map.insert(*dst, map_ptr.into());
                                continue;
                            }
                            // Sealed record (Stages 1–2): allocate the packed struct and store each
                            // field by offset — no string keys, no per-field box. Only a no-spread
                            // literal whose field set EXACTLY matches the type qualifies (a spread
                            // would add unknown fields → keep boxed; the checker only produces a
                            // sealed literal type when the fields line up). If a field value is
                            // missing (shouldn't happen for a well-typed sealed literal), fall
                            // through to the boxed path for safety.
                            //
                            // Each field's lowered temp is BORROWED (owned by a lowerer temp that is
                            // released at scope exit), so `already_owned = false`: `sealed_construct`
                            // retains heap fields it stores verbatim, and folds in any
                            // representation-changing coerce (e.g. an unsealed `{x,y}` literal into a
                            // nested sealed `Pt` field) as fresh-owned automatically.
                            // STAGE 3: the packed-vs-boxed decision is read from `func.repr` (the
                            // representation-inference pass already folded in the `sealed_scalar_fields`
                            // gate AND the no-spread/all-present check via `make_object_repr`). When the
                            // pass labelled this temp `Packed(PackedStruct)`, construct the packed struct;
                            // otherwise fall through to the boxed `LinObject` path. (Oracle-proven byte
                            // identical to the former `sealed_scalar_fields(ty) && all_present` gate.)
                            let repr = func.repr_of(*dst);
                            // UNBOXED SUM TYPE (unboxed-sumtype Stage 1): when the pass labelled this
                            // temp `Packed(SumNode)`, construct a `SumNode` directly — store the
                            // inline variant tag + each scalar payload field by offset (no string keys,
                            // no box). The variant is identified by the literal's discriminant value,
                            // which is the `StrLit` static type of the discriminant field's temp.
                            //
                            // NOTE: currently INERT — `repr::type_seed`/`make_object_repr` do not yet
                            // emit `Packed(SumNode)` (the seed is gated off pending the call ABI), so
                            // this branch is never taken on the present corpus. It is the wired
                            // construct site the ABI follow-up flips on by enabling the seed.
                            if let Some(sum_ty) = repr.sumnode_sum_ty() {
                                let sum_ty = sum_ty.clone();
                                if let Some(disc_key) = Self::sum_type_discriminant(&sum_ty) {
                                    // Find the discriminant value from the literal field's StrLit type.
                                    let disc_val = fields.iter().find_map(|(k, t)| {
                                        if k == &disc_key {
                                            match func.temp_types.get(t) {
                                                Some(Type::StrLit(s)) => Some(s.clone()),
                                                _ => None,
                                            }
                                        } else {
                                            None
                                        }
                                    });
                                    if let Some(disc_val) = disc_val {
                                        let field_vals: Vec<(String, BasicValueEnum<'ctx>, Type)> = fields
                                            .iter()
                                            .filter_map(|(k, t)| {
                                                temp_map.get(t).map(|v| {
                                                    let vty = func.temp_types.get(t).cloned().unwrap_or(Type::Null);
                                                    (k.clone(), *v, vty)
                                                })
                                            })
                                            .collect();
                                        let node = self.sumnode_construct(&sum_ty, &disc_val, &field_vals);
                                        temp_map.insert(*dst, node);
                                        continue;
                                    }
                                }
                                // Fall through to the boxed path if the discriminant could not be
                                // resolved statically (fail-safe — should not happen for a sum literal).
                            }
                            if let Some(sf) = repr.packed_struct_fields() {
                                {
                                    let field_vals: Vec<(String, BasicValueEnum<'ctx>, Type, bool)> = fields.iter().filter_map(|(k, t)| {
                                        temp_map.get(t).map(|v| {
                                            let vty = func.temp_types.get(t).cloned().unwrap_or(Type::Null);
                                            (k.clone(), *v, vty, false)
                                        })
                                    }).collect();
                                    // Sealed-records Stage 4: the escape analysis proved this
                                    // construction non-escaping → stack-allocate (reused entry-block
                                    // alloca, immortal refcount, no heap, no per-iteration growth).
                                    // Extra defensive gate: only when ALL fields are unboxed scalars
                                    // (no heap field needs RC) — the escape pass only sets `stack`
                                    // for all-scalar records, but re-check here so a heap-field record
                                    // can never reach the alloca path.
                                    let obj = if *stack && sf.values().all(Self::is_sealed_scalar_field) {
                                        self.sealed_construct_stack(sf, &field_vals, llvm_fn)
                                    } else {
                                        self.sealed_construct(sf, &field_vals)
                                    };
                                    temp_map.insert(*dst, obj);
                                    continue;
                                }
                            }
                            // Right-size the capacity. For a plain (no-spread) literal the final
                            // size is exactly the field count (after de-duplicating literal keys,
                            // below). With spreads the final size is unknown (spread sources add
                            // fields), so keep some headroom and let the buffer grow on demand.
                            let cap_hint = if spreads.is_empty() {
                                fields.len()
                            } else {
                                fields.len() + 4
                            };
                            let cap = i32_ty.const_int(cap_hint as u64, false);
                            let obj_ptr = self.builder.call(self.rt.object_alloc, &[cap.into()], "ir_obj").try_as_basic_value().unwrap_basic().into_pointer_value();
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
                                                self.builder.call(self.rt.unbox_ptr, &[sv.into()], "ir_spread_unbox").try_as_basic_value().unwrap_basic()
                                            } else { sv };
                                            self.builder.call(merge_fn, &[obj_ptr.into(), src.into()], "");
                                        }
                                    }
                                }
                            }
                            // For a no-spread literal the keys appended are statically
                            // known-distinct, so we can use the no-dup-check fast append
                            // (`lin_object_set_fresh`). But object-literal semantics are
                            // last-wins for a repeated key (`{"x":1,"x":2}["x"] == 2`), and the
                            // checker does NOT reject duplicate literal keys — so we must first
                            // de-duplicate, keeping the LAST occurrence of each key. (When spreads
                            // are present a literal field can collide with a spread-provided key,
                            // which we cannot detect statically, so that case keeps the
                            // dup-checking `lin_object_set`.)
                            let use_fresh = spreads.is_empty();
                            let last_idx: std::collections::HashMap<&String, usize> = if use_fresh {
                                let mut m = std::collections::HashMap::new();
                                for (i, (key, _)) in fields.iter().enumerate() {
                                    m.insert(key, i);
                                }
                                m
                            } else {
                                std::collections::HashMap::new()
                            };

                            // ── PHASES 1+2: inline object literal construction ────────────
                            // When the literal has no spreads and EVERY materialised field has a
                            // known concrete representation, emit the LinObject entry stores INLINE
                            // instead of calling lin_object_set_fresh per field. This removes one
                            // non-inlined runtime call per field — the ~46% construction half of
                            // per-object cost (see perf profiling: inlining the per-field CALL is the
                            // lever, the allocator is only ~3-4%).
                            //
                            // Eligible field types and their RC handling, mirroring the runtime's
                            // lin_object_set_fresh → retain_tagged_payload EXACTLY:
                            //   • scalars (Int/Float/Bool/Null): no heap payload → no retain (Phase 1)
                            //   • Str/StrLit/Array/FixedArray/Object: heap payload retained via the
                            //     offset-0 refcount with the immortal guard — which is PRECISELY what
                            //     lin_rc_retain does — so we emit one lin_rc_retain on the stored
                            //     pointer, the object takes ownership of a +1 reference (Phase 2).
                            // EXCLUDED (fall back to the runtime set_fresh path, identical behaviour):
                            //   • Function: retain_tagged_payload is a NO-OP for TAG_FUNCTION (it is
                            //     asymmetric with release); replicating that inline is error-prone, so
                            //     any Function field disqualifies the whole object.
                            //   • union/TypeVar/Named/Shared/Stream: already a boxed TaggedVal* with
                            //     its own tag/retain semantics; not a plain (tag,payload) pair here.
                            //
                            // lin_object_alloc(N) gives cap==N, len==0, entries → inline buffer; we
                            // write each entry's key/tag/payload directly and set len at the end.
                            // cap==N (the field count for a no-spread literal) guarantees no grow, so
                            // the inline entries buffer never moves.
                            fn inline_field_kind(t: &Type) -> Option<bool> {
                                // Some(needs_retain) if eligible for the inline path; None otherwise.
                                match t {
                                    Type::Null | Type::Bool
                                    | Type::Int8 | Type::Int16 | Type::Int32
                                    | Type::UInt8 | Type::UInt16 | Type::UInt32
                                    | Type::Int64 | Type::UInt64
                                    | Type::Float32 | Type::Float64 => Some(false),
                                    Type::Str | Type::StrLit(_)
                                    | Type::Array(_) | Type::FixedArray(_)
                                    | Type::Object { .. } => Some(true),
                                    _ => None,
                                }
                            }
                            let inline_eligible = use_fresh && fields.iter().enumerate().all(|(idx, (key, vt))| {
                                // Only the surviving (last-wins) fields are materialised.
                                last_idx.get(key) != Some(&idx) || {
                                    let t = func.temp_types.get(vt).cloned().unwrap_or(Type::Null);
                                    inline_field_kind(&t).is_some()
                                }
                            });
                            if inline_eligible {
                                let i8_ty = self.context.i8_type();
                                // entries = *(ptr*)(obj + 16)
                                let entries_pp = unsafe {
                                    self.builder.gep(i8_ty, obj_ptr, &[i64_ty.const_int(16, false)], "obj_entries_pp")
                                };
                                let entries = self.builder.load(ptr_ty, entries_pp, "obj_entries").into_pointer_value();
                                // LinObjectEntry stride = 24 bytes: key@0, value.tag@8, value.payload@16.
                                let mut slot: u64 = 0;
                                for (idx, (key, val_temp)) in fields.iter().enumerate() {
                                    if last_idx.get(key) != Some(&idx) { continue; }
                                    if let Some(&val) = temp_map.get(val_temp) {
                                        let val_ty = func.temp_types.get(val_temp).cloned().unwrap_or(Type::Null);
                                        let needs_retain = inline_field_kind(&val_ty) == Some(true);
                                        let key_str = self.compile_string_lit(key).into_pointer_value();
                                        let base = slot * 24;
                                        // key@base
                                        let key_p = unsafe {
                                            self.builder.gep(i8_ty, entries, &[i64_ty.const_int(base, false)], "ent_key_p")
                                        };
                                        self.builder.store(key_p, key_str);
                                        // tag@base+8
                                        let tag_p = unsafe {
                                            self.builder.gep(i8_ty, entries, &[i64_ty.const_int(base + 8, false)], "ent_tag_p")
                                        };
                                        self.builder.store(tag_p, i8_ty.const_int(Self::type_tag(&val_ty) as u64, false));
                                        // payload@base+16
                                        let payload = self.tagged_payload_i64(&val, &val_ty);
                                        let pay_p = unsafe {
                                            self.builder.gep(i8_ty, entries, &[i64_ty.const_int(base + 16, false)], "ent_pay_p")
                                        };
                                        self.builder.store(pay_p, payload);
                                        // The object now owns a reference to a heap payload — retain it,
                                        // mirroring retain_tagged_payload. lin_rc_retain bumps the
                                        // offset-0 refcount and no-ops on immortal (interned literal /
                                        // frozen) values, exactly matching the runtime for Str/Array/
                                        // Object. The matching release happens in lin_object_release.
                                        if needs_retain && val.is_pointer_value() {
                                            self.builder.call(self.rt.rc_retain, &[val.into_pointer_value().into()], "");
                                        }
                                    }
                                    slot += 1;
                                }
                                // len = slot  (*(u32*)(obj + 4))
                                let len_p = unsafe {
                                    self.builder.gep(i8_ty, obj_ptr, &[i64_ty.const_int(4, false)], "obj_len_p")
                                };
                                self.builder.store(len_p, i32_ty.const_int(slot, false));
                                let _ = ty;
                                temp_map.insert(*dst, obj_ptr.into());
                                continue;
                            }

                            for (idx, (key, val_temp)) in fields.iter().enumerate() {
                                // Skip earlier duplicates in the no-spread fast path so only the
                                // last write for a key is materialised (last-wins).
                                if use_fresh && last_idx.get(key) != Some(&idx) {
                                    continue;
                                }
                                if let Some(&val) = temp_map.get(val_temp) {
                                    let key_str = self.compile_string_lit(key).into_pointer_value();
                                    let val_ty = func.temp_types.get(val_temp).cloned().unwrap_or(Type::Null);
                                    // UNBOXED SUM TYPE (unboxed-sumtype Stage 3): a sum-typed field
                                    // value is physically a `*SumNode`, NOT a boxed TaggedVal*. The
                                    // generic union branch below would store the raw SumNode pointer as
                                    // if it were already a box → the read-back `object_get` reads a
                                    // SumNode header as a LinObject → garbage / crash.
                                    //
                                    // MATERIALIZE it to a real boxed LinObject — the safe, always-sound
                                    // boundary for a record field (which may flow to toString / match /
                                    // spread, and whose READ-back type partially expands the recursive
                                    // children, so a keep-packed `TAG_SUMNODE` store could not be matched
                                    // by a type-driven read decision — see the deferral note below).
                                    // `box_value` heap-boxes the materialized object as TAG_OBJECT (and
                                    // handles the `sum|Null` null case); the freshly materialized object
                                    // is +1, `object_set_fresh` retains it into the slot, release the
                                    // transient box after.
                                    //
                                    // KEEP-PACKED-BY-POINTER for a record/Json field slot is DEFERRED:
                                    // the field's READ-back type and the stored VALUE's type are
                                    // structurally different (the record field type expands the recursive
                                    // sum children one level to `Union`, while the value carries them as
                                    // `Named` — so `is_sum_type`/`sum_type_eligible` disagree between the
                                    // store and the read). A keep-packed decision must therefore be
                                    // REPR-driven (the repr pass carries a consistent label), which needs
                                    // the lowering/repr STEP-4 — out of this change's scope. The
                                    // TAG_SUMNODE runtime substrate + codegen helpers are in place.
                                    if Self::is_sum_type(&val_ty)
                                        || Self::sum_member_of_nullable_union(&val_ty).is_some()
                                    {
                                        // KEEP-PACKED-THROUGH-RECORD-FIELDS store: a sum-typed field
                                        // value is physically a `*SumNode`. Instead of materializing it
                                        // to a boxed LinObject (`lin_summat` + `lin_box_object` — the
                                        // O(n)-tree round-trip the interp cursor `{node,pos}` paid every
                                        // parse step), wrap the still-packed node by-pointer in a
                                        // `TaggedVal(TAG_SUMNODE)` (BoxKeepSumnode, O(1), zero copy). The
                                        // DISTINCT tag is the soundness mechanism: the slot's release
                                        // routes to `lin_sumnode_release_self`, retain to the offset-0 RC
                                        // bump, and toString/eq/json/transfer MATERIALIZE on demand (the
                                        // runtime walkers' TAG_SUMNODE arms). The read-back
                                        // (`compile_ir_field_get_sumnode_readback`) tag-dispatches, so a
                                        // slot stored EITHER keep-packed OR materialized reads correctly
                                        // — no static store/read asymmetry. Ownership matches the
                                        // materialize path: the IR `transfer_into_container` supplies the
                                        // slot's owning +1; `object_set*` retains the inner; the shell
                                        // release here undoes that duplicate, net-zero on the node.
                                        // A null value (the `sum | Null` null case) tags as TAG_NULL.
                                        let keep_packed = val.is_pointer_value();
                                        let stored = if keep_packed {
                                            self.compile_ir_box_keep_sumnode(val)
                                        } else {
                                            self.box_value(val, &val_ty)
                                        };
                                        let set_fn = if use_fresh { self.rt.object_set_fresh } else { self.rt.object_set };
                                        self.builder.call(set_fn, &[obj_ptr.into(), key_str.into(), stored.into()], "");
                                        if stored.is_pointer_value() {
                                            if keep_packed {
                                                // KEEP-PACKED: `object_set*` already retained the inner
                                                // `*SumNode` into the slot (its OWN +1, independent of
                                                // the source local). The source local keeps its own
                                                // reference and is released at scope exit. So free ONLY
                                                // the box SHELL here (lin_tagged_free_box) — a full
                                                // `tagged_release` would `lin_sumnode_release_self` the
                                                // shared node, dropping the slot's reference and freeing
                                                // a node the cursor still points to (UAF). Mirrors the
                                                // materializer's `free_box_shell` after a `set_fresh`.
                                                let free_shell = self.get_or_declare_fn(
                                                    "lin_tagged_free_box",
                                                    self.context.void_type().fn_type(&[ptr_ty.into()], false),
                                                );
                                                self.builder.call(free_shell, &[stored.into()], "");
                                            } else {
                                                self.builder.call(self.rt.tagged_release, &[stored.into()], "");
                                            }
                                        }
                                        continue;
                                    }
                                    // A union/Json-typed field value is ALREADY a boxed TaggedVal*
                                    // — pass it straight to lin_object_set. Re-wrapping it via
                                    // build_tagged_val_alloca would store the pointer under a
                                    // TAG_NULL tag (type_tag(TypeVar)=0), so later reads see null.
                                    let tagged = if Self::is_union_type(&val_ty) && val.is_pointer_value() {
                                        val.into_pointer_value()
                                    } else {
                                        self.build_tagged_val_alloca(&val, &val_ty)
                                    };
                                    let set_fn = if use_fresh { self.rt.object_set_fresh } else { self.rt.object_set };
                                    self.builder.call(set_fn, &[obj_ptr.into(), key_str.into(), tagged.into()], "");
                                    // No string_release: `compile_string_lit` returns an INTERNED,
                                    // immortal LinString (refcount == IMMORTAL_RC), so both the
                                    // `inc_ref` inside object_set* and a release here are runtime
                                    // no-ops — but the release is still an emitted call, hit once
                                    // per field per object construction. Object-heavy code builds
                                    // millions of objects, so dropping the dead call is a real win.
                                    // SOUND: the key is never freed (immortal), and object_set*'s
                                    // own inc_ref is also a no-op on it, so RC stays balanced.
                                }
                            }
                            let _ = ty;
                            temp_map.insert(*dst, obj_ptr.into());
                        }
                        Instruction::MakeArray { dst, elements, elem_ty } => {
                            let cap = i64_ty.const_int(elements.len().max(4) as u64, false);
                            // Sealed-record array (Stage 3): contiguous, unboxed, header-less
                            // elements. Allocate via lin_sealed_array_alloc(cap, stride, desc) and
                            // copy each element struct's field payload into the buffer (scalar
                            // fields → no retain). `elem_ty` is the sealed Object type.
                            // STAGE 3: the packed-sealed-array decision comes from `func.repr` (the
                            // pass's `make_array_repr` already applied the `sealed_array_elem` gate —
                            // sealed element with all-packable fields). The flat-scalar-vs-boxed split
                            // below is the ORTHOGONAL pre-existing flat-array path (assume sites dispatch
                            // on the array TYPE), which repr does not own, so it stays type-driven.
                            let arr_repr = func.repr_of(*dst);
                            let arr = if let Some(fields) = arr_repr.packed_sealed_array_layout() {
                                let fields = fields.clone();
                                let stride = Self::sealed_array_stride(&fields);
                                let desc = self.sealed_descriptor(&fields); // NULL for scalar-only
                                let has_heap = fields.values().any(|t| Self::sealed_field_kind(t).is_some());
                                let alloc_fn = self.get_or_declare_fn(
                                    "lin_sealed_array_alloc",
                                    ptr_ty.fn_type(&[i64_ty.into(), i64_ty.into(), ptr_ty.into()], false));
                                let arr_v = self.builder.call(alloc_fn,
                                    &[cap.into(), i64_ty.const_int(stride, false).into(), desc.into()],
                                    "ir_sarr").try_as_basic_value().unwrap_basic();
                                // Construct: each element struct `ev` is a BORROWED standalone struct
                                // (owned by its own temp, released at this scope's exit). A heap-field
                                // array must take its OWN +1 on every heap field as it copies the
                                // payload into the slot (`..._retaining`) — else the array's
                                // release-on-drop would double-free the still-borrowed inner. A
                                // scalar-only record has no heap field, so the plain payload copy
                                // (NULL desc → retaining push is a no-op for fields) is identical.
                                let push_name = if has_heap {
                                    "lin_sealed_array_push_struct_retaining"
                                } else {
                                    "lin_sealed_array_push_struct"
                                };
                                let push_fn = self.get_or_declare_fn(
                                    push_name,
                                    self.context.void_type().fn_type(&[ptr_ty.into(), ptr_ty.into()], false));
                                for e_temp in elements {
                                    if let Some(&ev) = temp_map.get(e_temp) {
                                        self.builder.call(push_fn, &[arr_v.into(), ev.into()], "");
                                    }
                                }
                                arr_v
                            } else if Self::is_flat_scalar(elem_ty) {
                                let suffix = Self::flat_suffix(elem_ty);
                                let alloc_fn = self.get_or_declare_fn(
                                    &format!("lin_flat_array_alloc_{}", suffix),
                                    ptr_ty.fn_type(&[i64_ty.into()], false));
                                let arr_v = self.builder.call(alloc_fn, &[cap.into()], "ir_farr").try_as_basic_value().unwrap_basic();
                                for e_temp in elements {
                                    if let Some(&ev) = temp_map.get(e_temp) {
                                        self.flat_array_push(arr_v, ev, elem_ty);
                                    }
                                }
                                arr_v
                            } else {
                                let arr_v = self.builder.call(self.rt.array_alloc, &[cap.into()], "ir_arr").try_as_basic_value().unwrap_basic();
                                for e_temp in elements {
                                    if let Some(&ev) = temp_map.get(e_temp) {
                                        self.tagged_array_push_value(arr_v, ev, elem_ty);
                                    }
                                }
                                arr_v
                            };
                            temp_map.insert(*dst, arr);
                        }
                        Instruction::MakeClosure { dst, func: fid, captures, capture_kinds, ret_ty: _ } => {
                            if let Some(&callee_fn) = ir_fn_to_llvm.get(fid) {
                                // If this function has default arguments, attach its descriptor
                                // so an indirect under-arity call fills the omitted defaults.
                                let descriptor = self.cls_descriptors.get(fid).copied();
                                let cls = if captures.is_empty() {
                                    // The target was lowered as a non-closure (no env param 0),
                                    // but closure call sites invoke fn_ptr(env, args...) -> ptr.
                                    // Wrap it in an env-ignoring stub that also boxes the return,
                                    // matching the uniform boxed closure ABI. Pass the function's
                                    // real Lin return type so a raw Str/Array/Object return is
                                    // boxed (the indirect caller always unboxes).
                                    let ret = module.function(*fid).map(|f| f.ret_ty.clone());
                                    // The wrapper is called through the uniform boxed closure ABI
                                    // (every arg a boxed ptr), so it must unbox each arg to the
                                    // named fn's concrete Lin param type. Thread those types so a
                                    // scalar/Str/Array param isn't reinterpreted from a boxed ptr
                                    // (the wrapper-ABI bug).
                                    let param_tys: Option<Vec<Type>> = module
                                        .function(*fid)
                                        .map(|f| f.params.iter().map(|(_, t)| t.clone()).collect());
                                    self.wrap_named_fn_as_closure_boxed_desc_ret(
                                        callee_fn, descriptor, ret.as_ref(), param_tys.as_deref())
                                } else {
                                    // Captures present ⇒ the closure body has an env param 0.
                                    // Its real args are still compiled with CONCRETE param types,
                                    // but every INDIRECT call uses the uniform boxed ABI (env +
                                    // boxed ptr args -> ptr). Store a boxed-ABI wrapper that
                                    // forwards the env, unboxes each arg to the body's concrete
                                    // param type, and boxes the return — exactly like the
                                    // capture-less path — so a capturing closure is callable
                                    // through an opaque `Function` value too (the wrapper-ABI bug
                                    // otherwise reinterprets a boxed ptr arg as the concrete type).
                                    let body = module.function(*fid);
                                    let ret = body.map(|f| f.ret_ty.clone());
                                    // params[0] is the env; the real arg types are params[1..].
                                    let arg_tys: Option<Vec<Type>> = body.map(|f| {
                                        f.params.iter().skip(1).map(|(_, t)| t.clone()).collect()
                                    });
                                    let wrapper_fn = self.boxed_abi_wrapper_full(
                                        callee_fn, ret.as_ref(), arg_tys.as_deref(), true);
                                    let fn_ptr = wrapper_fn.as_global_value().as_pointer_value();
                                    let capture_vals: Vec<BasicValueEnum> = captures
                                        .iter()
                                        .filter_map(|c| temp_map.get(c).copied())
                                        .collect();
                                    // Per-capture release kinds (ADR-041 owning captures). The env
                                    // OWNS one reference per owning capture, so the capture
                                    // descriptor is ALWAYS emitted: `lin_closure_release` walks it
                                    // to release heap captures on free, and the async transfer path
                                    // reuses the same encoding (CaptureRelease::code). The lowerer
                                    // already computed these in lockstep with the retain/CloneBox it
                                    // emitted, so codegen does not re-derive from temp types.
                                    let kinds: Vec<u8> = capture_kinds.iter().map(|k| k.code()).collect();
                                    self.make_closure_struct_desc_caps(
                                        fn_ptr.into(), &capture_vals, descriptor,
                                        Some(&kinds),
                                    )
                                };
                                temp_map.insert(*dst, cls);
                            }
                        }
                        Instruction::MakeNamedClosure { dst, sym, ty } => {
                            // Materialize an imported/FFI function symbol as a capture-less closure
                            // value (see the import_fn_slots branch in lower.rs LocalGet). Resolve
                            // the external symbol at its CONCRETE Lin signature — the same signature
                            // the import was compiled with — then wrap it in the uniform boxed-ABI
                            // stub exactly as a local named function value is.
                            let (param_tys, ret_ty): (Vec<Type>, Type) = match ty {
                                Type::Function { params, ret, .. } => (params.clone(), (**ret).clone()),
                                _ => (vec![], Type::Null),
                            };
                            let named_fn = match self.module.get_function(sym) {
                                Some(f) => f,
                                None => {
                                    let llvm_params: Vec<BasicMetadataTypeEnum> = param_tys
                                        .iter()
                                        .map(|t| self.llvm_param_type(t))
                                        .collect();
                                    let fn_ty = if matches!(ret_ty, Type::Null | Type::Never) {
                                        void_ty.fn_type(&llvm_params, false)
                                    } else {
                                        self.llvm_type(&ret_ty).fn_type(&llvm_params, false)
                                    };
                                    self.module.add_function(sym, fn_ty, None)
                                }
                            };
                            let cls = self.wrap_named_fn_as_closure_boxed_desc_ret(
                                named_fn, None, Some(&ret_ty), Some(&param_tys));
                            temp_map.insert(*dst, cls);
                        }
                        Instruction::Index { dst, object, key, obj_ty, key_ty, result_ty } => {
                            if let (Some(&obj_v), Some(&key_v)) = (temp_map.get(object), temp_map.get(key)) {
                                let obj_repr = func.repr_of(*object);
                                let result = self.compile_ir_index(obj_v, key_v, obj_ty, key_ty, result_ty, &obj_repr);
                                temp_map.insert(*dst, result);
                            }
                        }
                        Instruction::IndexSet { object, key, value, obj_ty, key_ty, val_ty } => {
                            if let (Some(&obj_v), Some(&key_v), Some(&val_v)) =
                                (temp_map.get(object), temp_map.get(key), temp_map.get(value))
                            {
                                let val_repr = func.repr_of(*value);
                                self.compile_ir_index_set(obj_v, key_v, val_v, obj_ty, key_ty, val_ty, &val_repr);
                            }
                        }
                        Instruction::FieldGet { dst, object, field, obj_ty, result_ty } => {
                            if let Some(&obj_v) = temp_map.get(object) {
                                let obj_repr = func.repr_of(*object);
                                let result = self.compile_ir_field_get(obj_v, field, obj_ty, result_ty, &obj_repr);
                                temp_map.insert(*dst, result);
                            }
                        }
                        Instruction::SealedArrayFieldGet { dst, array, index, field, arr_ty, result_ty } => {
                            if let (Some(&arr_v), Some(&idx_v)) = (temp_map.get(array), temp_map.get(index)) {
                                let arr_repr = func.repr_of(*array);
                                let result = self.compile_ir_sealed_array_field_get(arr_v, idx_v, field, arr_ty, result_ty, &arr_repr);
                                temp_map.insert(*dst, result);
                            }
                        }
                        Instruction::ObjectRest { dst, src, src_ty, exclude } => {
                            if let Some(&src_v) = temp_map.get(src) {
                                // Unbox a boxed Json object to the raw LinObject*.
                                let src_obj = if Self::is_union_type(src_ty) && src_v.is_pointer_value() {
                                    self.builder.call(self.rt.unbox_ptr, &[src_v.into()], "orest_unbox").try_as_basic_value().unwrap_basic()
                                } else { src_v };
                                let rest_obj = self.builder.call(self.rt.object_alloc,
                                    &[i32_ty.const_int(4, false).into()], "orest").try_as_basic_value().unwrap_basic().into_pointer_value();
                                let exclude_fn = self.get_or_declare_fn("lin_object_copy_except",
                                    void_ty.fn_type(&[ptr_ty.into(), ptr_ty.into(), ptr_ty.into(), i32_ty.into()], false));
                                let n_exc = exclude.len() as u32;
                                let arr_ty = ptr_ty.array_type(n_exc.max(1));
                                let keys_arr = self.builder.alloca(arr_ty, "orest_keys");
                                for (i, key) in exclude.iter().enumerate() {
                                    let key_str = self.compile_string_lit(key);
                                    let gep = unsafe { self.builder.gep(arr_ty, keys_arr,
                                        &[i32_ty.const_zero(), i32_ty.const_int(i as u64, false)], "orest_kp") };
                                    self.builder.store(gep, key_str);
                                }
                                let keys_ptr = self.builder.pointer_cast(keys_arr, ptr_ty, "orest_kps");
                                self.builder.call(exclude_fn,
                                    &[rest_obj.into(), src_obj.into(), keys_ptr.into(), i32_ty.const_int(n_exc as u64, false).into()], "");
                                let boxed = self.builder.call(self.rt.box_object, &[rest_obj.into()], "orest_boxed").try_as_basic_value().unwrap_basic();
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
                                    let r = self.builder.call(check_fn, &[v.into(), n_v.into(), at_v.into()], "alc").try_as_basic_value().unwrap_basic().into_int_value();
                                    self.builder.int_truncate_or_bit_cast(r, self.context.bool_type(), "alc_b").into()
                                } else {
                                    self.context.bool_type().const_zero().into()
                                };
                                temp_map.insert(*dst, result);
                            }
                        }
                        Instruction::GlobalValSet { slot, value, ty, immutable } => {
                            if let Some(&v) = temp_map.get(value) {
                                let llvm_ty = self.llvm_type(ty);
                                let glob = *ir_global_vals.entry(*slot).or_insert_with(|| {
                                    Self::add_module_global(&self.module, llvm_ty, *slot, *immutable)
                                });
                                // A top-level `var` global owns one reference to its current
                                // value. On reassignment its previous reference must be dropped,
                                // otherwise every reassignment leaks the old value. The lowerer
                                // pairs this with a Retain of the new value so the global holds
                                // an independent reference. Applies to concrete reference-counted
                                // types AND boxed Json/union globals: the lowerer now uses the
                                // SAME owning model (clone on store, clone+register on read,
                                // release-old here) for unions, so `emit_release` dispatches the
                                // tag-aware `lin_tagged_release` (null-safe: the global's zero
                                // initial value is a no-op release).
                                if Self::ty_is_concrete_rc(ty) || Self::is_union_type(ty) {
                                    let old = self.builder
                                        .load(llvm_ty, glob.as_pointer_value(), "ir_gv_old");
                                    self.emit_release(old, ty);
                                }
                                self.builder.store(glob.as_pointer_value(), v);
                            }
                        }
                        Instruction::GlobalValGet { dst, slot, ty, immutable } => {
                            let llvm_ty = self.llvm_type(ty);
                            let glob = *ir_global_vals.entry(*slot).or_insert_with(|| {
                                Self::add_module_global(&self.module, llvm_ty, *slot, *immutable)
                            });
                            let v = self.builder.load(llvm_ty, glob.as_pointer_value(), "ir_gvget");
                            temp_map.insert(*dst, v);
                        }
                        Instruction::MakeCell { dst, init, ty } => {
                            if let Some(&v) = temp_map.get(init) {
                                let llvm_ty = self.llvm_type(ty);
                                let size = llvm_ty.size_of().unwrap();
                                let size_i64 = self.builder.int_z_extend_or_bit_cast(size, i64_ty, "cell_sz");
                                let cell = self.builder.call(self.rt.alloc, &[size_i64.into()], "ir_cell").try_as_basic_value().unwrap_basic().into_pointer_value();
                                self.builder.store(cell, v);
                                temp_map.insert(*dst, cell.into());
                            }
                        }
                        Instruction::CellGet { dst, cell, ty } => {
                            if let Some(&c) = temp_map.get(cell) {
                                if c.is_pointer_value() {
                                    let llvm_ty = self.llvm_type(ty);
                                    let v = self.builder.load(llvm_ty, c.into_pointer_value(), "ir_cellget");
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
                                    // the cell holds an independent reference. Applies to concrete
                                    // reference-counted types AND boxed Json/union cells: the
                                    // lowerer uses the SAME owning model for unions (clone on
                                    // store, clone+register on read), so `emit_release` here
                                    // dispatches the tag-aware `lin_tagged_release`. The release
                                    // fns null-check the cell's initial zero.
                                    if Self::ty_is_concrete_rc(ty) || Self::is_union_type(ty) {
                                        let llvm_ty = self.llvm_type(ty);
                                        let old = self.builder
                                            .load(llvm_ty, c.into_pointer_value(), "ir_cell_old");
                                        self.emit_release(old, ty);
                                    }
                                    self.builder.store(c.into_pointer_value(), v);
                                }
                            }
                        }
                        Instruction::FreeCell { cell, ty } => {
                            if let Some(&c) = temp_map.get(cell) {
                                if c.is_pointer_value() {
                                    // Release the cell's CURRENT owned value, then free the cell
                                    // allocation. Mirrors CellSet's release-old (the cell holds
                                    // exactly one independent reference to its current value), but
                                    // there is no new value to store — this is the cell's final
                                    // teardown at the creating function's scope exit. Only emitted
                                    // for provably-non-escaping cells (lowerer escape analysis), so
                                    // no surviving closure can read the cell after this.
                                    let llvm_ty = self.llvm_type(ty);
                                    if Self::ty_is_concrete_rc(ty) || Self::is_union_type(ty) {
                                        let old = self.builder
                                            .load(llvm_ty, c.into_pointer_value(), "ir_cell_final");
                                        self.emit_release(old, ty);
                                    }
                                    // Free the raw cell allocation (no refcount header). Size
                                    // matches MakeCell's `lin_alloc(size_of ty)`.
                                    let size = llvm_ty.size_of().unwrap();
                                    let size_i64 = self.builder.int_z_extend_or_bit_cast(size, i64_ty, "cell_free_sz");
                                    let free_fn = self.get_or_declare_fn(
                                        "lin_cell_free",
                                        self.context.void_type().fn_type(&[ptr_ty.into(), i64_ty.into()], false),
                                    );
                                    self.builder.call(free_fn, &[c.into_pointer_value().into(), size_i64.into()], "");
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
                                        self.builder.gep(i8_ty, env_v.into_pointer_value(), &[offset], "ir_capgep")
                                    };
                                    let load_ty = self.llvm_type(ty);
                                    let loaded = self.builder.load(load_ty, gep, "ir_cap");
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
                        Instruction::SumTagEq { dst, val, sum_ty, disc_value } => {
                            if let Some(&v) = temp_map.get(val) {
                                // The O(1) sum dispatch: load the inline tag and compare to the
                                // variant's static tag. Read the proven repr — only emit the tag
                                // compare when `val` is genuinely a SumNode (the lowerer guarantees
                                // it, but fall back to a materialize+disc-string compare otherwise so
                                // a mis-seeded boxed value can never read garbage).
                                let result = if func.repr_of(*val).sumnode_sum_ty().is_some() {
                                    let tag = self.sumnode_tag_load(v);
                                    let want = Self::sumnode_variant_tag(sum_ty, disc_value)
                                        .expect("SumTagEq: unknown variant");
                                    self.builder.int_compare(
                                        inkwell::IntPredicate::EQ,
                                        tag,
                                        self.context.i32_type().const_int(want as u64, false),
                                        "sumtag_eq",
                                    )
                                } else {
                                    // Defensive fallback: materialize + boxed disc-string compare.
                                    let llvm_fn = self.builder.get_insert_block().unwrap().get_parent().unwrap();
                                    let obj = self.sumnode_materialize_to_object(v, sum_ty, llvm_fn);
                                    let boxed = self.box_value(obj, &Self::sumnode_first_variant_obj_ty(sum_ty));
                                    let disc_key = Self::sum_type_discriminant(sum_ty).unwrap_or_default();
                                    let kp = self.compile_string_lit(&disc_key).into_pointer_value();
                                    let got = self.builder.call(self.rt.object_get, &[boxed.into(), kp.into()], "").try_as_basic_value().unwrap_basic();
                                    let litr = self.compile_string_lit(disc_value);
                                    let lit = self.box_value(litr, &Type::Str);
                                    let eqfn = self.get_or_declare_fn("lin_tagged_eq", self.context.i8_type().fn_type(&[ptr_ty.into(), ptr_ty.into()], false));
                                    let e = self.builder.call(eqfn, &[got.into(), lit.into()], "").try_as_basic_value().unwrap_basic().into_int_value();
                                    self.builder.int_truncate_or_bit_cast(e, self.context.bool_type(), "")
                                };
                                temp_map.insert(*dst, result.into());
                            }
                        }
                        Instruction::HasPattern { dst, val, pattern } => {
                            if let Some(&v) = temp_map.get(val) {
                                let result = self.compile_ir_has_pattern(v, pattern);
                                temp_map.insert(*dst, result.into());
                            }
                        }
                        Instruction::MatchesSchema { dst, val, target, named_defs } => {
                            if let Some(&v) = temp_map.get(val) {
                                let result = self.compile_ir_matches_schema(v, target, named_defs);
                                temp_map.insert(*dst, result.into());
                            }
                        }
                        Instruction::Coerce { dst, src, from_ty, to_ty } => {
                            if let Some(&sv) = temp_map.get(src) {
                                // The SOURCE operand's repr decides the sum-type coercion direction:
                                // a SumNode source materializes/projects via the `sumnode_*` helpers,
                                // not the boxed `sealed_project_from`/`box` path (which would read a
                                // SumNode pointer as a TaggedVal → UAF). Threaded for the call ABI.
                                let src_repr = func.repr_of(*src);
                                let result = self.compile_ir_coerce_with_repr(sv, from_ty, to_ty, &src_repr, llvm_fn);
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
                                    self.builder.call(self.rt.panic, &[msg_v.into(), zero.into(), zero.into()], "");
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
                        Instruction::BoxKeepPacked { dst, src, arr, .. } => {
                            if let Some(&v) = temp_map.get(src) {
                                let result = self.compile_ir_box_keep_packed(v, *arr);
                                temp_map.insert(*dst, result);
                            }
                        }
                        Instruction::UnboxKeepPacked { dst, src, arr, .. } => {
                            if let Some(&v) = temp_map.get(src) {
                                let result = self.compile_ir_unbox_keep_packed(v, *arr);
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
                        let ret_val = temp_map.get(t).copied();
                        // TCO LOOP-EXIT release (Leak B fix): when this function is a TCO loop,
                        // each owned param slot may hold a value the loop PRODUCED on a prior
                        // back-edge (tracked by `tco_owns[i]`). The back-edge machinery releases
                        // INTERMEDIATE owned values when they are overwritten, but the FINAL value
                        // left in the slot when the loop returns was never released — it leaks once
                        // per outer call (e.g. the `cur: T|Null` threaded through `scanRouteAt`).
                        // Release it here, gated on the runtime owns-flag and (defensively) on the
                        // slot not aliasing the returned value (a function that returns its own
                        // owned param would otherwise double-free). Done BEFORE the `ret`.
                        let ret_ptr = ret_val.and_then(|v| if v.is_pointer_value() { Some(v.into_pointer_value()) } else { None });
                        // ALL pointer-typed entry params (the borrowed caller-owned values). A TCO
                        // loop may PERMUTE borrowed array params between slots (the merge-sort
                        // ping-pong), so an exit slot must be guarded against EVERY entry, not just
                        // its own — see emit_tco_release_final.
                        let tco_entry_ptrs: Vec<inkwell::values::PointerValue<'ctx>> = (0..func.params.len())
                            .filter_map(|i| llvm_fn.get_nth_param(i as u32))
                            .filter_map(|p| if p.is_pointer_value() { Some(p.into_pointer_value()) } else { None })
                            .collect();
                        for (i, (_t, ty)) in func.params.iter().enumerate() {
                            if let Some(Some(owns)) = tco_owns.get(i) {
                                if let Some(slot) = param_allocs.get(i) {
                                    let llvm_ty = self.llvm_type(ty);
                                    let slot_val = self.builder.load(llvm_ty, *slot, "tco_fslot");
                                    if slot_val.is_pointer_value() {
                                        self.emit_tco_release_final(llvm_fn, *owns, slot_val.into_pointer_value(), &tco_entry_ptrs, ret_ptr, ty);
                                    }
                                }
                            }
                        }
                        if let Some(v) = ret_val {
                            self.builder.r#return(Some(&v));
                        } else {
                            self.builder.r#return(None);
                        }
                    }
                    Terminator::Return(None) => {
                        // TCO LOOP-EXIT release (Leak B fix): see Return(Some) above. No return
                        // value, so no return-alias guard needed.
                        let tco_entry_ptrs: Vec<inkwell::values::PointerValue<'ctx>> = (0..func.params.len())
                            .filter_map(|i| llvm_fn.get_nth_param(i as u32))
                            .filter_map(|p| if p.is_pointer_value() { Some(p.into_pointer_value()) } else { None })
                            .collect();
                        for (i, (_t, ty)) in func.params.iter().enumerate() {
                            if let Some(Some(owns)) = tco_owns.get(i) {
                                if let Some(slot) = param_allocs.get(i) {
                                    let llvm_ty = self.llvm_type(ty);
                                    let slot_val = self.builder.load(llvm_ty, *slot, "tco_fslot");
                                    if slot_val.is_pointer_value() {
                                        self.emit_tco_release_final(llvm_fn, *owns, slot_val.into_pointer_value(), &tco_entry_ptrs, None, ty);
                                    }
                                }
                            }
                        }
                        self.builder.r#return(None);
                    }
                    Terminator::Jump(target) => {
                        let target_bb = ir_block_to_llvm[target];
                        self.builder.unconditional_branch(target_bb);
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
                        self.builder.conditional_branch(cond_i1, then_bb, else_bb);
                    }
                    Terminator::TailCall { args } => {
                        // TCO: store the new argument values into the param allocas and
                        // branch back to the loop header (the function's first IR block).
                        //
                        // Per-iteration owned-value release (fixes the dominant TCO loop leak):
                        // each owned (refcounted) param slot holds a reference that the function
                        // owns under the calling convention. Storing the next iteration's value
                        // OVER it without releasing the OLD value leaks one reference every
                        // iteration — e.g. a tail-recursive function whose recurring arg is a
                        // FRESH array/object/string/map/union built each round (RAPTOR's
                        // `scanRounds`/`getMarkedStops`). The scope-exit release the lowerer emits
                        // for these lands in the unreachable `tco_post` block (the back-edge means
                        // scope exit is never reached), so it never runs. Release the old value
                        // here instead.
                        //
                        // ALIAS HAZARD: the new value for a slot may BE the old value (a
                        // pass-through param threaded unchanged, e.g. a large borrowed `raptor`
                        // arg) or some OTHER new arg may alias this slot's old value. Releasing an
                        // old pointer that any new arg still references is a use-after-free /
                        // double-free. Guard with a runtime pointer compare: release `old_i` only
                        // if it differs from EVERY new arg value being stored this iteration.
                        //
                        // Capture every new value FIRST (they are already computed in temp_map),
                        // then load+conditionally-release each owned old value, then store. We do
                        // the release before the store so the slot still holds the old value when
                        // we load it; loads happen for all slots up front so a later store can't
                        // clobber an earlier old-value load.
                        let new_vals: Vec<Option<BasicValueEnum<'ctx>>> =
                            args.iter().map(|t| temp_map.get(t).copied()).collect();
                        // Pointer-typed new values that an old value could alias (skip non-pointers).
                        let new_ptrs: Vec<inkwell::values::PointerValue<'ctx>> = new_vals
                            .iter()
                            .filter_map(|v| v.and_then(|v| if v.is_pointer_value() { Some(v.into_pointer_value()) } else { None }))
                            .collect();
                        // Load all old values before storing any new value (a later store must not
                        // clobber an earlier old-value load when params share no slot, but be safe).
                        let mut old_vals: Vec<Option<BasicValueEnum<'ctx>>> = Vec::with_capacity(param_allocs.len());
                        for (i, (_t, ty)) in func.params.iter().enumerate() {
                            if tco_owns.get(i).copied().flatten().is_some() && param_allocs.get(i).is_some() {
                                let llvm_ty = self.llvm_type(ty);
                                old_vals.push(Some(self.builder.load(llvm_ty, param_allocs[i], "tco_old")));
                            } else {
                                old_vals.push(None);
                            }
                        }
                        // Conditionally release each LOOP-OWNED old value (guarded against aliasing).
                        // Only release when this slot's value was stored by a prior tail iteration
                        // (`tco_owns[i]` is true) — never the borrowed entry param — AND the old
                        // pointer differs from every new arg value being stored this iteration.
                        for (i, (_t, ty)) in func.params.iter().enumerate() {
                            if let (Some(old), Some(Some(owns))) = (old_vals[i], tco_owns.get(i)) {
                                if old.is_pointer_value() {
                                    let repr = func.repr_of(*_t).clone();
                                    self.emit_tco_release_old(llvm_fn, *owns, old.into_pointer_value(), &new_ptrs, ty, &repr);
                                }
                            }
                        }
                        // Now store the new values, and mark every owned slot as loop-owned (the
                        // value we just stored was produced by this iteration's body).
                        let bool_ty = self.context.bool_type();
                        for (i, &v) in new_vals.iter().enumerate() {
                            if let (Some(v), Some(slot)) = (v, param_allocs.get(i)) {
                                self.builder.store(*slot, v);
                            }
                            if let Some(Some(owns)) = tco_owns.get(i) {
                                self.builder.store(*owns, bool_ty.const_int(1, false));
                            }
                        }
                        if let Some(first_ir_bb) = func.blocks.first().and_then(|b| ir_block_to_llvm.get(&b.id)) {
                            self.builder.unconditional_branch(*first_ir_bb);
                        } else {
                            self.builder.unreachable();
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
                                self.builder.switch(int_v, def_bb, &case_bbs);
                            } else {
                                let def_bb = ir_block_to_llvm[default];
                                self.builder.unconditional_branch(def_bb);
                            }
                        } else {
                            self.builder.unreachable();
                        }
                    }
                    Terminator::Unreachable => {
                        self.builder.unreachable();
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

/// Whether a type mentions an unresolved generic `TypeVar` anywhere in its structure. Used by
/// coverage to recognise a kept GENERIC ORIGINAL function (whose params/return still name a
/// quantified type variable) and skip emitting its always-zero regions — its monomorphized
/// specializations carry the real coverage for the same source lines (attributed via
/// `LinFunction.coverage_origin`).
fn type_mentions_typevar(ty: &Type) -> bool {
    match ty {
        // The Json wildcard `TypeVar(u32::MAX)` is a concrete dynamic type, NOT a quantified
        // generic param — a function returning/taking Json is a real, callable function.
        Type::TypeVar(id) => *id != u32::MAX,
        Type::Array(t) | Type::Iterator(t) | Type::Shared(t) | Type::Stream(t) | Type::Map(t) => {
            type_mentions_typevar(t)
        }
        Type::FixedArray(ts) | Type::Union(ts) => ts.iter().any(type_mentions_typevar),
        Type::Function { params, ret, .. } => {
            params.iter().any(type_mentions_typevar) || type_mentions_typevar(ret)
        }
        Type::Object { fields, .. } => fields.values().any(type_mentions_typevar),
        _ => false,
    }
}
