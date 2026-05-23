//! Binary production pipeline for Lin.
//! Orchestrates: source -> lex -> parse -> type check -> LLVM codegen -> link -> binary.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use inkwell::context::Context;
use lin_check::typed_ir::{TypedModule, TypedStmt};
use lin_check::types::Type;
use lin_check::Checker;
use lin_codegen::Codegen;
use lin_lex::Lexer;
use lin_parse::ast::{Module, Stmt};
use lin_parse::Parser;

#[derive(Debug)]
pub struct CompileOptions {
    pub source_path: PathBuf,
    pub output_path: PathBuf,
    pub emit_ir: bool,
    pub optimize: bool,
}

#[derive(Debug)]
pub enum CompileError {
    Io(std::io::Error),
    TypeCheck(Vec<lin_common::Diagnostic>),
    Codegen(String),
    Link(String),
}

impl std::fmt::Display for CompileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CompileError::Io(e) => write!(f, "I/O error: {}", e),
            CompileError::TypeCheck(diags) => {
                for d in diags {
                    writeln!(f, "type error: {}", d.message)?;
                }
                Ok(())
            }
            CompileError::Codegen(msg) => write!(f, "codegen error: {}", msg),
            CompileError::Link(msg) => write!(f, "link error: {}", msg),
        }
    }
}

impl From<std::io::Error> for CompileError {
    fn from(e: std::io::Error) -> Self {
        CompileError::Io(e)
    }
}

pub fn compile(opts: &CompileOptions) -> Result<(), CompileError> {
    // 1. Read source
    let source = std::fs::read_to_string(&opts.source_path)?;
    let module_name = opts.source_path
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    let base_dir = opts.source_path.parent().unwrap_or(Path::new(".")).to_path_buf();

    // 2. Lex + Parse
    let mut lexer = Lexer::new(&source, 0);
    let tokens = lexer.tokenize();
    let mut parser = Parser::new(tokens);
    let ast_module = parser.parse_module();

    // 3a. Pre-resolve imports (parse + type-check imported modules) so we know
    //     the real export types before checking the main module.
    let mut imported_modules: HashMap<String, TypedModule> = HashMap::new();
    pre_resolve_imports_from_ast(&ast_module, &base_dir, &mut imported_modules)?;

    // 3b. Build the import_types map: (module_path, export_name) -> Type
    let mut import_type_map: HashMap<(String, String), Type> = HashMap::new();
    for (path, imp_module) in &imported_modules {
        for (name, ty) in extract_exports(imp_module) {
            import_type_map.insert((path.clone(), name), ty);
        }
    }

    // 3c. Type check main module with pre-resolved import types.
    let mut checker = Checker::new();
    checker.import_types = import_type_map;
    let typed_module = checker
        .check_module(&ast_module)
        .map_err(CompileError::TypeCheck)?;

    // 4. LLVM codegen
    let context = Context::create();
    let mut cg = Codegen::new(&context, &module_name);

    // Register imported modules with codegen so import slots get correct function pointers.
    for (path, imp_module) in &imported_modules {
        cg.register_import(path, imp_module);
    }

    cg.compile_module(&typed_module);

    // 5. Emit LLVM IR if requested (before verify so we can inspect broken IR)
    if opts.emit_ir {
        let ir_path = opts.output_path.with_extension("ll");
        cg.emit_llvm_ir(&ir_path).map_err(CompileError::Codegen)?;
    }

    if opts.optimize {
        cg.run_optimization_passes().map_err(CompileError::Codegen)?;
    }

    cg.verify().map_err(CompileError::Codegen)?;

    // 6. Emit object file
    let obj_path = opts.output_path.with_extension("o");
    cg.emit_object_file(&obj_path).map_err(CompileError::Codegen)?;

    // 7. Link with runtime
    link(&obj_path, &opts.output_path)?;

    // Clean up the .o file.
    let _ = std::fs::remove_file(&obj_path);

    Ok(())
}

/// Extract (name, type) pairs for all exported/top-level named bindings in a TypedModule.
fn extract_exports(module: &TypedModule) -> Vec<(String, Type)> {
    let mut result = Vec::new();
    for stmt in &module.statements {
        match stmt {
            TypedStmt::Val { name: Some(n), ty, .. } => {
                result.push((n.clone(), ty.clone()));
            }
            _ => {}
        }
    }
    result
}

