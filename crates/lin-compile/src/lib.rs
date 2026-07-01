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
pub use lin_codegen::PgoMode;
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
    /// `--debug`/`-g`: emit DWARF line tables and a source-mapped binary for stepping in a
    /// debugger (lldb/CodeLLDB). Implies `optimize = false` (O0, so line mapping holds), keeps the
    /// object file's debug sections, and keeps `val` globals at default linkage so the debugger can
    /// resolve them. Default false — normal builds are byte-unaffected.
    pub debug: bool,
    /// Profile-Guided Optimization mode. Defaults to `PgoMode::None` (standard O2, byte-identical
    /// to the previous behaviour). Set by `LIN_PGO_GEN=1` (instrument) or
    /// `LIN_PGO_USE=<profdata>` (use merged profile). See `docs/PERFORMANCE.md §5.8`.
    pub pgo: PgoMode,
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
    /// An `import` referred to a module file that does not exist. Carries the import path as
    /// written, the absolute path we tried to read, an optional did-you-mean suggestion, whether
    /// the import looked like a stdlib (`std/...`) import, the span of the import statement, and
    /// the file that contained it (for diagnostic rendering).
    ModuleNotFound {
        import_path: String,
        tried: PathBuf,
        suggestion: Option<String>,
        std_like: bool,
        span: lin_common::Span,
        importing_file: String,
    },
    /// The entry source file passed on the command line does not exist.
    SourceFileNotFound(PathBuf),
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
            CompileError::Link(msg) => write!(f, "build error: {}", msg),
            CompileError::ImportCycle(chain) => {
                write!(f, "circular import detected: {}", chain)
            }
            CompileError::ModuleNotFound {
                import_path,
                tried,
                suggestion,
                std_like,
                ..
            } => {
                write!(
                    f,
                    "module not found: could not resolve import \"{}\"\n  tried to read: {}",
                    import_path,
                    tried.display()
                )?;
                if *std_like {
                    write!(
                        f,
                        "\n  note: \"{}\" is not a built-in stdlib module",
                        import_path
                    )?;
                }
                if let Some(s) = suggestion {
                    write!(f, "\n  help: did you mean \"{}\"?", s)?;
                }
                Ok(())
            }
            CompileError::SourceFileNotFound(path) => {
                write!(f, "source file not found: {}", path.display())
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
    let source = std::fs::read_to_string(source_path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            CompileError::SourceFileNotFound(source_path.to_path_buf())
        } else {
            CompileError::Io(e)
        }
    })?;
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

    // Path-11 Leg 2, Stage 1 — shadow lambda-set statistics (env-gated; zero cost when off).
    // Aggregates the main module + every (transitively) imported module so the distribution covers
    // user code AND the stdlib combinators it pulls in. Pure measurement — no IR/codegen effect.
    if lin_check::lambda_set_stats::enabled() {
        emit_lambda_set_stats(&module_name, &typed_module, &imported_modules);
    }

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

/// Emit Path-11 lambda-set call-site statistics to stderr. Reports the main module and the union
/// of all imported modules (stdlib + user imports) separately, then a grand total. Static counts
/// (one per syntactic call site) — see FINDINGS.md for the dynamic-weighting caveat.
fn emit_lambda_set_stats(
    module_name: &str,
    main: &lin_check::TypedModule,
    imports: &HashMap<String, lin_check::TypedModule>,
) {
    use lin_check::lambda_set_stats::{collect_module, Stats};
    let main_stats = collect_module(main);
    let mut import_stats = Stats::default();
    // Sort import keys for deterministic output.
    let mut keys: Vec<&String> = imports.keys().collect();
    keys.sort();
    for k in keys {
        import_stats.add(&collect_module(&imports[k]));
    }
    let mut total = main_stats.clone();
    total.add(&import_stats);

    eprintln!("=== LIN_LAMBDA_STATS: {module_name} ===");
    eprintln!("[main]    callback-args : {}", main_stats.callback_args.summary());
    eprintln!("[main]    indirect-callees: {}", main_stats.indirect_callees.summary());
    eprintln!("[imports] callback-args : {}", import_stats.callback_args.summary());
    eprintln!("[imports] indirect-callees: {}", import_stats.indirect_callees.summary());
    eprintln!("[TOTAL]   callback-args : {}", total.callback_args.summary());
    eprintln!("[TOTAL]   indirect-callees: {}", total.indirect_callees.summary());
    eprintln!("[TOTAL]   ALL closure call sites: {}", total.combined().summary());
}

