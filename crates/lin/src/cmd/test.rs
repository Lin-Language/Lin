use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;

#[derive(clap::ValueEnum, Clone, Default, PartialEq)]
pub enum CoverageFormat {
    #[default]
    Console,
    LlvmCov,
}

/// Output reporter for `lin test`. `human` is the default eprintln summary; `json` emits
/// machine-readable NDJSON on stdout (one record per test + one per file), consumed by the
/// VSCode Test Explorer integration. Kept separate from the coverage `--format` flag (which is
/// gated on `--coverage`) to avoid a name clash.
#[derive(clap::ValueEnum, Clone, Default, PartialEq)]
pub enum Reporter {
    #[default]
    Human,
    Json,
}

/// The `##LINTEST## ` prefix the test runner (std/test) prepends to each NDJSON record it
/// prints. It lets the CLI separate runner records from arbitrary `print` output the user's own
/// code may emit on the same stdout stream.
const MARKER: &str = "##LINTEST## ";

/// A canonical CLI-emitted NDJSON record. `file` records describe a whole test file (including
/// ones that never produced per-test output, e.g. compile errors / timeouts); `test` records
/// carry one per-test result with its originating file attached. Re-serialized via serde so the
/// output is always valid/canonical JSON regardless of what the binary printed.
/// The NDJSON schema version emitted as the first `meta` record. Bump when the record shapes
/// change incompatibly so consumers (the VSCode extension) can detect a mismatch. The extension
/// defines its own `SUPPORTED_SCHEMA` and warns when it sees a newer one.
const NDJSON_SCHEMA: u32 = 2;

#[derive(Serialize)]
#[serde(tag = "event")]
enum JsonRecord {
    #[serde(rename = "meta")]
    Meta { schema: u32 },
    #[serde(rename = "test")]
    Test {
        file: String,
        name: String,
        status: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        message: Option<String>,
        // Structured expected/actual for equality-style failures (passed through untouched as
        // arbitrary JSON). Absent for passes, matchers without a meaningful pair, and tests with
        // multiple failing assertions.
        #[serde(skip_serializing_if = "Option::is_none")]
        expected: Option<serde_json::Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        actual: Option<serde_json::Value>,
        #[serde(rename = "durationMs", skip_serializing_if = "Option::is_none")]
        duration_ms: Option<u64>,
    },
    #[serde(rename = "file")]
    File {
        file: String,
        status: String,
        #[serde(rename = "durationMs")]
        duration_ms: u64,
        #[serde(skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },
    // Free-form stdout the test binary produced that ISN'T a `##LINTEST##` runner record —
    // i.e. the user's own `print(...)` output. Accumulated per file into one blob (newline-joined)
    // so the VSCode extension can surface it in the Test Results output tab. Schema v2.
    #[serde(rename = "output")]
    Output { file: String, text: String },
}

fn emit_record(rec: &JsonRecord) {
    // NDJSON goes to STDOUT so the extension can capture it; human mode uses stderr.
    if let Ok(line) = serde_json::to_string(rec) {
        println!("{}", line);
    }
}

#[derive(clap::Args)]
pub struct TestArgs {
    /// Files, directories, or glob patterns. Defaults to "."
    pub paths: Vec<String>,
    /// Only run tests whose path contains this substring
    #[arg(long)]
    pub filter: Option<String>,
    /// Number of parallel test runners (default: number of CPUs)
    #[arg(long)]
    pub parallel: Option<usize>,
    /// Kill test binary after this many seconds
    #[arg(long, default_value_t = 30)]
    pub timeout: u64,
    /// Show stdout/stderr from passing tests
    #[arg(short, long)]
    pub verbose: bool,
    /// Enable source coverage instrumentation
    #[arg(long)]
    pub coverage: bool,
    /// Coverage output format
    #[arg(long, default_value = "console", requires = "coverage")]
    pub format: CoverageFormat,
    /// Output file for coverage data (llvm-cov format only)
    #[arg(long, requires = "coverage")]
    pub output: Option<PathBuf>,
    /// Output reporter: human (default) or json (NDJSON on stdout)
    #[arg(long, value_enum, default_value = "human")]
    pub reporter: Reporter,
    /// Run only the test(s) with this exact name. Repeatable. When set, every other test in
    /// the matched files is skipped (its body is not evaluated and it emits no record).
    #[arg(long)]
    pub filter_test: Vec<String>,
}

struct TestResult {
    path: PathBuf,
    outcome: Outcome,
    elapsed: Duration,
    stdout: String,
    stderr: String,
}

enum Outcome {
    Pass,
    Fail,
    Timeout,
    CompileError,
}

pub fn run(args: &TestArgs) {
    use std::process;
    use rayon::prelude::*;

    let test_files = collect_test_files(&args.paths, args.filter.as_deref());
    if test_files.is_empty() {
        eprintln!("No *.test.lin files found.");
        process::exit(0);
    }

    // Configure rayon thread pool.
    let parallelism = args.parallel.unwrap_or_else(|| {
        std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1)
    });
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(parallelism)
        .build()
        .expect("failed to build thread pool");

