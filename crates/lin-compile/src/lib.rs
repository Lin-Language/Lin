//! Binary production pipeline for Lin.
//! Orchestrates: source -> lex -> parse -> type check -> LLVM codegen -> link -> binary.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use inkwell::context::Context;
use lin_check::typed_ir::TypedModule;
use lin_check::types::Type;
use lin_check::{Checker, ModuleSignature};
use lin_codegen::Codegen;
use lin_lex::Lexer;
use lin_parse::ast::{Module, Stmt};
use lin_parse::Parser;
use lin_ir::{lower_module_with_imports, rc_elide};

#[derive(Debug)]
pub struct CompileOptions {
    pub source_path: PathBuf,
    pub output_path: PathBuf,
    pub emit_ir: bool,
    pub optimize: bool,
    pub coverage: bool,
}

#[derive(Debug)]
pub struct CheckOptions {
    pub source_path: PathBuf,
}

#[derive(Debug)]
pub enum CompileError {
    Io(std::io::Error),
    TypeCheck(Vec<lin_common::Diagnostic>),
    Codegen(String),
    Link(String),
    /// A circular import was detected while resolving the module graph. Carries the
    /// cycle as a human-readable path chain (e.g. `a -> b -> a`).
    ImportCycle(String),
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
            CompileError::ImportCycle(chain) => {
                write!(f, "circular import detected: {}", chain)
            }
        }
    }
}

impl From<std::io::Error> for CompileError {
    fn from(e: std::io::Error) -> Self {
        CompileError::Io(e)
    }
}

/// The product of the shared front-end prefix: source text through type-checked main module
/// with all imports resolved. `compile()` continues into lowering/codegen; `check()` stops here.
struct CheckedFrontEnd {
    source: String,
    module_name: String,
    typed_module: TypedModule,
    imported_modules: HashMap<String, TypedModule>,
    import_order: Vec<String>,
    import_sources: HashMap<String, (String, String)>,
    /// Non-error diagnostics (warnings) produced while checking the main module.
    warnings: Vec<lin_common::Diagnostic>,
}

/// Run the shared front end: read source, lex+parse, recursively resolve+type-check imports,
/// then type-check the main module with the resolved import types. Used by both `compile()`
/// (which proceeds to codegen) and `check()` (which stops here), so the two never diverge on
/// how imports are resolved or how the module cache is consulted.
fn check_front_end(source_path: &Path) -> Result<CheckedFrontEnd, CompileError> {
    // 1. Read source
    let source = std::fs::read_to_string(source_path)?;
    let module_name = source_path
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    let base_dir = source_path.parent().unwrap_or(Path::new(".")).to_path_buf();

    // 2. Lex + Parse
    let ast_module = parse_source(&source).map_err(CompileError::TypeCheck)?;

    // 3a. Pre-resolve imports so we know real export types before checking the main module.
    // `import_order` preserves DFS insertion order so codegen registers dependencies first.
    let mut imported_modules: HashMap<String, TypedModule> = HashMap::new();
    let mut import_order: Vec<String> = Vec::new();
    // import_sources holds (abs_source_path, source_text) for user-defined (non-stdlib) imports only.
    let mut import_sources: HashMap<String, (String, String)> = HashMap::new();
    pre_resolve_imports_from_ast(&ast_module, &base_dir, source_path, &mut imported_modules, &mut import_order, &mut import_sources)?;

    // 3b. Type check main module with pre-resolved import types.
    let (typed_module, warnings) = check_module_with_imports(&ast_module, &imported_modules, false)
        .map_err(CompileError::TypeCheck)?;

    Ok(CheckedFrontEnd {
        source,
        module_name,
        typed_module,
        imported_modules,
        import_order,
        import_sources,
        warnings,
    })
}

/// Type-check `opts.source_path` and all of its (transitive) imports, stopping before any
/// lowering or codegen. Returns the non-error diagnostics (warnings) on success; errors are
/// returned via `Err(CompileError::TypeCheck(_))`. This shares the entire import-resolution
/// front end with `compile()`, so `lin check` and `lin build` agree on what they accept.
pub fn check(opts: &CheckOptions) -> Result<Vec<lin_common::Diagnostic>, CompileError> {
    let front = check_front_end(&opts.source_path)?;

    // Mirror `compile()`'s ADR-071 gate: `replace` is only valid in a `*.test.lin` file.
    let is_test_file = opts.source_path.to_string_lossy().ends_with(".test.lin");
    if !is_test_file && !front.typed_module.replacements.is_empty() {
        let span = front.typed_module.replacements[0].span;
        return Err(CompileError::TypeCheck(vec![lin_common::Diagnostic::error(
            span,
            "`replace` is only allowed in a `*.test.lin` file (it mocks an import for tests). \
             Remove it from this program (ADR-071)."
                .to_string(),
        )]));
    }

    Ok(front.warnings)
}

