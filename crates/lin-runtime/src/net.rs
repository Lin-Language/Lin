//! `std/net` runtime intrinsics — UDP and TCP sockets.
//!
//! Sockets are exposed to Lin as opaque integer fd handles (spec §27.4). Every
//! fallible call returns the `T | Error` result shape; a non-blocking read with
//! no data available yet returns `Null` (a null pointer).
//!
//! ## fd lifetime / registry
//!
//! The raw OS fd alone is not enough to drive a socket safely: a naive
//! `UdpSocket::from_raw_fd(fd)` reconstructed on every call would *close* the fd
//! when the temporary wrapper drops at end of scope. Instead we keep the owning
//! Rust socket wrappers alive in a global registry — a `Mutex<HashMap<i32, SocketKind>>`
//! keyed by the OS fd. bind/listen/connect/accept insert into the registry and
//! return the fd; every other op looks the socket up and borrows it; close removes
//! the entry (dropping the wrapper, which closes the fd). The registry lives for the
//! program's lifetime, so leak detection must be disabled for ASan runs (the harness
//! passes `ASAN_OPTIONS=detect_leaks=0`).
//!
//! ## recv buffer contract
//!
//! `recv`/`recvFrom`/`tcpRecv` fill a caller-owned `UInt8[]` and never transfer it
//! across the boundary. The buffer's element count (`lin_array_length`) bounds the
//! read: we read at most `len` bytes straight into the array's flat `u8` data and
//! return the number of bytes actually read. The caller pre-sizes the buffer.

use std::collections::HashMap;
use std::net::{TcpListener, TcpStream, UdpSocket};
use std::os::fd::AsRawFd;
use std::sync::Mutex;

use crate::array::{lin_array_length, LinArray};
use crate::fs::{make_error_tagged, resolve_lin_str};
use crate::object::{lin_object_alloc, lin_object_set};
use crate::tagged::{alloc_tagged, lin_box_int32, TaggedVal, TAG_ARRAY, TAG_INT32, TAG_STR};

enum SocketKind {
    Udp(UdpSocket),
    TcpListener(TcpListener),
    TcpStream(TcpStream),
}

static REGISTRY: Mutex<Option<HashMap<i32, SocketKind>>> = Mutex::new(None);

fn with_registry<R>(f: impl FnOnce(&mut HashMap<i32, SocketKind>) -> R) -> R {
    // Recover from a poisoned lock instead of `.unwrap()`-panicking: these registry ops run
    // from `extern "C"` functions, where unwinding is UB / process abort. Poisoning only means a
    // prior thread panicked while holding the guard (plausible: handler closures run user code
    // under this registry on worker/async paths). The HashMap itself stays consistent, so we take
    // the inner guard and carry on.
    let mut guard = REGISTRY.lock().unwrap_or_else(|e| e.into_inner());
    let map = guard.get_or_insert_with(HashMap::new);
    f(map)
}

/// Stream-source read: pull up to `max` bytes from the connected TCP socket `fd`. Returns
/// `Ok(None)` at EOF (0 bytes read on a blocking stream), `Ok(Some(bytes))` for a chunk, or
/// `Err` on an I/O error or a non-TCP/unknown fd. Used by `std/stream`'s `tcpStream` backend
/// (the registry stays encapsulated here). Blocking read — `WouldBlock` is surfaced as Eof so a
/// non-blocking socket terminates the stream rather than spinning.
pub fn tcp_stream_read(fd: i32, max: usize) -> Result<Option<Vec<u8>>, String> {
    use std::io::Read;
    let mut buf = vec![0u8; max];
    let result = with_registry(|m| match m.get_mut(&fd) {
        Some(SocketKind::TcpStream(stream)) => Some(stream.read(&mut buf)),
        _ => None,
    });
    match result {
        None => Err("not a connected TCP socket".to_string()),
        Some(Ok(0)) => Ok(None),
        Some(Ok(n)) => {
            buf.truncate(n);
            Ok(Some(buf))
        }
        Some(Err(e)) if e.kind() == std::io::ErrorKind::WouldBlock => Ok(None),
        Some(Err(e)) => Err(e.to_string()),
    }
}

/// Close + remove the TCP socket `fd` from the registry (dropping the stream → closing the fd).
/// Idempotent (a missing fd is a no-op). Used by `std/stream`'s `tcpStream` backend on close.
pub fn tcp_stream_close(fd: i32) {
    with_registry(|m| {
        m.remove(&fd);
    });
}

