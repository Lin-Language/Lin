use std::env;
use std::fs;
use std::io::{self, Read};
use std::process;

use lin_eval::Interpreter;

fn main() {
    let args: Vec<String> = env::args().collect();

    if args.len() < 2 {
        eprintln!("Usage: lin <file.lin>");
        eprintln!("       lin -          (read from stdin)");
        process::exit(1);
    }

    let (filename, source) = if args[1] == "-" {
        let mut buf = String::new();
        io::stdin().read_to_string(&mut buf).unwrap_or_else(|e| {
            eprintln!("Error reading stdin: {}", e);
            process::exit(1);
        });
        ("<stdin>".to_string(), buf)
    } else {
        let name = args[1].clone();
        let src = fs::read_to_string(&name).unwrap_or_else(|e| {
            eprintln!("Error reading {}: {}", name, e);
            process::exit(1);
        });
        (name, src)
    };

    let mut interpreter = Interpreter::new();
    match interpreter.run(&source) {
        Ok(_) => {}
        Err(e) => {
            eprintln!("error[{}]: {}", filename, e);
            process::exit(1);
        }
    }
}
