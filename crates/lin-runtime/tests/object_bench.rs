//! Microbenchmark for Json object key lookup: insert + lookup N distinct keys.
//!
//! Run with:  cargo test -p lin-runtime --release --test object_bench -- --nocapture --ignored
//!
//! Captures the build-a-map-of-N-distinct-keys curve. Before the hash side-index this is
//! O(n^2) (every `set` linear-scans for dup, every `get` linear-scans); after, it should be
//! ~O(n). The test is #[ignore] so it doesn't run in the normal suite.

use lin_runtime::object::{lin_object_alloc, lin_object_get, lin_object_set, lin_object_release};
use lin_runtime::string::lin_string_from_bytes;
use lin_runtime::tagged::{alloc_tagged, lin_tagged_release, TAG_INT32};
use std::time::Instant;

#[test]
#[ignore]
fn bench_insert_lookup_curve() {
    unsafe {
        let sizes = [1usize, 8, 16, 64, 1000, 16000];
        println!("\n  N        build(ms)   lookup(ms)   build/N(us)  lookup/N(us)");
        for &n in &sizes {
            // Pre-build the key strings so string allocation isn't timed.
            let keys: Vec<*mut _> = (0..n)
                .map(|i| {
                    let s = format!("key{i:08}");
                    lin_string_from_bytes(s.as_ptr(), s.len() as u32)
                })
                .collect();

            // BUILD: insert N distinct keys via repeated set (the O(n^2) hazard).
            let t0 = Instant::now();
            let obj = lin_object_alloc(4);
            for (i, &k) in keys.iter().enumerate() {
                let v = alloc_tagged(TAG_INT32, i as u64);
                lin_object_set(obj, k, v as *const _);
                lin_tagged_release(v);
            }
            let build = t0.elapsed();

            // LOOKUP: get every key once (each get is a scan/probe).
            let t1 = Instant::now();
            let mut sum: i64 = 0;
            for &k in &keys {
                let tv = lin_object_get(obj, k);
                assert!(!tv.is_null());
                sum += (*tv).payload as i64;
            }
            let lookup = t1.elapsed();
            assert_eq!(sum, (0..n as i64).sum::<i64>());

            let bms = build.as_secs_f64() * 1e3;
            let lms = lookup.as_secs_f64() * 1e3;
            println!(
                "  {n:<7}  {bms:>9.3}   {lms:>9.3}   {:>9.4}   {:>9.4}",
                bms * 1e3 / n as f64,
                lms * 1e3 / n as f64
            );

            lin_object_release(obj);
            for k in keys {
                lin_runtime::string::lin_string_release(k);
            }
        }
        println!();
    }
}
