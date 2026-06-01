// interp.rs — arithmetic expression interpreter (tokenize -> recursive-descent
// parse -> tree-walking eval) over 8 fixed expressions, REPS times. Faithful port
// of the Lin/calc algorithm: same grammar, same AST shape, i64 truncating
// division (Rust `/` truncates toward zero). Prints exactly one stdout line
// "RESULT=<int>".
//
// Parameters (identical across all languages): REPS=10000 over 8 fixed exprs.

const REPS: i64 = 10000;

const EXPRS: [&str; 8] = [
    "2 + 3 * 4",
    "(2 + 3) * 4",
    "100 / 5 / 2",
    "2 * (3 + (4 - 1)) * 2",
    "1 + 2 + 3 + 4 + 5 + 6",
    "((8 - 2) * (4 + 1)) / 3",
    "9 * 9 - 8 * 7 + 6",
    "1000 - 7 * (11 + 13) / 2",
];

#[derive(Clone)]
enum Tok {
    Num(i64),
    Op(u8),
    LParen,
    RParen,
}

enum Ast {
    Num(i64),
    BinOp(u8, Box<Ast>, Box<Ast>),
}

fn tokenize(src: &str) -> Vec<Tok> {
    let b = src.as_bytes();
    let n = b.len();
    let mut toks = Vec::new();
    let mut i = 0;
    while i < n {
        let c = b[i];
        if c == b' ' || c == b'\t' || c == b'\n' || c == b'\r' {
            i += 1;
        } else if c.is_ascii_digit() {
            let start = i;
            while i < n && b[i].is_ascii_digit() {
                i += 1;
            }
            let v: i64 = src[start..i].parse().unwrap();
            toks.push(Tok::Num(v));
        } else if c == b'(' {
            toks.push(Tok::LParen);
            i += 1;
        } else if c == b')' {
            toks.push(Tok::RParen);
            i += 1;
        } else {
            toks.push(Tok::Op(c));
            i += 1;
        }
    }
    toks
}

// Parser: each fn returns (node, pos).
fn parse_factor(toks: &[Tok], pos: usize) -> (Ast, usize) {
    match toks.get(pos) {
        Some(Tok::Num(v)) => (Ast::Num(*v), pos + 1),
        _ => {
            // assume '(' expr ')'
            let (inner, p) = parse_expr(toks, pos + 1);
            (inner, p + 1)
        }
    }
}

fn parse_term(toks: &[Tok], pos: usize) -> (Ast, usize) {
    let (mut left, mut pos) = parse_factor(toks, pos);
    loop {
        match toks.get(pos) {
            Some(Tok::Op(op)) if *op == b'*' || *op == b'/' => {
                let (right, p) = parse_factor(toks, pos + 1);
                left = Ast::BinOp(*op, Box::new(left), Box::new(right));
                pos = p;
            }
            _ => break,
        }
    }
    (left, pos)
}

fn parse_expr(toks: &[Tok], pos: usize) -> (Ast, usize) {
    let (mut left, mut pos) = parse_term(toks, pos);
    loop {
        match toks.get(pos) {
            Some(Tok::Op(op)) if *op == b'+' || *op == b'-' => {
                let (right, p) = parse_term(toks, pos + 1);
                left = Ast::BinOp(*op, Box::new(left), Box::new(right));
                pos = p;
            }
            _ => break,
        }
    }
    (left, pos)
}

fn eval_node(node: &Ast) -> i64 {
    match node {
        Ast::Num(v) => *v,
        Ast::BinOp(op, l, r) => {
            let a = eval_node(l);
            let b = eval_node(r);
            match op {
                b'+' => a + b,
                b'-' => a - b,
                b'*' => a * b,
                _ => a / b,
            }
        }
    }
}

fn eval1(src: &str) -> i64 {
    let (node, _) = parse_expr(&tokenize(src), 0);
    eval_node(&node)
}

fn main() {
    let mut total: i64 = 0;
    for _ in 0..REPS {
        for e in EXPRS.iter() {
            total += eval1(e);
        }
    }
    println!("RESULT={}", total);
}
