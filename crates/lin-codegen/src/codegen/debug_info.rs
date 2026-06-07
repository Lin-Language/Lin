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
//! Phase 3: named local variables. Each Lin `val`/`var`/parameter binding is emitted as a
//! `DILocalVariable` (a `DW_TAG_variable` / `DW_TAG_formal_parameter`) typed so the Phase 2 lldb
//! formatters apply automatically. The lowerer marks each binding with an `Instruction::DebugDeclare`
//! (temp + source name + Lin type); codegen (under `--debug`) stores the binding's value into a
//! per-variable stack "home" alloca and emits `llvm.dbg.declare` over it (O0 keeps the home, giving
//! the variable a stable address so it reliably shows in `frame variable` / the Variables panel).
//!
//! Type-name linkage (the crux of "auto-render"): the Phase 2 formatters are registered against the
//! linked runtime struct names (`TaggedVal`, `LinArray`, `LinString`, `LinObject`). We emit each
//! pointer-shaped Lin local with a DWARF pointer-to-struct DIType whose pointee STRUCT NAME is
//! exactly one of those names, and `lin_formatters.py` also registers the matching `<Name> *` forms,
//! so lldb runs the same summary/synthetic provider on the Lin local. Scalar locals (Int/Float/Bool)
//! get a primitive DIType and render as their raw (logical) value.
//!
//! The compile unit is marked NOT optimized; `--debug` forces O0 so the mapping holds.

use inkwell::debug_info::{
    AsDIScope, DICompileUnit, DIFile, DIFlagsConstants, DIScope, DISubprogram, DIType,
    DWARFEmissionKind, DWARFSourceLanguage, DebugInfoBuilder,
};
use inkwell::llvm_sys;
use inkwell::module::{FlagBehavior, Module};
use inkwell::values::{AsValueRef, FunctionValue, PointerValue};
use inkwell::AddressSpace;
use std::collections::HashMap;

use crate::coverage::offset_to_line_col;
use lin_check::types::Type;
use lin_ir::ir as lir;

