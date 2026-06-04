//! Correctness proof for the lazy hash side-index (RAPTOR #4b).
//!
//! For object sizes spanning the index threshold (N = 0,1,15,16,17,64,1000), interleave
//! set / overwrite / merge / copy_except / release and assert that `get` / `has` / `keys`
//! agree with a LINEAR-SCAN ORACLE (a plain Rust HashMap mirroring insertion-order assoc-list
//! semantics) on EVERY key — including keys that are absent. A stale or wrong slot index would
//! surface here as a get/has disagreement; the test is the safety net the proposal calls for.
//!
//! Run in the normal suite (`cargo test -p lin-runtime`). Also exercised under ASan.

use lin_runtime::object::{
    lin_object_alloc, lin_object_copy_except, lin_object_get, lin_object_has, lin_object_keys,
    lin_object_merge, lin_object_release, LinObject,
};
use lin_runtime::string::{lin_string_from_bytes, lin_string_release, LinString};
use lin_runtime::tagged::{alloc_tagged, lin_tagged_release, TaggedVal, TAG_INT32, TAG_STR};
use std::collections::HashMap;

unsafe fn mk_string(s: &str) -> *mut LinString {
    lin_string_from_bytes(s.as_ptr(), s.len() as u32)
}

unsafe fn str_of(p: *const LinString) -> String {
    let len = (*p).len as usize;
    let data = (*p).data.as_ptr();
    let bytes = std::slice::from_raw_parts(data, len);
    String::from_utf8_lossy(bytes).into_owned()
}

/// A linear-scan oracle: insertion-ordered assoc list with last-wins overwrite (Json object
/// semantics). Mirrors what the runtime stores in `entries`.
#[derive(Default, Clone)]
struct Oracle {
    keys: Vec<String>,
    vals: HashMap<String, i32>,
}
impl Oracle {
    fn set(&mut self, k: &str, v: i32) {
        if !self.vals.contains_key(k) {
            self.keys.push(k.to_string());
        }
        self.vals.insert(k.to_string(), v);
    }
    fn merge(&mut self, other: &Oracle) {
        for k in &other.keys {
            self.set(k, other.vals[k]);
        }
    }
    fn copy_except(&mut self, src: &Oracle, excluded: &[String]) {
        for k in &src.keys {
            if !excluded.contains(k) {
                self.set(k, src.vals[k]);
            }
        }
    }
}

/// Build a runtime object from an oracle (in insertion order) via `lin_object_set`. Returns the
/// object; caller releases it. Uses `set` so the index maintenance path is exercised.
unsafe fn build_obj(or: &Oracle) -> *mut LinObject {
    let obj = lin_object_alloc(4);
    for k in &or.keys {
        let key = mk_string(k);
        let v = or.vals[k];
        let vb = alloc_tagged(TAG_INT32, v as u32 as u64);
        lin_runtime::object::lin_object_set(obj, key, vb as *const TaggedVal);
        lin_tagged_release(vb);
        lin_string_release(key); // object holds its own inc_ref'd copy
    }
    obj
}

/// Assert the runtime object agrees with the oracle on get/has/keys for a probe set that
/// includes every present key plus several guaranteed-absent keys.
unsafe fn assert_agrees(obj: *const LinObject, or: &Oracle, label: &str) {
    // get/has on every present key.
    for k in &or.keys {
        let key = mk_string(k);
        let tv = lin_object_get(obj, key);
        assert!(!tv.is_null(), "{label}: get({k}) returned null but oracle has it");
        let got = (*tv).payload as i32;
        let want = or.vals[k];
        assert_eq!(got, want, "{label}: get({k}) = {got}, oracle = {want}");
        assert_eq!(lin_object_has(obj, key), 1, "{label}: has({k}) = 0, oracle has it");
        lin_string_release(key);
    }
    // get/has on absent keys.
    for probe in &["__absent__", "zzz", "key99999999", "", "k", "MERGED_ONLY_X"] {
        if or.vals.contains_key(*probe) {
            continue;
        }
        let key = mk_string(probe);
        assert!(
            lin_object_get(obj, key).is_null(),
            "{label}: get({probe}) non-null but oracle absent"
        );
        assert_eq!(lin_object_has(obj, key), 0, "{label}: has({probe}) = 1 but absent");
        lin_string_release(key);
    }
    // keys() must match the oracle's key set (order-independent comparison).
    let karr = lin_object_keys(obj);
    let klen = (*karr).len as usize;
    assert_eq!(klen, or.keys.len(), "{label}: keys() len mismatch");
    let mut got_keys = Vec::new();
    for i in 0..klen {
        let slot = (*karr).data.add(i);
        assert_eq!((*slot).tag, TAG_STR);
        got_keys.push(str_of((*slot).payload as *const LinString));
    }
    let mut want_keys = or.keys.clone();
    got_keys.sort();
    want_keys.sort();
    assert_eq!(got_keys, want_keys, "{label}: keys() set mismatch");
    lin_runtime::array::lin_array_release(karr);
}

