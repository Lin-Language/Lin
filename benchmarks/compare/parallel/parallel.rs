// parallel.rs — CPU-bound fan-out: 8 std::thread workers each run the same walk.
// Prints exactly one stdout line "RESULT=<int>".
use std::thread;

const START: i32 = 27;
const ITERS: i32 = 30_000_000;
const CHUNKS: usize = 8;

fn chunk() -> i64 {
    let mut start: i32 = START;
    let mut n: i32 = ITERS;
    let mut steps: i64 = 0;
    while n != 0 {
        let next = if start == 1 {
            27
        } else if start % 2 == 0 {
            start / 2
        } else {
            3 * start + 1
        };
        steps += start as i64;
        start = next;
        n -= 1;
    }
    steps
}

fn main() {
    let handles: Vec<_> = (0..CHUNKS).map(|_| thread::spawn(chunk)).collect();
    let mut sum: i64 = 0;
    for h in handles {
        sum += h.join().unwrap();
    }
    println!("RESULT={}", sum);
}
