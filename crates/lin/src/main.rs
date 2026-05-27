use std::env;
use std::fs;
use std::path::Path;
use std::process;

fn main() {
    let args: Vec<String> = env::args().collect();

    if args.len() < 2 {
        eprintln!("Usage: lin build <file.lin> [-o output]");
        eprintln!("       lin check <file.lin>");
        eprintln!("       lin test [<dir>]");
        process::exit(1);
    }

    match args[1].as_str() {
        "check" => {
            if args.len() < 3 {
                eprintln!("Usage: lin check <file.lin>");
                process::exit(1);
            }
            run_check(&args[2]);
        }
        "build" => {
            if args.len() < 3 {
                eprintln!("Usage: lin build <file.lin> [-o output]");
                process::exit(1);
            }
            let output = if args.len() >= 5 && args[3] == "-o" {
                args[4].clone()
            } else {
                Path::new(&args[2])
                    .file_stem()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string()
            };
            run_build(&args[2], &output);
        }
        "test" => {
            let dir = if args.len() >= 3 { &args[2] } else { "." };
            run_tests(dir);
        }
        _ => {
            eprintln!("Unknown subcommand: {}", args[1]);
            eprintln!("Usage: lin build <file.lin> [-o output]");
            eprintln!("       lin check <file.lin>");
            eprintln!("       lin test [<dir>]");
            process::exit(1);
        }
    }
}

fn run_check(path: &str) {
    let source = fs::read_to_string(path).unwrap_or_else(|e| {
        eprintln!("Error reading {}: {}", path, e);
        process::exit(1);
    });

    let mut lexer = lin_lex::Lexer::new(&source, 0);
    let tokens = lexer.tokenize();
    let mut parser = lin_parse::Parser::new(tokens);
    let module = parser.parse_module();

    if !parser.diagnostics.is_empty() {
        for diag in &parser.diagnostics {
            diag.render(path, &source);
        }
        process::exit(1);
    }

    let mut checker = lin_check::Checker::new();
    match checker.check_module(&module) {
        Ok(_) => {
            eprintln!("Type check passed.");
        }
        Err(diagnostics) => {
            for diag in &diagnostics {
                diag.render(path, &source);
            }
            process::exit(1);
        }
    }
}

fn run_build(path: &str, output: &str) {
    use lin_compile::{compile, CompileOptions, CompileError};
    use std::path::PathBuf;

    let opts = CompileOptions {
        source_path: PathBuf::from(path),
        output_path: PathBuf::from(output),
        emit_ir: std::env::var("LIN_EMIT_IR").is_ok(),
        optimize: !std::env::var("LIN_NO_OPT").is_ok(),
    };

    match compile(&opts) {
        Ok(()) => {
            eprintln!("Built: {}", output);
        }
        Err(CompileError::TypeCheck(diagnostics)) => {
            let source = fs::read_to_string(path).unwrap_or_default();
            for diag in &diagnostics {
                diag.render(path, &source);
            }
            process::exit(1);
        }
        Err(e) => {
            eprintln!("Build failed: {}", e);
            process::exit(1);
        }
    }
}

/// Find all *.test.lin files under `dir` (non-recursive for now).
fn find_test_files(dir: &str) -> Vec<std::path::PathBuf> {
    let mut files = Vec::new();
    let read = match fs::read_dir(dir) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Cannot read directory {}: {}", dir, e);
            return files;
        }
    };
    for entry in read.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("lin") {
            if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                if stem.ends_with(".test") {
                    files.push(path);
                }
            }
        }
    }
    files.sort();
    files
}

fn run_tests(dir: &str) {
    use lin_compile::{compile, CompileOptions, CompileError};
    use std::path::PathBuf;
    use std::process::Command;

    let test_files = find_test_files(dir);

    if test_files.is_empty() {
        eprintln!("No *.test.lin files found in {}", dir);
        process::exit(0);
    }

    let mut passed = 0usize;
    let mut failed = 0usize;

    for src_path in &test_files {
        let display = src_path.display().to_string();
        let bin_path = src_path.with_extension("test-bin");

        let opts = CompileOptions {
            source_path: src_path.clone(),
            output_path: bin_path.clone(),
            emit_ir: false,
            optimize: false,
        };

        let compile_result = compile(&opts);

        match compile_result {
            Err(CompileError::TypeCheck(diagnostics)) => {
                eprintln!("FAIL (compile) {}", display);
                let source = fs::read_to_string(src_path).unwrap_or_default();
                for diag in &diagnostics {
                    diag.render(&display, &source);
                }
                failed += 1;
                continue;
            }
            Err(e) => {
                eprintln!("FAIL (compile) {}: {}", display, e);
                failed += 1;
                continue;
            }
            Ok(()) => {}
        }

        let run_out = Command::new(&bin_path).output();
        let _ = fs::remove_file(&bin_path);

        match run_out {
            Err(e) => {
                eprintln!("FAIL (run) {}: {}", display, e);
                failed += 1;
            }
            Ok(out) if !out.status.success() => {
                eprint!("{}", String::from_utf8_lossy(&out.stderr));
                print!("{}", String::from_utf8_lossy(&out.stdout));
                failed += 1;
            }
            Ok(out) => {
                print!("{}", String::from_utf8_lossy(&out.stdout));
                passed += 1;
            }
        }
    }

    eprintln!("");
    if failed == 0 {
        eprintln!("{} test file(s) passed", passed);
    } else {
        eprintln!("{} passed, {} failed", passed, failed);
        process::exit(1);
    }
}
