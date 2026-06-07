use notify::EventKind;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;

#[derive(clap::ValueEnum, Clone, Default)]
pub enum WatchCommand {
    #[default]
    Build,
    Test,
    Run,
}

#[derive(clap::Args)]
pub struct WatchArgs {
    /// Source file (entry point for build/run; directory for test)
    pub file: PathBuf,
    /// Glob patterns to include (comma-separated or repeated)
    #[arg(long, value_delimiter = ',')]
    pub include: Vec<String>,
    /// Glob patterns to exclude (comma-separated or repeated)
    #[arg(long, value_delimiter = ',')]
    pub exclude: Vec<String>,
    /// Command to re-run on change
    #[arg(long, default_value = "build")]
    pub command: WatchCommand,
}

pub fn run(args: &WatchArgs) {
    use notify::{RecursiveMode, Watcher};

    let watch_root = args
        .file
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));

    let (tx, rx) = mpsc::channel();
    let mut watcher = notify::recommended_watcher(move |res| {
        if let Ok(event) = res {
            let _ = tx.send(event);
        }
    })
    .expect("failed to create file watcher");

    watcher
        .watch(&watch_root, RecursiveMode::Recursive)
        .unwrap_or_else(|e| eprintln!("Watch failed: {}", e));

    eprintln!("[watching {}]", watch_root.display());
    execute_command(args);

    let debounce = Duration::from_millis(200);

    loop {
        // Block until the first event.
        let first = match rx.recv() {
            Ok(e) => e,
            Err(_) => break,
        };

        if !is_relevant(&first.kind, &first.paths, args) {
            continue;
        }

        // Drain additional events within the debounce window.
        loop {
            match rx.recv_timeout(debounce) {
                Ok(_) => {}
                Err(mpsc::RecvTimeoutError::Timeout) => break,
                Err(mpsc::RecvTimeoutError::Disconnected) => return,
            }
        }

        // Clear terminal and re-run.
        print!("\x1b[2J\x1b[H");
        execute_command(args);
        eprintln!("[watching {}]", watch_root.display());
    }
}

fn is_relevant(kind: &EventKind, paths: &[PathBuf], args: &WatchArgs) -> bool {
    use notify::EventKind::*;
    if !matches!(kind, Modify(_) | Create(_) | Remove(_)) {
        return false;
    }

    let include_patterns: Vec<glob::Pattern> = args
        .include
        .iter()
        .filter_map(|p| glob::Pattern::new(p).ok())
        .collect();

    let exclude_patterns: Vec<glob::Pattern> = args
        .exclude
        .iter()
        .filter_map(|p| glob::Pattern::new(p).ok())
        .collect();

    paths.iter().any(|path| {
        let path_str = path.to_string_lossy();

        // If include patterns are specified, at least one must match.
        if !include_patterns.is_empty()
            && !include_patterns
                .iter()
                .any(|pat| pat.matches(&path_str))
        {
            return false;
        }

        // Exclude patterns must not match.
        !exclude_patterns.iter().any(|pat| pat.matches(&path_str))
    })
}

fn execute_command(args: &WatchArgs) {
    match args.command {
        WatchCommand::Build => {
            super::build::run(&super::build::BuildArgs {
                file: args.file.clone(),
                output: None,
                emit_ir: false,
                no_opt: false,
                debug: false,
                verbose: false,
            });
        }
        WatchCommand::Run => {
            super::run::run(&super::run::RunArgs {
                file: args.file.clone(),
                emit_ir: false,
                no_opt: false,
                debug: false,
                program_args: vec![],
            });
        }
        WatchCommand::Test => {
            let path = args.file.display().to_string();
            super::test::run(&super::test::TestArgs {
                paths: vec![path],
                filter: None,
                parallel: None,
                timeout: 30,
                verbose: false,
                coverage: false,
                format: super::test::CoverageFormat::Console,
                output: None,
                reporter: super::test::Reporter::Human,
                filter_test: Vec::new(),
            });
        }
    }
}
