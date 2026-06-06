# std/http

HTTP client and server. All client functions are synchronous and blocking.

```lin
import { fetch, fetchJson, fetchWith, postJson } from "std/http"
import { serve, json, text, redirect, notFound, badRequest, matchPath, parseBody } from "std/http"
```

## Types

```lin
type HttpRequest = {
  "method":  String,
  "path":    String,
  "query":   String,
  "headers": { String: String },
  "body":    String
}

type HttpResponse = {
  "status":  Int32,
  "headers": { String: String },
  "body":    String
}

type HttpOptions = {
  "method":  String,
  "headers": { String: String },
  "body":    String
}
```

## Client functions

| Function | Signature | Description |
| --- | --- | --- |
| `fetch` | `(String) -> HttpResponse \| Error` | GET a URL |
| `fetchJson` | `(String) -> Json \| Error` | GET a URL and parse body as JSON |
| `fetchWith` | `(String, HttpOptions) -> HttpResponse \| Error` | Request with custom options |
| `postJson` | `(String, Json) -> HttpResponse \| Error` | POST a JSON body |

### `fetch`

```lin
val result = fetch("https://example.com/ping")
match result
  is Error => print("network error: ${result["message"]}")
  else => print(result["status"])
```

### `fetchJson`

```lin
val users = fetchJson("https://api.example.com/users")
match users
  is Error => print("failed: ${users["message"]}")
  else => users.for(u => print(u["name"]))
```

### `fetchWith`

```lin
val resp = fetchWith("https://api.example.com/items", {
  "method": "DELETE",
  "headers": { "Authorization": "Bearer ${token}" },
  "body": ""
})
```

### `postJson`

```lin
postJson("https://api.example.com/users", { "name": "Alice" })
```

## Server functions

| Function | Signature | Description |
| --- | --- | --- |
| `serve` | `((HttpRequest) -> HttpResponse, Int32) -> Null` | Start HTTP server (sequential) |
| `json` | `(Int32, Json) -> HttpResponse` | Build JSON response |
| `text` | `(Int32, String) -> HttpResponse` | Build plain-text response |
| `redirect` | `(String) -> HttpResponse` | Build 302 redirect |
| `notFound` | `HttpResponse` | Pre-built 404 response (value, not function) |
| `badRequest` | `(String) -> HttpResponse` | Build 400 response |
| `matchPath` | `(String, String) -> { ...String } \| Null` | Match URL path against pattern |
| `parseBody` | `(HttpRequest) -> Json \| Error` | Parse request body as JSON |

### `serve`

The handler is the **first** argument, so the dot-call form `handler.serve(port)` reads naturally:

```lin
val handler = req =>
  match req["path"]
    is "/ping" => text(200, "pong")
    else       => notFound

handler.serve(3000)
```

### `matchPath`

`matchPath(path, pattern)` returns an object of captured `:name` params, or `Null` if the path does not match.

```lin
val params = matchPath("/users/42", "/users/:id")
// { "id": "42" }

// Chain off request:
val params = req["path"].matchPath("/users/:id/posts")
```

### `parseBody`

```lin
val handler = req =>
  val body = parseBody(req)
  match body
    is Error => badRequest(body["message"])
    else => json(200, { "received": body })

handler.serve(3000)
```
