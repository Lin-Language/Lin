# Web server — a small maps/routing service

An HTTP backend built on `std/http` and `std/template`. On startup it loads and
validates its configuration, then binds the configured port and serves requests:
an HTML index page, a JSON status endpoint, a small user resource, and a
**shortest-path routing API** (`/route/:from/:to`) backed by Dijkstra over a
weighted graph loaded from JSON.

## What it demonstrates

- **Startup config** (`config.lin`): a schema with defaults + type validation,
  returning a tagged `LoadResult = Success | Failure`. `main.lin` resolves the raw
  config into a typed `Config { host, port, debug, name }`, binds `config.port`
  (not a hard-coded port), and surfaces `name` in the index page and `/api/status`.
- **A real routing feature** (`routing.lin`): `solve(graphPath, from, to)` reads a
  weighted directed graph (`graph.json`), builds an adjacency map, runs Dijkstra,
  and reconstructs the route — returning a precise tagged union
  `RouteFound | RouteNone | RouteError` (no `Json` outside the file-read boundary).
- **Path routing** with `match` + `when` guards and `std/http`'s `matchPath`
  (`/route/:from/:to`, `/users/:id`), plus the `json` / `text` / `badRequest`
  response helpers.
- **HTML templating** with `std/template`'s `render`, using a layout: `index.jinja`
  `{% extends %}` `base.jinja`, which `{% include %}`s `footer.jinja`.
- **Graceful shutdown** with `std/signal`: `serve` blocks, so `main.lin` runs it on
  a background thread (`std/async`) and blocks the main thread on `waitSignal(15)`
  (SIGTERM); when the signal arrives it logs and exits.

## The /route API

`GET /route/:from/:to` resolves the shortest path over `graph.json`:

```
GET /route/A/E  ->  200 {"from":"A","to":"E","path":["A","C","B","D","E"],"distance":14}
GET /route/A/F  ->  404 {"error":"no route from A to F"}        # unreachable
                ->  500 {"error":"routing unavailable: ..."}     # graph unreadable
```

## Structure

| File | What it is |
| --- | --- |
| `main.lin` | Loads config, builds the router, serves on a background thread, waits for SIGTERM. Not run in CI (binds a port / blocks). |
| `config.lin` | `schema` / `applyDefaults` / `validate` / `load` → `LoadResult`. Owns `Config`. |
| `router.lin` | `route(req, config)` dispatches by path; `makeRouter(config)` returns the `serve` handler closure. |
| `handlers.lin` | `getIndex` / `getStatus` / `getRoute` / `getUser` — produce responses. |
| `routing.lin` | The routing engine: `buildAdj`, `dijkstra`, `reconstructPath`, `solve`. Owns `Edge`, `Neighbor`, `SolveResult`. |
| `graph.json` | The weighted graph the `/route` endpoint queries. |
| `views/*.jinja` | The index page template + shared layout + footer partial. |
| `config.test.lin` | Schema defaults/validation + `load` success/failure. |
| `routing.test.lin` | `buildAdj` / `dijkstra` / `reconstructPath` / `solve` (`std/fs` mocked). |
| `router.test.lin` / `handlers.test.lin` | Routing + handler responses (`render`/`solve` mocked). |
| `integration.test.lin` | End-to-end dispatch through `route`, running the real routing engine over an in-memory graph. |
| `template.test.lin` | Renders the real `index.jinja` file and asserts the holes are filled. |

## Run / Test

```bash
lin build examples/web-server/main.lin -o web-server   # compile the server
./web-server                                           # listen on the configured port
# then, in another shell:
curl localhost:3000/             # rendered HTML
curl localhost:3000/api/status   # {"status": "ok", "name": "Lin Maps", ...}
curl localhost:3000/route/A/E    # {"path": ["A","C","B","D","E"], "distance": 14}
curl localhost:3000/users/1      # {"id": "1", "name": "User 1"}

lin test examples/web-server/    # config + routing + router + handler + template suites
```

Note: `main.lin` binds a port and blocks in `serve`, so it is not run by the CI
example sweep; the config loader, routing engine, router, handlers, and templating
are covered by the `*.test.lin` suites and a Rust serve integration test
(`test_serve_real_http`).
