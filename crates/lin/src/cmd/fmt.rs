use std::path::PathBuf;
use std::process;

#[derive(clap::Args)]
pub struct FmtArgs {
    /// Source files or directories to format (defaults to **/*.lin in the current directory)
    pub files: Vec<PathBuf>,
    /// Check mode: exit 1 if any file would be reformatted, without writing
    #[arg(long)]
    pub check: bool,
    /// Glob patterns to exclude (comma-separated or repeated), e.g. `**/wip/**`.
    /// Matched against each candidate file path; excludes work-in-progress trees
    /// from CI's fmt gate without removing them from the repo.
    #[arg(long, value_delimiter = ',')]
    pub exclude: Vec<String>,
}

pub fn run(args: &FmtArgs) {
    // Collect all .lin files to process, dropping any that match an --exclude glob.
    let files = collect_files(&args.files, &args.exclude);

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
    // Delegates to the single canonical formatter (`lin_parse::format_source`),
    // mapping its `Vec<Diagnostic>` parse error to the CLI's existing "; "-joined
    // message string so output/tests are unchanged.
    lin_parse::format_source(source).map_err(|diags| {
        diags
            .iter()
            .map(|d| d.message.clone())
            .collect::<Vec<String>>()
            .join("; ")
    })
}

/// Collect .lin files from the given paths. If a path is a directory, glob
/// `**/*.lin` under it, skipping `.lin-cache/`. If no paths are given, glob
/// `**/*.lin` in the current directory (skipping `.lin-cache/`). Any file whose
/// path matches one of `exclude` (glob patterns) is dropped — this applies to
/// both globbed directory contents and explicitly-listed files.
fn collect_files(input: &[PathBuf], exclude: &[String]) -> Vec<PathBuf> {
    let patterns: Vec<glob::Pattern> = exclude
        .iter()
        .filter_map(|p| glob::Pattern::new(p).ok())
        .collect();
    let excluded = |path: &PathBuf| {
        let s = path.to_string_lossy();
        patterns.iter().any(|pat| pat.matches(&s))
    };

    let mut result = Vec::new();
    if input.is_empty() {
        result.extend(glob_lin_files(&PathBuf::from(".")));
    } else {
        for p in input {
            if p.is_dir() {
                result.extend(glob_lin_files(p));
            } else {
                result.push(p.clone());
            }
        }
    }
    result.retain(|p| !excluded(p));
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exclude_glob_drops_matching_files() {
        // Explicitly-listed files (non-existent paths are treated as files, not dirs)
        // are filtered by the exclude globs just like globbed directory contents.
        let inputs = vec![
            PathBuf::from("benchmarks/compare/raptor/lin-manually-typed/src/query/q.lin"),
            PathBuf::from("benchmarks/compare/raptor/lin/src/main.lin"),
            PathBuf::from("examples/calc/main.lin"),
        ];
        let kept = collect_files(&inputs, &["**/lin-manually-typed/**".to_string()]);
        assert_eq!(
            kept,
            vec![
                PathBuf::from("benchmarks/compare/raptor/lin/src/main.lin"),
                PathBuf::from("examples/calc/main.lin"),
            ],
            "the lin-manually-typed WIP tree must be excluded; everything else kept"
        );
    }

    #[test]
    fn no_exclude_keeps_everything() {
        let inputs = vec![PathBuf::from("examples/calc/main.lin")];
        let kept = collect_files(&inputs, &[]);
        assert_eq!(kept, inputs);
    }
}