/// Resolve the caller's UInt8[] buffer to (data ptr, capacity in bytes).
/// `buf` may be a TaggedVal*(Array) or a raw LinArray*; the inner array is a flat
/// UInt8 buffer whose element count bounds the read/send.
unsafe fn buf_parts(buf: *const u8) -> Option<(*mut u8, usize)> {
    if buf.is_null() {
        return None;
    }
    let tag = *buf;
    let lin_arr = if tag == TAG_ARRAY {
        (*(buf as *const TaggedVal)).payload as *const LinArray
    } else {
        buf as *const LinArray
    };
    if lin_arr.is_null() {
        return None;
    }
    let len = lin_array_length(lin_arr) as usize;
    let data = (*lin_arr).data as *mut u8;
    Some((data, len))
}

/// Build a `{ "len": Int32, "addr": String, "port": Int32 }` Json object (TaggedVal*(Object)).
unsafe fn make_len_addr_port(len: i32, addr: &str, port: i32) -> *mut u8 {
    let obj = lin_object_alloc(4);

    let len_key = crate::fs::make_string("len");
    let mut len_tv: TaggedVal = std::mem::zeroed();
    len_tv.tag = TAG_INT32;
    len_tv.payload = len as i64 as u64;
    lin_object_set(obj, len_key, &len_tv);

    let addr_key = crate::fs::make_string("addr");
    let addr_val = crate::fs::make_string(addr);
    let mut addr_tv: TaggedVal = std::mem::zeroed();
    addr_tv.tag = TAG_STR;
    addr_tv.payload = addr_val as u64;
    lin_object_set(obj, addr_key, &addr_tv);

    let port_key = crate::fs::make_string("port");
    let mut port_tv: TaggedVal = std::mem::zeroed();
    port_tv.tag = TAG_INT32;
    port_tv.payload = port as i64 as u64;
    lin_object_set(obj, port_key, &port_tv);

    alloc_tagged(crate::tagged::TAG_OBJECT, obj as u64)
}

/// Build a `{ "fd": Int32, "addr": String, "port": Int32 }` Json object (TaggedVal*(Object)).
unsafe fn make_fd_addr_port(fd: i32, addr: &str, port: i32) -> *mut u8 {
    let obj = lin_object_alloc(4);

    let fd_key = crate::fs::make_string("fd");
    let mut fd_tv: TaggedVal = std::mem::zeroed();
    fd_tv.tag = TAG_INT32;
    fd_tv.payload = fd as i64 as u64;
    lin_object_set(obj, fd_key, &fd_tv);

    let addr_key = crate::fs::make_string("addr");
    let addr_val = crate::fs::make_string(addr);
    let mut addr_tv: TaggedVal = std::mem::zeroed();
    addr_tv.tag = TAG_STR;
    addr_tv.payload = addr_val as u64;
    lin_object_set(obj, addr_key, &addr_tv);

    let port_key = crate::fs::make_string("port");
    let mut port_tv: TaggedVal = std::mem::zeroed();
    port_tv.tag = TAG_INT32;
    port_tv.payload = port as i64 as u64;
    lin_object_set(obj, port_key, &port_tv);

    alloc_tagged(crate::tagged::TAG_OBJECT, obj as u64)
}

// ===========================================================================
// UDP
// ===========================================================================

/// udpBind: (port) => Int32 | Error. Bind a UDP socket to 0.0.0.0:port.
#[no_mangle]
pub unsafe extern "C" fn lin_udp_bind(port: i32) -> *mut u8 {
    match UdpSocket::bind(("0.0.0.0", port as u16)) {
        Ok(sock) => {
            let fd = sock.as_raw_fd();
            with_registry(|m| m.insert(fd, SocketKind::Udp(sock)));
            lin_box_int32(fd)
        }
        Err(e) => make_error_tagged(&e.to_string()),
    }
}

/// udpRecv: (fd, buf) => Int32 | Null | Error. Read a datagram into buf.
#[no_mangle]
pub unsafe extern "C" fn lin_udp_recv(fd: i32, buf: *const u8) -> *mut u8 {
    let (data, cap) = match buf_parts(buf) {
        Some(p) => p,
        None => return make_error_tagged("invalid buffer"),
    };
    let result = with_registry(|m| match m.get(&fd) {
        Some(SocketKind::Udp(sock)) => {
            let slice = std::slice::from_raw_parts_mut(data, cap);
            Some(sock.recv(slice))
        }
        _ => None,
    });
    match result {
        None => make_error_tagged("not a bound UDP socket"),
        Some(Ok(n)) => lin_box_int32(n as i32),
        Some(Err(e)) if e.kind() == std::io::ErrorKind::WouldBlock => std::ptr::null_mut(),
        Some(Err(e)) => make_error_tagged(&e.to_string()),
    }
}