// DWARF base-type encodings (DW_ATE_*), for `create_basic_type`.
const DW_ATE_BOOLEAN: u32 = 0x02;
const DW_ATE_FLOAT: u32 = 0x04;
const DW_ATE_SIGNED: u32 = 0x05;
const DW_ATE_UNSIGNED: u32 = 0x07;

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
    /// Cache of `DIType`s by a small key string, so identical types reuse one metadata node
    /// (LLVM also dedups, but this avoids rebuilding the named runtime-struct pointer types per
    /// variable). Phase 3 only.
    ditype_cache: HashMap<String, DIType<'ctx>>,
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
            ditype_cache: HashMap::new(),
        }
    }

    /// A DWARF pointer-to-named-struct `DIType` whose pointee struct is named `struct_name`
    /// (e.g. `"TaggedVal"`). The struct body is left empty (forward-declared / opaque): the Phase 2
    /// formatters read the target's bytes by raw offset, so they only need lldb to RESOLVE the
    /// pointer's type name to one the formatter is registered against (`<struct_name> *`). Cached.
    fn runtime_ptr_type(
        &mut self,
        context: &'ctx inkwell::context::Context,
        struct_name: &str,
    ) -> DIType<'ctx> {
        if let Some(t) = self.ditype_cache.get(struct_name) {
            return *t;
        }
        let ptr_bits = 64;
        // An opaque struct DIType named exactly `struct_name` so lldb reports the pointee as that
        // type (matching the formatter registrations + the linked runtime's own DWARF copy).
        let st = self.builder.create_struct_type(
            self.file.as_debug_info_scope(),
            struct_name,
            self.file,
            /* line */ 0,
            /* size_in_bits */ 0,
            /* align_in_bits */ 0,
            inkwell::debug_info::DIFlags::ZERO,
            /* derived_from */ None,
            /* elements */ &[],
            /* runtime_language */ 0,
            /* vtable_holder */ None,
            /* unique_id */ struct_name,
        );
        let pt = self
            .builder
            .create_pointer_type(
                "",
                st.as_type(),
                ptr_bits,
                ptr_bits as u32,
                AddressSpace::default(),
            )
            .as_type();
        self.ditype_cache.insert(struct_name.to_string(), pt);
        let _ = context;
        pt
    }

    /// A primitive DWARF base type (cached) for scalar Lin locals.
    fn basic_type(&mut self, name: &str, size_bits: u64, encoding: u32) -> DIType<'ctx> {
        if let Some(t) = self.ditype_cache.get(name) {
            return *t;
        }
        let t = self
            .builder
            .create_basic_type(name, size_bits, encoding, inkwell::debug_info::DIFlags::ZERO)
            .expect("non-empty basic-type name")
            .as_type();
        self.ditype_cache.insert(name.to_string(), t);
        t
    }

    /// Map a Lin `Type` to the `DIType` to attach to a local of that type. Pointer-shaped Lin
    /// values use a pointer-to-named-runtime-struct so the Phase 2 lldb formatters apply
    /// (`TaggedVal`/`LinArray`/`LinString`/`LinObject`); scalars use a primitive base type and
    /// render as their raw logical value.
    fn ditype_for(&mut self, context: &'ctx inkwell::context::Context, ty: &Type) -> DIType<'ctx> {
        match ty {
            Type::Bool => self.basic_type("Boolean", 8, DW_ATE_BOOLEAN),
            Type::Int8 => self.basic_type("Int8", 8, DW_ATE_SIGNED),
            Type::Int16 => self.basic_type("Int16", 16, DW_ATE_SIGNED),
            Type::Int32 => self.basic_type("Int32", 32, DW_ATE_SIGNED),
            Type::Int64 => self.basic_type("Int64", 64, DW_ATE_SIGNED),
            Type::UInt8 => self.basic_type("UInt8", 8, DW_ATE_UNSIGNED),
            Type::UInt16 => self.basic_type("UInt16", 16, DW_ATE_UNSIGNED),
            Type::UInt32 => self.basic_type("UInt32", 32, DW_ATE_UNSIGNED),
            Type::UInt64 => self.basic_type("UInt64", 64, DW_ATE_UNSIGNED),
            Type::Float32 => self.basic_type("Float32", 32, DW_ATE_FLOAT),
            Type::Float64 => self.basic_type("Float64", 64, DW_ATE_FLOAT),
            Type::Str | Type::StrLit(_) => self.runtime_ptr_type(context, "LinString"),
            Type::Array(_) | Type::FixedArray(_) | Type::Iterator(_) => {
                self.runtime_ptr_type(context, "LinArray")
            }
            // Concrete objects are a `LinObject*`. Maps are a `LinMap*` (no dedicated formatter;
            // the TaggedVal renderer shows them minimally) — route through the TaggedVal type so a
            // boxed map still gets the dispatcher.
            Type::Object { .. } => self.runtime_ptr_type(context, "LinObject"),
            // Everything else is a boxed `TaggedVal*` at runtime (union / Json / Null / Named /
            // function / shared / stream / map): the TaggedVal summary+synthetic dispatcher decodes
            // the tag and renders/expands whatever it holds.
            _ => self.runtime_ptr_type(context, "TaggedVal"),
        }
    }

    /// Emit a `DILocalVariable` (`DW_TAG_variable` / `DW_TAG_formal_parameter`) for a Lin binding and
    /// an `llvm.dbg.declare` associating it with the stack `storage` slot. `arg_no` is `Some(n)`
    /// (1-based) for a parameter, `None` for a `val`/`var`. `def_offset` is the binding-site byte
    /// offset (for the declared line). `subprogram` is the enclosing function's `DISubprogram` (read
    /// directly from the physical LLVM function via `get_subprogram`, so the variable's scope always
    /// matches the function the `storage` alloca lives in — keying by `FuncId` is unreliable across
    /// monomorphized specializations whose IR ids do not line up with the `subprograms` map).
    pub fn declare_local(
        &mut self,
        context: &'ctx inkwell::context::Context,
        subprogram: DISubprogram<'ctx>,
        name: &str,
        ty: &Type,
        arg_no: Option<u32>,
        def_offset: u32,
        storage: PointerValue<'ctx>,
        block: inkwell::basic_block::BasicBlock<'ctx>,
    ) {
        let (line, col) = offset_to_line_col(&self.main_source, def_offset);
        let dity = self.ditype_for(context, ty);
        let scope: DIScope<'ctx> = subprogram.as_debug_info_scope();
        let var = match arg_no {
            Some(n) => self.builder.create_parameter_variable(
                scope, name, n, self.file, line, dity, /* always_preserve */ true,
                inkwell::debug_info::DIFlags::ZERO,
            ),
            None => self.builder.create_auto_variable(
                scope, name, self.file, line, dity, /* always_preserve */ true,
                inkwell::debug_info::DIFlags::ZERO, /* align */ 0,
            ),
        };
        let loc = self.builder.create_debug_location(
            context, line, col, scope, /* inlined_at */ None,
        );
        // NB: we deliberately call the raw `LLVMDIBuilderInsertDeclareRecordAtEnd` rather than
        // inkwell 0.9's `insert_declare_at_end`. On LLVM 19+ (we build against LLVM 22) the insert
        // intrinsic returns a `DbgRecord`, not a `Value`; inkwell's wrapper unconditionally feeds
        // that ref to `InstructionValue::new`, whose `assert!(value.is_instruction())` then panics.
        // Going through the FFI directly (and discarding the returned record) sidesteps that bug.
        let empty_expr = self.builder.create_expression(Vec::new());
        unsafe {
            llvm_sys::debuginfo::LLVMDIBuilderInsertDeclareRecordAtEnd(
                self.builder.as_mut_ptr(),
                storage.as_value_ref(),
                var.as_mut_ptr(),
                empty_expr.as_mut_ptr(),
                loc.as_mut_ptr(),
                block.as_mut_ptr(),
            );
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
