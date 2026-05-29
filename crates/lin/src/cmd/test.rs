use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(clap::ValueEnum, Clone, Default, PartialEq)]
pub enum CoverageFormat {
    #[default]
    Console,
    LlvmCov,
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

    // Compile phase (sequential — cache writes are atomic but keep it simple).
    let compiled: Vec<Option<PathBuf>> = test_files
        .iter()
        .map(|src| compile_test(src, coverage))
        .collect();

    let stdout_lock = Arc::new(Mutex::new(()));

    // Run phase (parallel).
    let mut results: Vec<TestResult> = pool.install(|| {
        test_files
            .par_iter()
            .zip(compiled.par_iter())
            .map(|(src, bin_opt)| {
                let bin = match bin_opt {
                    Some(b) => b,
                    None => {
                        return TestResult {
                            path: src.clone(),
                            outcome: Outcome::CompileError,
                            elapsed: Duration::ZERO,
                            stdout: String::new(),
                            stderr: String::new(),
                        };
                    }
                };

                let profraw = src.with_extension("profraw");
                let t = Instant::now();
                let (outcome, stdout, stderr) =
                    run_binary(bin, if coverage { Some(&profraw) } else { None }, timeout);
                let elapsed = t.elapsed();

                // Keep the binary when collecting coverage — llvm-cov needs it to map the
                // .profraw counters back to source. run_coverage_report cleans it up after.
                if !coverage {
                    let _ = std::fs::remove_file(bin);
                }

                let result = TestResult { path: src.clone(), outcome, elapsed, stdout, stderr };
                print_result(&result, verbose, &stdout_lock);
                result
            })
            .collect()
    });

    results.sort_by(|a, b| a.path.cmp(&b.path));

    let passed = results.iter().filter(|r| matches!(r.outcome, Outcome::Pass)).count();
    let failed = results.len() - passed;

    eprintln!();
    if failed == 0 {
        eprintln!("{} test file(s) passed", passed);
    } else {
        eprintln!("{} passed, {} failed", passed, failed);
    }

    if coverage {
        run_coverage_report(&test_files, &compiled, args);
    }

    if failed > 0 {
        process::exit(1);
    }
}

fn compile_test(src: &PathBuf, coverage: bool) -> Option<PathBuf> {
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
    };

    match compile(&opts) {
        Ok(()) => Some(bin),
        Err(CompileError::TypeCheck(diagnostics)) => {
            eprintln!("FAIL (compile) {}", src.display());
            let source = fs::read_to_string(src).unwrap_or_default();
            let path = src.display().to_string();
            for diag in &diagnostics {
                diag.render(&path, &source);
            }
            None
        }
        Err(e) => {
            eprintln!("FAIL (compile) {}: {}", src.display(), e);
            None
        }
    }
}

fn run_binary(
    bin: &PathBuf,
    profraw: Option<&PathBuf>,
    timeout: Duration,
) -> (Outcome, String, String) {
    use std::process::{Command, Stdio};
    use std::sync::mpsc;
    use std::thread;

    let mut cmd = Command::new(bin);
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    if let Some(p) = profraw {
        cmd.env("LLVM_PROFILE_FILE", p);
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

fn run_coverage_report(
    test_files: &[PathBuf],
    compiled: &[Option<PathBuf>],
    args: &TestArgs,
) {
    use std::fs;
    use std::process::Command;

    // Collect the profraw files that actually exist.
    let pairs: Vec<(&PathBuf, &PathBuf)> = test_files
        .iter()
        .zip(compiled.iter())
        .filter_map(|(src, bin_opt)| bin_opt.as_ref().map(|b| (src, b)))
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
    let mut merge_cmd = Command::new("llvm-profdata-22");
    merge_cmd.arg("merge").arg("-sparse").arg("-o").arg(&profdata_path);
    for (src, _) in &pairs {
        merge_cmd.arg(src.with_extension("profraw"));
    }
    match merge_cmd.status() {
        Err(e) => { eprintln!("llvm-profdata-22 failed: {}", e); return; }
        Ok(s) if !s.success() => { eprintln!("llvm-profdata-22 exited non-zero"); return; }
        Ok(_) => {}
    }

    match args.format {
        CoverageFormat::Console => {
            // Print a text summary for each binary.
            for (_, bin) in &pairs {
                let out = Command::new("llvm-cov-22")
                    .arg("report")
                    .arg(bin)
                    .arg(format!("-instr-profile={}", profdata_path.display()))
                    .output();
                match out {
                    Ok(o) if o.status.success() => {
                        print!("{}", String::from_utf8_lossy(&o.stdout));
                    }
                    Ok(o) => eprintln!("{}", String::from_utf8_lossy(&o.stderr)),
                    Err(e) => eprintln!("llvm-cov-22 failed: {}", e),
                }
            }
        }
        CoverageFormat::LlvmCov => {
            let lcov_path = args.output.clone().unwrap_or_else(|| root.join("lcov.info"));
            let mut lcov_data = String::new();
            for (_, bin) in &pairs {
                let out = Command::new("llvm-cov-22")
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
                    Err(e) => eprintln!("llvm-cov-22 failed: {}", e),
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