    let timeout = Duration::from_secs(args.timeout);
    let verbose = args.verbose;
    let coverage = args.coverage;
    let json = args.reporter == Reporter::Json;
    let filter_test = args.filter_test.clone();

    // Emit the schema-version record as the very FIRST NDJSON line, so a consumer can verify
    // it understands the stream before parsing any per-test/per-file records.
    if json {
        emit_record(&JsonRecord::Meta { schema: NDJSON_SCHEMA });
    }

    // Compile phase (parallel). Each file compiles in its own LLVM context (`Context::create()`
    // per `compile()` call) and links its own binary, so the compiles are independent. The one
    // shared mutable resource is the `.lin-cache/` written for shared imports (e.g. std/string):
    // its writes are write-to-temp-then-rename with a per-WRITER unique temp name (lin-compile's
    // `unique_tmp_path`), so concurrent writers don't clobber each other and the final file is
    // always a complete image. This is the bulk of `lin test`'s wall time — each compile is a full
    // LLVM build + `cc` link — so parallelising it is the main win. Rendering of human-mode
    // compile diagnostics still happens inside `compile_test` (to stderr); interleaving across
    // threads is acceptable (each diagnostic block is emitted by a single `eprintln!` sequence and
    // failures are also surfaced per-file in the run phase below).
    let compiled: Vec<Result<PathBuf, String>> = pool.install(|| {
        test_files
            .par_iter()
            .map(|src| compile_test(src, coverage, json))
            .collect()
    });

    let stdout_lock = Arc::new(Mutex::new(()));

    // Run phase (parallel).
    let mut results: Vec<TestResult> = pool.install(|| {
        test_files
            .par_iter()
            .zip(compiled.par_iter())
            .map(|(src, bin_res)| {
                let bin = match bin_res {
                    Ok(b) => b,
                    Err(msg) => {
                        let result = TestResult {
                            path: src.clone(),
                            outcome: Outcome::CompileError,
                            elapsed: Duration::ZERO,
                            stdout: String::new(),
                            stderr: msg.clone(),
                        };
                        if json {
                            emit_json_for_result(&result, &stdout_lock);
                        }
                        return result;
                    }
                };

                let profraw = src.with_extension("profraw");
                let t = Instant::now();
                let (outcome, stdout, stderr) = run_binary(
                    bin,
                    if coverage { Some(&profraw) } else { None },
                    timeout,
                    json,
                    &filter_test,
                );
                let elapsed = t.elapsed();

                // Keep the binary when collecting coverage — llvm-cov needs it to map the
                // .profraw counters back to source. run_coverage_report cleans it up after.
                if !coverage {
                    let _ = std::fs::remove_file(bin);
                }

                let result = TestResult { path: src.clone(), outcome, elapsed, stdout, stderr };
                if json {
                    emit_json_for_result(&result, &stdout_lock);
                } else {
                    print_result(&result, verbose, &stdout_lock);
                }
                result
            })
            .collect()
    });

    results.sort_by(|a, b| a.path.cmp(&b.path));

    let passed = results.iter().filter(|r| matches!(r.outcome, Outcome::Pass)).count();
    let failed = results.len() - passed;

    // Human mode prints a trailing summary to stderr; json mode keeps stdout to pure NDJSON.
    if !json {
        eprintln!();
        if failed == 0 {
            eprintln!("{} test file(s) passed", passed);
        } else {
            eprintln!("{} passed, {} failed", passed, failed);
        }
    }

    if coverage {
        run_coverage_report(&test_files, &compiled, args);
    }

    if failed > 0 {
        process::exit(1);
    }
}

