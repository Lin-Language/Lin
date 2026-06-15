use std::path::PathBuf;

#[derive(clap::Args)]
pub struct CheckArgs {
    /// Source file to type-check
    pub file: PathBuf,
}

pub fn run(args: &CheckArgs) {
    use lin_compile::{check, CheckOptions, CompileError};
    use std::fs;
    use std::process;

    // Use the SAME import-resolving front end as `lin build`, stopping before lowering/codegen,
    // so `check` and `build` agree on what they accept (it previously checked the bare parsed
    // module and silently passed any error that depended on an imported symbol's real type).
    let opts = CheckOptions {
        source_path: args.file.clone(),
    };

    match check(&opts) {
        Ok(warnings) => {
            // Render any warnings (e.g. exhaustiveness, did-you-mean) against the source, then
            // report success — mirroring how `build` surfaces warnings.
            if !warnings.is_empty() {
                let source = fs::read_to_string(&args.file).unwrap_or_default();
                let path = args.file.display().to_string();
                for diag in &warnings {
                    diag.render(&path, &source);
                }
                let n = warnings.len();
                eprintln!(
                    "Type check passed ({} warning{}).",
                    n,
                    if n == 1 { "" } else { "s" }
                );
            } else {
                eprintln!("Type check passed.");
            }
        }
        Err(CompileError::TypeCheck(diagnostics)) => {
            let source = fs::read_to_string(&args.file).unwrap_or_default();
            let path = args.file.display().to_string();
            for diag in &diagnostics {
                diag.render(&path, &source);
            }
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
            process::exit(1);
        }
        Err(e) => {
            eprintln!("Check failed: {}", e);
            process::exit(1);
        }
    }
}
