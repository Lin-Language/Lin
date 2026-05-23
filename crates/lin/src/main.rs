use std::env;
use std::fs;
use std::io::{self, Read};
use std::path::Path;
use std::process;

use lin_eval::Interpreter;

fn main() {
    let args: Vec<String> = env::args().collect();

    if args.len() < 2 {
        eprintln!("Usage: lin <file.lin>");
        eprintln!("       lin run <file.lin>");
        eprintln!("       lin check <file.lin>");
        eprintln!("       lin build <file.lin> [-o output]");
        eprintln!("       lin -          (read from stdin)");
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
        "run" => {
            if args.len() < 3 {
                eprintln!("Usage: lin run <file.lin>");
                process::exit(1);
            }
            run_interpret(&args[2]);
        }
        _ => {
            if args[1] == "-" {
                run_interpret_stdin();
            } else {
                run_interpret(&args[1]);
            }
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

    let mut checker = lin_check::Checker::new();
    match checker.check_module(&module) {
        Ok(_) => {
            eprintln!("Type check passed.");
        }
        Err(diagnostics) => {
            for diag in &diagnostics {
                let (line, col) = diag.span.line_col(&source);
                eprintln!(
                    "{}:{}:{}: {:?}: {}",
                    path, line, col, diag.severity, diag.message
                );
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
                let (line, col) = diag.span.line_col(&source);
                eprintln!(
                    "{}:{}:{}: {:?}: {}",
                    path, line, col, diag.severity, diag.message
                );
            }
            process::exit(1);
        }
        Err(e) => {
            eprintln!("Build failed: {}", e);
            process::exit(1);
        }
    }
}

fn run_interpret(path: &str) {
    let path = Path::new(path);
    let mut interpreter = Interpreter::new();
    let result = interpreter
        .run_file(path)
        .map_err(|e| format!("error[{}]: {}", path.display(), e));

    if let Err(e) = result {
        eprintln!("{}", e);
        process::exit(1);
    }
}

fn run_interpret_stdin() {
    let mut buf = String::new();
    io::stdin().read_to_string(&mut buf).unwrap_or_else(|e| {
        eprintln!("Error reading stdin: {}", e);
        process::exit(1);
    });

    let mut interpreter = Interpreter::new();
    let result = interpreter
        .run(&buf)
        .map_err(|e| format!("error[<stdin>]: {}", e));

    if let Err(e) = result {
        eprintln!("{}", e);
        process::exit(1);
    }
}
