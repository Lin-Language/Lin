# HTTP & Web

The `std/http` module is both an HTTP client and a small server, and `std/template` renders HTML (or any text) from a data record. Together they are enough to build a real web service.

## Making requests

`fetch` performs a GET and returns a response record — `{ "status", "headers", "body" }`:

```lin
import { print } from "std/io"
import { fetch } from "std/http"

val res = fetch("https://example.com/health")
print("status: ${res["status"]}")
print(res["body"])
```

When the body is JSON, `fetchJson` fetches and parses it in one step, handing back a plain `Json` value you read and match like any other:

```lin
import { print } from "std/io"
import { fetchJson } from "std/http"

val user = fetchJson("https://api.example.com/users/1")
print("name: ${user["name"]}")
```

To send data, `postJson` serialises a value as a JSON request body:

```lin
import { postJson } from "std/http"

val created = postJson("https://api.example.com/users", { "name": "Ada", "age": 36 })
```

For full control over method, headers, and body, use `fetchWith(url, options)` where `options` is `{ "method", "headers", "body" }`.

## A server

A server is a single handler function from a request to a response. `serve(handler, port)` — or `handler.serve(port)` with dot application — starts it and blocks, dispatching each request through your handler.

A request is `{ "method", "path", "query", "headers", "body" }`. The natural way to route is to **pattern-match** on the method and path shape:

```lin
import { serve, json, text } from "std/http"

val handler = (req: Json): Json =>
  match req
    has { "method": "GET", "path": "/" }       => text(200, "Welcome to Lin")
    has { "method": "GET", "path": "/health" } => json(200, { "ok": true })
    else => json(404, { "error": "not found" })

handler.serve(3000)
```

The `json` and `text` helpers build response records with the right `Content-Type`. There are also `redirect(location)`, `notFound`, and `badRequest(message)`.

## Path parameters

`matchPath(path, pattern)` matches a request path against a pattern with `:name` segments and returns the captured parameters as an object, or `null` if it doesn't match:

```lin
import { json, matchPath } from "std/http"

val handler = (req: Json): Json =>
  val params = matchPath(req["path"], "/users/:id")
  if params == null then json(404, { "error": "not found" })
  else json(200, { "id": params["id"] })
```

## Reading a request body

`parseBody(req)` parses the request's body as JSON:

```lin
import { json, parseBody } from "std/http"

val createUser = (req: Json): Json =>
  val body = parseBody(req)
  if body["name"] == null then json(400, { "error": "name required" })
  else json(201, { "created": body["name"] })
```

## Templating

`std/template` is Jinja-style (backed by minijinja): it fills `{{ ... }}` holes — plus `{% for %}` loops and `{% if %}` conditionals — with values from a data record. `renderWith` renders a template string; `render` reads a `.lint` template file from disk:

```lin
import { print } from "std/io"
import { renderWith } from "std/template"

val html = renderWith(
  "<h1>{{ title }}</h1><p>{{ count }} messages for {{ user.name }}</p>",
  { "title": "Inbox", "count": 3, "user": { "name": "Ada" } }
)
print(html)
```

Holes may use dotted paths (`{{ user.name }}`) to reach into nested objects. A missing key renders as the empty string; a malformed template returns an `Error`. Combine templating with the server to return HTML pages:

```lin
import { text } from "std/http"
import { render } from "std/template"

val page = (req: Json): Json =>
  val html = render("views/home.lint", { "title": "Home", "year": 2026 })
  match html
    is { "type": "error", "message": _ } => text(500, "template error")
    else => text(200, html)
```

## What's next?

- [Pattern Matching](/tutorials/05-pattern-matching.html) — the routing technique used above
- [Error Handling](/tutorials/08-error-handling.html) — handling failed requests as values
- [std/http reference](/stdlib/http.html) and [std/template reference](/stdlib/template.html) — the full API
