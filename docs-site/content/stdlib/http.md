# std/http

std/http — HTTP client and server, plus URL parsing/building.

All client functions are synchronous and blocking. Requests return an HttpResponse or an Error
shape (`{ "type": "error", ... }`) that you narrow with `is Error`. On the server side, write a
handler `(HttpRequest) -> HttpResponse` and pass it to `serve`; build responses with json / text
/ redirect / badRequest / notFound, route with matchPath, and read request bodies with parseBody.
The handler is the first argument to serve, so the dot-call form `handler.serve(port)` reads
naturally. The URL layer (Url / parse / build / join / withQuery / withPath, folded in from the
former std/url) splits a string into RFC 3986 components verbatim — no percent-decoding.

```lin
import { fetch, fetchJson, fetchWith, postJson } from "std/http"
import { serve, json, text, redirect, notFound, badRequest, matchPath, parseBody } from "std/http"
```

## Reference

#### `HttpRequest`

```lin
type HttpRequest = { "method": String, "path": String, "query": String, "headers": { String: String }, "body": String }
```

An incoming HTTP request passed to a `serve` handler: method, path, raw query string,
header map, and the raw request body.

#### `HttpResponse`

```lin
type HttpResponse = { "status": Int32, "headers": { String: String }, "body": String }
```

An HTTP response: numeric status, header map, and body string. Built by `json`/`text`/
`redirect`/`badRequest`/`notFound` or constructed directly.

#### `HttpOptions`

```lin
type HttpOptions = { "method": String, "headers": { String: String }, "body": String }
```

Options for `fetchWith`: HTTP method, request headers, and body.

#### `fetch`

```lin
val fetch = (url: String): HttpResponse | Error
```

GET `url`.
- **`url`** — the URL to fetch.
- **Returns** an `HttpResponse`, or an `Error` (`{ "type":"error", ... }`) if the request fails.

**Example:**

```lin
val result = fetch("https://example.com/ping")   // then result["status"]
```

#### `fetchWith`

```lin
val fetchWith = (url: String, options: AnyVal): HttpResponse | Error
```

Issue a request to `url` with a custom method/headers/body.
- **`url`** — the URL to request.
- **`options`** — an `HttpOptions`-shaped record (method, headers, body).
- **Returns** an `HttpResponse`, or an `Error` if the request fails.

**Example:**

```lin
fetchWith("https://api.example.com/items", { "method": "DELETE", "headers": {}, "body": "" })
```

#### `fetchJson`

```lin
val fetchJson = (url: String): AnyVal | Error
```

GET `url` and parse the response body as JSON.
- **`url`** — the URL to fetch.
- **Returns** the parsed JSON value, or an `Error` if the request fails (the error is passed through
         unparsed).

**Example:**

```lin
val users = fetchJson("https://api.example.com/users")   // then users.for(u => ...)
```

#### `postJson`

```lin
val postJson = (url: String, body: AnyVal): HttpResponse | Error
```

POST `body` to `url` as `application/json`.
- **`url`** — the target URL.
- **`body`** — any JSON-serialisable value; sent as the JSON request body.
- **Returns** an `HttpResponse`, or an `Error` if the request fails.

**Example:**

```lin
postJson("https://api.example.com/users", { "name": "Alice" })
```

#### `json`

```lin
val json = (status: Int32, data: AnyVal): HttpResponse
```

Build an `application/json` response with the given status and a JSON-serialised body.
- **`status`** — the HTTP status code.
- **`data`** — any JSON-serialisable value; becomes the response body.
- **Returns** the `HttpResponse`.

#### `text`

```lin
val text = (status: Int32, body: String): HttpResponse
```

Build a `text/plain; charset=utf-8` response with the given status and body.
- **`status`** — the HTTP status code.
- **`body`** — the plain-text body.
- **Returns** the `HttpResponse`.

#### `redirect`

```lin
val redirect = (location: String): HttpResponse
```

Build a 302 redirect response to `location`.
- **`location`** — the URL to redirect to (sent in the `Location` header).
- **Returns** the `HttpResponse`.

#### `notFound`

```lin
val notFound: HttpResponse
```

A ready-made 404 Not Found response.

#### `badRequest`

```lin
val badRequest = (message: String): HttpResponse
```

Build a 400 Bad Request response carrying `message` as the body.
- **`message`** — the error text to return as the body.
- **Returns** the `HttpResponse`.

#### `parseBody`

```lin
val parseBody = (req: AnyVal): AnyVal | Error
```

Parse a request's body as JSON.
- **`req`** — the `HttpRequest` (its `body` field is parsed).
- **Returns** the parsed JSON value, or an `Error` if the body is not valid JSON.

#### `matchPath`

```lin
val matchPath = (path: String, pattern: String): AnyVal
```

Match a request path against a route pattern (e.g. `/users/:id`), extracting path params.
- **`path`** — the concrete request path.
- **`pattern`** — the route pattern with `:name` capture segments.
- **Returns** a map of captured params if the path matches, or a non-match result otherwise.

**Example:**

```lin
matchPath("/users/42", "/users/:id")   // { "id": "42" }
```

#### `serve`

```lin
val serve = (handler: AnyVal, port: Int32): Null
```

Start a blocking HTTP server on `port`, dispatching each request to `handler`.
- **`handler`** — a function `(HttpRequest) -> HttpResponse`.
- **`port`** — the TCP port to listen on.
- **Returns** never returns under normal operation (runs the accept loop).

**Example:**

```lin
handler.serve(3000)   // handler = req => match req["path"] is "/ping" => text(200, "pong") else => notFound
```

### URL parsing/building (folded in from the former std/url module)

#### `Url`

```lin
type Url = { "scheme": String, "userinfo": String | Null, "host": String, "port": Int32 | Null, "path": String, "query": String | Null, "fragment": String | Null }
```

A parsed URL, split into its RFC 3986 components (each emitted/stored verbatim, not decoded):
  scheme    "https"; lowercased; "" if the input was relative
  userinfo  raw "user:pass" between "//" and "@", undecoded; Null if absent
  host      "example.com", "[::1]"; "" if no authority
  port      443; Null if absent (scheme defaults are not filled in)
  path      "/a/b"; raw, percent-encoding preserved
  query     raw, without the leading "?"; Null if absent
  fragment  raw, without the leading "#"; Null if absent

#### `parse`

```lin
val parse = (s: String): Url | Error
```

Parse `s` as an RFC 3986 URI (or relative reference). Components are split, not decoded.
- **`s`** — the URL string.
- **Returns** the parsed `Url`, or an `Error` if `s` violates the generic URI syntax.

#### `build`

```lin
val build = (u: Url): String
```

Serialise a `Url` back to a string — the inverse of `parse`. Components are emitted verbatim
(already-encoded); separators are inserted only for the components that are present.

#### `join`

```lin
val join = (base: String, ref: String): String | Error
```

Resolve `ref` against `base` using RFC 3986 §5 reference resolution and return the resulting
absolute URL string. Returns Error if `base` is not absolute or either side fails to parse.
Dot-call form reads "resolve this link against where I am": `base.join(href)`.

#### `withQuery`

```lin
val withQuery = (u: Url, query: String): Url
```

Return a copy of `u` with `query` replaced (raw query string, no leading "?").

#### `withPath`

```lin
val withPath = (u: Url, path: String): Url
```

Return a copy of `u` with `path` replaced (path should already be percent-encoded).