/// udpRecvFrom: (fd, buf) => { len, addr, port } | Null | Error.
#[no_mangle]
pub unsafe extern "C" fn lin_udp_recv_from(fd: i32, buf: *const u8) -> *mut u8 {
    let (data, cap) = match buf_parts(buf) {
        Some(p) => p,
        None => return make_error_tagged("invalid buffer"),
    };
    let result = with_registry(|m| match m.get(&fd) {
        Some(SocketKind::Udp(sock)) => {
            let slice = std::slice::from_raw_parts_mut(data, cap);
            Some(sock.recv_from(slice))
        }
        _ => None,
    });
    match result {
        None => make_error_tagged("not a bound UDP socket"),
        Some(Ok((n, peer))) => {
            make_len_addr_port(n as i32, &peer.ip().to_string(), peer.port() as i32)
        }
        Some(Err(e)) if e.kind() == std::io::ErrorKind::WouldBlock => std::ptr::null_mut(),
        Some(Err(e)) => make_error_tagged(&e.to_string()),
    }
}

/// udpSendTo: (fd, addr, port, buf) => Int32 | Error.
#[no_mangle]
pub unsafe extern "C" fn lin_udp_send_to(
    fd: i32,
    addr: *const u8,
    port: i32,
    buf: *const u8,
) -> *mut u8 {
    let addr_str = match resolve_lin_str(addr) {
        Some(s) => s,
        None => return make_error_tagged("invalid address"),
    };
    let (data, len) = match buf_parts(buf) {
        Some(p) => p,
        None => return make_error_tagged("invalid buffer"),
    };
    let result = with_registry(|m| match m.get(&fd) {
        Some(SocketKind::Udp(sock)) => {
            let slice = std::slice::from_raw_parts(data, len);
            Some(sock.send_to(slice, (addr_str.as_str(), port as u16)))
        }
        _ => None,
    });
    match result {
        None => make_error_tagged("not a bound UDP socket"),
        Some(Ok(n)) => lin_box_int32(n as i32),
        Some(Err(e)) => make_error_tagged(&e.to_string()),
    }
}

/// udpSetNonblocking: (fd, on) => Null | Error.
#[no_mangle]
pub unsafe extern "C" fn lin_udp_set_nonblocking(fd: i32, on: i32) -> *mut u8 {
    let result = with_registry(|m| match m.get(&fd) {
        Some(SocketKind::Udp(sock)) => Some(sock.set_nonblocking(on != 0)),
        _ => None,
    });
    match result {
        None => make_error_tagged("not a bound UDP socket"),
        Some(Ok(())) => std::ptr::null_mut(),
        Some(Err(e)) => make_error_tagged(&e.to_string()),
    }
}

/// udpClose: (fd) => Null | Error. Removing from the registry drops the socket.
#[no_mangle]
pub unsafe extern "C" fn lin_udp_close(fd: i32) -> *mut u8 {
    with_registry(|m| m.remove(&fd));
    std::ptr::null_mut()
}

// ===========================================================================
// TCP
// ===========================================================================

/// tcpListen: (port) => Int32 | Error. Bind + listen on 0.0.0.0:port.
#[no_mangle]
pub unsafe extern "C" fn lin_tcp_listen(port: i32) -> *mut u8 {
    match TcpListener::bind(("0.0.0.0", port as u16)) {
        Ok(listener) => {
            let fd = listener.as_raw_fd();
            with_registry(|m| m.insert(fd, SocketKind::TcpListener(listener)));
            lin_box_int32(fd)
        }
        Err(e) => make_error_tagged(&e.to_string()),
    }
}

/// tcpAccept: (fd) => { fd, addr, port } | Null | Error. The accepted stream is
/// registered under its own fd. Null = would-block (listener is non-blocking).
#[no_mangle]
pub unsafe extern "C" fn lin_tcp_accept(fd: i32) -> *mut u8 {
    let result = with_registry(|m| match m.get(&fd) {
        Some(SocketKind::TcpListener(listener)) => Some(listener.accept()),
        _ => None,
    });
    match result {
        None => make_error_tagged("not a TCP listener"),
        Some(Ok((stream, peer))) => {
            // Default the accepted stream to blocking on every platform. On Linux an
            // accepted socket never inherits the listener's O_NONBLOCK, but on the BSD
            // socket stack (macOS) it DOES — so without this, a recv() on a freshly
            // accepted peer of a non-blocking listener would-blocks (returns Null)
            // instead of waiting for the bytes. Resetting it makes accept() portable;
            // callers that want a non-blocking peer can still tcpSetNonblocking(fd, true).
            let _ = stream.set_nonblocking(false);
            let new_fd = stream.as_raw_fd();
            let ip = peer.ip().to_string();
            let port = peer.port() as i32;
            with_registry(|m| m.insert(new_fd, SocketKind::TcpStream(stream)));
            make_fd_addr_port(new_fd, &ip, port)
        }
        Some(Err(e)) if e.kind() == std::io::ErrorKind::WouldBlock => std::ptr::null_mut(),
        Some(Err(e)) => make_error_tagged(&e.to_string()),
    }
}

