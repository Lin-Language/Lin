//! `std/signal` runtime intrinsics — minimal, race-free signal waiting.
//!
//! Per spec §27.8:
//! ```txt
//! waitSignal: (sig: Int32) => Int32   // block until the signal is delivered
//! ```
//!
//! ## Approach: block-then-sigwait (no handler)
//!
//! `lin_signal_wait` builds a `sigset_t` containing just the requested signal,
//! blocks that signal in the calling thread's mask via `pthread_sigmask(SIG_BLOCK)`,
//! then calls `sigwait` on the set. `sigwait` atomically suspends the thread until a
//! signal in the set is pending, consumes it, and writes the caught signal number out.
//!
//! This is the clean, race-free idiom: because the signal is *blocked* before we wait,
//! a signal that arrives between the mask change and `sigwait` stays pending and is
//! still consumed by `sigwait` (no lost wakeup). It also avoids installing a handler
//! and the `pause()`/handler race.
//!
//! ## Limitations
//!
//! - The signal mask is **per-thread** (`pthread_sigmask`). `waitSignal` blocks and waits
//!   on the thread that calls it. Lin worker threads each have their own mask.
//! - Only a **single** signal is waited on per call (the set contains exactly `sig`).
//! - On any libc setup failure (`sigemptyset`/`sigaddset`/`pthread_sigmask`/`sigwait`),
//!   it returns -1. On success it returns the delivered signal number (== `sig`).

use std::mem::MaybeUninit;

/// Block until signal `sig` is delivered to the calling thread; return the caught signal.
///
/// Returns the signal number on success, or -1 if any libc call fails.
#[no_mangle]
pub extern "C" fn lin_signal_wait(sig: i32) -> i32 {
    unsafe {
        let mut set = MaybeUninit::<libc::sigset_t>::uninit();
        if libc::sigemptyset(set.as_mut_ptr()) != 0 {
            return -1;
        }
        if libc::sigaddset(set.as_mut_ptr(), sig) != 0 {
            return -1;
        }
        let set = set.assume_init();

        // Block the signal in this thread's mask so it stays pending until sigwait
        // consumes it (race-free vs. a handler + pause).
        if libc::pthread_sigmask(libc::SIG_BLOCK, &set, std::ptr::null_mut()) != 0 {
            return -1;
        }

        let mut caught: libc::c_int = 0;
        if libc::sigwait(&set, &mut caught) != 0 {
            return -1;
        }
        caught as i32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic, self-contained sigwait round-trip.
    ///
    /// We block SIGUSR1 in this thread, then `raise()` it to ourselves. Because the
    /// signal is blocked it becomes *pending* rather than being delivered, so it
    /// cannot kill the test and is guaranteed to still be pending when `sigwait`
    /// (inside `lin_signal_wait`) consumes it. No real asynchronous signal source is
    /// needed, so the test never blocks indefinitely.
    #[test]
    fn sigwait_roundtrip() {
        unsafe {
            // Block SIGUSR1 first so the raised signal stays pending.
            let mut set = MaybeUninit::<libc::sigset_t>::uninit();
            assert_eq!(libc::sigemptyset(set.as_mut_ptr()), 0);
            assert_eq!(libc::sigaddset(set.as_mut_ptr(), libc::SIGUSR1), 0);
            let set = set.assume_init();
            assert_eq!(
                libc::pthread_sigmask(libc::SIG_BLOCK, &set, std::ptr::null_mut()),
                0
            );

            // Deliver SIGUSR1 to ourselves; it is blocked, so it becomes pending.
            assert_eq!(libc::raise(libc::SIGUSR1), 0);

            // lin_signal_wait re-blocks SIGUSR1 (idempotent) and consumes the pending
            // signal via sigwait, returning the caught number.
            let caught = lin_signal_wait(libc::SIGUSR1);
            assert_eq!(caught, libc::SIGUSR1);
        }
    }
}