pub fn compile(opts: &CompileOptions) -> Result<(), CompileError> {
    let CheckedFrontEnd {
        source,
        module_name,
        typed_module,
        imported_modules,
        import_order,
        import_sources,
        warnings,
    } = check_front_end(&opts.source_path)?;

    // Surface any type-check warnings (e.g. exhaustiveness, did-you-mean) rendered against the
    // main module's source.
    for w in &warnings {
        w.render(&opts.source_path.to_string_lossy(), &source);
    }

    // ADR-071: `replace` is a TEST-ONLY mock — valid only in a `*.test.lin`. Gating on the
    // FILENAME (not the subcommand) means it holds for every entry point: `lin run`/`lin build`
    // on a normal program reject it (a shipped binary must never silently swap an import like
    // stdlib `fs`), while `lin test` AND the ASan CI leg (which runs `lin build <f>.test.lin`)
    // both accept it. A `replace` outside a `.test.lin` is a hard error.
    let is_test_file = opts
        .source_path
        .to_string_lossy()
        .ends_with(".test.lin");
    if !is_test_file && !typed_module.replacements.is_empty() {
        let span = typed_module.replacements[0].span;
        return Err(CompileError::TypeCheck(vec![lin_common::Diagnostic::error(
            span,
            "`replace` is only allowed in a `*.test.lin` file (it mocks an import for tests). \
             Remove it from this program (ADR-071)."
                .to_string(),
        )]));
    }

    // 4. LLVM codegen via the LinIR pipeline (the sole compilation backend).
    // When `opts.coverage` is set, the codegen instruments per-block counters and emits the
    // LLVM coverage-mapping globals; only the main module and user (non-stdlib) imports are
    // instrumented (stdlib import sources are not tracked, so they pass `None` below).
    let context = Context::create();
    let mut cg = Codegen::new(&context, &module_name, opts.coverage);

    // Determine, before any function is declared, whether the whole program may spawn an
    // async boundary — it references any concurrency intrinsic (the `lin_async`/`lin_parallel`/
    // `lin_worker`/… family, reachable only via `std/async`). When it does, codegen must NOT
    // mark user functions `nounwind`, because a runtime fault inside a thunk unwinds through
    // Lin frames to the thread boundary (spec §24.2.2, ADR-042). Scan the main module and every
    // import's intrinsic map.
    let async_intrinsics = [
        "lin_async", "lin_await", "lin_parallel", "lin_race", "lin_timeout", "lin_retry",
        "lin_thread_pool", "lin_worker", "lin_request", "lin_message", "lin_close",
        "lin_pool_async", "lin_serve",
    ];
    let mut uses_async = typed_module.intrinsics.values().any(|n| async_intrinsics.contains(&n.as_str()));
    for m in imported_modules.values() {
        if m.intrinsics.values().any(|n| async_intrinsics.contains(&n.as_str())) {
            uses_async = true;
        }
    }
    cg.set_uses_async(uses_async);

    // Point coverage at the main module's source (canonical absolute path so llvm-cov can
    // locate the file when reporting).
    if opts.coverage {
        let abs = std::fs::canonicalize(&opts.source_path)
            .unwrap_or_else(|_| opts.source_path.clone())
            .to_string_lossy()
            .to_string();
        cg.set_main_source(&abs, &source);
    }

    // ADR-071: group the main module's test `replace` overrides by the import path they target,
    // so each imported module is compiled WITHOUT emitting the replaced export's body — the main
    // module supplies the canonical symbol instead.
    let mut replaced_by_path: HashMap<String, std::collections::HashSet<String>> = HashMap::new();
    for r in &typed_module.replacements {
        replaced_by_path
            .entry(r.module_path.clone())
            .or_default()
            .insert(r.export_name.clone());
    }
    let empty_replaced: std::collections::HashSet<String> = std::collections::HashSet::new();

    // Register imported modules with codegen in dependency order so cross-module slot
    // resolution works correctly (dependencies must be registered before dependents). Each
    // imported module is lowered and compiled through the same LinIR pipeline as the main
    // module (compile_import_from_ir).
    for path in &import_order {
        let imp_module = imported_modules.get(path).unwrap();
        let src = if opts.coverage { import_sources.get(path) } else { None };
        let replaced = replaced_by_path.get(path).unwrap_or(&empty_replaced);
        cg.compile_import_from_ir(path, imp_module, src, &imported_modules, replaced);
    }

    // Compile the main module through LinIR.
    {
        // Collect foreign-library link paths from the main module's ForeignImport stmts so
        // the linker receives them.
        for stmt in &typed_module.statements {
            if let lin_check::typed_ir::TypedStmt::ForeignImport { path, .. } = stmt {
                if path != "lin-runtime" && !cg.foreign_lib_paths.contains(path) {
                    cg.foreign_lib_paths.push(path.clone());
                }
            }
        }
        let (mut ir_module, mono_diags) = lower_module_with_imports(&typed_module, &imported_modules);
        // Monomorphization diagnostics: errors (e.g. an uninferrable type parameter) abort the
        // build; warnings (e.g. specialization-budget overflow → boxed fallback) are rendered but
        // do not stop compilation, since the fallback still produces a correct program.
        let (errors, warnings): (Vec<_>, Vec<_>) = mono_diags
            .into_iter()
            .partition(|d| matches!(d.severity, lin_common::Severity::Error));
        for w in &warnings {
            w.render(&opts.source_path.to_string_lossy(), &source);
        }
        if !errors.is_empty() {
            return Err(CompileError::TypeCheck(errors));
        }
        rc_elide::elide_rc(&mut ir_module);
        cg.compile_module_from_ir(&ir_module);
    }

    // Emit the module-level coverage globals once every module has been compiled.
    if opts.coverage {
        cg.finalize_coverage();
    }

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

    // 7. Collect foreign library paths and validate they exist
    let foreign_libs = cg.foreign_lib_paths.clone();
    for lib in &foreign_libs {
        let lib_path = Path::new(lib);
        if !lib_path.exists() {
            return Err(CompileError::Link(format!(
                "Foreign library '{}' not found; cannot link",
                lib
            )));
        }
    }

    // 8. Link with runtime and any foreign libraries
    link(&obj_path, &opts.output_path, &foreign_libs, opts.coverage)?;

    // Clean up the .o file.
    let _ = std::fs::remove_file(&obj_path);

    Ok(())
}

