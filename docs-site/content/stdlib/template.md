# std/template

Jinja-style template rendering, backed by the [minijinja](https://crates.io/crates/minijinja) engine.

```lin
import { render, renderWith } from "std/template"
```

## Function reference

| Function | Signature | Description |
| --- | --- | --- |
| `render` | `(String, {}) -> String \| Error` | Load a `.lint` file and render with data |
| `renderWith` | `(String, {}) -> String \| Error` | Render a template string directly |

## Template syntax

Templates use Jinja syntax. The data record `{}` is the render context.

- **Substitutions** — `{{ name }}`, `{{ stats.score }}` (dot-separated paths into the data record).
- **Loops** — `{% for item in items %}...{% endfor %}`.
- **Conditionals** — `{% if cond %}...{% else %}...{% endif %}`.
- **Filters** — the standard minijinja builtin filters, e.g. `{{ name | upper }}`.
- **Layouts** — `{% extends "base.lint" %}` + `{% block %}`, and partials via `{% include "footer.lint" %}` (file-based `render` only — see [Layouts](#layouts)).

```
Hello, {{ name }}!
You have {{ stats.messages }} unread messages.
{% for tag in tags %}#{{ tag }} {% endfor %}
```

**Undefined / missing variables render as the empty string** (not `null`, not an error). A **template syntax error or render failure** is returned as an `Error` value (`{ "type": "error", "message": ... }`), discriminated with `is Error`.

---

### `renderWith`

```lin
val html = renderWith(
  "<h1>{{ title }}</h1><p>{{ body }}</p>",
  { "title": "Hello", "body": "World" }
)
// "<h1>Hello</h1><p>World</p>"
```

Loops and conditionals work too:

```lin
renderWith(
  "{% for n in nums %}{{ n }}{% if not loop.last %}, {% endif %}{% endfor %}",
  { "nums": [1, 2, 3] }
)
// "1, 2, 3"
```

A missing variable renders as the empty string; a malformed template returns an `Error`.

---

### `render`

```lin
val result = render("templates/email.lint", {
  "name": "Alice",
  "subject": "Welcome"
})
match result
  is Error => print("template error: ${result["message"]}")
  else => sendEmail(result)
```

`render` reads the template file from disk, then renders it against the data record. It returns an `Error` if the file cannot be read or the template fails to render.

---

### Template files (`.lint`)

Create a `greeting.lint` file:

```
Hello, {{ name }}!
Your score is {{ stats.score }}.
{% if stats.score > 40 %}Great work!{% endif %}
```

Load and render it:

```lin
import { render } from "std/template"
import { print } from "std/io"

val result = render("greeting.lint", {
  "name": "Bob",
  "stats": { "score": 42 }
})

match result
  is Error => print("error: ${result["message"]}")
  else => print(result)
```

Output:

```
Hello, Bob!
Your score is 42.
Great work!
```

---

## Layouts

`render` (the file-based entry point) supports template **inheritance**, so pages share a
single layout instead of duplicating their chrome. Referenced templates are loaded by name
from the same directory as the file passed to `render`.

A base layout declares the skeleton with fillable blocks:

```
<!-- templates/base.lint -->
<!DOCTYPE html>
<html>
<head><title>{{ title }}</title></head>
<body{% block body_attrs %}{% endblock %}>
  {% block main %}{% endblock %}
  {% include "footer.lint" %}
</body>
</html>
```

A page extends it and fills in the blocks (an unfilled block keeps the base's default):

```
<!-- templates/page.lint -->
{% extends "base.lint" %}

{% block main %}
  <article>{{ content }}</article>
{% endblock %}
```

```lin
render("templates/page.lint", { "title": "Docs", "content": "<p>Hi</p>" })
```

`base.lint` and `footer.lint` are resolved from `templates/` automatically. This is how the
docs site itself is built — see `docs-site/templates/`.

> `renderWith` takes an in-memory string with no source directory, so it **cannot** resolve
> `{% extends %}` / `{% include %}`. Use `render` (file-based) for layouts.
