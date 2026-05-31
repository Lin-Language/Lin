# std/template

String template rendering. Templates use `${key}` holes (plain text, not Lin string interpolation).

```lin
import { render, renderWith } from "std/template"
```

## Function reference

| Function | Signature | Description |
| --- | --- | --- |
| `render` | `(String, {}) -> String \| Error` | Load a `.lint` file and render with data |
| `renderWith` | `(String, {}) -> String` | Render a template string directly |

## Template syntax

Templates use `${key}` where `key` is a field name or dot-separated path into the data record:

```
Hello, ${name}!
You have ${stats.messages} unread messages.
```

Note: `${...}` in templates is not Lin string interpolation — it is template syntax. Template files typically use the `.lint` extension to avoid confusion.

---

### `renderWith`

```lin
val html = renderWith(
  "<h1>${title}</h1><p>${body}</p>",
  { "title": "Hello", "body": "World" }
)
// "<h1>Hello</h1><p>World</p>"
```

Missing keys render as `"null"`.

---

### `render`

```lin
match render("templates/email.lint", {
  "name": "Alice",
  "subject": "Welcome"
})
  has { "type": "failure", error } => print("template error: ${error}")
  else => sendEmail(result)
```

`render` reads the template file from disk, then substitutes values from the data record.

---

### Template files (`.lint`)

Create a `greeting.lint` file:

```
Hello, ${name}!
Your score is ${stats.score}.
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
  has { "type": "failure", error } => print("error: ${error}")
  else => print(result)
```

Output:

```
Hello, Bob!
Your score is 42.
```