/// Type-check `opts.source_path` and all of its (transitive) imports, stopping before any
/// lowering or codegen. Returns the non-error diagnostics (warnings) on success; errors are
/// returned via `Err(CompileError::TypeCheck(_))`. This shares the entire import-resolution
/// front end with `compile()`, so `lin check` and `lin build` agree on what they accept.
pub fn check(opts: &CheckOptions) -> Result<Vec<lin_common::Diagnostic>, CompileError> {
    let front = check_front_end(&opts.source_path)?;

    // Mirror `compile()`'s ADR-046 gate: `replace` is only valid in a `*.test.lin` file.
    let is_test_file = opts.source_path.to_string_lossy().ends_with(".test.lin");
    if !is_test_file && !front.typed_module.replacements.is_empty() {
        let span = front.typed_module.replacements[0].span;
        return Err(CompileError::TypeCheck(vec![lin_common::Diagnostic::error(
            span,
            "`replace` is only allowed in a `*.test.lin` file (it mocks an import for tests). \
             Remove it from this program (ADR-046)."
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

    // ADR-046: `replace` is a TEST-ONLY mock — valid only in a `*.test.lin`. Gating on the
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
             Remove it from this program (ADR-046)."
                .to_string(),
        )]));
    }

    // 4. LLVM codegen via the LinIR pipeline (the sole compilation backend).
    // When `opts.coverage` is set, the codegen instruments per-block counters and emits the
    // LLVM coverage-mapping globals; only the main module and user (non-stdlib) imports are
    // instrumented (stdlib import sources are not tracked, so they pass `None` below).
    let context = Context::create();
    let mut cg = Codegen::new(&context, &module_name, opts.coverage, opts.debug);

    // DWARF (--debug): register the main module's source for line-table emission. Use the canonical
    // absolute path so the debugger can locate the `.lin` file. No-op without `--debug`.
    if opts.debug {
        let abs = std::fs::canonicalize(&opts.source_path)
            .unwrap_or_else(|_| opts.source_path.clone())
            .to_string_lossy()
            .to_string();
        cg.init_debug_info(&abs, &source);
    }

    // Determine, before any function is declared, whether the whole program may spawn an
    // async boundary — it references any concurrency intrinsic (the `lin_async`/`lin_parallel`/
    // `lin_worker`/… family, reachable only via `std/async`). When it does, codegen must NOT
    // mark user functions `nounwind`, because a runtime fault inside a thunk unwinds through
    // Lin frames to the thread boundary (spec §24.2.2, ADR-027). Scan the main module and every
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

    // ADR-046: group the main module's test `replace` overrides by the import path they target,
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
        // Representation-inference pass (repr.rs) — runs after monomorphize+lower, immediately
        // before rc_elide so RC sees representation-stable IR. STAGE 3: stores the per-temp repr
        // table on each `func.repr` for codegen to consume at every packed-vs-boxed DECIDE / ASSUME
        // site; in debug builds also asserts the Stage-2 oracle (new analysis == old type
        // predicates) + the soundness verifier.
        // Ownership conventions — `ownership_verify` is the load-bearing RC authority. `infer_conventions`
        // annotates each IR function with per-param/per-return owning strategy, escape-alias maps, and
        // container-insert conventions; lower.rs consumes these at 12+ call sites (owning_strategy,
        // container_insert_convention, escape_alias_convention, box_shell_reclaim, …) to decide actual
        // Retain/Release emission. When `LIN_OWNERSHIP_SHADOW` is set, the verifier runs over the
        // lowered IR and hard-rejects UnauditedIntrinsic violations (also enforced by codegen verify_module).
        // Both calls are load-bearing: removing either changes RC output or drops the soundness check.
        lin_ir::ownership_verify::infer_conventions(&mut ir_module);
        if std::env::var("LIN_OWNERSHIP_SHADOW").is_ok() {
            emit_ownership_shadow_report(&ir_module, &opts.source_path.to_string_lossy());
        }
        // 0xFE inline gate: must run BEFORE repr::run so repr seeding sees the updated inline flags.
        lin_ir::escape::analyze_array_inline(&mut ir_module);
        lin_ir::repr::run(&mut ir_module);
        rc_elide::elide_rc(&mut ir_module, &cg.import_named_convs);
        // Sealed-records Stage 4: mark non-escaping all-scalar sealed-record constructions for
        // stack allocation AND suppress their Retain/Release emission (see lin_ir::escape). Runs
        // after RC elision so it sees and removes the surviving Retain/Release on stack values.
        lin_ir::escape::analyze(&mut ir_module);
        // Stack-allocate non-escaping `var` cells (entry-block alloca instead of lin_alloc) so
        // mem2reg/LICM/bounds_elide are not defeated by an opaque heap pointer in hot loops.
        lin_ir::escape::analyze_cells(&mut ir_module);
        // Redundant-read elimination (CSE): replace repeated Index/FieldGet on the same
        // object+key within a basic block with a Copy of the first result, when no
        // intervening mutation or call could have changed the slot value (escape-gated).
        lin_ir::redundant_read::run(&mut ir_module);
        // Box/unbox cancellation peephole (RT.2b): cancel a scalar value that is
        // boxed into a union slot and immediately unboxed back to the same scalar
        // type. Runs after rc_elide and escape so the IR is in its final form
        // (no RC pairs being re-added later). Safe to run before rc_verify: a
        // cancelled pair leaves no dangling RC obligations (only scalars are
        // cancelled — no heap retain/release involved). Canonicalises the pair to
        // a single Copy, which is a no-op from the LLVM perspective and lets LLVM
        // fold away any bookkeeping around the now-dead box temp.
        lin_ir::box_unbox_elide::elide_box_unbox(&mut ir_module);
        // Bounds-check elision: mark flat-scalar-array Index instructions as
        // `proven_inbounds` when the IR-level analysis proves `0 <= key < len`.
        // Runs last (after all other passes) so it sees the final IR state.
        lin_ir::bounds_elide::elide_bounds(&mut ir_module);
        lin_ir::substr_map_fuse::run(&mut ir_module);
        lin_ir::getset_map_fuse::run(&mut ir_module);
        // Static RC-balance verifier (Cluster 2) — VERIFICATION ONLY, gated on `LIN_VERIFY_RC=1`,
        // OFF by default so it can never affect a normal build. Runs on the FINAL lowered IR (after
        // RC insertion + rc_elide + escape stack-alloc) and reports per-path leak / over-release /
        // use-after-release imbalances to stderr. Never mutates `ir_module`.
        lin_ir::rc_verify::verify_if_enabled(&ir_module, &opts.source_path.to_string_lossy());
        cg.compile_module_from_ir(&ir_module);
    }

    // Emit the module-level coverage globals once every module has been compiled.
    if opts.coverage {
        cg.finalize_coverage();
    }

    // DWARF (--debug): finalise all debug metadata before any IR/object emission. No-op otherwise.
    cg.finalize_debug_info();

    // Debug builds are O0: the LLVM optimisation pipeline would mangle/coalesce instructions and
    // break the line-table mapping. `opts.optimize` is already false in `--debug` (set by the CLI),
    // but guard here too so debug info is never run through the optimiser.
    if opts.optimize && !opts.debug {
        // BL.1: bitcode-runtime merge (opt-in via LIN_BC_RUNTIME=1).
        // Loads the runtime bitcode produced by
        //   RUSTFLAGS="--emit=llvm-bc -C codegen-units=1 -C opt-level=2" cargo build -p lin-runtime
        // and merges it into the user module before the O2 pass, so the
        // inliner can see every runtime body and cancel box/unbox/RC pairs.
        // Default build is unchanged; only the flag-on path differs.
        if std::env::var_os("LIN_BC_RUNTIME").is_some() {
            match find_runtime_bc() {
                Some(bc_path) => {
                    match std::fs::read(&bc_path) {
                        Ok(bc_bytes) => {
                            cg.merge_runtime_bitcode(&bc_bytes)
                                .map_err(|e| CompileError::Codegen(format!("LIN_BC_RUNTIME merge failed: {e}")))?;
                        }
                        Err(e) => {
                            eprintln!("Warning: LIN_BC_RUNTIME set but could not read {:?}: {e}", bc_path);
                        }
                    }
                }
                None => {
                    eprintln!("Warning: LIN_BC_RUNTIME set but no runtime bitcode found.");
                    eprintln!("  Build it first:");
                    eprintln!("  RUSTFLAGS=\"--emit=llvm-bc -C codegen-units=1 -C opt-level=2\" cargo build -p lin-runtime");
                }
            }
        }
        cg.run_optimization_passes(&opts.pgo).map_err(CompileError::Codegen)?;
    }

    // 5. Emit LLVM IR if requested. Emitted AFTER the optimisation pipeline so it reflects the IR
    // that is actually compiled into the object — with `LIN_NO_OPT=1`/`--debug` (optimize == false)
    // the pipeline is skipped, so this is the raw pre-optimisation IR (what the IR-inspection tests
    // rely on); with optimisation on it is the optimised IR (so e.g. inlined leaf helpers are gone).
    // Still before `verify` so broken IR can be inspected.
    if opts.emit_ir {
        let ir_path = opts.output_path.with_extension("ll");
        cg.emit_llvm_ir(&ir_path).map_err(CompileError::Codegen)?;
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
                "could not build your program: a required external library could not be found (missing library: {})",
                lib
            )));
        }
    }

    // 8. Link with runtime and any foreign libraries
    link(&obj_path, &opts.output_path, &foreign_libs, opts.coverage, &opts.pgo, opts.debug)?;

    // Clean up the .o file — but KEEP it for debug builds. On Linux lldb reads DWARF from the linked
    // binary, but on macOS the debug map points lldb at the individual .o, so removing it breaks
    // source-line debugging there. Harmless to keep on Linux.
    if !opts.debug {
        let _ = std::fs::remove_file(&obj_path);
    }

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
// Bumped to 2: Stage 0.5 of sealed-records changed `Type::Object` from a tuple variant
// `Object(IndexMap)` to a struct variant `Object { fields, sealed }`, altering the bincode
// layout of every serialized `Type`. A `.typed`/`.sig` written by a v1 binary must be rejected
// rather than mis-deserialized. See ADR-057 (Type serialization changed → cache version bump).
// v3 (Path-11): `Type::Function` gained an inert `lset: LambdaSet` field and `TypedExpr::Function`
// gained `lambda_id: u32`, changing the bincode layout of every serialized `Type`/`TypedStmt`.
// A `.typed`/`.sig` written by a v2 binary must be rejected (stale layout → mis-decode).
// v4 (feat/tar-entries): `Type::TarEntry` added as a new enum variant (after `Promise`), shifting
// the bincode discriminant of every variant that follows. Stale v3 `.typed`/`.sig` must be rejected.
// v5 (fix/lsp-named-type-display): `Type::Object` gained a `name: Option<String>` field. Old caches
// decode as `None` via `#[serde(default)]`, but the stamp is bumped to be safe.
// v6 (refactor/opaque-type): `Type::TarEntry` (unit variant) replaced by `Type::Opaque(String)`
// (newtype variant). The bincode discriminant shifts for every variant after `Promise`, and the
// payload of what was `TarEntry` changes from unit → string. Stale v5 caches must be rejected.
const CACHE_FORMAT_VERSION: u32 = 6;

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

