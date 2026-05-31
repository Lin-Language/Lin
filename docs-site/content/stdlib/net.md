# std/net

Low-level UDP and TCP sockets — the byte-stream layer beneath `std/http`, for non-HTTP protocols and custom framing. Every socket is an opaque integer file descriptor (`Int32`); there are no socket objects in user code. Data is read and written through caller-owned `UInt8[]` buffers. IPv4 only; `udpBind`/`tcpListen` bind to `0.0.0.0`.

```lin
import { tcpListen, tcpAccept, tcpRecv, tcpSend, tcpClose } from "std/net"
import { udpBind, udpRecv, udpRecvFrom, udpSendTo, udpClose } from "std/net"
```

Every fallible call returns the `T | Error` result shape (an `Error` is an object with `"type": "error"`). A non-blocking read with no data available yet returns `Null`, so a poll loop reads naturally. Construct a buffer with a type-annotated array literal: `val buf: UInt8[] = [0, 0, 0, 0]`.

## UDP functions

| Function | Signature | Description |
| --- | --- | --- |
| `udpBind` | `(Int32) -> Int32 \| Error` | Bind a UDP socket to a port; returns the fd |
| `udpRecv` | `(Int32, UInt8[]) -> Int32 \| Null \| Error` | Receive into a buffer; bytes read |
| `udpRecvFrom` | `(Int32, UInt8[]) -> { ...Json } \| Null \| Error` | Receive with sender address |
| `udpSendTo` | `(Int32, String, Int32, UInt8[]) -> Int32 \| Error` | Send a datagram to addr/port; bytes sent |
| `udpSetNonblocking` | `(Int32, Boolean) -> Null \| Error` | Toggle non-blocking mode |
| `udpClose` | `(Int32) -> Null \| Error` | Close the socket |

## TCP functions

| Function | Signature | Description |
| --- | --- | --- |
| `tcpListen` | `(Int32) -> Int32 \| Error` | Listen on a port; returns the listener fd |
| `tcpAccept` | `(Int32) -> Int32 \| Null \| Error` | Accept a connection; returns the client fd |
| `tcpConnect` | `(String, Int32) -> Int32 \| Error` | Connect to host/port; returns the fd |
| `tcpRecv` | `(Int32, UInt8[]) -> Int32 \| Null \| Error` | Receive into a buffer; bytes read |
| `tcpSend` | `(Int32, UInt8[]) -> Int32 \| Error` | Send a buffer; bytes sent |
| `tcpSetNonblocking` | `(Int32, Boolean) -> Null \| Error` | Toggle non-blocking mode |
| `tcpClose` | `(Int32) -> Null \| Error` | Close the socket |

---

### UDP round-trip

```lin
val sock = udpBind(39303)
val msg: UInt8[] = [72, 105, 33]            // "Hi!"
val sent = udpSendTo(sock, "127.0.0.1", 39303, msg)   // 3

val buf: UInt8[] = [0, 0, 0, 0, 0, 0, 0, 0]
val res = udpRecvFrom(sock, buf)
res["len"]    // 3
res["addr"]   // "127.0.0.1"
// buf now holds [72, 105, 33, ...]

udpClose(sock)
```

---

### Non-blocking receive

After `udpSetNonblocking(fd, true)`, a receive with no data pending returns `Null` instead of blocking:

```lin
val sock = udpBind(39302)
udpSetNonblocking(sock, true)
val buf: UInt8[] = [0, 0, 0, 0]
val r = udpRecv(sock, buf)
match r
  is Null => print("nothing yet")
  else    => print("read ${r} bytes")
udpClose(sock)
```

---

### TCP echo client

```lin
val fd = tcpConnect("127.0.0.1", 8080)
tcpSend(fd, [104, 105])         // "hi"
val buf: UInt8[] = [0, 0, 0, 0]
val n = tcpRecv(fd, buf)        // bytes read; 0 = peer closed
tcpClose(fd)
```

---

### TCP echo server

```lin
import { slice } from "std/array"

val listener = tcpListen(8080)
val client = tcpAccept(listener)   // blocks until a connection arrives
val buf: UInt8[] = [0, 0, 0, 0, 0, 0, 0, 0]
val n = tcpRecv(client, buf)
tcpSend(client, slice(buf, 0, n))
tcpClose(client)
tcpClose(listener)
```
