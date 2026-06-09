//! Microbenchmark for Json object key lookup + object equality.
//!
//! Run with:  cargo test -p lin-runtime --release --test object_bench -- --nocapture --ignored
//!
//! The equality benches A/B THREE eq strategies on ONE binary (apples-to-apples, no rebuilds):
//!   * BASELINE-LINEAR  (`lin_object_eq_spike_linear`)  — pre-index pure O(n*m) scan.
//!   * SHIPPED-INDEX    (`lin_object_eq_spike_index`)    — current master: hash-index probe, no
//!                                                         positional walk.
//!   * PROTO-POSITIONAL (`lin_object_eq`)                — positional fast path + index fallback.
//!
//! Keys are SHARED pointers between the two objects (models same-record-type / interned-literal
//! keys: slot-for-slot pointer-identical), so the positional path's `key_a == key_b` shortcut fires.
//! All #[ignore] so they don't run in the normal suite.

use lin_runtime::object::{
    lin_object_alloc, lin_object_eq, lin_object_eq_spike_index, lin_object_eq_spike_linear,
    lin_object_get, lin_object_release, lin_object_set, LinObject,
};
use lin_runtime::string::{lin_string_from_bytes, lin_string_release, LinString};
use lin_runtime::tagged::{alloc_tagged, lin_tagged_release, TAG_INT32};
use std::time::Instant;

#[test]
#[ignore]
fn bench_insert_lookup_curve() {
    unsafe {
        let sizes = [1usize, 8, 16, 64, 1000, 16000];
        println!("\n  N        build(ms)   lookup(ms)   build/N(us)  lookup/N(us)");
        for &n in &sizes {
            let keys: Vec<*mut _> = (0..n)
                .map(|i| {
                    let s = format!("key{i:08}");
                    lin_string_from_bytes(s.as_ptr(), s.len() as u32)
                })
                .collect();

            let t0 = Instant::now();
            let obj = lin_object_alloc(4);
            for (i, &k) in keys.iter().enumerate() {
                let v = alloc_tagged(TAG_INT32, i as u64);
                lin_object_set(obj, k, v as *const _);
                lin_tagged_release(v);
            }
            let build = t0.elapsed();

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
                lin_string_release(k);
            }
        }
        println!();
    }
}

// ── object equality A/B harness ─────────────────────────────────────────────────────────────────

type EqFn = unsafe extern "C" fn(*const LinObject, *const LinObject) -> u8;

const VARIANTS: [(&str, EqFn); 3] = [
    ("BASELINE-LINEAR", lin_object_eq_spike_linear),
    ("SHIPPED-INDEX", lin_object_eq_spike_index),
    ("PROTO-POSITIONAL", lin_object_eq),
];

/// Allocate `n` distinct key strings. Caller owns them and must release.
unsafe fn make_keys(n: usize) -> Vec<*mut LinString> {
    (0..n)
        .map(|i| {
            let s = format!("key{i:08}");
            lin_string_from_bytes(s.as_ptr(), s.len() as u32)
        })
        .collect()
}

/// Build an object from SHARED keys. `reversed` inserts them in reverse slot order (so slot 0 keys
/// differ between a same-order and a reversed object → forces the order-independent fallback).
/// `diff_at` flips one value (by logical key index) to 0xDEAD.
unsafe fn build_from_keys(
    keys: &[*mut LinString],
    reversed: bool,
    diff_at: Option<usize>,
) -> *mut LinObject {
    let n = keys.len();
    let obj = lin_object_alloc(n as u32);
    let order: Vec<usize> = if reversed {
        (0..n).rev().collect()
    } else {
        (0..n).collect()
    };
    for &i in &order {
        let val = if Some(i) == diff_at { 0xDEAD_u64 } else { i as u64 };
        let v = alloc_tagged(TAG_INT32, val);
        lin_object_set(obj, keys[i], v as *const _); // shares the key ptr (inc_ref internally)
        lin_tagged_release(v);
    }
    obj
}

/// Median of repeated timing closures (ns/compare). Runs `runs` measured passes.
fn median_ns<F: FnMut() -> f64>(runs: usize, mut f: F) -> f64 {
    let mut v: Vec<f64> = (0..runs).map(|_| f()).collect();
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[v.len() / 2]
}

const RUNS: usize = 5;