/// A process-wide monotonic counter used to make cache temp-file names unique PER WRITE — not just
/// per process. `lin test` compiles many files concurrently on rayon threads (all sharing this
/// pid), and files that share an import resolve to the SAME cache `key`; if two threads wrote to
/// `{key}.tmp.{pid}` at once they would clobber each other's in-flight temp before the rename.
/// Pid keeps distinct `lin` processes apart; this counter keeps concurrent writers within one
/// process apart. Each writer renames its OWN temp onto the final path — rename is atomic, so the
/// final cache file is always a complete, consistent image regardless of who wins the last rename
/// (the content is identical: same key == same bytes).
static CACHE_TMP_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Build a writer-unique temp path next to `final_path` for the write-to-temp-then-rename dance.
fn unique_tmp_path(cache_dir: &Path, key: &str, ext: &str) -> PathBuf {
    let seq = CACHE_TMP_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    cache_dir.join(format!("{}.{}.tmp.{}.{}", key, ext, std::process::id(), seq))
}

/// Save a `TypedModule` to `.lin-cache/` keyed by `key`.
/// Uses write-to-temp-then-rename for atomic, concurrent-safe cache writes.
fn save_cache(key: &str, module: &TypedModule, base_dir: &Path) {
    let cache_dir = base_dir.join(".lin-cache");
    if std::fs::create_dir_all(&cache_dir).is_err() {
        return;
    }
    let final_path = cache_dir.join(format!("{}.typed", key));
    let tmp_path = unique_tmp_path(&cache_dir, key, "typed");
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
    let tmp_path = unique_tmp_path(&cache_dir, key, "sig");
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

/// Stamp every diagnostic in `diags` with a source file path, so renderers can load the right
/// source text. Called when type errors originate in an imported module rather than the entry file.
fn tag_diagnostics(diags: Vec<lin_common::Diagnostic>, file: Option<&str>) -> Vec<lin_common::Diagnostic> {
    if let Some(f) = file {
        diags.into_iter().map(|d| d.with_file(f)).collect()
    } else {
        diags
    }
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
    let mut import_overloads: HashMap<(String, String), Vec<(Type, String)>> = HashMap::new();
    for (path, imp_module) in imported_modules {
        let sig = ModuleSignature::from_module(imp_module);
        for (name, ty) in sig.exports {
            import_type_map.insert((path.clone(), name), ty);
        }
        for (name, decl) in sig.type_exports {
            import_type_decls.insert((path.clone(), name), decl);
        }
        for (name, members) in sig.overloads {
            import_overloads.insert(
                (path.clone(), name),
                members.into_iter().map(|o| (o.ty, o.symbol)).collect(),
            );
        }
    }
    let mut checker = Checker::new();
    checker.import_types = import_type_map;
    checker.import_overloads = import_overloads;
    checker.stdlib_export_index = build_stdlib_export_index();
    checker.import_type_decls = import_type_decls;
    // Every import here is a fully-resolved `TypedModule`, so unknown-export validation is safe.
    checker.fully_resolved_import_paths = imported_modules.keys().cloned().collect();
    // The trusted stdlib forwards Json handles into concrete intrinsic/foreign params by
    // design, so it checks Json->concrete leniently (ADR-045). User code does not.
    checker.lenient_json = lenient_json;
    // `lin_*` intrinsics are accessible only to trusted stdlib modules; the LIN_ALLOW_INTRINSICS
    // env var is a test-only escape hatch for the compiler's own intrinsic-exercising fixtures.
    checker.allow_intrinsics = lenient_json || std::env::var_os("LIN_ALLOW_INTRINSICS").is_some();
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
    seeded_types: &HashMap<(String, String), (Vec<String>, Type)>,
    lenient_json: bool,
) -> Result<(TypedModule, Vec<lin_common::Diagnostic>), Vec<lin_common::Diagnostic>> {
    let mut import_type_map: HashMap<(String, String), Type> = HashMap::new();
    let mut import_type_decls: HashMap<(String, String), (Vec<String>, Type)> = HashMap::new();
    let mut import_overloads: HashMap<(String, String), Vec<(Type, String)>> = HashMap::new();
    // Already-resolved (acyclic) imports first.
    for (path, imp_module) in imported_modules {
        let sig = ModuleSignature::from_module(imp_module);
        for (name, ty) in sig.exports {
            import_type_map.insert((path.clone(), name), ty);
        }
        for (name, decl) in sig.type_exports {
            import_type_decls.insert((path.clone(), name), decl);
        }
        for (name, members) in sig.overloads {
            import_overloads.insert(
                (path.clone(), name),
                members.into_iter().map(|o| (o.ty, o.symbol)).collect(),
            );
        }
    }
    // Provisional peer signatures from within the SCC override/augment the map (a peer is not in
    // `imported_modules` yet, so this is the only source of its type).
    for ((path, name), ty) in seeded {
        import_type_map.insert((path.clone(), name.clone()), ty.clone());
    }
    // Provisional peer TYPE aliases — same role for the type namespace. Without this, a cross-cycle
    // `import { T } from "./peer"` used in type position resolves to "Unknown type 'T'" because the
    // peer module is not in `imported_modules` yet (ADR-083).
    for ((path, name), decl) in seeded_types {
        import_type_decls.insert((path.clone(), name.clone()), decl.clone());
    }
    let mut checker = Checker::new();
    checker.import_types = import_type_map;
    checker.import_overloads = import_overloads;
    checker.stdlib_export_index = build_stdlib_export_index();
    checker.import_type_decls = import_type_decls;
    // Only the acyclic deps (`imported_modules`) are fully resolved. The seeded SCC peers carry
    // VALUE exports only, so they are deliberately NOT marked fully-resolved — a type import across
    // the cycle must fall back to a TypeVar, not be rejected as "no export".
    checker.fully_resolved_import_paths = imported_modules.keys().cloned().collect();
    checker.lenient_json = lenient_json;
    // `lin_*` intrinsics are accessible only to trusted stdlib modules; the LIN_ALLOW_INTRINSICS
    // env var is a test-only escape hatch for the compiler's own intrinsic-exercising fixtures.
    checker.allow_intrinsics = lenient_json || std::env::var_os("LIN_ALLOW_INTRINSICS").is_some();
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

/// Embedded stdlib source files (mirrors interpreter's include_str! approach), as an enumerable
/// `(module-name, source)` table. The `include_str!` paths must stay literal, so this is the single
/// source of truth both for `stdlib_source` lookups and for did-you-mean candidate enumeration.
const STDLIB_MODULES: &[(&str, &str)] = &[
    ("std/io",       include_str!("../../../stdlib/io.lin")),
    ("std/json",     include_str!("../../../stdlib/json.lin")),
    ("std/string",   include_str!("../../../stdlib/string.lin")),
    ("std/number",   include_str!("../../../stdlib/number.lin")),
    ("std/array",    include_str!("../../../stdlib/array.lin")),
    ("std/iter",     include_str!("../../../stdlib/iter.lin")),
    ("std/fs",       include_str!("../../../stdlib/fs.lin")),
    ("std/ffi",      include_str!("../../../stdlib/ffi.lin")),
    ("std/http",     include_str!("../../../stdlib/http.lin")),
    ("std/object",   include_str!("../../../stdlib/object.lin")),
    ("std/template", include_str!("../../../stdlib/template.lin")),
    ("std/async",    include_str!("../../../stdlib/async.lin")),
    ("std/env",      include_str!("../../../stdlib/env.lin")),
    ("std/test",     include_str!("../../../stdlib/test.lin")),
    ("std/time",     include_str!("../../../stdlib/time.lin")),
    ("std/datetime", include_str!("../../../stdlib/datetime.lin")),
    ("std/path",     include_str!("../../../stdlib/path.lin")),
    ("std/math",     include_str!("../../../stdlib/math.lin")),
    ("std/bytes",    include_str!("../../../stdlib/bytes.lin")),
    ("std/regex",    include_str!("../../../stdlib/regex.lin")),
    ("std/crypto",   include_str!("../../../stdlib/crypto.lin")),
    ("std/csv",      include_str!("../../../stdlib/csv.lin")),
    ("std/encoding", include_str!("../../../stdlib/encoding.lin")),
    ("std/random",   include_str!("../../../stdlib/random.lin")),
    ("std/bignum",   include_str!("../../../stdlib/bignum.lin")),
    ("std/decimal",  include_str!("../../../stdlib/decimal.lin")),
    ("std/net",      include_str!("../../../stdlib/net.lin")),
    ("std/process",  include_str!("../../../stdlib/process.lin")),
    ("std/tty",      include_str!("../../../stdlib/tty.lin")),
    ("std/signal",   include_str!("../../../stdlib/signal.lin")),
    ("std/yaml",     include_str!("../../../stdlib/yaml.lin")),
    ("std/jq",       include_str!("../../../stdlib/jq.lin")),
    ("std/stream",   include_str!("../../../stdlib/stream.lin")),
    ("std/compress", include_str!("../../../stdlib/compress.lin")),
    ("std/archive",  include_str!("../../../stdlib/archive.lin")),
    ("std/event",    include_str!("../../../stdlib/event.lin")),
];

/// Embedded stdlib source for `path`, if `path` names a built-in module.
fn stdlib_source(path: &str) -> Option<&'static str> {
    STDLIB_MODULES
        .iter()
        .find(|(name, _)| *name == path)
        .map(|(_, src)| *src)
}

/// Build an `export-name -> [module paths]` index across the embedded stdlib, so the checker can
/// suggest the RIGHT module when an `import { x } from "m"` names an `x` that some OTHER stdlib
/// module exports (e.g. `gunzip` lives in `std/compress`, not `std/stream`).
///
/// A simple per-line textual scan: stdlib is canonically formatted, so every export is a line of
/// the form `export val NAME = ...` or `export type NAME ...` (modulo leading whitespace). We take
/// the identifier immediately after `export val `/`export type `.
fn build_stdlib_export_index() -> HashMap<String, Vec<String>> {
    fn ident_after<'a>(rest: &'a str) -> Option<&'a str> {
        let rest = rest.trim_start();
        let end = rest
            .find(|c: char| !(c.is_alphanumeric() || c == '_'))
            .unwrap_or(rest.len());
        if end == 0 {
            None
        } else {
            Some(&rest[..end])
        }
    }

    let mut index: HashMap<String, Vec<String>> = HashMap::new();
    for (module_name, source) in STDLIB_MODULES.iter() {
        let module_name: &str = module_name;
        for line in source.lines() {
            let line = line.trim_start();
            let ident = if let Some(rest) = line.strip_prefix("export val ") {
                ident_after(rest)
            } else if let Some(rest) = line.strip_prefix("export type ") {
                ident_after(rest)
            } else {
                None
            };
            if let Some(ident) = ident {
                let entry = index.entry(ident.to_string()).or_default();
                if !entry.iter().any(|m| m.as_str() == module_name) {
                    entry.push(module_name.to_string());
                }
            }
        }
    }
    index
}

