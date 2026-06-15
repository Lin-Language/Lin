use std::path::PathBuf;

#[derive(clap::Args)]
pub struct RunArgs {
    /// Source file to compile and run
    pub file: PathBuf,
    /// Emit LLVM IR alongside the binary
    #[arg(long)]
    pub emit_ir: bool,
    /// Disable optimisation passes
    #[arg(long)]
    pub no_opt: bool,
    /// Emit DWARF debug info (implies -O0). Mostly useful with `lin build`; accepted here for parity.
    #[arg(long, short = 'g')]
    pub debug: bool,
    /// Arguments forwarded to the compiled binary
    #[arg(last = true)]
    pub program_args: Vec<String>,
}

pub fn run(args: &RunArgs) {
    use lin_compile::{compile, CompileOptions, CompileError};
    use std::fs;
    use std::process::{self, Command};

    // Place temp binary in .lin-cache/ next to the source to avoid /tmp noexec issues.
    let src_dir = args
        .file
        .parent()
        .unwrap_or(std::path::Path::new("."));
    let cache_dir = src_dir.join(".lin-cache");
    let _ = fs::create_dir_all(&cache_dir);
    let bin = cache_dir.join(format!("run-tmp-{}", process::id()));

    let opts = CompileOptions {
        source_path: args.file.clone(),
        output_path: bin.clone(),
        emit_ir: args.emit_ir,
        optimize: !(args.no_opt || args.debug || std::env::var("LIN_NO_OPT").is_ok()),
        coverage: false,
        debug: args.debug,
    };

    let path = args.file.display().to_string();
    match compile(&opts) {
        Ok(()) => {}
        Err(CompileError::TypeCheck(diagnostics)) => {
            let source = fs::read_to_string(&args.file).unwrap_or_default();
            for diag in &diagnostics {
                diag.render(&path, &source);
            }
            let _ = fs::remove_file(&bin);
            process::exit(1);
        }
        Err(CompileError::ModuleNotFound { import_path, tried, suggestion, std_like, span, importing_file }) => {
            use lin_common::Diagnostic;
            let source = fs::read_to_string(&importing_file).unwrap_or_default();
            let mut diag = Diagnostic::error(span, format!("module not found: \"{}\"", import_path));
            diag.notes.push((span, format!("tried to read: {}", tried.display())));
            let mut help_parts: Vec<String> = Vec::new();
            if std_like {
                help_parts.push(format!("\"{}\" is not a built-in stdlib module", import_path));
            }
            if let Some(s) = suggestion {
                help_parts.push(format!("did you mean \"{}\"?", s));
            }
            if !help_parts.is_empty() {
                diag.help = Some(help_parts.join("\n"));
            }
            diag.render(&importing_file, &source);
            let _ = fs::remove_file(&bin);
            process::exit(1);
        }
        Err(e) => {
            eprintln!("Build failed: {}", e);
            let _ = fs::remove_file(&bin);
            process::exit(1);
        }
    }

    let status = Command::new(&bin)
        .args(&args.program_args)
        .status()
        .unwrap_or_else(|e| {
            eprintln!("Failed to run {}: {}", bin.display(), e);
            let _ = fs::remove_file(&bin);
            process::exit(1);
        });

    let _ = fs::remove_file(&bin);
    process::exit(status.code().unwrap_or(1));
}