// -------------------------------------------------------------------------
// Module cache
// -------------------------------------------------------------------------

/// Compute the SHA-256 hash of source bytes, returned as a hex string.
fn source_hash(source: &str) -> String {
    use sha2::{Sha256, Digest};
    let mut hasher = Sha256::new();
    hasher.update(source.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Try to load a cached `TypedModule` for `source` from `.lin-cache/`.
/// Returns `None` if no cache entry exists or it is unreadable.
fn load_cache(source: &str, base_dir: &Path) -> Option<TypedModule> {
    let hash = source_hash(source);
    let cache_path = base_dir.join(".lin-cache").join(format!("{}.typed", hash));
    let bytes = std::fs::read(&cache_path).ok()?;
    bincode::deserialize(&bytes).ok()
}

/// Save a `TypedModule` to `.lin-cache/` keyed by the SHA-256 of `source`.
/// Uses write-to-temp-then-rename for atomic, concurrent-safe cache writes.
fn save_cache(source: &str, module: &TypedModule, base_dir: &Path) {
    let hash = source_hash(source);
    let cache_dir = base_dir.join(".lin-cache");
    if std::fs::create_dir_all(&cache_dir).is_err() {
        return;
    }
    let final_path = cache_dir.join(format!("{}.typed", hash));
    let tmp_path = cache_dir.join(format!("{}.typed.tmp.{}", hash, std::process::id()));
    if let Ok(bytes) = bincode::serialize(module) {
        if std::fs::write(&tmp_path, &bytes).is_ok() {
            let _ = std::fs::rename(&tmp_path, &final_path);
        }
    }
}

/// Save the `ModuleSignature` for a module alongside its TypedModule cache.
/// Uses write-to-temp-then-rename for atomic, concurrent-safe cache writes.
fn save_signature(source: &str, sig: &ModuleSignature, base_dir: &Path) {
    let hash = source_hash(source);
    let cache_dir = base_dir.join(".lin-cache");
    if std::fs::create_dir_all(&cache_dir).is_err() {
        return;
    }
    let final_path = cache_dir.join(format!("{}.sig", hash));
    let tmp_path = cache_dir.join(format!("{}.sig.tmp.{}", hash, std::process::id()));
    if let Some(bytes) = sig.to_bytes() {
        if std::fs::write(&tmp_path, &bytes).is_ok() {
            let _ = std::fs::rename(&tmp_path, &final_path);
        }
    }
}

/// Load a cached `ModuleSignature` for `source`. Returns `None` if not found or unreadable.
fn load_signature(source: &str, base_dir: &Path) -> Option<ModuleSignature> {
    let hash = source_hash(source);
    let sig_path = base_dir.join(".lin-cache").join(format!("{}.sig", hash));
    let bytes = std::fs::read(&sig_path).ok()?;
    ModuleSignature::from_bytes(&bytes)
}

/// Lex and parse a Lin source string into an AST module.
/// Returns Err with parse diagnostics if any parse errors occurred.
fn parse_source(source: &str) -> Result<Module, Vec<lin_common::Diagnostic>> {
    let tokens = Lexer::new(source, 0).tokenize();
    let mut parser = Parser::new(tokens);
    let module = parser.parse_module();
    if !parser.diagnostics.is_empty() {
        return Err(parser.diagnostics);
    }
    Ok(module)
}

/// Build an import_types map from already-typed imported modules, then type-check `ast_module`.
/// Uses `ModuleSignature` for each import — only needs the public name→type map, not the full IR.
/// On success returns the `TypedModule` together with the checker's non-error diagnostics
/// (warnings), which callers may render.
fn check_module_with_imports(
    ast_module: &Module,
    imported_modules: &HashMap<String, TypedModule>,
    lenient_json: bool,
) -> Result<(TypedModule, Vec<lin_common::Diagnostic>), Vec<lin_common::Diagnostic>> {
    let mut import_type_map: HashMap<(String, String), Type> = HashMap::new();
    let mut import_type_decls: HashMap<(String, String), (Vec<String>, Type)> = HashMap::new();
    for (path, imp_module) in imported_modules {
        let sig = ModuleSignature::from_module(imp_module);
        for (name, ty) in sig.exports {
            import_type_map.insert((path.clone(), name), ty);
        }
        for (name, decl) in sig.type_exports {
            import_type_decls.insert((path.clone(), name), decl);
        }
    }
    let mut checker = Checker::new();
    checker.import_types = import_type_map;
    checker.import_type_decls = import_type_decls;
    // The trusted stdlib forwards Json handles into concrete intrinsic/foreign params by
    // design, so it checks Json->concrete leniently (ADR-046). User code does not.
    checker.lenient_json = lenient_json;
    checker.protect_import_typevars();
    let typed = checker.check_module(ast_module)?;
    // On success, `check_module` leaves only non-error diagnostics (warnings) on the checker.
    let warnings = checker.diagnostics().to_vec();
    Ok((typed, warnings))
}

/// Embedded stdlib source files (mirrors interpreter's include_str! approach).
fn stdlib_source(path: &str) -> Option<&'static str> {
    match path {
        "std/io"     => Some(include_str!("../../../stdlib/io.lin")),
        "std/json"   => Some(include_str!("../../../stdlib/json.lin")),
        "std/string" => Some(include_str!("../../../stdlib/string.lin")),
        "std/number" => Some(include_str!("../../../stdlib/number.lin")),
        "std/array"  => Some(include_str!("../../../stdlib/array.lin")),
        "std/fs"     => Some(include_str!("../../../stdlib/fs.lin")),
        "std/http"   => Some(include_str!("../../../stdlib/http.lin")),
        "std/object"   => Some(include_str!("../../../stdlib/object.lin")),
        "std/template" => Some(include_str!("../../../stdlib/template.lin")),
        "std/async"    => Some(include_str!("../../../stdlib/async.lin")),
        "std/env"      => Some(include_str!("../../../stdlib/env.lin")),
        "std/test"     => Some(include_str!("../../../stdlib/test.lin")),
        "std/time"     => Some(include_str!("../../../stdlib/time.lin")),
        "std/path"     => Some(include_str!("../../../stdlib/path.lin")),
        "std/math"     => Some(include_str!("../../../stdlib/math.lin")),
        "std/hash"     => Some(include_str!("../../../stdlib/hash.lin")),
        "std/bytes"    => Some(include_str!("../../../stdlib/bytes.lin")),
        "std/net"      => Some(include_str!("../../../stdlib/net.lin")),
        "std/process"  => Some(include_str!("../../../stdlib/process.lin")),
        "std/tty"      => Some(include_str!("../../../stdlib/tty.lin")),
        "std/signal"   => Some(include_str!("../../../stdlib/signal.lin")),
        "std/yaml"     => Some(include_str!("../../../stdlib/yaml.lin")),
        "std/jq"       => Some(include_str!("../../../stdlib/jq.lin")),
        "std/stream"   => Some(include_str!("../../../stdlib/stream.lin")),
        _ => None,
    }
}

/// Recursively type-check all imported modules, populating `cache` in dependency order.
/// `import_sources` is populated with (abs_path, source_text) for user-defined (non-stdlib) imports.
fn pre_resolve_imports_from_ast(
    ast_module: &Module,
    base_dir: &Path,
    entry_path: &Path,
    cache: &mut HashMap<String, TypedModule>,
    order: &mut Vec<String>,
    import_sources: &mut HashMap<String, (String, String)>,
) -> Result<(), CompileError> {
    // `visiting` is the DFS stack of module identities currently being resolved; it both
    // detects cycles and supplies the chain for the diagnostic. Seed it with the entry
    // module so a cycle that loops back to the entry point is also caught and reported
    // with a complete chain.
    let entry_identity = entry_path
        .canonicalize()
        .unwrap_or_else(|_| entry_path.to_path_buf())
        .to_string_lossy()
        .to_string();
    let mut visiting: Vec<String> = vec![entry_identity];
    pre_resolve_imports_inner(ast_module, base_dir, cache, order, import_sources, &mut visiting)
}

/// A module's stable identity for cycle detection. Stdlib paths (`std/...`) are already
/// canonical; user modules are keyed by their canonicalised absolute file path so that two
/// different spellings of the same file (`../a` vs `a`) map to one identity.
fn module_identity(path: &str, base_dir: &Path) -> String {
    if stdlib_source(path).is_some() {
        return path.to_string();
    }
    let file_path = base_dir.join(format!("{}.lin", path));
    file_path
        .canonicalize()
        .unwrap_or(file_path)
        .to_string_lossy()
        .to_string()
}

fn pre_resolve_imports_inner(
    ast_module: &Module,
    base_dir: &Path,
    cache: &mut HashMap<String, TypedModule>,
    order: &mut Vec<String>,
    import_sources: &mut HashMap<String, (String, String)>,
    visiting: &mut Vec<String>,
) -> Result<(), CompileError> {
    for stmt in &ast_module.statements {
        let Stmt::Import { path, .. } = stmt else { continue };
        if cache.contains_key(path.as_str()) {
            continue;
        }

        // Detect a circular import before descending: if this module is already on the
        // DFS stack, recursing would loop forever (and overflow the stack). Report the
        // cycle as a readable `a -> b -> a` chain instead.
        let identity = module_identity(path, base_dir);
        if let Some(start) = visiting.iter().position(|m| m == &identity) {
            let mut chain: Vec<String> = visiting[start..].to_vec();
            chain.push(identity);
            return Err(CompileError::ImportCycle(chain.join(" -> ")));
        }
        visiting.push(identity);

        // From here, any early `?` aborts the whole compile, so `visiting` is discarded;
        // we only need to `pop` on the paths that fall through to the next sibling import.
        let (ast_mod, src_text, imported_base, abs_path) = if let Some(src) = stdlib_source(path.as_str()) {
            let ast = parse_source(src).map_err(CompileError::TypeCheck)?;
            pre_resolve_imports_inner(&ast, base_dir, cache, order, import_sources, visiting)?;
            (ast, src.to_string(), base_dir.to_path_buf(), None)
        } else {
            let file_path = base_dir.join(format!("{}.lin", path));
            let src = std::fs::read_to_string(&file_path)?;
            let ast = parse_source(&src).map_err(CompileError::TypeCheck)?;
            let imported_base = file_path.parent().unwrap_or(base_dir).to_path_buf();
            pre_resolve_imports_inner(&ast, &imported_base, cache, order, import_sources, visiting)?;
            let abs = file_path.canonicalize().unwrap_or(file_path);
            (ast, src, imported_base, Some(abs.to_string_lossy().to_string()))
        };

        // Try cache hit first (skip re-checking if source unchanged).
        if let Some(cached) = load_cache(&src_text, &imported_base) {
            // Ensure signature is also cached (for dependent modules to use).
            if load_signature(&src_text, &imported_base).is_none() {
                let sig = ModuleSignature::from_module(&cached);
                save_signature(&src_text, &sig, &imported_base);
            }
            if let Some(ap) = abs_path {
                import_sources.entry(path.clone()).or_insert((ap, src_text));
            }
            order.push(path.clone());
            cache.insert(path.clone(), cached);
            visiting.pop();
            continue;
        }

        // Only the embedded stdlib is trusted to forward Json into concrete params (ADR-046);
        // user-defined imported modules are checked strictly, like the main module.
        let is_stdlib = stdlib_source(path.as_str()).is_some();
        let (typed, _warnings) = check_module_with_imports(&ast_mod, cache, is_stdlib)
            .map_err(CompileError::TypeCheck)?;
        let sig = ModuleSignature::from_module(&typed);
        save_cache(&src_text, &typed, &imported_base);
        save_signature(&src_text, &sig, &imported_base);
        if let Some(ap) = abs_path {
            import_sources.entry(path.clone()).or_insert((ap, src_text.clone()));
        }
        order.push(path.clone());
        cache.insert(path.clone(), typed);
        visiting.pop();
    }
    Ok(())
}

fn link(obj_path: &Path, output_path: &Path, foreign_libs: &[String], coverage: bool) -> Result<(), CompileError> {
    // Find the lin-runtime static library.
    let runtime_lib = find_runtime_lib();

    let mut cmd = Command::new("cc");
    cmd.arg(obj_path)
        .arg("-o")
        .arg(output_path);

    if let Some(lib) = &runtime_lib {
        cmd.arg(lib);
    } else {
        eprintln!("Warning: lin-runtime library not found, linking may fail");
    }

    // Add foreign library paths.
    for lib in foreign_libs {
        let lib_path = Path::new(lib);
        if lib.ends_with(".a") || lib.ends_with(".o") {
            cmd.arg(lib_path);
        } else if lib.ends_with(".so") || lib.ends_with(".dylib") {
            let parent = lib_path.parent().unwrap_or(Path::new("."));
            let stem = lib_path.file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or(lib);
            let lib_name = stem.strip_prefix("lib").unwrap_or(stem);
            cmd.arg(format!("-L{}", parent.display()))
               .arg(format!("-l{}", lib_name));
        } else {
            cmd.arg(lib_path);
        }
    }

    // Link clang_rt.profile when coverage instrumentation is enabled.
    // Use --whole-archive so the profile runtime's constructor and atexit handlers are linked in.
    if coverage {
        let profile_lib = "/usr/lib/llvm-22/lib/clang/22/lib/linux/libclang_rt.profile-x86_64.a";
        cmd.arg("-Wl,--whole-archive")
           .arg(profile_lib)
           .arg("-Wl,--no-whole-archive");
        // profile runtime needs pthread and dl on Linux
        cmd.arg("-lpthread").arg("-ldl").arg("-lrt");
    }

    // Link system libraries needed by lin-runtime (libc via cc, libm for math).
    cmd.arg("-lm");
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
    // 1. Next to the running executable (installed / bundled binary).
    if let Ok(exe) = std::env::current_exe() {
        let dir = exe.parent()?;
        let p = dir.join("liblin_runtime.a");
        if p.exists() {
            return Some(p);
        }
    }

    // 2. Standard cargo target directories (dev / workspace build).
    let candidates = [
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

    // 3. CARGO_MANIFEST_DIR-relative paths (works in tests).
    if let Ok(manifest) = std::env::var("CARGO_MANIFEST_DIR") {
        let base = Path::new(&manifest);
        for candidate in &candidates {
            let path = base.join(candidate);
            if path.exists() {
                return Some(path);
            }
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