/// Closest stdlib module name to `import_path` within `max_dist`, for did-you-mean diagnostics.
fn closest_stdlib_module(import_path: &str, max_dist: usize) -> Option<String> {
    lin_common::closest_match(
        import_path,
        STDLIB_MODULES.iter().map(|(name, _)| *name),
        max_dist,
    )
    .map(|s| s.to_string())
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
    current_file: &str,
    graph: &mut ImportGraph,
) -> Result<Vec<String>, CompileError> {
    let mut deps = Vec::new();
    for stmt in &ast_module.statements {
        let Stmt::Import { path, span, .. } = stmt else { continue };
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
                // Stdlib sources are embedded strings (not on disk), so we leave parse-error
                // diagnostics untagged (file: None). The renderer will fall back to the entry
                // file, which is harmless: stdlib parse errors don't occur in practice because
                // CI type-checks stdlib on every push.
                let ast = parse_source(src).map_err(CompileError::TypeCheck)?;
                (ast, src.to_string(), base_dir.to_path_buf(), None, true)
            } else {
                let file_path = base_dir.join(format!("{}.lin", path));
                let src = std::fs::read_to_string(&file_path).map_err(|e| {
                    if e.kind() == std::io::ErrorKind::NotFound {
                        let std_like = path.starts_with("std/");
                        // `std/std/stream` -> `std/stream` is edit distance 4; allow a little
                        // headroom (6) without letting nonsense matches through.
                        let suggestion = if std_like {
                            closest_stdlib_module(path, 6)
                        } else {
                            None
                        };
                        CompileError::ModuleNotFound {
                            import_path: path.clone(),
                            tried: file_path.clone(),
                            suggestion,
                            std_like,
                            span: *span,
                            importing_file: current_file.to_string(),
                        }
                    } else {
                        CompileError::Io(e)
                    }
                })?;
                // Tag parse-error diagnostics with the imported file's absolute path so the
                // renderer points at the right file and offset (not the entry file).
                let ast = parse_source(&src).map_err(|diags| {
                    let abs = file_path.canonicalize().unwrap_or_else(|_| file_path.clone());
                    CompileError::TypeCheck(tag_diagnostics(diags, Some(&abs.to_string_lossy())))
                })?;
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
            abs_path: abs_path.clone(),
            is_stdlib,
            deps: Vec::new(),
        });
        graph.order.push(identity.clone());

        let child_file = abs_path.as_deref().unwrap_or("<stdlib>");
        let child_deps = build_import_graph(&ast, &imported_base, child_file, graph)?;
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
        Expr::Coalesce { left, right, .. } => { go!(left); go!(right); }
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
        Expr::Block(stmts, tail, _, _) => {
            for s in stmts { walk_stmt_exprs(s, f); }
            go!(tail);
        }
        Expr::Function { body, .. } => go!(body),
        Expr::Array(elems, _, _) => for e in elems { go!(e.inner_expr()); },
        Expr::Object(fields, _, _) => for field in fields {
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
    build_import_graph(ast_module, base_dir, &_entry_path.display().to_string(), &mut graph)?;
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
        .map_err(|diags| CompileError::TypeCheck(tag_diagnostics(diags, m.abs_path.as_deref())))?;
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

/// Emit the SHADOW-MODE ownership report (Path-10/11 Leg 1) to stderr. Report-only: it never
/// changes compilation. Prints a one-line per-module summary (function/param convention split +
/// violation counts by kind) and the individual violations, so a corpus run can be grepped and
/// classified. Gated by the `LIN_OWNERSHIP_SHADOW` env var at the single call site.
fn emit_ownership_shadow_report(module: &lin_ir::LinModule, source: &str) {
    use lin_ir::ownership_verify::{shadow_summary, ViolationKind};
    let s = shadow_summary(module);
    let mut by_kind: std::collections::BTreeMap<&'static str, usize> = std::collections::BTreeMap::new();
    for v in &s.violations {
        *by_kind.entry(v.kind.label()).or_insert(0) += 1;
    }
    eprintln!(
        "[ownership-shadow] {src}: fns={fns} params={pt} (borrow={b} own={o} inout={io}) violations={nv} {bk:?}",
        src = source,
        fns = s.functions,
        pt = s.params_total,
        b = s.params_borrow,
        o = s.params_own,
        io = s.params_inout,
        nv = s.violations.len(),
        bk = by_kind,
    );
    for v in &s.violations {
        // Suppress the un-audited-intrinsic noise from the per-violation dump unless it is the only
        // kind present — those are a table-completeness checklist, summarized in the count line.
        if v.kind == ViolationKind::UnauditedIntrinsic {
            eprintln!("[ownership-shadow]   GAP unaudited-intrinsic {} (fn {})", v.detail, v.func);
            continue;
        }
        eprintln!(
            "[ownership-shadow]   {kind} fn={fn_} block={blk}: {detail}",
            kind = v.kind.label(),
            fn_ = v.func,
            blk = v.block,
            detail = v.detail,
        );
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

    // (1a) Phase 1, sweep A: extract every member's exported TYPE aliases. Functions/values flow
    // through `provisional` (import_types) below; exported `type T = ...` aliases must flow
    // separately into `import_type_decls`, or a cross-cycle `import { T } from "./peer"` used in
    // type position resolves to "Unknown type 'T'" (ADR-083).
    //
    // This is a FIXPOINT, and it harvests type aliases WITHOUT requiring the member's value/function
    // BODIES to type-check (`collect_exported_type_decls`). The body-tolerance matters: a member
    // whose parameter is annotated with a PEER alias used as a MAP (e.g. `(src: ST)` where
    // `type ST = { String: UInt32 }` lives in the peer) cannot type-check its body until that alias
    // is seeded — `ST` would resolve to a placeholder TypeVar, so `src[k] ?? d` reports "left operand
    // is never null". A full `check_module` would surface that body error and abort the SCC before
    // the (perfectly resolvable) alias was ever harvested. The fixpoint also lets an alias that
    // references a PEER's alias resolve: each pass seeds the previous pass's harvest back in, so a
    // dependency chain of length k settles within `members.len()` passes.
    //
    // The harvest must also see each member's ACYCLIC (non-cycle) imported type aliases. A member
    // can annotate its own alias body with a peer-OUTSIDE-the-SCC type — most commonly an
    // index-signature key alias: `type M = { Sid: UInt32 }` where `Sid = String` is imported from a
    // module that is NOT in the cycle (already fully resolved, sitting in `cache`). Those decls are
    // not in `provisional_types` (that map only carries SCC peers), so without seeding them the key
    // `Sid` resolves to a placeholder TypeVar and the index-signature arm cannot prove it is
    // String-keyed — `{ Sid: UInt32 }` then falls back to a fixed-shape RECORD with a field literally
    // named "Sid", and dynamic `m[k]` is rejected. Pre-collect every cached (acyclic) module's
    // exported type decls, keyed by the same import-path-string the cache uses, and layer the
    // provisional SCC peers on top each pass.
    let mut acyclic_type_decls: HashMap<(String, String), (Vec<String>, Type)> = HashMap::new();
    for (path, imp_module) in cache.iter() {
        let sig = ModuleSignature::from_module(imp_module);
        for (name, decl) in sig.type_exports {
            acyclic_type_decls.insert((path.clone(), name), decl);
        }
    }
    let mut provisional_types: HashMap<(String, String), (Vec<String>, Type)> = HashMap::new();
    for _pass in 0..members.len().max(1) {
        let before = provisional_types.clone();
        for m in &members {
            let mut checker = Checker::new();
            // Acyclic imported type aliases first; provisional SCC-peer aliases override them.
            checker.import_type_decls = acyclic_type_decls.clone();
            checker.import_type_decls.extend(provisional_types.clone());
            checker.stdlib_export_index = build_stdlib_export_index();
            checker.allow_intrinsics =
                m.is_stdlib || std::env::var_os("LIN_ALLOW_INTRINSICS").is_some();
            for (name, decl) in checker.collect_exported_type_decls(&m.ast) {
                for p in &m.paths {
                    provisional_types.insert((p.clone(), name.clone()), decl.clone());
                }
            }
        }
        // Settle once a pass neither adds a binding nor refines an existing one (an alias that
        // referenced a peer's still-placeholder alias resolves to a more-concrete body on the next
        // pass — that is a content change with no key change, so compare the whole map).
        if provisional_types == before {
            break;
        }
    }

    // (1b) Phase 1, sweep B: re-check each member with the peer TYPE aliases seeded so cross-cycle
    // type annotations (e.g. a param `(t: T)` where `T` is a peer alias) resolve to their REAL
    // structural type rather than the placeholder TypeVar. Only now are the provisional VALUE
    // signatures accurate — a function whose parameter is a peer record must advertise that record
    // (its concrete representation) so callers box/seal the argument consistently with the callee.
    let mut provisional: HashMap<(String, String), Type> = HashMap::new();
    for m in &members {
        let (typed, _w) =
            check_module_with_seeded_imports(&m.ast, cache, &HashMap::new(), &provisional_types, m.is_stdlib)
                .map_err(|diags| CompileError::TypeCheck(tag_diagnostics(diags, m.abs_path.as_deref())))?;
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
        let (typed, _w) =
            check_module_with_seeded_imports(&m.ast, cache, &provisional, &provisional_types, m.is_stdlib)
                .map_err(|diags| CompileError::TypeCheck(tag_diagnostics(diags, m.abs_path.as_deref())))?;
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

/// Turn raw link-step diagnostics into a clean, jargon-free, user-facing message. The raw text
/// (from `cc`/`ld`/`collect2`) is full of paths and mangled symbols an end user can't act on, so
/// we classify the failure and emit something actionable. We never surface `ld`/`linker`/`collect2`
/// or `exited with status` wording in the top line.
fn classify_link_failure(raw: &str) -> String {
    // An undefined reference to an INTERNAL/mangled symbol (stdlib/runtime/compiler-generated)
    // means a component the compiler should have provided is missing — a compiler/stdlib bug, not
    // a user mistake. With the checker's "module has no export" error (Fix 1) this is now
    // unreachable from a user import typo, but keep the safety net.
    if raw.contains("undefined reference to") {
        // Pull the first referenced symbol for bug-reportability, if we can.
        let symbol = raw
            .split("undefined reference to")
            .nth(1)
            .and_then(|rest| {
                let rest = rest.trim_start();
                // Symbols are usually quoted (`ld` uses backticks/quotes); fall back to the first token.
                let trimmed = rest.trim_start_matches(['`', '\'', '"']);
                let end = trimmed
                    .find(['`', '\'', '"', '\n'])
                    .unwrap_or(trimmed.len());
                let sym = trimmed[..end].trim();
                if sym.is_empty() { None } else { Some(sym.to_string()) }
            });
        let internal = symbol.as_deref().map(|s| {
            s.contains("std_") || s.contains("lin_") || s.ends_with("__val") || s.ends_with("__fn")
        });
        if internal.unwrap_or(true) {
            let mut msg = String::from(
                "could not finish building your program: an internal component was missing. \
This is likely a compiler bug — please report it at https://github.com/linusnorton/Lin/issues",
            );
            if let Some(sym) = symbol {
                msg.push_str(&format!("\n(details: undefined symbol `{}`)", sym));
            }
            return msg;
        }
    }

    // A missing external/foreign library — name it if we can extract it cheaply.
    let missing_lib = raw
        .split("cannot find -l")
        .nth(1)
        .map(|rest| {
            let end = rest
                .find(|c: char| c.is_whitespace())
                .unwrap_or(rest.len());
            rest[..end].to_string()
        })
        .or_else(|| {
            raw.split("library not found for -l").nth(1).map(|rest| {
                let end = rest
                    .find(|c: char| c.is_whitespace())
                    .unwrap_or(rest.len());
                rest[..end].to_string()
            })
        });
    if let Some(lib) = missing_lib {
        if !lib.is_empty() {
            return format!(
                "could not build your program: a required external library could not be found \
(missing library: {})",
                lib
            );
        }
    }

    // Anything else: we could not attribute the failure to a specific cause. Emit a clean,
    // jargon-free top line WITHOUT asserting a cause we don't actually know (e.g. claiming a
    // missing library when the real problem is something else), and keep the raw diagnostic on a
    // quiet `details:` line so a bug report still carries the underlying linker output.
    let mut msg = String::from(
        "could not build your program: the final build step failed for an unrecognised reason",
    );
    let detail = raw.trim();
    if !detail.is_empty() {
        let snippet = select_link_detail_lines(detail);
        if !snippet.is_empty() {
            msg.push_str(&format!("\n(details: {})", snippet.join(" | ")));
        }
    }
    msg
}

/// Pick the most useful non-empty lines from raw linker output for the `(details: …)` suffix.
///
/// The naive "first N non-empty lines" approach is wrong on macOS, where the linker output often
/// opens with a run of benign `ld: warning:` lines (e.g. "object file … was built for newer
/// 'macOS' version") — taking the first few keeps only warnings and truncates away the actual
/// error. So we prioritise lines that look like real errors and demote pure warning lines,
/// falling back to warnings only when there is genuinely nothing else.
fn select_link_detail_lines(detail: &str) -> Vec<&str> {
    // A pure warning line carries no failure cause; demote it.
    fn is_warning(line: &str) -> bool {
        let l = line.trim();
        l.starts_with("ld: warning:") || l.starts_with("warning:")
    }
    // Lines that positively look like the failure we want to surface.
    fn looks_like_error(line: &str) -> bool {
        let lower = line.to_ascii_lowercase();
        lower.contains("error")
            || lower.contains("fatal")
            || lower.contains("undefined")
            || lower.contains("duplicate symbol")
            || lower.contains("cannot find")
            // An `ld:`-prefixed line that isn't a warning is almost always the real diagnostic.
            || (lower.trim_start().starts_with("ld:") && !is_warning(line))
    }

    // An indented line is a CONTINUATION of the diagnostic above it. macOS ld prints the actual
    // missing symbol on indented lines UNDER the "Undefined symbols for architecture …:" header
    // (`  "_lin_foo", referenced from:` / `      _bar in baz.o`), and those lines match none of the
    // `looks_like_error` keywords. Capturing them is the whole point — the header alone doesn't name
    // the symbol. We only treat an indented line as a continuation when it FOLLOWS a kept error line
    // (so stray indented noise elsewhere isn't pulled in).
    fn is_continuation(line: &str) -> bool {
        line.starts_with(' ') || line.starts_with('\t')
    }

    const MAX_LINES: usize = 8;
    let lines = || detail.lines().filter(|l| !l.trim().is_empty());

    // First choice: error lines, each WITH the indented continuation lines beneath it (the symbol
    // names). Walk in order so a header and its symbol list stay together and in sequence.
    let mut errors: Vec<&str> = Vec::new();
    let mut keeping = false;
    for line in lines() {
        if looks_like_error(line) {
            keeping = true;
            errors.push(line);
        } else if keeping && is_continuation(line) {
            errors.push(line);
        } else {
            keeping = false;
        }
        if errors.len() >= MAX_LINES {
            break;
        }
    }
    if !errors.is_empty() {
        return errors;
    }

    // Otherwise: any non-warning lines (still better than warnings).
    let non_warnings: Vec<&str> = lines().filter(|l| !is_warning(l)).take(MAX_LINES).collect();
    if !non_warnings.is_empty() {
        return non_warnings;
    }

    // Last resort: nothing but warnings — surface them so a bug report still carries something.
    lines().take(MAX_LINES).collect()
}

fn link(obj_path: &Path, output_path: &Path, foreign_libs: &[String], coverage: bool, pgo: &PgoMode, debug: bool) -> Result<(), CompileError> {
    // Find the lin-runtime static library.
    let runtime_lib = find_runtime_lib();

    // The default link driver is the system `cc` (typically gcc). For coverage or PGO-GEN we
    // instead drive the link through clang, because the LLVM profile runtime
    // (`libclang_rt.profile`) and the `-fprofile-instr-generate` flag that pulls it in are
    // clang's — gcc doesn't understand them. clang locates the correct host-arch runtime itself.
    let pgo_gen = matches!(pgo, PgoMode::Generate);
    let driver = if coverage || pgo_gen { coverage_link_driver() } else { "cc".to_string() };

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
    // We prefer a loader-relative rpath so the produced binary + its vendored shared library are
    // RELOCATABLE: copy both together (preserving their relative layout) anywhere and the binary
    // still resolves the library, because the rpath token is expanded by the dynamic loader to the
    // directory the binary lives in at launch.
    //
    // The token is PLATFORM-SPECIFIC:
    //   * Linux/ELF: `$ORIGIN` — the relative-path math below produces e.g. `$ORIGIN/../libs`.
    //   * macOS/Mach-O: `@loader_path` — `$ORIGIN` is meaningless to dyld. (`@executable_path` is
    //     the sibling token; `@loader_path` is correct for the main executable too and is what we
    //     emit.)
    // We pick the token via `cfg!(target_os = "macos")`. This is HOST cfg, which is correct because
    // `lin` compiles for its own host (host == target). A cross-compiling Lin would need target-
    // aware selection; Lin does not cross-compile today. Using the runtime `cfg!(...)` (not
    // `#[cfg]`) keeps BOTH arms type-checked on every host.
    //
    // The token must reach the dynamic loader LITERALLY (the loader, not the linker or shell, does
    // the expansion). The link command runs via std::process::Command — not a shell — so the token
    // in `-Wl,-rpath,<token>/...` is passed as a literal argv element and is NOT shell-expanded.
    // `readelf -d` (Linux) / `otool -l` (macOS) on the output confirms the literal token survives.
    //
    // macOS install_name subtlety (handled by `macos_fixup_dylib_rpaths` after a successful link):
    // an executable's load command for a dylib records the DYLIB'S OWN install_name (LC_ID_DYLIB),
    // NOT the path we linked against. If that install_name is absolute or a bare leaf, dyld uses it
    // and NEVER consults the rpath. For the rpath to matter, the executable's reference must be
    // `@rpath/<leaf>`. A dylib we build ourselves can set `-install_name @rpath/<leaf>`; for a
    // vendored dylib we did not build we post-link rewrite the load command with
    // `install_name_tool -change <recorded> @rpath/<leaf> <exe>` (best-effort).
    let rpath_token = if cfg!(target_os = "macos") { "@loader_path" } else { "$ORIGIN" };
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

            // Compute a loader-relative rpath from the output binary's directory to the library's
            // directory. If a clean relative path can't be derived (e.g. the two live on different
            // mounts with no common base, or canonicalization fails), fall back to an ABSOLUTE
            // canonicalized rpath — robust, just not relocatable. dyld accepts a plain absolute
            // rpath too, so the fallback is identical on both OSes.
            let abs_out_dir = out_dir.canonicalize().unwrap_or_else(|_| out_dir.to_path_buf());
            let abs_parent = parent.canonicalize().unwrap_or_else(|_| parent.to_path_buf());
            let rpath = match relative_path(&abs_out_dir, &abs_parent) {
                Some(rel) if rel.as_os_str().is_empty() => rpath_token.to_string(),
                Some(rel) => format!("{}/{}", rpath_token, rel.display()),
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

    // Link the LLVM profile runtime when coverage or PGO instrumentation is enabled. Rather than
    // hardcoding the absolute path to `libclang_rt.profile-<arch>.a` (which bakes in the LLVM
    // patch version, host arch, and install prefix), let the `cc` driver locate and link the
    // correct runtime for this host via `-fprofile-instr-generate`. clang resolves the right
    // libclang_rt.profile itself, including its required deps (pthread/dl/rt on Linux). This is
    // portable across LLVM minor versions, architectures, and distros.
    if coverage || pgo_gen {
        cmd.arg("-fprofile-instr-generate");
    }

    // Link system libraries needed by lin-runtime (libc via cc, libm for math).
    cmd.arg("-lm");

    // macOS: lin-runtime statically embeds the `sysinfo` crate (std/process `memInfo`/`loadAverage`/
    // `uptime`/…), which on macOS reaches Apple's CoreFoundation (via `objc2-core-foundation`) and
    // IOKit. Those framework link directives live in sysinfo's build script and only fire when cargo
    // links the final artifact — but we link the user's binary ourselves with `cc` against
    // `liblin_runtime.a`, so they're lost and the CF/IOKit symbols come up undefined. The failure was
    // INTERMITTENT only because `-dead_strip` drops the sysinfo objects when the program never reaches
    // them; a program that does (e.g. calls `memInfo`) fails the link. Pass the frameworks explicitly.
    // No-op on every other OS (the cfg is the host linker target — `lin` always links for its host).
    if cfg!(target_os = "macos") {
        cmd.arg("-framework").arg("CoreFoundation");
        cmd.arg("-framework").arg("IOKit");
    }

    // DWARF (--debug): pass `-g` so the link driver preserves the object's debug sections in the
    // output binary (cc/clang default behaviour, but make it explicit). On Linux lldb then reads the
    // `.debug_*` sections straight from the linked binary. `--gc-sections` (below) keeps the debug
    // info of every section that survives, so it does not conflict.
    if debug {
        cmd.arg("-g");
    }

    // Garbage-collect unreferenced sections at link time. `lin-runtime.a` is a single static
    // archive carrying the WHOLE runtime (every intrinsic, every flat-array variant, all the
    // refcount/string/object machinery) plus, in dev builds, ~260MB of DWARF debug info across
    // ~1000 codegen-unit objects. A given Lin program references only a fraction of those symbols,
    // but the linker pulls in and emits every object that satisfies any reference and drags its
    // debug info along — so the cold link of a trivial program is ~5s, dominated entirely by the
    // archive, not the program. `--gc-sections` lets the linker drop sections (functions/data, and
    // their attached debug sections) that are transitively unreachable from the entry point. rustc
    // emits per-function/-data sections by default, so this is effective without recompiling the
    // runtime. Measured: cold link 5.2s -> 1.8s on a minimal program; whole-program reachability is
    // unchanged, so it never removes a symbol the program actually uses. We deliberately do NOT
    // strip debug info (e.g. via a `debug=false` profile): the ASan UAF-hunting workflow relies on
    // runtime symbolization, and `--gc-sections` keeps the debug info for sections that survive.
    // The flag is linker-specific: GNU ld / lld (Linux) take `--gc-sections`; Apple ld64 (macOS)
    // spells the same dead-code elimination `-dead_strip`. Pick via host cfg — `lin` always links
    // for its own host, so host cfg is the target linker.
    let dead_strip = if cfg!(target_os = "macos") { "-Wl,-dead_strip" } else { "-Wl,--gc-sections" };
    cmd.arg(dead_strip);

    // Capture stderr/stdout so a build-step failure can be CLASSIFIED into a clean, jargon-free,
    // user-facing message by `classify_link_failure` (no `ld`/`collect2`/`linker` wording leaks to
    // the user). A successful link normally writes nothing, so capturing doesn't change observable
    // behaviour on success.
    let output = cmd.output().map_err(|e| {
        CompileError::Link(format!(
            "could not build your program: the build toolchain could not be run ({})",
            e
        ))
    })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let combined = format!("{}\n{}", stderr, stdout);
        return Err(CompileError::Link(classify_link_failure(&combined)));
    }

    // macOS-only, best-effort: rewrite the executable's load commands for each vendored dylib to
    // `@rpath/<leaf>` so the `@loader_path` rpath emitted above is actually consulted by dyld.
    // No-op on Linux (the `cfg!` guard inside is a runtime false there, but BOTH arms compile).
    macos_fixup_dylib_rpaths(output_path, foreign_libs);

    Ok(())
}

/// macOS post-link fixup (best-effort, no-op on every other OS).
///
/// dyld resolves a dependent dylib by the path RECORDED in the executable's load command — which is
/// the dylib's own install_name (LC_ID_DYLIB), not the path we linked against. If that recorded
/// path is absolute or a bare leaf, the `@loader_path` rpath we baked in is never consulted. For a
/// dylib WE built we can set `-install_name @rpath/<leaf>` at build time; for a VENDORED dylib we
/// did not build, we rewrite the executable's reference to `@rpath/<leaf>` here, so the rpath is
/// used.
///
/// Mechanism: parse `otool -L <exe>` (each dependency line is whitespace-indented
/// `<path> (compatibility version ..., current version ...)`), find the recorded path whose leaf
/// filename matches the vendored dylib's leaf (allowing a soname-versioned variant such as
/// `libfoo.1.dylib`), and if it is not already `@rpath/<leaf>` run
/// `install_name_tool -change <recorded> @rpath/<leaf> <exe>`.
///
/// This is BEST-EFFORT: if `otool` / `install_name_tool` are missing or fail (e.g. a self-built
/// dylib that already records `@rpath/...`, so there is nothing to change), we warn and continue —
/// the link itself is NOT failed. The whole body is guarded by `cfg!(target_os = "macos")` so it
/// compiles on Linux (both arms are type-checked) but does nothing at runtime there.
fn macos_fixup_dylib_rpaths(output_path: &Path, foreign_libs: &[String]) {
    if !cfg!(target_os = "macos") {
        return;
    }

    // Collect the leaf filenames of the vendored dylibs we linked.
    let dylib_leaves: Vec<String> = foreign_libs
        .iter()
        .filter(|lib| lib.ends_with(".dylib"))
        .filter_map(|lib| {
            Path::new(lib)
                .file_name()
                .and_then(|s| s.to_str())
                .map(|s| s.to_string())
        })
        .collect();
    if dylib_leaves.is_empty() {
        return;
    }

    // Read the executable's recorded dependency paths via `otool -L`.
    let otool_out = match Command::new("otool")
        .args(["-L", &output_path.display().to_string()])
        .output()
    {
        Ok(o) if o.status.success() => o,
        Ok(_) | Err(_) => {
            eprintln!(
                "Warning: macOS rpath fixup skipped — `otool -L` unavailable or failed; the \
                 vendored dylib may not be found at runtime unless it records an @rpath \
                 install_name"
            );
            return;
        }
    };
    let stdout = String::from_utf8_lossy(&otool_out.stdout);

    // Each dependency line looks like:
    //   \t/abs/or/relative/path/libfoo.1.dylib (compatibility version 1.0.0, current version 1.0.0)
    // The first whitespace-indented token up to " (" is the recorded path.
    for line in stdout.lines() {
        if !line.starts_with(char::is_whitespace) {
            // The first line is the binary's own name (not indented); skip non-dependency lines.
            continue;
        }
        let trimmed = line.trim();
        // Strip the trailing " (compatibility ...)" annotation to isolate the path.
        let recorded = match trimmed.split(" (").next() {
            Some(p) if !p.is_empty() => p,
            _ => continue,
        };
        let recorded_leaf = Path::new(recorded)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("");

        // Match this recorded entry to one of our vendored dylibs by leaf filename. Accept an
        // exact match (libfoo.dylib) or a soname-versioned variant (libfoo.1.dylib) for the same
        // base stem (the part before the first '.').
        let matched_leaf = dylib_leaves.iter().find(|leaf| {
            recorded_leaf == leaf.as_str() || dylib_leaf_matches(recorded_leaf, leaf)
        });
        let leaf = match matched_leaf {
            Some(l) => l,
            None => continue,
        };

        let desired = format!("@rpath/{}", leaf);
        if recorded == desired {
            continue; // Already @rpath/<leaf> (e.g. a self-built dylib) — nothing to do.
        }

        let status = Command::new("install_name_tool")
            .args(["-change", recorded, &desired, &output_path.display().to_string()])
            .status();
        match status {
            Ok(s) if s.success() => {}
            _ => {
                eprintln!(
                    "Warning: macOS rpath fixup — `install_name_tool -change {} {}` failed; the \
                     vendored dylib may not be found via the @loader_path rpath at runtime",
                    recorded, desired
                );
            }
        }
    }
}

/// True if `recorded_leaf` is a soname-versioned variant of vendored dylib leaf `leaf` — i.e. they
/// share the same base stem (the substring before the first `.`) and both end in `.dylib`. This
/// matches e.g. recorded `libfoo.1.dylib` against vendored `libfoo.dylib`. The exact-equality case
/// is handled by the caller; this only covers the versioned variant.
fn dylib_leaf_matches(recorded_leaf: &str, leaf: &str) -> bool {
    if !recorded_leaf.ends_with(".dylib") || !leaf.ends_with(".dylib") {
        return false;
    }
    let base = |s: &str| s.split('.').next().unwrap_or(s).to_string();
    base(recorded_leaf) == base(leaf)
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
    // 0. Explicit override via LIN_RUNTIME_LIB. The integration-test harness sets this to a
    //    debug-stripped COPY of liblin_runtime.a: the suite links hundreds of tiny programs and
    //    each link is dominated by pulling the ~250MB DWARF-laden archive through the linker, so a
    //    stripped (~95MB) copy roughly halves total suite wall-clock. The canonical archive is left
    //    untouched, so local ASan/UAF-hunting (which relies on runtime symbolization) is unaffected.
    if let Ok(p) = std::env::var("LIN_RUNTIME_LIB") {
        let path = PathBuf::from(p);
        if path.exists() {
            return Some(path);
        }
    }

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

/// Find the runtime bitcode file produced by building lin-runtime with
/// `RUSTFLAGS="--emit=llvm-bc -C codegen-units=1"`. The `.bc` file lands in
/// `target/{profile}/deps/lin_runtime-<hash>.bc`. We glob for any file whose
/// name starts with `lin_runtime` and ends with `.bc` in the standard deps dirs.
fn find_runtime_bc() -> Option<PathBuf> {
    // Explicit override takes priority.
    if let Ok(p) = std::env::var("LIN_RUNTIME_BC") {
        let path = PathBuf::from(p);
        if path.exists() {
            return Some(path);
        }
    }

    let deps_dirs = [
        "target/debug/deps",
        "target/release/deps",
        "../target/debug/deps",
        "../target/release/deps",
    ];

    let search_dirs: Vec<PathBuf> = {
        let mut dirs: Vec<PathBuf> = deps_dirs.iter().map(PathBuf::from).collect();
        if let Ok(manifest) = std::env::var("CARGO_MANIFEST_DIR") {
            let base = Path::new(&manifest);
            for d in &deps_dirs {
                dirs.push(base.join(d));
                if let Some(parent) = base.parent() {
                    dirs.push(parent.join(d));
                }
            }
        }
        dirs
    };

    for dir in &search_dirs {
        if let Ok(entries) = std::fs::read_dir(dir) {
            let mut bc_file: Option<PathBuf> = None;
            let mut bc_mtime = std::time::SystemTime::UNIX_EPOCH;
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name_str = name.to_string_lossy();
                if name_str.starts_with("lin_runtime") && name_str.ends_with(".bc") {
                    // Pick the most-recently-modified .bc if multiple exist (stale CGU artefacts).
                    if let Ok(meta) = entry.metadata() {
                        if let Ok(mt) = meta.modified() {
                            if mt >= bc_mtime {
                                bc_mtime = mt;
                                bc_file = Some(entry.path());
                            }
                        }
                    }
                }
            }
            if let Some(p) = bc_file {
                return Some(p);
            }
        }
    }

    None
}

#[cfg(test)]
mod link_error_tests {
    use super::*;

    #[test]
    fn details_prefer_real_error_over_leading_warnings() {
        // macOS-shaped output: a run of benign warnings, then the real failure last.
        let raw = "\
ld: warning: object file (.../liblin_runtime.a[173](curve25519.o)) was built for newer 'macOS' version (15.5) than being linked (15.0)
ld: warning: object file (.../liblin_runtime.a[174](sha512.o)) was built for newer 'macOS' version (15.5) than being linked (15.0)
ld: warning: object file (.../liblin_runtime.a[175](aes.o)) was built for newer 'macOS' version (15.5) than being linked (15.0)
ld: warning: object file (.../liblin_runtime.a[176](poly1305.o)) was built for newer 'macOS' version (15.5) than being linked (15.0)
Undefined symbols for architecture arm64:
  \"_lin_process_spawn\", referenced from: ...
ld: symbol(s) not found for architecture arm64";

        let msg = classify_link_failure(raw);
        assert!(
            msg.contains("could not build your program: the final build step failed for an unrecognised reason"),
            "top line changed: {msg}"
        );
        // The real error must survive.
        assert!(msg.contains("Undefined symbols for architecture arm64"), "lost the real error: {msg}");
        assert!(msg.contains("symbol(s) not found"), "lost the trailing ld error: {msg}");
        // …AND the indented continuation that actually NAMES the symbol — the header alone is
        // useless for root-causing. This is the line the first cut of this matcher dropped.
        assert!(msg.contains("_lin_process_spawn"), "lost the symbol name continuation: {msg}");
        // The benign warnings must be deprioritised out of the details.
        assert!(!msg.contains("built for newer 'macOS' version"), "warnings leaked into details: {msg}");
    }

    #[test]
    fn details_keep_indented_symbol_names_under_undefined_header() {
        // The exact ld64 shape: a header, then several indented "_sym", referenced from: blocks,
        // each followed by a further-indented "_x in y.o" location line. All of it is the diagnostic.
        let raw = "\
Undefined symbols for architecture arm64:
  \"_lin_process_spawn\", referenced from:
      _main in process.test.bin.o
  \"_lin_process_wait\", referenced from:
      _main in process.test.bin.o
ld: symbol(s) not found for architecture arm64
clang: error: linker command failed with exit code 1 (use -v to see invocation)";
        let lines = select_link_detail_lines(raw);
        // Both missing symbol names must appear — they're what a bug report needs.
        assert!(lines.iter().any(|l| l.contains("_lin_process_spawn")), "missing first symbol: {lines:?}");
        assert!(lines.iter().any(|l| l.contains("_lin_process_wait")), "missing second symbol: {lines:?}");
        assert!(lines.iter().any(|l| l.contains("symbol(s) not found")), "missing trailing error: {lines:?}");
    }

    #[test]
    fn details_drop_indented_lines_not_under_an_error() {
        // An indented line that does NOT follow a kept error line is not pulled in as a continuation.
        let raw = "\
  some indented preamble line with no diagnostic keyword
clang: error: linker command failed with exit code 1";
        let lines = select_link_detail_lines(raw);
        assert!(lines.iter().any(|l| l.contains("error: linker command failed")));
        assert!(!lines.iter().any(|l| l.contains("indented preamble")), "stray indent leaked: {lines:?}");
    }

    #[test]
    fn details_fall_back_to_warnings_when_nothing_else() {
        let raw = "\
ld: warning: a
ld: warning: b";
        let msg = classify_link_failure(raw);
        // With nothing better, warnings are still surfaced rather than empty details.
        assert!(msg.contains("ld: warning: a"), "expected warning fallback: {msg}");
    }

    #[test]
    fn select_lines_prefers_errors() {
        let raw = "\
ld: warning: w1
ld: warning: w2
clang: error: linker command failed with exit code 1
some trailing note";
        let lines = select_link_detail_lines(raw);
        assert!(lines.iter().any(|l| l.contains("error: linker command failed")));
        assert!(!lines.iter().any(|l| l.contains("warning")));
    }
}
