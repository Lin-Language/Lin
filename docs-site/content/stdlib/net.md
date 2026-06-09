# std/net

std/net — low-level UDP and TCP sockets, the byte-stream layer beneath std/http for
non-HTTP protocols and custom framing.

Every socket is an opaque integer file descriptor (`Int32`) — there are no socket objects
in user code. Data is read and written through caller-owned `UInt8[]` buffers; construct
one with a type-annotated array literal, e.g. `val buf: UInt8[] = [0, 0, 0, 0]`. IPv4 only;
`udpBind`/`tcpListen` bind to `0.0.0.0` (all interfaces).

Every fallible call returns the `T | Error` result shape (an `Error` is an object with
`"type": "error"`). A non-blocking read with no data available yet returns `Null`, so a poll
loop reads naturally:

```lin
import { tcpListen, tcpAccept, tcpRecv, tcpSend, tcpClose } from "std/net"
import { udpBind, udpRecv, udpRecvFrom, udpSendTo, udpClose } from "std/net"
```

For a higher-level lazy byte stream over a connection, see `tcpStream` (feeds std/stream
adapters).

## Reference

#### `Datagram`

```lin
type Datagram = { "len": Int32, "addr": String, "port": Int32 }
```

A received UDP datagram's metadata: byte count plus the sender's address and port.

#### `TcpPeer`

```lin
type TcpPeer = { "fd": Int32, "addr": String, "port": Int32 }
```

An accepted TCP connection: the new client socket's descriptor and the peer's address/port.

### UDP

#### `udpBind`

```lin
val udpBind = (port: Int32): Int32 | Error
```

Bind a UDP socket to `port` on all interfaces.
- **`port`** — the local port to bind (0 to let the OS choose).
- **Returns** the socket file descriptor, or an `Error` if the bind failed.

#### `udpRecv`

```lin
val udpRecv = (fd: Int32, buf: UInt8[]): Int32 | Null | Error
```

Receive a UDP datagram into `buf` (sender address discarded).
- **`fd`** — the socket descriptor from `udpBind`.
- **`buf`** — the byte buffer to fill.
- **Returns** the number of bytes received, or an `Error` on failure. After
  `udpSetNonblocking(fd, true)`, a receive with no data pending returns `Null` instead of
  blocking, so a poll loop can `match` on `Null` for "nothing yet".

#### `udpRecvFrom`

```lin
val udpRecvFrom = (fd: Int32, buf: UInt8[]): Datagram | Null | Error
```

Receive a UDP datagram into `buf`, keeping the sender's address.
- **`fd`** — the socket descriptor from `udpBind`.
- **`buf`** — the byte buffer to fill.
- **Returns** a `Datagram` (`{ len, addr, port }`) for the received packet, `Null` if non-blocking
  and nothing is pending, or an `Error` on failure.

**Example:**

```lin
val sock = udpBind(39303)
```

**Example:**

```lin
udpSendTo(sock, "127.0.0.1", 39303, [72, 105, 33])   // 3 bytes sent ("Hi!")
```

**Example:**

```lin
val buf: UInt8[] = [0, 0, 0, 0, 0, 0, 0, 0]
```

**Example:**

```lin
val res = udpRecvFrom(sock, buf)   // res["len"] == 3, res["addr"] == "127.0.0.1"
```

#### `udpSendTo`

```lin
val udpSendTo = (fd: Int32, addr: String, port: Int32, buf: UInt8[]): Int32 | Error
```

Send the bytes in `buf` as a UDP datagram to `addr:port`.
- **`fd`** — the socket descriptor from `udpBind`.
- **`addr`** — the destination host/IP.
- **`port`** — the destination port.
- **`buf`** — the bytes to send.
- **Returns** the number of bytes sent, or an `Error` on failure.

#### `udpSetNonblocking`

```lin
val udpSetNonblocking = (fd: Int32, on: Boolean): Null | Error
```

