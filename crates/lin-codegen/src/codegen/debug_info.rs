//! DWARF debug-info emission for `--debug` builds (Phase 1: line tables).
//!
//! When `lin build --debug` is used, codegen builds a per-module `DebugInfoBuilder` +
//! `DICompileUnit` naming the `.lin` source file, emits one `DISubprogram` per Lin function (so the
//! debugger sees function boundaries), and—before emitting each IR instruction that carries a source
//! span—sets the current `DILocation` (line, col, function scope). This produces a DWARF `.debug_line`
//! table mapping machine instructions back to `.lin` source lines, which is what gives CodeLLDB /
//! lldb real source-line breakpoints and single-stepping.
//!
//! The byte-offset → (line, col) conversion reuses [`crate::coverage::offset_to_line_col`] so the
//! line mapping is identical to the coverage subsystem's.
//!
//! Scope (Phase 1): line tables only — no variable/type info (that is Phase 3). The compile unit is
//! marked NOT optimized; `--debug` forces O0 so the mapping holds.

use inkwell::debug_info::{
    AsDIScope, DICompileUnit, DIFile, DIFlagsConstants, DISubprogram, DWARFEmissionKind,
    DWARFSourceLanguage, DebugInfoBuilder,
};
use inkwell::module::{FlagBehavior, Module};
use inkwell::values::FunctionValue;
use std::collections::HashMap;

use crate::coverage::offset_to_line_col;
use lin_ir::ir as lir;

/// All DWARF state for the module currently being compiled. Created once, when the main module's
/// source path is known (`set_main_source` equivalent for debug). Lives in [`crate::codegen::Codegen`]
/// behind an `Option`, so it is entirely absent for non-debug builds.
pub struct DebugInfoState<'ctx> {
    pub builder: DebugInfoBuilder<'ctx>,
    pub compile_unit: DICompileUnit<'ctx>,
    pub file: DIFile<'ctx>,
    /// The main module's source text, for byte-offset → (line, col) conversion of instruction spans.
    pub main_source: String,
    /// Per-IR-function `DISubprogram` scope, keyed by `FuncId`. Used as the scope of every
    /// `DILocation` emitted while compiling that function's body.
    pub subprograms: HashMap<lir::FuncId, DISubprogram<'ctx>>,
}

impl<'ctx> DebugInfoState<'ctx> {
    /// Initialise the module's debug info: add the "Debug Info Version" module flag (so the metadata
    /// survives into the object file), then create the `DebugInfoBuilder` + `DICompileUnit` for the
    /// given `.lin` source file. `abs_source_path` should be an absolute path so the debugger can
    /// locate the file; `source_text` is its contents (for line mapping).
    pub fn new(
        context: &'ctx inkwell::context::Context,
        module: &Module<'ctx>,
        abs_source_path: &str,
        source_text: &str,
    ) -> Self {
        // Required so LLVM keeps the !llvm.dbg.cu / DILocation metadata through to the object file.
        let debug_metadata_version = context.i32_type().const_int(3, false);
        module.add_basic_value_flag(
            "Debug Info Version",
            FlagBehavior::Warning,
            debug_metadata_version,
        );

        let path = std::path::Path::new(abs_source_path);
        let filename = path
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| abs_source_path.to_string());
        let directory = path
            .parent()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|| ".".to_string());

        let (builder, compile_unit) = module.create_debug_info_builder(
            /* allow_unresolved */ true,
            // Lin has no DWARF language code; C is the conventional choice for a C-ABI native
            // frontend and lldb handles it fine for line-table stepping.
            DWARFSourceLanguage::C,
            &filename,
            &directory,
            /* producer */ "lin",
            /* is_optimized */ false,
            /* flags */ "",
            /* runtime_ver */ 0,
            /* split_name */ "",
            DWARFEmissionKind::Full,
            /* dwo_id */ 0,
            /* split_debug_inlining */ false,
            /* debug_info_for_profiling */ false,
            /* sysroot */ "",
            /* sdk */ "",
        );
        let file = compile_unit.get_file();

        Self {
            builder,
            compile_unit,
            file,
            main_source: source_text.to_string(),
            subprograms: HashMap::new(),
        }
    }

    /// Create and attach a `DISubprogram` for one Lin function, and record it for later
    /// `DILocation` scoping. `symbol` is the emitted LLVM symbol (used as the linkage name);
    /// `def_offset` is the byte offset of the function's definition span (for the declared line).
    pub fn declare_function(
        &mut self,
        context: &'ctx inkwell::context::Context,
        func_id: lir::FuncId,
        llvm_fn: FunctionValue<'ctx>,
        symbol: &str,
        def_offset: u32,
    ) {
        let (line, _col) = offset_to_line_col(&self.main_source, def_offset);
        // Phase 1 emits line tables only: a parameterless subroutine type is sufficient for the
        // debugger to resolve the function and step through its lines (variable/return types are
        // Phase 3). Reuse one void-ish subroutine type per function.
        let subroutine_type = self.builder.create_subroutine_type(
            self.file,
            /* return type */ None,
            /* parameter types */ &[],
            inkwell::debug_info::DIFlags::PUBLIC,
        );
        let subprogram = self.builder.create_function(
            self.compile_unit.as_debug_info_scope(),
            symbol,
            Some(symbol),
            self.file,
            line,
            subroutine_type,
            /* is_local_to_unit */ false,
            /* is_definition */ true,
            /* scope_line */ line,
            inkwell::debug_info::DIFlags::PUBLIC,
            /* is_optimized */ false,
        );
        llvm_fn.set_subprogram(subprogram);
        self.subprograms.insert(func_id, subprogram);
        let _ = context;
    }

    /// Build a `DILocation` for `offset` scoped to `func_id`'s subprogram. Returns `None` if no
    /// subprogram was declared for this function (e.g. an import without registered source).
    pub fn location_for(
        &self,
        context: &'ctx inkwell::context::Context,
        func_id: lir::FuncId,
        offset: u32,
    ) -> Option<inkwell::debug_info::DILocation<'ctx>> {
        let subprogram = self.subprograms.get(&func_id)?;
        let (line, col) = offset_to_line_col(&self.main_source, offset);
        Some(self.builder.create_debug_location(
            context,
            line,
            col,
            subprogram.as_debug_info_scope(),
            /* inlined_at */ None,
        ))
    }

    /// Finalise all debug metadata. Must be called before object emission, after every function's
    /// locations have been emitted.
    pub fn finalize(&self) {
        self.builder.finalize();
    }
}
