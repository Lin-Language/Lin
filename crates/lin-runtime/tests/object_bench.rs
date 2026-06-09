//! Microbenchmark for Json object key lookup: insert + lookup N distinct keys.
//!
//! Run with:  cargo test -p lin-runtime --release --test object_bench -- --nocapture --ignored
//!
//! Captures the build-a-map-of-N-distinct-keys curve. Before the hash side-index this is
//! O(n^2) (every `set` linear-scans for dup, every `get` linear-scans); after, it should be
//! ~O(n). The test is #[ignore] so it doesn't run in the normal suite.

use lin_runtime::object::{
    lin_object_alloc, lin_object_eq, lin_object_get, lin_object_release, lin_object_set,
};
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

// ── Object equality (`lin_object_eq`) microbenchmark ───────────────────────────────────────────
//
// Run with:  cargo test -p lin-runtime --release --test object_bench -- --nocapture --ignored
//
// Measures the four eq surfaces that matter for "free win, no regression":
//   * EQUAL-LARGE in a loop      — two equal N-key objects compared repeatedly (the headline win).
//   * FAST-REJECT-LARGE in loop  — same but a differs in one value (early-out on the first miss).
//   * ONE-SHOT large             — build two fresh N-key objects and compare ONCE (the regression
//                                  risk: you pay an index build that is never reused vs one scan).
//   * SMALL in a loop            — sub-threshold objects (must stay flat; indexed path is skipped).
//
// The indexed path lives behind HASH_INDEX_THRESHOLD (=16); we sweep 8/16/24/32/64 to locate the
// crossover and confirm small stays flat. Each cell prints ns/compare so A/B (linear vs indexed
// runtime builds) is directly comparable.

unsafe fn build_obj(n: usize, diff_at: Option<usize>) -> *mut lin_runtime::object::LinObject {
    let obj = lin_object_alloc(n as u32);
    for i in 0..n {
        let s = format!("key{i:08}");
        let k = lin_string_from_bytes(s.as_ptr(), s.len() as u32);
        let val = if Some(i) == diff_at { 0xDEAD_u64 } else { i as u64 };
        let v = alloc_tagged(TAG_INT32, val);
        lin_object_set(obj, k, v as *const _);
        lin_tagged_release(v);
        lin_runtime::string::lin_string_release(k);
    }
    obj
}

#[test]
#[ignore]
fn bench_object_eq() {
    unsafe {
        let sizes = [8usize, 16, 24, 32, 64];

        println!("\n  == EQUAL-LARGE (loop, reused objects) ==");
        println!("  N      iters     total(ms)   ns/compare");
        for &n in &sizes {
            let a = build_obj(n, None);
            let b = build_obj(n, None);
            let iters = 2_000_000usize / n.max(1);
            // warm: build any index once so the loop measures steady-state probe, not the build.
            assert_eq!(lin_object_eq(a, b), 1);
            let t = Instant::now();
            let mut acc: u64 = 0;
            for _ in 0..iters {
                acc += lin_object_eq(a, b) as u64;
            }
            let el = t.elapsed();
            assert_eq!(acc, iters as u64);
            let ms = el.as_secs_f64() * 1e3;
            println!("  {n:<5}  {iters:<8}  {ms:>9.3}   {:>9.2}", el.as_nanos() as f64 / iters as f64);
            lin_object_release(a);
            lin_object_release(b);
        }

        println!("\n  == FAST-REJECT-LARGE (loop, one value differs) ==");
        println!("  N      iters     total(ms)   ns/compare");
        for &n in &sizes {
            let a = build_obj(n, None);
            let b = build_obj(n, Some(n / 2)); // mid-key value differs
            let iters = 2_000_000usize / n.max(1);
            assert_eq!(lin_object_eq(a, b), 0);
            let t = Instant::now();
            let mut acc: u64 = 0;
            for _ in 0..iters {
                acc += lin_object_eq(a, b) as u64;
            }
            let el = t.elapsed();
            assert_eq!(acc, 0);
            let ms = el.as_secs_f64() * 1e3;
            println!("  {n:<5}  {iters:<8}  {ms:>9.3}   {:>9.2}", el.as_nanos() as f64 / iters as f64);
            lin_object_release(a);
            lin_object_release(b);
        }

        println!("\n  == ONE-SHOT large (build two fresh, compare once) — index built but never reused ==");
        println!("  N      iters     total(ms)   ns/compare(incl build of both)");
        for &n in &sizes {
            let iters = 200_000usize / n.max(1);
            let t = Instant::now();
            let mut acc: u64 = 0;
            for _ in 0..iters {
                let a = build_obj(n, None);
                let b = build_obj(n, None);
                acc += lin_object_eq(a, b) as u64;
                lin_object_release(a);
                lin_object_release(b);
            }
            let el = t.elapsed();
            assert_eq!(acc, iters as u64);
            let ms = el.as_secs_f64() * 1e3;
            println!("  {n:<5}  {iters:<8}  {ms:>9.3}   {:>9.2}", el.as_nanos() as f64 / iters as f64);
        }
        println!();
    }
}
