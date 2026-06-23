/// Report-only verifier for sealed-record → LinMap conversions (Stage 1).
///
/// When `LIN_VERIFY_REPR=1` is set, every sealed→map conversion site bumps an atomic counter.
/// At process exit the counts are printed to stderr so we can inventory the conversion load
/// before later stages eliminate the conversions structurally. Zero cost when unset (one
/// relaxed atomic load on the hot path).
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Once;

static REPR_VERIFY_ENABLED: AtomicBool = AtomicBool::new(false);
static REPR_VERIFY_CHECKED: AtomicBool = AtomicBool::new(false);

// Fixed site table — one counter per instrumented function.
// Order: array.rs × 3, map.rs × 2.
const SITES: &[&str] = &[
    "lin_sealed_ptr_array_to_tagged",
    "lin_sealed_array_to_tagged",
    "lin_sealed_any_to_tagged",
    "dynamic_to_map",
    "lin_union_force_to_map",
];

const N: usize = 5;
static COUNTERS: [AtomicU64; N] = [
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
];

fn site_index(site: &'static str) -> usize {
    for (i, s) in SITES.iter().enumerate() {
        if std::ptr::eq(*s, site) || *s == site {
            return i;
        }
    }
    // Unknown site — should never happen; fall through silently.
    usize::MAX
}

pub fn repr_verify_enabled() -> bool {
    if !REPR_VERIFY_CHECKED.load(Ordering::Relaxed) {
        ensure_init();
    }
    REPR_VERIFY_ENABLED.load(Ordering::Relaxed)
}

fn ensure_init() {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        REPR_VERIFY_CHECKED.store(true, Ordering::Relaxed);
        if std::env::var("LIN_VERIFY_REPR").as_deref() == Ok("1") {
            REPR_VERIFY_ENABLED.store(true, Ordering::Relaxed);
            unsafe { libc::atexit(repr_verify_atexit); }
        }
    });
}

extern "C" fn repr_verify_atexit() {
    repr_verify_dump();
}

/// Bump the counter for `site`. The `site` argument must be one of the compile-time constants
/// in SITES — pass the function name as a string literal so pointer equality fast-paths.
#[inline]
pub fn repr_note(site: &'static str) {
    if !REPR_VERIFY_CHECKED.load(Ordering::Relaxed) {
        ensure_init();
    }
    if !REPR_VERIFY_ENABLED.load(Ordering::Relaxed) {
        return;
    }
    let idx = site_index(site);
    if idx < N {
        COUNTERS[idx].fetch_add(1, Ordering::Relaxed);
    }
}

/// Print the conversion inventory to stderr. Only meaningful when `LIN_VERIFY_REPR=1`.
pub fn repr_verify_dump() {
    if !REPR_VERIFY_ENABLED.load(Ordering::Relaxed) {
        return;
    }
    // Collect and sort descending by count.
    let mut rows: Vec<(&'static str, u64)> = SITES
        .iter()
        .enumerate()
        .map(|(i, &s)| (s, COUNTERS[i].load(Ordering::Relaxed)))
        .collect();
    rows.sort_by(|a, b| b.1.cmp(&a.1));

    eprintln!("=== LIN_VERIFY_REPR: sealed-record → LinMap conversions ===");
    for (name, count) in &rows {
        eprintln!("  {:<50} {}", name, count);
    }
    let total: u64 = rows.iter().map(|(_, c)| c).sum();
    eprintln!("  {:<50} {}", "TOTAL", total);
}