Set or clear non-blocking mode on a UDP socket.
- **`fd`** — the socket descriptor.
- **`on`** — `true` for non-blocking, `false` for blocking.
- **Returns** `Null` on success, or an `Error` on failure.

#### `udpClose`

```lin
val udpClose = (fd: Int32): Null
```

Close a UDP socket.
- **`fd`** — the socket descriptor to close.
- **Returns** `Null` (always succeeds).

### TCP

#### `tcpListen`

```lin
val tcpListen = (port: Int32): Int32 | Error
```

Listen for TCP connections on `port` on all interfaces.
- **`port`** — the local port to listen on.
- **Returns** the listening socket descriptor, or an `Error` if the listen failed.

#### `tcpAccept`

```lin
val tcpAccept = (fd: Int32): TcpPeer | Null | Error
```

Accept the next pending connection on a listening socket (blocks until one arrives, unless the
listener is non-blocking).
- **`fd`** — the listening socket descriptor from `tcpListen`.
- **Returns** a `TcpPeer` (`{ fd, addr, port }`) for the accepted connection, `Null` if non-blocking
  and none is pending, or an `Error` on failure.

**Example:**

```lin
val listener = tcpListen(8080)
```

**Example:**

```lin
val client = tcpAccept(listener)   // blocks until a connection arrives
```

**Example:**

```lin
val buf: UInt8[] = [0, 0, 0, 0, 0, 0, 0, 0]
```

**Example:**

```lin
val n = tcpRecv(client, buf)
```

**Example:**

```lin
tcpSend(client, slice(buf, 0, n))  // echo it back (slice from std/array)
```

#### `tcpConnect`

```lin
val tcpConnect = (host: String, port: Int32): Int32 | Error
```

Open a TCP connection to `host:port`.
- **`host`** — the destination host/IP.
- **`port`** — the destination port.
- **Returns** the connected socket descriptor, or an `Error` if the connect failed.

**Example:**

```lin
val fd = tcpConnect("127.0.0.1", 8080)
```

**Example:**

```lin
tcpSend(fd, [104, 105])        // "hi"
```

**Example:**

```lin
val buf: UInt8[] = [0, 0, 0, 0]
```

**Example:**

```lin
val n = tcpRecv(fd, buf)       // bytes read; 0 = peer closed
```

**Example:**

```lin
tcpClose(fd)
```

#### `tcpRecv`

```lin
val tcpRecv = (fd: Int32, buf: UInt8[]): Int32 | Null | Error
```

Receive bytes from a connected TCP socket into `buf`.
- **`fd`** — the connected socket descriptor.
- **`buf`** — the byte buffer to fill.
- **Returns** the number of bytes received (0 at EOF), `Null` if non-blocking and nothing is pending,
  or an `Error` on failure.

#### `tcpSend`

```lin
val tcpSend = (fd: Int32, buf: UInt8[]): Int32 | Error
```

Send the bytes in `buf` over a connected TCP socket.
- **`fd`** — the connected socket descriptor.
- **`buf`** — the bytes to send.
- **Returns** the number of bytes sent, or an `Error` on failure.

#### `tcpSetNonblocking`

```lin
val tcpSetNonblocking = (fd: Int32, on: Boolean): Null | Error
```

Set or clear non-blocking mode on a TCP socket.
- **`fd`** — the socket descriptor.
- **`on`** — `true` for non-blocking, `false` for blocking.
- **Returns** `Null` on success, or an `Error` on failure.

#### `tcpClose`

```lin
val tcpClose = (fd: Int32): Null
```

Close a TCP socket.
- **`fd`** — the socket descriptor to close.
- **Returns** `Null` (always succeeds).

#### `tcpStream`

```lin
val tcpStream = (fd: Int32): Stream<UInt8[]>
```

Wrap a connected TCP socket as a lazy byte stream.
- **`fd`** — the connected socket descriptor.
- **Returns** a `Stream<UInt8[]>` that pulls from the socket until EOF; closing the stream closes the socket.
