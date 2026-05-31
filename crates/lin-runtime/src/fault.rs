//! Fault isolation at the async thread boundary (spec §24.2.2).
//!
//! Every runtime fault (array OOB, division by zero, non-exhaustive match, explicit
//! `lin_panic`, null-spread) historically called `std::process::exit(1)` — uncatchable,
//! which is correct for the top-level program (spec §20.1) but wrong inside an `async`
//! thunk, where the fault must be *caught at the thread boundary* and surfaced as an
//! `Error` value at `await`.
//!
//! Mechanism: a thread-local "async boundary depth". A spawned thunk runs inside
//! `with_async_boundary`, which bumps the depth and wraps the call in
//! `std::panic::catch_unwind`. While depth > 0, a fault `panic!`s (unwinds to the
//! boundary, where it becomes an `Error`); while depth == 0 (the main thread, outside
//! any async), a fault keeps the old `process::exit(1)` behaviour.
//!
//! Unwinding crosses the LLVM/Rust boundary: a fault panics in a Rust `lin_*` frame and
//! unwinds back through the compiled Lin frames to the `catch_unwind` on the worker
//! thread. For that to be sound the compiled frames must NOT be marked `nounwind` when
//! the program uses async — codegen drops the attribute in that case (see
//! `Codegen::module_uses_async`).

use std::cell::Cell;

thread_local! {
    /// How many async boundaries are active on this thread. Faults unwind (instead of
    /// `process::exit`) iff this is > 0.
    static ASYNC_DEPTH: Cell<u32> = const { Cell::new(0) };
}

/// True if the current thread is executing inside an async boundary.
#[inline]
pub fn in_async_boundary() -> bool {
    ASYNC_DEPTH.with(|d| d.get() > 0)
}

/// The payload carried by a fault panic so the boundary can rebuild an `Error` value.
pub struct FaultPanic {
    pub message: String,
}

/// Raise a runtime fault. Inside an async boundary this `panic!`s (caught at the boundary
/// and turned into an `Error`); otherwise it prints to stderr and exits the process — the
/// historical, uncatchable top-level behaviour (spec §20.1).
///
/// `-> !` is a lie inside a boundary (we unwind rather than diverge to exit), but every
/// caller treats it as diverging, which a panic also does, so it is sound.
#[cold]
#[inline(never)]
pub fn runtime_fault(message: &str) -> ! {
    if in_async_boundary() {
        // Unwind to the boundary's catch_unwind. The payload lets the boundary rebuild a
        // structured Error; the string is also what `panic!`'s default hook would show.
        std::panic::panic_any(FaultPanic { message: message.to_string() });
    } else {
        eprintln!("{}", message);
        std::process::exit(1);
    }
}

/// Run `f` inside an async boundary, catching any runtime fault it raises.
///
/// Returns `Ok(value)` on normal completion or `Err(message)` if a fault unwound to here.
/// The depth is restored even if `f` panics (the guard's Drop runs during unwind).
pub fn with_async_boundary<F, R>(f: F) -> Result<R, String>
where
    F: FnOnce() -> R,
{
    struct DepthGuard;
    impl DepthGuard {
        fn enter() -> Self {
            ASYNC_DEPTH.with(|d| d.set(d.get() + 1));
            DepthGuard
        }
    }
    impl Drop for DepthGuard {
        fn drop(&mut self) {
            ASYNC_DEPTH.with(|d| d.set(d.get().saturating_sub(1)));
        }
    }

    // Suppress the default panic hook's backtrace spam for *fault* panics while still
    // letting genuine bugs print. We only quiet our own FaultPanic payload.
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _guard = DepthGuard::enter();
        f()
    }));

    match result {
        Ok(v) => Ok(v),
        Err(payload) => {
            let msg = if let Some(fp) = payload.downcast_ref::<FaultPanic>() {
                fp.message.clone()
            } else if let Some(s) = payload.downcast_ref::<&str>() {
                (*s).to_string()
            } else if let Some(s) = payload.downcast_ref::<String>() {
                s.clone()
            } else {
                "runtime error".to_string()
            };
            Err(msg)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fault_outside_boundary_flag_is_false() {
        assert!(!in_async_boundary());
    }

    #[test]
    fn boundary_catches_fault_as_error() {
        install_quiet_fault_hook();
        let r: Result<i32, String> = with_async_boundary(|| {
            assert!(in_async_boundary());
            runtime_fault("boom");
        });
        assert_eq!(r, Err("boom".to_string()));
        // Depth restored after the boundary.
        assert!(!in_async_boundary());
    }

    #[test]
    fn boundary_returns_value_on_success() {
        let r = with_async_boundary(|| 7);
        assert_eq!(r, Ok(7));
        assert!(!in_async_boundary());
    }

    #[test]
    fn nested_boundaries_restore_depth() {
        install_quiet_fault_hook();
        let outer: Result<Result<i32, String>, String> = with_async_boundary(|| {
            assert!(in_async_boundary());
            let inner = with_async_boundary(|| -> i32 { runtime_fault("inner") });
            // Still inside the outer boundary after the inner one unwound+caught.
            assert!(in_async_boundary());
            inner
        });
        assert_eq!(outer, Ok(Err("inner".to_string())));
        assert!(!in_async_boundary());
    }
}

/// Install a panic hook that stays silent for `FaultPanic` (which is caught and turned into
/// an `Error`) but defers to the default hook for any other panic (genuine runtime bug).
/// Idempotent; called lazily the first time a boundary is set up.
pub fn install_quiet_fault_hook() {
    use std::sync::Once;
    static HOOK: Once = Once::new();
    HOOK.call_once(|| {
        let default = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            if info.payload().is::<FaultPanic>() {
                // Caught at the boundary and surfaced as an Error — no stderr noise.
                return;
            }
            default(info);
        }));
    });
}
