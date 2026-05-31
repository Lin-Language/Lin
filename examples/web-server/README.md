# Web server

A small HTTP server built on `std/http` and `std/template`: it listens on a TCP
port, routes each incoming request to a handler by path, and returns a response.

## What it demonstrates

- A real HTTP server via `serve` — `router.serve(3000)` (dot-call sugar for
  `serve(router, 3000)`) binds the port and serves requests sequentially.
- Path routing with `match` + a `when` guard (`matchPath(path, "/users/:id")`).
- `std/http` response helpers: `json`, `text`, `badRequest`, `matchPath`.
- HTML templating with `std/template`'s `render` (filling `${...}` holes) — the
  rendered HTML is returned in the response body.
- Imported types: `HttpRequest`/`HttpResponse` from `std/http`, brought in under
  `as Request`/`as Response` aliases and used in every handler's signature.

## Structure

- **`main.lin`** — imports `router` and calls `router.serve(3000)` (blocks forever).
- **`router.lin`** — `router(req)`: dispatches a `Request` to the right handler by path.
- **`handlers.lin`** — `getIndex` / `getStatus` / `getUser`: produce responses.
- **`views/index.lint`** — the HTML template rendered by `getIndex`.
- **`router.test.lin` / `handlers.test.lin`** — assert routed/handler responses
  (including that `getIndex` returns the rendered HTML body).
- **`template.test.lin`** — renders `index.lint` and asserts every `${...}` hole is filled.

## Run / Test

```bash
lin build examples/web-server/main.lin -o web-server   # compile the server
./web-server                                           # listen on http://localhost:3000
# then, in another shell:
curl localhost:3000/            # rendered HTML
curl localhost:3000/api/status  # {"status": "ok", ...}
curl localhost:3000/users/1     # {"id": "1", "name": "User 1"}

lin test examples/web-server/   # router + handler + template suites
```

Note: `main.lin` calls `serve`, which blocks forever, so it is not run by the
example sweep in CI; the router, handlers, and templating are covered by the
`*.test.lin` suites and a Rust serve integration test (`test_serve_real_http`).