fn keyname(i: usize) -> String {
    format!("k{i:06}")
}

#[test]
fn fuzz_index_vs_linear_oracle() {
    unsafe {
        for &n in &[0usize, 1, 15, 16, 17, 64, 1000] {
            // 1. Build N distinct keys via set.
            let mut or = Oracle::default();
            for i in 0..n {
                or.set(&keyname(i), i as i32);
            }
            let obj = build_obj(&or);
            assert_agrees(obj, &or, &format!("N={n} fresh"));

            // 2. Overwrite a scattered subset (no index change; must still read new values).
            if n > 0 {
                let obj2 = build_obj(&or);
                let mut step = (n / 7).max(1);
                if step == 0 {
                    step = 1;
                }
                let mut i = 0;
                while i < n {
                    let k = keyname(i);
                    let nv = 1_000_000 + i as i32;
                    or.set(&k, nv);
                    let key = mk_string(&k);
                    let vb = alloc_tagged(TAG_INT32, nv as u32 as u64);
                    lin_runtime::object::lin_object_set(obj2, key, vb as *const TaggedVal);
                    lin_tagged_release(vb);
                    lin_string_release(key);
                    i += step;
                }
                assert_agrees(obj2, &or, &format!("N={n} overwrite"));
                lin_object_release(obj2);
            }

            // 3. Merge: spread a second object (some new keys, some overlapping) into a copy.
            {
                let mut merged_or = Oracle::default();
                for i in 0..n {
                    merged_or.set(&keyname(i), i as i32);
                }
                let dst = build_obj(&merged_or);
                // src has overlapping (even) + brand-new keys.
                let mut src_or = Oracle::default();
                for i in (0..n).step_by(2) {
                    src_or.set(&keyname(i), 5_000_000 + i as i32);
                }
                for j in 0..20 {
                    src_or.set(&format!("new{j:03}"), 9_000_000 + j as i32);
                }
                let src = build_obj(&src_or);
                lin_object_merge(dst, src);
                merged_or.merge(&src_or);
                assert_agrees(dst, &merged_or, &format!("N={n} merge"));
                lin_object_release(src);
                lin_object_release(dst);
            }

            // 4. copy_except: copy all but an excluded set into a fresh object.
            {
                let src = build_obj(&or);
                let excluded: Vec<String> =
                    (0..n).step_by(3).map(keyname).collect();
                let excl_strs: Vec<*mut LinString> =
                    excluded.iter().map(|k| mk_string(k)).collect();
                let excl_ptrs: Vec<*const LinString> =
                    excl_strs.iter().map(|p| *p as *const LinString).collect();
                let dst = lin_object_alloc(4);
                lin_object_copy_except(
                    dst,
                    src,
                    excl_ptrs.as_ptr(),
                    excl_ptrs.len() as u32,
                );
                let mut want = Oracle::default();
                want.copy_except(&or, &excluded);
                assert_agrees(dst, &want, &format!("N={n} copy_except"));
                for p in excl_strs {
                    lin_string_release(p);
                }
                lin_object_release(dst);
                lin_object_release(src);
            }

            // 5. release the original (UAF/double-free check under ASan).
            lin_object_release(obj);
        }
    }
}
