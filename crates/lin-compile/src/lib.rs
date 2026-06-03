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
    // `check_warnings` carries non-error diagnostics (e.g. the streams must-use WARNING); the
    // callers (`check()` / `compile()`) render them — `check_front_end` only collects.
    let (typed_module, check_warnings) = check_module_with_imports(&ast_module, &imported_modules, false)
        .map_err(CompileError::TypeCheck)?;

    Ok(CheckedFrontEnd {
        source,
        module_name,
        typed_module,
        imported_modules,
        import_order,
        import_sources,
        warnings: check_warnings,
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

/// On-disk cache format version. Bump this whenever the layout of the serialized `TypedModule`
/// or any `Type`/`TypedStmt`/`ModuleSignature` it embeds changes, OR when the cache-key derivation
/// below changes. A bump invalidates every existing `.typed`/`.sig` entry: the embedded stamp no
/// longer matches, so a stale entry written by an older binary is rejected (not silently
/// bincode-deserialized into a wrong-but-structurally-valid struct).
const CACHE_FORMAT_VERSION: u32 = 1;

/// Magic prefix written at the head of every `.typed`/`.sig` cache file. Combined with the
/// compiler version and `CACHE_FORMAT_VERSION`, this is the on-disk compatibility stamp checked
/// before any bincode payload is deserialized.
const CACHE_MAGIC: &[u8] = b"LINC";

/// The version stamp that prefixes every cache payload on disk: magic + format version + the
/// compiler package version. Any mismatch (older binary, layout bump, different `lin` build)
/// rejects the cache entry at read time.
fn cache_stamp() -> Vec<u8> {
    let mut stamp = Vec::new();
    stamp.extend_from_slice(CACHE_MAGIC);
    stamp.extend_from_slice(&CACHE_FORMAT_VERSION.to_le_bytes());
    let pkg = env!("CARGO_PKG_VERSION").as_bytes();
    stamp.extend_from_slice(&(pkg.len() as u32).to_le_bytes());
    stamp.extend_from_slice(pkg);
    stamp
}

/// The cache-key salt folded into every module's cache key alongside its source and import
/// signatures. Distinct from `cache_stamp` (which gates on-disk *deserialization*); this salt
/// changes the *filename* so a key computed by an incompatible binary never even names the same
/// file. Kept in sync with the stamp components.
fn cache_key_salt() -> String {
    format!("linc:v{}:{}", CACHE_FORMAT_VERSION, env!("CARGO_PKG_VERSION"))
}

/// Compute a module's cache key. Folds in (a) the module's own source bytes, (b) a stable,
/// order-independent fingerprint of every imported module's resolved signature (each import's
/// `ModuleSignature::content_hash()`), and (c) the compiler-version/format salt. Two builds with
/// byte-identical source but a changed import signature produce DIFFERENT keys, so a dependent is
/// never served a `.typed` that was checked against an older version of its imports (the core
/// stale-cache bug). `import_content_hashes` are sorted so the key is independent of import order.
fn compute_cache_key(source: &str, import_content_hashes: &[String]) -> String {
    use sha2::{Sha256, Digest};
    let mut sorted: Vec<&String> = import_content_hashes.iter().collect();
    sorted.sort();
    let mut hasher = Sha256::new();
    hasher.update(cache_key_salt().as_bytes());
    hasher.update(b"\0src\0");
    hasher.update(source.as_bytes());
    hasher.update(b"\0imports\0");
    for h in sorted {
        hasher.update(h.as_bytes());
        hasher.update(b"\0");
    }
    format!("{:x}", hasher.finalize())
}

/// Strip and verify the cache stamp at the head of a cache file, returning the bincode payload
/// slice if (and only if) the stamp matches this binary. A mismatch yields `None` so the caller
/// falls back to a fresh check instead of deserializing incompatible bytes.
fn verify_stamp(bytes: &[u8]) -> Option<&[u8]> {
    let stamp = cache_stamp();
    if bytes.len() < stamp.len() || bytes[..stamp.len()] != stamp[..] {
        return None;
    }
    Some(&bytes[stamp.len()..])
}

/// Prepend the cache stamp to a serialized payload before writing it to disk.
fn with_stamp(payload: &[u8]) -> Vec<u8> {
    let mut out = cache_stamp();
    out.extend_from_slice(payload);
    out
}

/// Try to load a cached `TypedModule` for cache `key` from `.lin-cache/`.
/// Returns `None` if no cache entry exists, it is unreadable, or its version stamp is incompatible.
fn load_cache(key: &str, base_dir: &Path) -> Option<TypedModule> {
    let cache_path = base_dir.join(".lin-cache").join(format!("{}.typed", key));
    let bytes = std::fs::read(&cache_path).ok()?;
    let payload = verify_stamp(&bytes)?;
    bincode::deserialize(payload).ok()
}

/// Save a `TypedModule` to `.lin-cache/` keyed by `key`.
/// Uses write-to-temp-then-rename for atomic, concurrent-safe cache writes.
fn save_cache(key: &str, module: &TypedModule, base_dir: &Path) {
    let cache_dir = base_dir.join(".lin-cache");
    if std::fs::create_dir_all(&cache_dir).is_err() {
        return;
    }
    let final_path = cache_dir.join(format!("{}.typed", key));
    let tmp_path = cache_dir.join(format!("{}.typed.tmp.{}", key, std::process::id()));
    if let Ok(bytes) = bincode::serialize(module) {
        let stamped = with_stamp(&bytes);
        if std::fs::write(&tmp_path, &stamped).is_ok() {
            let _ = std::fs::rename(&tmp_path, &final_path);
        }
    }
}

/// Save the `ModuleSignature` for a module alongside its TypedModule cache.
/// Uses write-to-temp-then-rename for atomic, concurrent-safe cache writes.
fn save_signature(key: &str, sig: &ModuleSignature, base_dir: &Path) {
    let cache_dir = base_dir.join(".lin-cache");
    if std::fs::create_dir_all(&cache_dir).is_err() {
        return;
    }
    let final_path = cache_dir.join(format!("{}.sig", key));
    let tmp_path = cache_dir.join(format!("{}.sig.tmp.{}", key, std::process::id()));
    if let Some(bytes) = sig.to_bytes() {
        let stamped = with_stamp(&bytes);
        if std::fs::write(&tmp_path, &stamped).is_ok() {
            let _ = std::fs::rename(&tmp_path, &final_path);
        }
    }
}

/// Load a cached `ModuleSignature` for cache `key`. Returns `None` if not found, unreadable, or
/// its version stamp is incompatible.
fn load_signature(key: &str, base_dir: &Path) -> Option<ModuleSignature> {
    let sig_path = base_dir.join(".lin-cache").join(format!("{}.sig", key));
    let bytes = std::fs::read(&sig_path).ok()?;
    let payload = verify_stamp(&bytes)?;
    ModuleSignature::from_bytes(payload)
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
    // Surface non-error diagnostics (e.g. the streams must-use WARNING) collected during a
    // SUCCESSFUL check — `check_module` only returns `Err` for hard errors, leaving warnings in
    // the checker, which would otherwise be dropped with it. Returned to the caller to render.
    let warnings: Vec<lin_common::Diagnostic> = checker
        .diagnostics()
        .iter()
        .filter(|d| !matches!(d.severity, lin_common::Severity::Error))
        .cloned()
        .collect();
    Ok((typed, warnings))
}

/// Like `check_module_with_imports`, but additionally seeds `seeded` provisional export types
/// (keyed by `(import-path-string, export-name)`) into the checker's `import_types` BEFORE the
/// resolved `imported_modules` signatures. Used by `check_scc` Phase 2 so a cyclic member's
/// cross-module references resolve to a peer's (provisional) inferred type instead of falling back
/// to a fresh TypeVar — the seam that lets unannotated mutual recursion type-check.
fn check_module_with_seeded_imports(
    ast_module: &Module,
    imported_modules: &HashMap<String, TypedModule>,
    seeded: &HashMap<(String, String), Type>,
    lenient_json: bool,
) -> Result<(TypedModule, Vec<lin_common::Diagnostic>), Vec<lin_common::Diagnostic>> {
    let mut import_type_map: HashMap<(String, String), Type> = HashMap::new();
    let mut import_type_decls: HashMap<(String, String), (Vec<String>, Type)> = HashMap::new();
    // Already-resolved (acyclic) imports first.
    for (path, imp_module) in imported_modules {
        let sig = ModuleSignature::from_module(imp_module);
        for (name, ty) in sig.exports {
            import_type_map.insert((path.clone(), name), ty);
        }
        for (name, decl) in sig.type_exports {
            import_type_decls.insert((path.clone(), name), decl);
        }
    }
    // Provisional peer signatures from within the SCC override/augment the map (a peer is not in
    // `imported_modules` yet, so this is the only source of its type).
    for ((path, name), ty) in seeded {
        import_type_map.insert((path.clone(), name.clone()), ty.clone());
    }
    let mut checker = Checker::new();
    checker.import_types = import_type_map;
    checker.import_type_decls = import_type_decls;
    checker.lenient_json = lenient_json;
    checker.protect_import_typevars();
    let typed = checker.check_module(ast_module)?;
    let warnings: Vec<lin_common::Diagnostic> = checker
        .diagnostics()
        .iter()
        .filter(|d| !matches!(d.severity, lin_common::Severity::Error))
        .cloned()
        .collect();
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
        "std/iter"   => Some(include_str!("../../../stdlib/iter.lin")),
        "std/fs"     => Some(include_str!("../../../stdlib/fs.lin")),
        "std/ffi"    => Some(include_str!("../../../stdlib/ffi.lin")),
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
        "std/compress" => Some(include_str!("../../../stdlib/compress.lin")),
        "std/archive"  => Some(include_str!("../../../stdlib/archive.lin")),
        _ => None,
    }
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

/// A loaded-but-not-yet-checked import module, keyed in `ImportGraph` by its stable identity.
struct LoadedModule {
    /// Every import-path string this module was reached by, in first-seen order. Codegen mangles
    /// an imported symbol from the importer's path string, so each distinct spelling needs its own
    /// compiled copy (the registration loop emits one `cache`/`order` entry per path string).
    paths: Vec<String>,
    ast: Module,
    src_text: String,
    /// Directory imports inside this module resolve relative to.
    base_dir: PathBuf,
    /// `Some(abs_path)` for user (non-stdlib) modules; `None` for the embedded stdlib.
    abs_path: Option<String>,
    is_stdlib: bool,
    /// Identities of the modules this module imports (graph adjacency).
    deps: Vec<String>,
}

/// The full import graph, loaded by parsing every transitively-reachable module exactly once
/// (keyed by stable identity). SCC analysis runs over this graph so genuine cycles can be
/// type-checked together instead of being rejected outright.
struct ImportGraph {
    /// identity -> loaded module.
    modules: HashMap<String, LoadedModule>,
    /// Insertion order of identities (first-seen DFS order), used to keep SCC processing and the
    /// resulting `import_order` deterministic.
    order: Vec<String>,
}

/// Parse `ast_module`'s imports and, recursively, the whole reachable module graph, loading each
/// module once keyed by identity. Records adjacency (and every path string each module is reached
/// by) without type-checking anything yet.
fn build_import_graph(
    ast_module: &Module,
    base_dir: &Path,
    graph: &mut ImportGraph,
) -> Result<Vec<String>, CompileError> {
    let mut deps = Vec::new();
    for stmt in &ast_module.statements {
        let Stmt::Import { path, .. } = stmt else { continue };
        let identity = module_identity(path, base_dir);
        deps.push(identity.clone());

        if let Some(existing) = graph.modules.get_mut(&identity) {
            // Already loaded — just record this additional path spelling (so it gets its own
            // compiled copy + mangled symbols, matching the per-path-string registration the
            // non-cyclic resolver has always produced for absolute-vs-relative spellings).
            if !existing.paths.contains(path) {
                existing.paths.push(path.clone());
            }
            continue;
        }

        let (ast, src_text, imported_base, abs_path, is_stdlib) =
            if let Some(src) = stdlib_source(path.as_str()) {
                let ast = parse_source(src).map_err(CompileError::TypeCheck)?;
                (ast, src.to_string(), base_dir.to_path_buf(), None, true)
            } else {
                let file_path = base_dir.join(format!("{}.lin", path));
                let src = std::fs::read_to_string(&file_path)?;
                let ast = parse_source(&src).map_err(CompileError::TypeCheck)?;
                let imported_base = file_path.parent().unwrap_or(base_dir).to_path_buf();
                let abs = file_path.canonicalize().unwrap_or(file_path);
                (ast, src, imported_base, Some(abs.to_string_lossy().to_string()), false)
            };

        // Insert a placeholder before recursing so a cycle back to this identity finds it
        // (and does not reload it). `deps` is filled in after the recursive load returns.
        graph.modules.insert(identity.clone(), LoadedModule {
            paths: vec![path.clone()],
            ast: ast.clone(),
            src_text,
            base_dir: imported_base.clone(),
            abs_path,
            is_stdlib,
            deps: Vec::new(),
        });
        graph.order.push(identity.clone());

        let child_deps = build_import_graph(&ast, &imported_base, graph)?;
        if let Some(m) = graph.modules.get_mut(&identity) {
            m.deps = child_deps;
        }
    }
    Ok(deps)
}

/// Tarjan's strongly-connected-components over the import graph, restricted to identities reachable
/// from `graph.order`. Returns the SCCs in reverse-topological order (dependencies before
/// dependents) — exactly the order codegen needs imports registered in. Each SCC is a list of
/// member identities; a singleton with no self-edge is an ordinary acyclic module.
fn tarjan_sccs(graph: &ImportGraph) -> Vec<Vec<String>> {
    struct State<'a> {
        graph: &'a ImportGraph,
        index: u32,
        indices: HashMap<String, u32>,
        lowlink: HashMap<String, u32>,
        on_stack: std::collections::HashSet<String>,
        stack: Vec<String>,
        sccs: Vec<Vec<String>>,
    }
    fn strongconnect(s: &mut State, v: &str) {
        s.indices.insert(v.to_string(), s.index);
        s.lowlink.insert(v.to_string(), s.index);
        s.index += 1;
        s.stack.push(v.to_string());
        s.on_stack.insert(v.to_string());

        let deps = s.graph.modules.get(v).map(|m| m.deps.clone()).unwrap_or_default();
        for w in &deps {
            if !s.indices.contains_key(w) {
                strongconnect(s, w);
                let lw = s.lowlink[w];
                let lv = s.lowlink[v];
                s.lowlink.insert(v.to_string(), lv.min(lw));
            } else if s.on_stack.contains(w) {
                let iw = s.indices[w];
                let lv = s.lowlink[v];
                s.lowlink.insert(v.to_string(), lv.min(iw));
            }
        }

        if s.lowlink[v] == s.indices[v] {
            let mut scc = Vec::new();
            loop {
                let w = s.stack.pop().unwrap();
                s.on_stack.remove(&w);
                let done = w == v;
                scc.push(w);
                if done { break; }
            }
            s.sccs.push(scc);
        }
    }

    let mut s = State {
        graph,
        index: 0,
        indices: HashMap::new(),
        lowlink: HashMap::new(),
        on_stack: std::collections::HashSet::new(),
        stack: Vec::new(),
        sccs: Vec::new(),
    };
    for id in &graph.order {
        if !s.indices.contains_key(id) {
            strongconnect(&mut s, id);
        }
    }
    // Tarjan already emits SCCs in reverse-topological order (a dependency's SCC is finished and
    // popped before its dependents'), which is the order we want.
    s.sccs
}

/// True if `expr` references any name in `names` (a free identifier). Conservative — it descends
/// the whole expression tree, so a nested function body counts too; callers pass non-function
/// initializers only, where that is exactly the init-order dependency we must catch.
fn expr_references_any(expr: &lin_parse::ast::Expr, names: &std::collections::HashSet<String>) -> bool {
    use lin_parse::ast::Expr;
    let mut found = false;
    walk_expr(expr, &mut |e| {
        if let Expr::Ident(n, _) = e {
            if names.contains(n) {
                found = true;
            }
        }
    });
    found
}

/// Visit every sub-expression of `expr`, applying `f` to each node (pre-order).
fn walk_expr(expr: &lin_parse::ast::Expr, f: &mut impl FnMut(&lin_parse::ast::Expr)) {
    use lin_parse::ast::{Expr, ObjectField, StringPart};
    f(expr);
    macro_rules! go { ($e:expr) => { walk_expr($e, f) }; }
    match expr {
        Expr::BinaryOp { left, right, .. } => { go!(left); go!(right); }
        Expr::UnaryOp { operand, .. } => go!(operand),
        Expr::Call { func, args, .. } => { go!(func); for a in args { go!(a); } }
        Expr::DotCall { receiver, args, .. } => {
            go!(receiver);
            if let Some(args) = args { for a in args { go!(a); } }
        }
        Expr::Index { object, key, .. } => { go!(object); go!(key); }
        Expr::If { condition, then_branch, else_branch, .. } => {
            go!(condition); go!(then_branch); go!(else_branch);
        }
        Expr::Match { scrutinee, arms, .. } => {
            go!(scrutinee);
            for arm in arms {
                if let Some(g) = &arm.guard { go!(g); }
                go!(&arm.body);
            }
        }
        Expr::Block(stmts, tail, _) => {
            for s in stmts { walk_stmt_exprs(s, f); }
            go!(tail);
        }
        Expr::Function { body, .. } => go!(body),
        Expr::Array(elems, _) => for e in elems { go!(e); },
        Expr::Object(fields, _) => for field in fields {
            match field {
                ObjectField::Pair(k, v) => { go!(k); go!(v); }
                ObjectField::Spread(e) => go!(e),
            }
        },
        Expr::Assign { value, .. } => go!(value),
        Expr::IndexAssign { object, key, value, .. } => { go!(object); go!(key); go!(value); }
        Expr::Is { expr, .. } | Expr::Has { expr, .. } => go!(expr),
        Expr::StringInterp(parts, _) => {
            for p in parts {
                if let StringPart::Expr(e) = p { go!(e); }
            }
        }
        Expr::TupleArgs(elems, _) => for e in elems { go!(e); },
        _ => {}
    }
}

fn walk_stmt_exprs(stmt: &Stmt, f: &mut impl FnMut(&lin_parse::ast::Expr)) {
    match stmt {
        Stmt::Val { value, .. } | Stmt::Var { value, .. } => walk_expr(value, f),
        Stmt::Expr(e) | Stmt::Replace { value: e, .. } => walk_expr(e, f),
        _ => {}
    }
}

/// Recursively type-check all imported modules, populating `cache` in dependency order.
/// `import_sources` is populated with (abs_path, source_text) for user-defined (non-stdlib) imports.
///
/// Replaces the old DFS-with-hard-cycle-reject: the import graph is loaded up front, decomposed
/// into strongly-connected components, and each SCC is checked together. A singleton SCC (no
/// self-edge) takes exactly the old per-module path (incl. the `.lin-cache` fast path); a true
/// multi-module cycle is type-checked via `check_scc` so inference flows across the boundary with
/// no userland annotations required (function-reference cycles). Genuine value-init cycles are
/// still rejected with a clean diagnostic.
fn pre_resolve_imports_from_ast(
    ast_module: &Module,
    base_dir: &Path,
    _entry_path: &Path,
    cache: &mut HashMap<String, TypedModule>,
    order: &mut Vec<String>,
    import_sources: &mut HashMap<String, (String, String)>,
) -> Result<(), CompileError> {
    let mut graph = ImportGraph { modules: HashMap::new(), order: Vec::new() };
    build_import_graph(ast_module, base_dir, &mut graph)?;
    let sccs = tarjan_sccs(&graph);

    // identity -> content_hash of that module's resolved `ModuleSignature`. Populated as each SCC
    // resolves. Because Tarjan emits SCCs in reverse-topological order (dependencies before
    // dependents), every dependency of a module is already present here by the time we compute that
    // module's cache key — so a dependent's key can fold in its imports' (post-check) content
    // hashes. This is the ordering assumption that makes import-signature-aware keys correct.
    let mut dep_hashes: HashMap<String, String> = HashMap::new();

    for scc in &sccs {
        let is_cycle = scc.len() > 1 || {
            // A singleton is still a cycle if it imports itself.
            let id = &scc[0];
            graph.modules.get(id).map(|m| m.deps.contains(id)).unwrap_or(false)
        };
        if !is_cycle {
            resolve_singleton(&scc[0], &graph, cache, order, import_sources, &mut dep_hashes)?;
        } else {
            check_scc(scc, &graph, cache, order, import_sources, &mut dep_hashes)?;
        }
    }
    Ok(())
}

/// The cache key for `m`: its own source folded together with the resolved-signature content
/// hashes of every module it imports (looked up from `dep_hashes`, which holds every dependency
/// by the reverse-topological ordering invariant). A dependency we somehow have no recorded hash
/// for (should not happen for an acyclic module, but is possible for a self/peer reference inside
/// an SCC) contributes nothing, which is sound: SCC members are never read from the `.typed` cache.
fn module_cache_key(m: &LoadedModule, dep_hashes: &HashMap<String, String>) -> String {
    let import_hashes: Vec<String> = m
        .deps
        .iter()
        .filter_map(|dep| dep_hashes.get(dep).cloned())
        .collect();
    compute_cache_key(&m.src_text, &import_hashes)
}

/// Resolve one acyclic module: consult the `.lin-cache`, else type-check it against the already
/// resolved `cache`, then register it under every path string it was imported by. This is the
/// unchanged single-module path, lifted out of the old recursive DFS.
fn resolve_singleton(
    identity: &str,
    graph: &ImportGraph,
    cache: &mut HashMap<String, TypedModule>,
    order: &mut Vec<String>,
    import_sources: &mut HashMap<String, (String, String)>,
    dep_hashes: &mut HashMap<String, String>,
) -> Result<(), CompileError> {
    let m = graph.modules.get(identity).expect("scc member loaded");

    // Cache key folds in this module's source AND the resolved-signature content hashes of its
    // imports, so a changed import signature invalidates this entry even when the source is
    // byte-identical (the stale-cache bug).
    let key = module_cache_key(m, dep_hashes);

    // Cache fast path.
    if let Some(cached) = load_cache(&key, &m.base_dir) {
        let sig = load_signature(&key, &m.base_dir)
            .unwrap_or_else(|| ModuleSignature::from_module(&cached));
        save_signature(&key, &sig, &m.base_dir);
        dep_hashes.insert(identity.to_string(), sig.content_hash());
        register_resolved(m, cached, cache, order, import_sources);
        return Ok(());
    }

    let (typed, _warnings) = check_module_with_imports(&m.ast, cache, m.is_stdlib)
        .map_err(CompileError::TypeCheck)?;
    let sig = ModuleSignature::from_module(&typed);
    save_cache(&key, &typed, &m.base_dir);
    save_signature(&key, &sig, &m.base_dir);
    dep_hashes.insert(identity.to_string(), sig.content_hash());
    register_resolved(m, typed, cache, order, import_sources);
    Ok(())
}

/// Register a resolved `TypedModule` into `cache`/`order`/`import_sources` under EVERY path string
/// the module was imported by — codegen mangles symbols from the importer's path string, so each
/// distinct spelling needs its own compiled copy (preserving long-standing absolute-vs-relative
/// duplication behaviour).
fn register_resolved(
    m: &LoadedModule,
    typed: TypedModule,
    cache: &mut HashMap<String, TypedModule>,
    order: &mut Vec<String>,
    import_sources: &mut HashMap<String, (String, String)>,
) {
    for path in &m.paths {
        if cache.contains_key(path) {
            continue;
        }
        if let Some(ap) = &m.abs_path {
            import_sources.entry(path.clone()).or_insert((ap.clone(), m.src_text.clone()));
        }
        order.push(path.clone());
        cache.insert(path.clone(), typed.clone());
    }
}

/// Type-check the members of a true import cycle (a multi-module SCC, or a self-importing module)
/// together, so inference flows across the import boundary without requiring userland annotations.
///
/// Strategy (seed-and-recheck fixed point, Route A.a):
///   1. Phase 1 — check every member with whatever peer signatures are available so far (peers not
///      yet checked fall back to fresh TypeVars at the existing import-binding seam). Extract each
///      member's provisional `ModuleSignature` (this also tells us which peer exports are functions
///      vs. eager values).
///   2. Reject genuine VALUE-init cycles: a top-level non-function `val`/`var` whose initializer
///      reads a peer export that is ITSELF a non-function value is infinite module-init recursion
///      (spec §7.3) and stays a hard error. Binding a peer FUNCTION as a value, or calling one, is
///      fine — function symbols are resolved by name, not recomputed at init.
///   3. Phase 2 — re-check every member with ALL provisional peer signatures seeded into
///      `import_types`. Cross-module calls now resolve to concrete (provisional) types, so an
///      unannotated mutually-recursive function infers correctly.
/// The Phase 2 `TypedModule`s are the ones registered. (Cyclic members are not consulted from the
/// `.lin-cache` — their types depend on peers, so they are always freshly checked.)
fn check_scc(
    scc: &[String],
    graph: &ImportGraph,
    cache: &mut HashMap<String, TypedModule>,
    order: &mut Vec<String>,
    import_sources: &mut HashMap<String, (String, String)>,
    dep_hashes: &mut HashMap<String, String>,
) -> Result<(), CompileError> {
    let members: Vec<&LoadedModule> = scc
        .iter()
        .map(|id| graph.modules.get(id).expect("scc member loaded"))
        .collect();

    // (1) Phase 1: provisional check of each member against the current `cache` (peers fall back
    // to fresh TypeVars). Collect provisional signatures keyed by (path-string, export-name).
    let mut provisional: HashMap<(String, String), Type> = HashMap::new();
    for m in &members {
        let (typed, _w) = check_module_with_imports(&m.ast, cache, m.is_stdlib)
            .map_err(CompileError::TypeCheck)?;
        let sig = ModuleSignature::from_module(&typed);
        for (name, ty) in sig.exports {
            // Seed under every path string this member is reached by, so a peer importing it by
            // any spelling finds the provisional type.
            for p in &m.paths {
                provisional.insert((p.clone(), name.clone()), ty.clone());
            }
        }
    }

    // (2) Reject value-init cycles, using the Phase 1 signatures to tell function exports (safe to
    // reference — resolved by symbol) from eager value exports (an init-time read that recurses).
    for m in &members {
        // Names imported by THIS module that resolve into the SCC AND name a peer NON-function
        // export — reading one of these at init time is the unbreakable cycle.
        let mut peer_value_imports: std::collections::HashSet<String> = std::collections::HashSet::new();
        for stmt in &m.ast.statements {
            if let Stmt::Import { bindings, path, .. } = stmt {
                let dep_identity = module_identity(path, &m.base_dir);
                if !scc.iter().any(|s| s == &dep_identity) {
                    continue;
                }
                for b in bindings {
                    let is_value = matches!(
                        provisional.get(&(path.clone(), b.name.clone())),
                        Some(t) if !matches!(t, Type::Function { .. })
                    );
                    if is_value {
                        peer_value_imports.insert(b.alias.clone().unwrap_or_else(|| b.name.clone()));
                    }
                }
            }
        }
        if peer_value_imports.is_empty() {
            continue;
        }
        for stmt in &m.ast.statements {
            let value = match stmt {
                Stmt::Val { value, .. } | Stmt::Var { value, .. } => value,
                _ => continue,
            };
            // A function-literal initializer is lazy — referencing a peer in its BODY is the
            // legitimate mutual-recursion case. Only eager (non-function) initializers can recurse.
            if matches!(value, lin_parse::ast::Expr::Function { .. }) {
                continue;
            }
            if expr_references_any(value, &peer_value_imports) {
                return Err(CompileError::ImportCycle(format!(
                    "{} (a top-level value initializer reads an imported VALUE from a module that \
                     imports it back — module initialization would recurse forever; break the cycle \
                     by moving the value behind a function, spec §7.3)",
                    scc.join(" <-> ")
                )));
            }
        }
    }

    // (3) Phase 2: re-check each member with the provisional peer signatures merged into the
    // import-type map, so cross-module references resolve to concrete types.
    for (identity, m) in scc.iter().zip(members.iter()) {
        let (typed, _w) = check_module_with_seeded_imports(&m.ast, cache, &provisional, m.is_stdlib)
            .map_err(CompileError::TypeCheck)?;
        let sig = ModuleSignature::from_module(&typed);
        // Cyclic members are always freshly checked (their type depends on peers), so their `.typed`
        // is never read back from the cache. We still persist the signature — under the same
        // import-signature-aware key scheme — so dependents OUTSIDE the SCC can fold this member's
        // content hash into their own keys and use the `.sig`/`.typed` fast path. Peer hashes within
        // the SCC are not yet recorded; `module_cache_key` skips them (sound, since SCC `.typed`
        // entries are never consulted).
        let key = module_cache_key(m, dep_hashes);
        save_signature(&key, &sig, &m.base_dir);
        dep_hashes.insert(identity.clone(), sig.content_hash());
        register_resolved(m, typed, cache, order, import_sources);
    }
    Ok(())
}

/// Compute a relative path from `from` to `to`, both assumed absolute and normalized
/// (canonicalized) directories. Returns `Some(rel)` where joining `from/rel` reaches `to`, using
/// `..` segments to ascend to a common ancestor. Returns `Some("")` when `from == to`. Returns
/// `None` if no relative path can be expressed (e.g. different roots / mounts) — callers fall back
/// to an absolute path. This is a small pure-Rust helper (no `pathdiff` dependency) covering the
/// common case where the binary and the .so share an ancestor directory.
fn relative_path(from: &Path, to: &Path) -> Option<PathBuf> {
    use std::path::Component;
    let from_comps: Vec<Component> = from.components().collect();
    let to_comps: Vec<Component> = to.components().collect();

    // Find the length of the shared prefix.
    let mut common = 0;
    while common < from_comps.len()
        && common < to_comps.len()
        && from_comps[common] == to_comps[common]
    {
        common += 1;
    }

    // If there is no shared component at all, the paths have no common base (different roots /
    // mounts); a relative path can't be expressed.
    if common == 0 {
        return None;
    }

    let mut rel = PathBuf::new();
    // Ascend out of `from` down to the common ancestor.
    for _ in common..from_comps.len() {
        rel.push("..");
    }
    // Descend into `to` from the common ancestor.
    for comp in &to_comps[common..] {
        rel.push(comp.as_os_str());
    }
    Some(rel)
}

fn link(obj_path: &Path, output_path: &Path, foreign_libs: &[String], coverage: bool) -> Result<(), CompileError> {
    // Find the lin-runtime static library.
    let runtime_lib = find_runtime_lib();

    // The default link driver is the system `cc` (typically gcc). For coverage we instead drive
    // the link through clang, because the LLVM profile runtime (`libclang_rt.profile`) and the
    // `-fprofile-instr-generate` flag that pulls it in are clang's — gcc doesn't understand them.
    // clang locates the correct host-arch runtime itself, so we never hardcode a path or arch.
    let driver = if coverage { coverage_link_driver() } else { "cc".to_string() };

    let mut cmd = Command::new(&driver);
    cmd.arg(obj_path)
        .arg("-o")
        .arg(output_path);

    if let Some(lib) = &runtime_lib {
        cmd.arg(lib);
    } else {
        eprintln!("Warning: lin-runtime library not found, linking may fail");
    }

    // Add foreign library paths. For shared libraries we also emit an rpath so the produced
    // binary can locate the vendored .so at RUNTIME without it being on the system path
    // (LD_LIBRARY_PATH / ldconfig). rpath dirs are deduped so each distinct rpath string is added
    // once.
    //
    // We prefer a `$ORIGIN`-relative rpath so the produced binary + its vendored .so are
    // RELOCATABLE: copy both together (preserving their relative layout) anywhere and the binary
    // still resolves the library, because `$ORIGIN` is expanded by the dynamic loader to the
    // directory the binary lives in at launch. This is the Linux/ELF mechanism. macOS uses
    // `@loader_path` plus the dylib's `install_name` instead — that is a deliberate FOLLOW-UP and
    // is not handled here (the `$ORIGIN` token is meaningless to the macOS loader).
    //
    // `$ORIGIN` must reach the dynamic loader LITERALLY (the loader, not the linker or shell, does
    // the expansion). The link command runs via std::process::Command — not a shell — so the
    // `$ORIGIN` in `-Wl,-rpath,$ORIGIN/...` is passed as a literal argv element and is NOT
    // shell-expanded. `readelf -d` on the output confirms RUNPATH carries a literal `$ORIGIN`.
    let mut rpath_specs: Vec<String> = Vec::new();
    let out_dir = output_path.parent().unwrap_or(Path::new("."));
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

            // Compute a `$ORIGIN`-relative rpath from the output binary's directory to the .so's
            // directory. If a clean relative path can't be derived (e.g. the two live on different
            // mounts with no common base, or canonicalization fails), fall back to an ABSOLUTE
            // canonicalized rpath — robust, just not relocatable.
            let abs_out_dir = out_dir.canonicalize().unwrap_or_else(|_| out_dir.to_path_buf());
            let abs_parent = parent.canonicalize().unwrap_or_else(|_| parent.to_path_buf());
            let rpath = match relative_path(&abs_out_dir, &abs_parent) {
                Some(rel) if rel.as_os_str().is_empty() => "$ORIGIN".to_string(),
                Some(rel) => format!("$ORIGIN/{}", rel.display()),
                None => abs_parent.display().to_string(),
            };
            if !rpath_specs.contains(&rpath) {
                rpath_specs.push(rpath.clone());
                cmd.arg(format!("-Wl,-rpath,{}", rpath));
            }
        } else {
            cmd.arg(lib_path);
        }
    }

    // Link the LLVM profile runtime when coverage instrumentation is enabled. Rather than
    // hardcoding the absolute path to `libclang_rt.profile-<arch>.a` (which bakes in the LLVM
    // patch version, host arch, and install prefix), let the `cc` driver locate and link the
    // correct runtime for this host via `-fprofile-instr-generate`. clang resolves the right
    // libclang_rt.profile itself, including its required deps (pthread/dl/rt on Linux). This is
    // portable across LLVM minor versions, architectures, and distros.
    if coverage {
        cmd.arg("-fprofile-instr-generate");
    }

    // Link system libraries needed by lin-runtime (libc via cc, libm for math).
    cmd.arg("-lm");

    // Capture stderr so a link failure surfaces the real linker diagnostic (e.g. "cannot find
    // libclang_rt.profile...") instead of a bare exit status. A successful link normally writes
    // nothing, so capturing doesn't change observable behaviour on success.
    let output = cmd.output().map_err(|e| CompileError::Link(e.to_string()))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let mut msg = format!("linker exited with status {}", output.status);
        if !stderr.trim().is_empty() {
            msg.push_str("\n");
            msg.push_str(stderr.trim_end());
        }
        if !stdout.trim().is_empty() {
            msg.push_str("\n");
            msg.push_str(stdout.trim_end());
        }
        return Err(CompileError::Link(msg));
    }

    Ok(())
}

/// Pick the clang driver to use for coverage links. Prefers the LLVM-22-matched `clang-22`
/// (matching the codegen toolchain), falling back to the unversioned `clang`, then bare `clang`
/// even if not yet probed (the link step surfaces a clear error if it's truly absent). Using
/// clang lets `-fprofile-instr-generate` resolve the host-correct `libclang_rt.profile` with no
/// hardcoded path.
fn coverage_link_driver() -> String {
    for candidate in ["clang-22", "clang"] {
        if Command::new(candidate)
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
        {
            return candidate.to_string();
        }
    }
    // Last resort: let the link attempt produce a clear "clang not found" error.
    "clang".to_string()
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