/// tcpConnect: (host, port) => Int32 | Error. Connect a stream; register under its fd.
#[no_mangle]
pub unsafe extern "C" fn lin_tcp_connect(host: *const u8, port: i32) -> *mut u8 {
    let host_str = match resolve_lin_str(host) {
        Some(s) => s,
        None => return make_error_tagged("invalid host"),
    };
    match TcpStream::connect((host_str.as_str(), port as u16)) {
        Ok(stream) => {
            let fd = stream.as_raw_fd();
            with_registry(|m| m.insert(fd, SocketKind::TcpStream(stream)));
            lin_box_int32(fd)
        }
        Err(e) => make_error_tagged(&e.to_string()),
    }
}

/// tcpRecv: (fd, buf) => Int32 | Null | Error. 0 = peer closed; Null = would-block.
#[no_mangle]
pub unsafe extern "C" fn lin_tcp_recv(fd: i32, buf: *const u8) -> *mut u8 {
    use std::io::Read;
    let (data, cap) = match buf_parts(buf) {
        Some(p) => p,
        None => return make_error_tagged("invalid buffer"),
    };
    let result = with_registry(|m| match m.get_mut(&fd) {
        Some(SocketKind::TcpStream(stream)) => {
            let slice = std::slice::from_raw_parts_mut(data, cap);
            Some(stream.read(slice))
        }
        _ => None,
    });
    match result {
        None => make_error_tagged("not a connected TCP socket"),
        Some(Ok(n)) => lin_box_int32(n as i32),
        Some(Err(e)) if e.kind() == std::io::ErrorKind::WouldBlock => std::ptr::null_mut(),
        Some(Err(e)) => make_error_tagged(&e.to_string()),
    }
}

/// tcpSend: (fd, buf) => Int32 | Error. Returns bytes written.
#[no_mangle]
pub unsafe extern "C" fn lin_tcp_send(fd: i32, buf: *const u8) -> *mut u8 {
    use std::io::Write;
    let (data, len) = match buf_parts(buf) {
        Some(p) => p,
        None => return make_error_tagged("invalid buffer"),
    };
    let result = with_registry(|m| match m.get_mut(&fd) {
        Some(SocketKind::TcpStream(stream)) => {
            let slice = std::slice::from_raw_parts(data, len);
            Some(stream.write(slice))
        }
        _ => None,
    });
    match result {
        None => make_error_tagged("not a connected TCP socket"),
        Some(Ok(n)) => lin_box_int32(n as i32),
        Some(Err(e)) => make_error_tagged(&e.to_string()),
    }
}

/// tcpSetNonblocking: (fd, on) => Null | Error. Works on listeners and streams.
#[no_mangle]
pub unsafe extern "C" fn lin_tcp_set_nonblocking(fd: i32, on: i32) -> *mut u8 {
    let result = with_registry(|m| match m.get(&fd) {
        Some(SocketKind::TcpListener(listener)) => Some(listener.set_nonblocking(on != 0)),
        Some(SocketKind::TcpStream(stream)) => Some(stream.set_nonblocking(on != 0)),
        _ => None,
    });
    match result {
        None => make_error_tagged("not a TCP socket"),
        Some(Ok(())) => std::ptr::null_mut(),
        Some(Err(e)) => make_error_tagged(&e.to_string()),
    }
}

/// tcpClose: (fd) => Null | Error. Removing from the registry drops the socket.
#[no_mangle]
pub unsafe extern "C" fn lin_tcp_close(fd: i32) -> *mut u8 {
    with_registry(|m| m.remove(&fd));
    std::ptr::null_mut()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Poison the REGISTRY mutex (panic in a closure while holding the guard, caught), then
    /// confirm `with_registry` recovers the inner guard instead of panicking. A panic here would,
    /// in a real `extern "C"` runtime call, abort the process — this is the FFI-safety guarantee.
    #[test]
    fn with_registry_recovers_from_poison() {
        // Force the lock into a poisoned state by panicking while holding it.
        let poisoned = std::panic::catch_unwind(|| {
            let _guard = REGISTRY.lock().unwrap();
            panic!("simulated panic while holding the registry lock");
        });
        assert!(poisoned.is_err(), "the closure should have panicked");
        assert!(
            REGISTRY.is_poisoned(),
            "the registry mutex should now be poisoned"
        );

        // The next access must NOT panic — it recovers the inner guard.
        let n = with_registry(|m| {
            m.insert(424242, SocketKind::Udp(
                UdpSocket::bind(("127.0.0.1", 0)).expect("bind ephemeral UDP socket"),
            ));
            m.len()
        });
        assert!(n >= 1, "insert should succeed after poison recovery");

        // Clean up so we don't leak the test entry / leave the registry dirty.
        with_registry(|m| {
            m.remove(&424242);
        });
    }
}