/// Compile a single test file. On failure returns a human-readable error message. In human
/// mode the rich Ariadne diagnostics are still rendered to stderr here (preserving the previous
/// behaviour); in json mode rendering is suppressed and the message is instead folded into a
/// `compile_error` NDJSON file record by the caller.
fn compile_test(src: &PathBuf, coverage: bool, json: bool) -> Result<PathBuf, String> {
    use lin_compile::{compile, CompileOptions, CompileError};
    use std::fs;

    // Place binaries in .lin-cache/test-bins/ to avoid collisions.
    let cache_dir = src
        .parent()
        .unwrap_or(std::path::Path::new("."))
        .join(".lin-cache")
        .join("test-bins");
    let _ = fs::create_dir_all(&cache_dir);

    let stem = src.file_stem().unwrap_or_default().to_string_lossy();
    let bin = cache_dir.join(format!("{}.bin", stem));

    let opts = CompileOptions {
        source_path: src.clone(),
        output_path: bin.clone(),
        emit_ir: false,
        optimize: false,
        coverage,
        debug: false,
        pgo: lin_compile::PgoMode::None,
    };

    match compile(&opts) {
        Ok(()) => Ok(bin),
        Err(CompileError::TypeCheck(diagnostics)) => {
            let entry_path = src.display().to_string();
            let entry_source = fs::read_to_string(src).unwrap_or_default();
            if !json {
                eprintln!("FAIL (compile) {}", src.display());
                for diag in &diagnostics {
                    let (path, source) = match &diag.file {
                        Some(f) => (f.as_str(), fs::read_to_string(f).unwrap_or_default()),
                        None => (entry_path.as_str(), entry_source.clone()),
                    };
                    diag.render(path, &source);
                }
            }
            let mut msg = format!("type check failed in {}", entry_path);
            for diag in &diagnostics {
                msg.push('\n');
                msg.push_str(&diag.message);
            }
            Err(msg)
        }
        Err(e) => {
            if !json {
                eprintln!("FAIL (compile) {}: {}", src.display(), e);
            }
            Err(format!("{}", e))
        }
    }
}

fn run_binary(
    bin: &PathBuf,
    profraw: Option<&PathBuf>,
    timeout: Duration,
    json: bool,
    filter_test: &[String],
) -> (Outcome, String, String) {
    use std::process::{Command, Stdio};
    use std::sync::mpsc;
    use std::thread;

    let mut cmd = Command::new(bin);
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    if let Some(p) = profraw {
        cmd.env("LLVM_PROFILE_FILE", p);
    }
    // Gate std/test's NDJSON emission on this env var (see stdlib/test.lin `report`).
    if json {
        cmd.env("LIN_TEST_JSON", "1");
    }
    // When the user asked for specific tests, pass the names (newline-separated) so std/test's
    // `test` skips every non-selected body. Test names are single-line string literals, so a
    // newline separator can never collide with a name.
    if !filter_test.is_empty() {
        cmd.env("LIN_TEST_ONLY", filter_test.join("\n"));
    }

    let child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            return (
                Outcome::Fail,
                String::new(),
                format!("failed to spawn: {}", e),
            );
        }
    };

    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let out = child.wait_with_output();
        let _ = tx.send(out);
    });

    match rx.recv_timeout(timeout) {
        Ok(Ok(out)) => {
            let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
            let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
            if out.status.success() {
                (Outcome::Pass, stdout, stderr)
            } else {
                (Outcome::Fail, stdout, stderr)
            }
        }
        Ok(Err(e)) => (Outcome::Fail, String::new(), format!("IO error: {}", e)),
        Err(_) => (Outcome::Timeout, String::new(), String::new()),
    }
}

