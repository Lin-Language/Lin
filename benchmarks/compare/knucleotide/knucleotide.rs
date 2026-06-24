// knucleotide.rs — k-mer frequency count: HashMap<String,i64> throughput + per-window
// substring. A deterministic Park-Miller MINSTD generator builds an N-base ACGT
// sequence; a sliding K-wide window is counted into a string-keyed map. Each window
// allocates an owned String key (matching the other languages' per-window substring).
// Prints exactly one stdout line "RESULT=<int>".
//
// RESULT = (sum over keys of count^2) + (number of distinct keys) — order-independent.
// Parameters (identical across all languages): N=4000000, K=8.
use std::collections::HashMap;

const N: usize = 4000000;
const K: usize = 8;
const CODES: &[u8; 4] = b"ACGT";

fn main() {
    let mut state: i64 = 42;
    let mut buf = vec![0u8; N];
    for i in 0..N {
        state = (state * 16807) % 2147483647;
        buf[i] = CODES[(state % 4) as usize];
    }
    let seq = String::from_utf8(buf).unwrap();

    let mut counts: HashMap<String, i64> = HashMap::new();
    let end = N - K + 1;
    for i in 0..end {
        *counts.entry(seq[i..i + K].to_string()).or_insert(0) += 1;
    }

    let mut sumsq: i64 = 0;
    for v in counts.values() {
        sumsq += v * v;
    }
    let result = sumsq + counts.len() as i64;
    println!("RESULT={}", result);
}
