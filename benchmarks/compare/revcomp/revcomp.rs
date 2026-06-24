// revcomp.rs — byte-buffer throughput (Computer Language Benchmarks Game
// "reverse-complement", checksum form). A deterministic Park-Miller MINSTD
// generator fills an N-base ACGT Vec<u8>; it is reverse-complemented (A<->T, C<->G,
// read back-to-front) into a second buffer; then a rolling checksum is folded over
// the result. Prints exactly one stdout line "RESULT=<int>".
//
// RESULT = fold h = (h*31 + code) mod 1000000007 over the reverse-complement.
// Parameters (identical across all languages): N=20000000.
const N: usize = 20000000;

fn main() {
    let codes = [65u8, 67, 71, 84];
    let mut comp = [0u8; 128];
    comp[65] = 84;
    comp[84] = 65;
    comp[67] = 71;
    comp[71] = 67;

    let mut state: i64 = 42;
    let mut seq = vec![0u8; N];
    for i in 0..N {
        state = (state * 16807) % 2147483647;
        seq[i] = codes[(state % 4) as usize];
    }

    let mut out = vec![0u8; N];
    for i in 0..N {
        out[i] = comp[seq[N - 1 - i] as usize];
    }

    let mut h: i64 = 0;
    for j in 0..N {
        h = (h * 31 + out[j] as i64) % 1000000007;
    }
    println!("RESULT={}", h);
}
