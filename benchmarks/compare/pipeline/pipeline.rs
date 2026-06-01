// pipeline.rs — range -> map -> filter -> reduce, materializing each stage into
// an explicit Vec (no lazy iterator fusion). Prints "RESULT=<int>".
const N: i64 = 20_000_000;

fn main() {
    let a: Vec<i64> = (0..N).collect();
    let b: Vec<i64> = a.iter().map(|x| x * 2).collect();
    let c: Vec<i64> = b.iter().cloned().filter(|x| x % 3 == 0).collect();
    let mut total: i64 = 0;
    for x in &c {
        total += x;
    }
    println!("RESULT={}", total);
}
