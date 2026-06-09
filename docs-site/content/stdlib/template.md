# std/template

std/template — Jinja-style template rendering, backed by the minijinja engine.

`renderWith` renders an in-memory template string; `render` loads a `.jinja` file from disk
(and supports `{% extends %}`/`{% include %}` layouts, resolved from the file's directory).
The data record `{}` is the render context. Both return `String | Error` — a syntax/render
failure is the canonical `Error` value `{ "type": "error", "message": ... }`, matched with
`is Error`.

```lin
import { render, renderWith } from "std/template"
```

Template syntax: substitutions `{{ name }}` / `{{ stats.score }}` (dot-paths into the data
record); loops `{% for x in xs %}...{% endfor %}`; conditionals `{% if c %}...{% else %}...{% endif %}`;
the standard minijinja filters (`{{ name | upper }}`). Undefined/missing variables render as the
empty string (not null, not an error). Only the file-based `render` can resolve layouts —
`renderWith` has no source directory, so it cannot follow `{% extends %}`/`{% include %}`.

## Reference

#### `renderWith`

```lin
val renderWith = (template: String, data: {  }): String | Error
```

Render an in-memory Jinja-style template string (minijinja-backed).
- **`template`** — the template source. Because it is in-memory, `{% extends %}`/`{% include %}`
  cannot resolve (no directory to load from) — use `render` for layout/inheritance.
- **`data`** — the variables to render against; undefined variables render as `""`.
- **Returns** the rendered `String`, or an `Error` object `{ type, message }` on a syntax/render
  error.

**Example:**

```lin
renderWith("<h1>{{ t }}</h1>", { "t": "Hi" })  // "<h1>Hi</h1>"
```

#### `render`

```lin
val render = (path: String, data: {  }): String | Error
```

Read a template from `path` and render it, with full layout support: the template may
`{% extends "base.jinja" %}` and fill `{% block %}`s, or pull in partials via
`{% include "_nav.jinja" %}` — referenced files are loaded by name from the same directory as
`path`.
- **`path`** — the template file path.
- **`data`** — the variables to render against.
- **Returns** the rendered `String`, or an `Error` object `{ type, message }` on a missing file or a
  syntax/render error (discriminated via `is Error`).