/// Embedded stdlib source files (mirrors interpreter's include_str! approach).
fn stdlib_source(path: &str) -> Option<&'static str> {
    match path {
        "std/io"     => Some(include_str!("../../../stdlib/io.lin")),
        "std/string" => Some(include_str!("../../../stdlib/string.lin")),
        "std/number" => Some(include_str!("../../../stdlib/number.lin")),
        "std/array"  => Some(include_str!("../../../stdlib/array.lin")),
        "std/iter"   => Some(include_str!("../../../stdlib/iter.lin")),
        "std/result" => Some(include_str!("../../../stdlib/result.lin")),
        _ => None,
    }
}

/// Pre-resolve imports from a parsed AST module (before type-checking the main module).
fn pre_resolve_imports_from_ast(
    ast_module: &Module,
    base_dir: &Path,
    cache: &mut HashMap<String, TypedModule>,
) -> Result<(), CompileError> {
    for stmt in &ast_module.statements {
        if let Stmt::Import { path, .. } = stmt {
            if cache.contains_key(path.as_str()) {
                continue;
            }
            let source: String;
            let ast_mod;
            if let Some(stdlib_src) = stdlib_source(path.as_str()) {
                // Parse stdlib embedded source.
                let mut lexer = Lexer::new(stdlib_src, 0);
                let tokens = lexer.tokenize();
                let mut parser = Parser::new(tokens);
                ast_mod = parser.parse_module();
                // Recursively resolve any stdlib imports this module depends on.
                pre_resolve_imports_from_ast(&ast_mod, base_dir, cache)?;
            } else {
                let file_path = base_dir.join(format!("{}.lin", path));
                source = std::fs::read_to_string(&file_path).map_err(CompileError::Io)?;
                let mut lexer = Lexer::new(&source, 0);
                let tokens = lexer.tokenize();
                let mut parser = Parser::new(tokens);
                ast_mod = parser.parse_module();
                // Recursively pre-resolve imports in the imported module first.
                let imported_base = file_path.parent().unwrap_or(base_dir).to_path_buf();
                pre_resolve_imports_from_ast(&ast_mod, &imported_base, cache)?;
            }
            // Build import_types for transitive imports.
            let mut import_type_map: HashMap<(String, String), Type> = HashMap::new();
            for (dep_path, dep_module) in cache.iter() {
                for (name, ty) in extract_exports(dep_module) {
                    import_type_map.insert((dep_path.clone(), name), ty);
                }
            }
            let mut checker = Checker::new();
            checker.import_types = import_type_map;
            let typed = checker.check_module(&ast_mod).map_err(CompileError::TypeCheck)?;
            cache.insert(path.clone(), typed);
        }
    }
    Ok(())
}

fn link(obj_path: &Path, output_path: &Path) -> Result<(), CompileError> {
    // Find the lin-runtime static library.
    // When running from a cargo build, it should be in the same target directory.
    let runtime_lib = find_runtime_lib();

    let mut cmd = Command::new("cc");
    cmd.arg(obj_path)
        .arg("-o")
        .arg(output_path);

    if let Some(lib) = &runtime_lib {
        cmd.arg(lib);
    } else {
        // Try to find it relative to the cargo output directory.
        // Fall back to assuming it's installed system-wide (future: pkg-config).
        eprintln!("Warning: lin-runtime library not found, linking may fail");
    }

    // Link system libraries needed by lin-runtime (libc via cc).
    let status = cmd.status().map_err(|e| CompileError::Link(e.to_string()))?;

    if !status.success() {
        return Err(CompileError::Link(format!(
            "linker exited with status {}",
            status
        )));
    }

    Ok(())
}

fn find_runtime_lib() -> Option<PathBuf> {
    // Check standard cargo target directories in order.
    let candidates = [
        // Development build (running from workspace)
        "target/debug/liblin_runtime.a",
        "target/release/liblin_runtime.a",
        "../target/debug/liblin_runtime.a",
        "../target/release/liblin_runtime.a",
    ];

    for candidate in &candidates {
        let path = Path::new(candidate);
        if path.exists() {
            return Some(path.to_path_buf());
        }
    }

    // Try CARGO_MANIFEST_DIR-relative paths (works in tests).
    if let Ok(manifest) = std::env::var("CARGO_MANIFEST_DIR") {
        let base = Path::new(&manifest);
        for candidate in &candidates {
            let path = base.join(candidate);
            if path.exists() {
                return Some(path);
            }
            // Go up one level (workspace root)
            if let Some(parent) = base.parent() {
                let path = parent.join(candidate);
                if path.exists() {
                    return Some(path);
                }
            }
        }
    }

    None
}