#[test]
#[ignore]
fn bench_object_eq_matrix() {
    unsafe {
        let sizes = [8usize, 16, 24, 32, 64];

        // Scenario helper: loop with REUSED objects (steady state). Returns median ns/compare.
        macro_rules! loop_scenario {
            ($a:expr, $b:expr, $expect:expr, $eq:expr, $n:expr) => {{
                let a = $a;
                let b = $b;
                let iters = (4_000_000usize / ($n as usize).max(1)).max(1);
                assert_eq!($eq(a, b), $expect, "correctness mismatch in loop scenario");
                let ns = median_ns(RUNS, || {
                    let t = Instant::now();
                    let mut acc: u64 = 0;
                    for _ in 0..iters {
                        acc += $eq(a, b) as u64;
                    }
                    let el = t.elapsed();
                    assert_eq!(acc, if $expect == 1 { iters as u64 } else { 0 });
                    el.as_nanos() as f64 / iters as f64
                });
                lin_object_release(a);
                lin_object_release(b);
                ns
            }};
        }

        println!("\n  === SCENARIO 1: ONE-SHOT same-order (build two fresh, compare once, discard) ===");
        println!("  (the regression case: SHIPPED pays an index build that's never reused)");
        println!("  N      BASELINE-LINEAR   SHIPPED-INDEX   PROTO-POSITIONAL   (ns/compare incl 2x build)");
        for &n in &sizes {
            let keys = make_keys(n);
            let mut row = String::new();
            for (_name, eq) in VARIANTS {
                let iters = (400_000usize / n.max(1)).max(1);
                let ns = median_ns(RUNS, || {
                    let t = Instant::now();
                    let mut acc: u64 = 0;
                    for _ in 0..iters {
                        let a = build_from_keys(&keys, false, None);
                        let b = build_from_keys(&keys, false, None);
                        acc += eq(a, b) as u64;
                        lin_object_release(a);
                        lin_object_release(b);
                    }
                    let el = t.elapsed();
                    assert_eq!(acc, iters as u64);
                    el.as_nanos() as f64 / iters as f64
                });
                row.push_str(&format!("{ns:>14.2}  "));
            }
            println!("  {n:<5}  {row}");
            for k in keys {
                lin_string_release(k);
            }
        }

        println!("\n  === SCENARIO 1b: ONE-SHOT same-order, FRESH (non-shared) keys per object ===");
        println!("  (worst case for linear: byte-compares, not ptr-eq; reproduces the reported regression)");
        println!("  N      BASELINE-LINEAR   SHIPPED-INDEX   PROTO-POSITIONAL   (ns/compare incl 2x build)");
        for &n in &sizes {
            let mut row = String::new();
            for (_name, eq) in VARIANTS {
                let iters = (400_000usize / n.max(1)).max(1);
                let ns = median_ns(RUNS, || {
                    let t = Instant::now();
                    let mut acc: u64 = 0;
                    for _ in 0..iters {
                        // Fresh keys per object → slot keys are byte-equal but NOT pointer-equal,
                        // so lin_string_key_eq does a full memcmp (the linear-scan worst case) and
                        // the positional walk still aligns (same byte content, same order).
                        let ka = make_keys(n);
                        let kb = make_keys(n);
                        let a = build_from_keys(&ka, false, None);
                        let b = build_from_keys(&kb, false, None);
                        acc += eq(a, b) as u64;
                        lin_object_release(a);
                        lin_object_release(b);
                        for k in ka { lin_string_release(k); }
                        for k in kb { lin_string_release(k); }
                    }
                    let el = t.elapsed();
                    assert_eq!(acc, iters as u64);
                    el.as_nanos() as f64 / iters as f64
                });
                row.push_str(&format!("{ns:>14.2}  "));
            }
            println!("  {n:<5}  {row}");
        }

        println!("\n  === SCENARIO 2: EQUAL-LARGE loop, SAME order (reused objects) ===");
        println!("  N      BASELINE-LINEAR   SHIPPED-INDEX   PROTO-POSITIONAL   (ns/compare)");
        for &n in &sizes {
            let keys = make_keys(n);
            let mut row = String::new();
            for (_name, eq) in VARIANTS {
                let ns = loop_scenario!(
                    build_from_keys(&keys, false, None),
                    build_from_keys(&keys, false, None),
                    1,
                    eq,
                    n
                );
                row.push_str(&format!("{ns:>14.2}  "));
            }
            println!("  {n:<5}  {row}");
            for k in keys {
                lin_string_release(k);
            }
        }

        println!("\n  === SCENARIO 3: EQUAL-LARGE loop, REVERSED key order (forces fallback) ===");
        println!("  (proto's positional walk fails at slot 0 -> falls to index; must not regress vs SHIPPED)");
        println!("  N      BASELINE-LINEAR   SHIPPED-INDEX   PROTO-POSITIONAL   (ns/compare)");
        for &n in &sizes {
            let keys = make_keys(n);
            let mut row = String::new();
            for (_name, eq) in VARIANTS {
                let ns = loop_scenario!(
                    build_from_keys(&keys, false, None),
                    build_from_keys(&keys, true, None), // b reversed -> slot-0 key differs
                    1,
                    eq,
                    n
                );
                row.push_str(&format!("{ns:>14.2}  "));
            }
            println!("  {n:<5}  {row}");
            for k in keys {
                lin_string_release(k);
            }
        }

        println!("\n  === SCENARIO 4: FAST-REJECT loop, SAME order (one value differs) ===");
        println!("  N      BASELINE-LINEAR   SHIPPED-INDEX   PROTO-POSITIONAL   (ns/compare)");
        for &n in &sizes {
            let keys = make_keys(n);
            let mut row = String::new();
            for (_name, eq) in VARIANTS {
                let ns = loop_scenario!(
                    build_from_keys(&keys, false, None),
                    build_from_keys(&keys, false, Some(n / 2)), // mid value differs
                    0,
                    eq,
                    n
                );
                row.push_str(&format!("{ns:>14.2}  "));
            }
            println!("  {n:<5}  {row}");
            for k in keys {
                lin_string_release(k);
            }
        }

        println!();
    }
}
