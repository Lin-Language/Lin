// binarytrees.rs — allocation churn (Computer Language Benchmarks Game
// "binary-trees"). Bottom-up allocate many short-lived 2-pointer nodes (each child
// a heap Box), traverse each to a node count, and drop them. Prints exactly one
// stdout line "RESULT=<int>".
//
// RESULT = stretchCheck + (sum of all iteration checks) + longLivedCheck.
// Parameters (identical across all languages): MIN_DEPTH=4, MAX_DEPTH=16.
const MIN_DEPTH: i32 = 4;
const MAX_DEPTH: i32 = 16;

struct Tree {
    l: Option<Box<Tree>>,
    r: Option<Box<Tree>>,
}

fn make(d: i32) -> Tree {
    if d == 0 {
        Tree { l: None, r: None }
    } else {
        Tree {
            l: Some(Box::new(make(d - 1))),
            r: Some(Box::new(make(d - 1))),
        }
    }
}

fn check(t: &Tree) -> i64 {
    match &t.l {
        None => 1,
        Some(l) => 1 + check(l) + check(t.r.as_ref().unwrap()),
    }
}

fn main() {
    let max_depth = MAX_DEPTH.max(MIN_DEPTH + 2);
    let stretch_check = check(&make(max_depth + 1));
    let long_lived = make(max_depth);

    let mut total = stretch_check;
    let mut depth = MIN_DEPTH;
    while depth <= max_depth {
        let iterations: i64 = 1i64 << (max_depth - depth + MIN_DEPTH);
        let mut s: i64 = 0;
        for _ in 0..iterations {
            s += check(&make(depth));
        }
        total += s;
        depth += 2;
    }

    total += check(&long_lived);
    println!("RESULT={}", total);
}