/// Re-emit a finished test file's results as canonical NDJSON on stdout (json reporter mode).
/// For each `##LINTEST## `-prefixed line the binary printed, the trailing JSON is parsed, the
/// originating file path is attached, and the record is re-serialized (so output is always
/// valid). A single `file` record always follows, carrying the overall file status + (for
/// compile errors / timeouts) the diagnostic message — this is how the consumer learns about
/// files that never produced per-test records. The shared lock serializes whole-file output so
/// records from parallel runners don't interleave mid-line.
fn emit_json_for_result(result: &TestResult, lock: &Arc<Mutex<()>>) {
    let _guard = lock.lock().unwrap();
    let file = result.path.display().to_string();

    // Collect the user's own stdout (every non-marker line) so it can be forwarded as an
    // `output` record. Runner records (`##LINTEST## ...`) are handled separately below.
    let mut user_output: Vec<&str> = Vec::new();

    for line in result.stdout.lines() {
        let Some(rest) = line.strip_prefix(MARKER) else {
            user_output.push(line);
            continue;
        };
        // Parse the runner's record, then rebuild a canonical one with the file attached.
        let Ok(val) = serde_json::from_str::<serde_json::Value>(rest) else { continue };
        let name = val.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let status = val.get("status").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let message = val.get("message").and_then(|v| v.as_str()).map(|s| s.to_string());
        // Per-test timing comes from std/test's `ms` field (monotonic elapsed millis around the
        // test body). Clamp negatives to 0; absent/non-numeric → no durationMs.
        let duration_ms = val.get("ms").and_then(|v| v.as_i64()).map(|m| m.max(0) as u64);
        // Structured expected/actual (present only for equality-style single-assertion failures)
        // flow through untouched as arbitrary JSON.
        let expected = val.get("expected").cloned();
        let actual = val.get("actual").cloned();
        emit_record(&JsonRecord::Test {
            file: file.clone(),
            name,
            status,
            message,
            expected,
            actual,
            duration_ms,
        });
    }

    // Forward the user's `print(...)` output (if any) as one `output` record, emitted before the
    // file-summary record. Skip when empty so consumers don't get noise.
    if !user_output.is_empty() {
        emit_record(&JsonRecord::Output {
            file: file.clone(),
            text: user_output.join("\n"),
        });
    }

    let (status, message) = match result.outcome {
        Outcome::Pass => ("pass", None),
        Outcome::Fail => ("fail", non_empty(&result.stderr)),
        Outcome::Timeout => ("timeout", Some("test binary exceeded the timeout".to_string())),
        Outcome::CompileError => ("compile_error", non_empty(&result.stderr)),
    };
    emit_record(&JsonRecord::File {
        file,
        status: status.to_string(),
        duration_ms: result.elapsed.as_millis() as u64,
        message,
    });
}

fn non_empty(s: &str) -> Option<String> {
    if s.trim().is_empty() {
        None
    } else {
        Some(s.to_string())
    }
}

fn print_result(result: &TestResult, verbose: bool, lock: &Arc<Mutex<()>>) {
    let _guard = lock.lock().unwrap();
    let label = match result.outcome {
        Outcome::Pass => "PASS",
        Outcome::Fail => "FAIL",
        Outcome::Timeout => "TIMEOUT",
        Outcome::CompileError => "FAIL",
    };
    eprintln!(
        "{}  {}  ({:.2}s)",
        label,
        result.path.display(),
        result.elapsed.as_secs_f64()
    );
    let show_output =
        verbose || !matches!(result.outcome, Outcome::Pass | Outcome::CompileError);
    if show_output {
        if !result.stdout.is_empty() {
            eprintln!("  --- stdout ---");
            for line in result.stdout.lines() {
                eprintln!("  {}", line);
            }
        }
        if !result.stderr.is_empty() {
            eprintln!("  --- stderr ---");
            for line in result.stderr.lines() {
                eprintln!("  {}", line);
            }
        }
    }
}

/// Resolve an LLVM tool name, preferring the version-matched binary (`<tool>-22`) but falling
/// back to the unversioned `<tool>` when the versioned one isn't on PATH. This keeps coverage
/// working on hosts whose LLVM 22 tools aren't suffixed (e.g. a `clang`-only install).
fn llvm_tool(tool: &str) -> String {
    use std::process::Command;
    let versioned = format!("{}-22", tool);
    let found = Command::new(&versioned)
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if found { versioned } else { tool.to_string() }
}

