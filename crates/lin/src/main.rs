use std::env;
use std::io::{self, Read};
use std::path::Path;
use std::process;

use lin_eval::Interpreter;

fn main() {
    let args: Vec<String> = env::args().collect();

    if args.len() < 2 {
        eprintln!("Usage: lin <file.lin>");
        eprintln!("       lin -          (read from stdin)");
        process::exit(1);
    }

    let mut interpreter = Interpreter::new();

    let result = if args[1] == "-" {
        let mut buf = String::new();
        io::stdin().read_to_string(&mut buf).unwrap_or_else(|e| {
            eprintln!("Error reading stdin: {}", e);
            process::exit(1);
        });
        interpreter.run(&buf).map_err(|e| format!("error[<stdin>]: {}", e))
    } else {
        let path = Path::new(&args[1]);
        interpreter.run_file(path)
            .map_err(|e| format!("error[{}]: {}", args[1], e))
    };

    if let Err(e) = result {
        eprintln!("{}", e);
        process::exit(1);
    }
}
