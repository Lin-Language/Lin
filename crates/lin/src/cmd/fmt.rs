use std::path::PathBuf;
use std::process;

#[derive(clap::Args)]
pub struct FmtArgs {
    /// Source files or directories to format (defaults to **/*.lin in the current directory)
    pub files: Vec<PathBuf>,
    /// Check mode: exit 1 if any file would be reformatted, without writing
    #[arg(long)]
    pub check: bool,
}

pub fn run(args: &FmtArgs) {
    // Collect all .lin files to process.
    let files = collect_files(&args.files);

    if files.is_empty() {
        eprintln!("lin fmt: no .lin files found");
        return;
    }

    let mut would_reformat: Vec<PathBuf> = Vec::new();
    let mut had_errors = false;

    for path in &files {
        match process_file(path, args.check) {
            Ok(changed) => {
                if changed {
                    if args.check {
                        eprintln!("Would reformat: {}", path.display());
                        would_reformat.push(path.clone());
                    } else {
                        eprintln!("Formatted: {}", path.display());
                    }
                }
            }
            Err(e) => {
                eprintln!("Error processing {}: {}", path.display(), e);
                had_errors = true;
            }
        }
    }

    if had_errors {
        process::exit(1);
    }

    if args.check && !would_reformat.is_empty() {
        process::exit(1);
    }
}

/// Process a single file. Returns `Ok(true)` if the file was (or would be) reformatted,
/// `Ok(false)` if it was already canonical.
fn process_file(path: &PathBuf, check: bool) -> Result<bool, String> {
    let source =
        std::fs::read_to_string(path).map_err(|e| format!("read error: {}", e))?;

    let formatted = format_source(&source).map_err(|e| format!("parse error: {}", e))?;

    if formatted == source {
        return Ok(false);
    }

    if !check {
        std::fs::write(path, &formatted).map_err(|e| format!("write error: {}", e))?;
    }

    Ok(true)
}

/// Lex, parse, and format a Lin source string.
/// Returns an error string if there are parse errors.
pub fn format_source(source: &str) -> Result<String, String> {
    let tokens = lin_lex::Lexer::new(source, 0).tokenize();
    let mut parser = lin_parse::Parser::new(tokens);
    let module = parser.parse_module();

    if !parser.diagnostics.is_empty() {
        let msgs: Vec<String> = parser
            .diagnostics
            .iter()
            .map(|d| d.message.clone())
            .collect();
        return Err(msgs.join("; "));
    }

    Ok(lin_parse::Formatter::new().format_module(&module))
}

/// Collect .lin files from the given paths. If a path is a directory, glob
/// `**/*.lin` under it, skipping `.lin-cache/`. If no paths are given, glob
/// `**/*.lin` in the current directory (skipping `.lin-cache/`).
fn collect_files(input: &[PathBuf]) -> Vec<PathBuf> {
    if input.is_empty() {
        return glob_lin_files(&PathBuf::from("."));
    }

    let mut result = Vec::new();
    for p in input {
        if p.is_dir() {
            result.extend(glob_lin_files(p));
        } else {
            result.push(p.clone());
        }
    }
    result
}

fn glob_lin_files(base: &PathBuf) -> Vec<PathBuf> {
    let pattern = format!("{}/**/*.lin", base.display());
    let mut files = Vec::new();
    if let Ok(paths) = glob::glob(&pattern) {
        for entry in paths.flatten() {
            // Skip .lin-cache directories.
            let skip = entry.components().any(|c| {
                c.as_os_str() == ".lin-cache"
            });
            if !skip {
                files.push(entry);
            }
        }
    }
    files.sort();
    files
}