fn run_coverage_report(
    test_files: &[PathBuf],
    compiled: &[Result<PathBuf, String>],
    args: &TestArgs,
) {
    use std::fs;
    use std::process::Command;

    let profdata_tool = llvm_tool("llvm-profdata");
    let cov_tool = llvm_tool("llvm-cov");

    // Collect the profraw files that actually exist.
    let pairs: Vec<(&PathBuf, &PathBuf)> = test_files
        .iter()
        .zip(compiled.iter())
        .filter_map(|(src, bin_res)| bin_res.as_ref().ok().map(|b| (src, b)))
        .filter(|(src, _)| src.with_extension("profraw").exists())
        .collect();

    if pairs.is_empty() {
        eprintln!("No coverage data collected.");
        return;
    }

    // Determine output root.
    let root = test_files
        .first()
        .and_then(|p| p.parent())
        .unwrap_or(std::path::Path::new("."));

    let profdata_path = root.join("coverage.profdata");

    // Merge .profraw → .profdata.
    let mut merge_cmd = Command::new(&profdata_tool);
    merge_cmd.arg("merge").arg("-sparse").arg("-o").arg(&profdata_path);
    for (src, _) in &pairs {
        merge_cmd.arg(src.with_extension("profraw"));
    }
    match merge_cmd.status() {
        Err(e) => { eprintln!("{} failed: {}", profdata_tool, e); return; }
        Ok(s) if !s.success() => { eprintln!("{} exited non-zero", profdata_tool); return; }
        Ok(_) => {}
    }

    match args.format {
        CoverageFormat::Console => {
            // Print a text summary for each binary.
            for (_, bin) in &pairs {
                let out = Command::new(&cov_tool)
                    .arg("report")
                    .arg(bin)
                    .arg(format!("-instr-profile={}", profdata_path.display()))
                    .output();
                match out {
                    Ok(o) if o.status.success() => {
                        print!("{}", String::from_utf8_lossy(&o.stdout));
                    }
                    Ok(o) => eprintln!("{}", String::from_utf8_lossy(&o.stderr)),
                    Err(e) => eprintln!("{} failed: {}", cov_tool, e),
                }
            }
        }
        CoverageFormat::LlvmCov => {
            let lcov_path = args.output.clone().unwrap_or_else(|| root.join("lcov.info"));
            let mut lcov_data = String::new();
            for (_, bin) in &pairs {
                let out = Command::new(&cov_tool)
                    .arg("export")
                    .arg(bin)
                    .arg(format!("-instr-profile={}", profdata_path.display()))
                    .arg("--format=lcov")
                    .output();
                match out {
                    Ok(o) if o.status.success() => {
                        lcov_data.push_str(&String::from_utf8_lossy(&o.stdout));
                    }
                    Ok(o) => eprintln!("{}", String::from_utf8_lossy(&o.stderr)),
                    Err(e) => eprintln!("{} failed: {}", cov_tool, e),
                }
            }
            fs::write(&lcov_path, &lcov_data).unwrap_or_else(|e| {
                eprintln!("Failed to write {}: {}", lcov_path.display(), e);
            });
            eprintln!("Coverage report: {}", lcov_path.display());
        }
    }

    // Cleanup profraw files, profdata, and the instrumented test binaries (the run phase
    // leaves them in place under coverage so llvm-cov can read them here).
    for (src, bin) in &pairs {
        let _ = fs::remove_file(src.with_extension("profraw"));
        let _ = fs::remove_file(bin);
    }
    let _ = fs::remove_file(&profdata_path);
}

/// Collect *.test.lin files from paths (dirs, files, or globs).
pub fn collect_test_files(paths: &[String], filter: Option<&str>) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = Vec::new();

    let inputs: Vec<String> = if paths.is_empty() {
        vec![".".to_string()]
    } else {
        paths.to_vec()
    };

    for input in &inputs {
        let has_glob = input.contains('*') || input.contains('?') || input.contains('[');
        if has_glob {
            match glob::glob(input) {
                Ok(entries) => {
                    for entry in entries.flatten() {
                        if is_test_lin(&entry) {
                            files.push(entry);
                        }
                    }
                }
                Err(e) => eprintln!("Invalid glob pattern '{}': {}", input, e),
            }
        } else {
            let path = PathBuf::from(input);
            if path.is_dir() {
                collect_from_dir(&path, &mut files);
            } else if is_test_lin(&path) {
                files.push(path);
            }
        }
    }

    files.sort();
    files.dedup();

    if let Some(f) = filter {
        files.retain(|p| p.display().to_string().contains(f));
    }

    files
}

fn collect_from_dir(dir: &std::path::Path, out: &mut Vec<PathBuf>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("Cannot read {}: {}", dir.display(), e);
            return;
        }
    };
    for entry in entries.flatten() {
        let p = entry.path();
        if p.is_dir() {
            collect_from_dir(&p, out);
        } else if is_test_lin(&p) {
            out.push(p);
        }
    }
}

fn is_test_lin(p: &std::path::Path) -> bool {
    p.extension().and_then(|e| e.to_str()) == Some("lin")
        && p.file_stem()
            .and_then(|s| s.to_str())
            .map(|s| s.ends_with(".test"))
            .unwrap_or(false)
}
