//! `std/tty` runtime intrinsics — raw terminal mode and non-blocking key reads.
//!
//! Per spec §35.7:
//! ```txt
//! rawMode: (on: Boolean) => Null | Error
//! readKey: ()           => Int32 | Null   // keycode, or Null if no key available
//! ```
//!
//! ## raw mode / termios save-restore
//!
//! `rawMode(true)` reads the current termios of stdin (fd 0) with `tcgetattr`, saves
//! it in a global `Mutex<Option<termios>>`, then disables canonical mode and echo
//! (`ICANON | ECHO`) and sets `VMIN = 0 / VTIME = 0` so `read` returns immediately
//! with whatever is available (non-blocking single-key reads). `rawMode(false)`
//! restores the exact saved termios; if nothing was saved it re-enables `ICANON | ECHO`
//! as a sane cooked default.
//!
//! If stdin is not a TTY, `tcgetattr` fails (typically `ENOTTY`) and we return an
//! Error object rather than panicking. This makes the call safe to invoke under a
//! test harness whose stdin is a pipe.
//!
//! ## readKey
//!
//! `readKey` does a non-blocking single-byte `read(0, ..)`. With `VMIN=0/VTIME=0`
//! set by rawMode, `read` returns 0 when no key is available → `Null`. One byte read
//! → the boxed byte value (0..255). Escape sequences (arrow keys etc.) are multi-byte;
//! `readKey` returns one byte at a time, so callers reassemble sequences themselves.

use std::sync::Mutex;

use crate::fs::make_error_tagged;
use crate::tagged::lin_box_int32;

static SAVED_TERMIOS: Mutex<Option<libc::termios>> = Mutex::new(None);

/// rawMode: (on) => Null | Error. on != 0 enables raw mode; on == 0 restores it.
#[no_mangle]
pub unsafe extern "C" fn lin_tty_raw_mode(on: i32) -> *mut u8 {
    let fd = libc::STDIN_FILENO;

    let mut term: libc::termios = std::mem::zeroed();
    if libc::tcgetattr(fd, &mut term) != 0 {
        let e = std::io::Error::last_os_error();
        return make_error_tagged(&format!("tcgetattr: {}", e));
    }

    if on != 0 {
        // Save the original attributes once (only if not already in raw mode).
        {
            let mut saved = SAVED_TERMIOS.lock().unwrap();
            if saved.is_none() {
                *saved = Some(term);
            }
        }
        // Minimal raw mode: disable canonical input and echo; non-blocking reads.
        term.c_lflag &= !(libc::ICANON | libc::ECHO);
        term.c_cc[libc::VMIN] = 0;
        term.c_cc[libc::VTIME] = 0;
        if libc::tcsetattr(fd, libc::TCSANOW, &term) != 0 {
            let e = std::io::Error::last_os_error();
            return make_error_tagged(&format!("tcsetattr: {}", e));
        }
    } else {
        // Restore the saved attributes, or fall back to a sane cooked default.
        let restore = {
            let mut saved = SAVED_TERMIOS.lock().unwrap();
            saved.take()
        };
        match restore {
            Some(orig) => {
                if libc::tcsetattr(fd, libc::TCSANOW, &orig) != 0 {
                    let e = std::io::Error::last_os_error();
                    return make_error_tagged(&format!("tcsetattr: {}", e));
                }
            }
            None => {
                term.c_lflag |= libc::ICANON | libc::ECHO;
                if libc::tcsetattr(fd, libc::TCSANOW, &term) != 0 {
                    let e = std::io::Error::last_os_error();
                    return make_error_tagged(&format!("tcsetattr: {}", e));
                }
            }
        }
    }
    std::ptr::null_mut()
}

/// readKey: () => Int32 | Null. Non-blocking single-byte read from stdin.
/// 0 bytes available → Null; 1 byte → boxed byte value; would-block → Null.
#[no_mangle]
pub unsafe extern "C" fn lin_tty_read_key() -> *mut u8 {
    let mut byte: u8 = 0;
    let n = libc::read(
        libc::STDIN_FILENO,
        &mut byte as *mut u8 as *mut libc::c_void,
        1,
    );
    if n == 1 {
        lin_box_int32(byte as i32)
    } else {
        // n == 0: no key available (VMIN=0/VTIME=0) or EOF.
        // n  < 0: error (WouldBlock/EINTR/etc.). The spec types readKey as Int32 | Null
        // with no Error variant, so any non-success collapses to Null.
        std::ptr::null_mut()
    }
}
