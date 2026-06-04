// records.rs — record-access-bound stateful simulation. A single value-semantics
// struct threaded through field-read + reconstruct cycles. Prints "RESULT=<int>".
//
// Parameters (identical across all languages): N=50000000, MOD=2147483647.
const N: i64 = 50_000_000;
const MOD: i64 = 2147483647;

#[derive(Clone, Copy)]
struct State {
    a: i64,
    b: i64,
    c: i64,
    d: i64,
    e: i64,
    f: i64,
}

fn step(s: State) -> State {
    // Each pre-mod product (e.g. a*1103515245 ~ 2.3e18) fits in i64; % brings it
    // back under 2^31 so the next iteration's multiply stays in range.
    let a = (s.a * 1103515245 + s.f + 12345) % MOD;
    let b = (s.b + s.a * 3) % MOD;
    let c = (s.c * 5 + s.b) % MOD;
    let d = (s.d + s.c * 7) % MOD;
    let e = (s.e * 9 + s.d) % MOD;
    let f = (s.f + s.e * 11) % MOD;
    State { a, b, c, d, e, f }
}

fn main() {
    let mut s = State { a: 1, b: 2, c: 3, d: 4, e: 5, f: 6 };
    for _ in 0..N {
        s = step(s);
    }
    let sum = (s.a + s.b + s.c + s.d + s.e + s.f) % MOD;
    println!("RESULT={}", sum);
}
