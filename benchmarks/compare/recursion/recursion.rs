// recursion.rs — naive recursive fib + iterative sumTo. Prints "RESULT=<int>".
const FIB_N: i32 = 42;
const SUM_N: i64 = 50_000_000;

fn fib(n: i32) -> i64 {
    if n < 2 {
        n as i64
    } else {
        fib(n - 1) + fib(n - 2)
    }
}

fn sum_to(n: i64) -> i64 {
    let mut acc: i64 = 0;
    let mut i: i64 = 1;
    while i <= n {
        acc += i;
        i += 1;
    }
    acc
}

fn main() {
    let f = fib(FIB_N);
    let s = sum_to(SUM_N);
    let result = f * 1_000_000_007 + s;
    println!("RESULT={}", result);
}
