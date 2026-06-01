# Lin documentation site

The Lin documentation site — including the static-site generator that builds it —
is written in Lin. It is a working showcase of the language and standard library
(`std/fs`, `std/string`, `std/array`, `std/path`, `std/template`, `std/io`).

## Layout

```
docs-site/
  builder/        the generator, written in Lin
    markdown.lin  Markdown → JSON block model → HTML
    nav.lin       navigation manifest → sidebar HTML
    main.lin      orchestrates: read content, render, write output, copy assets
  content/        documentation source (Markdown) + nav.json manifest
  templates/      base.jinja + page.jinja / home.jinja — the HTML shell (Jinja layout)
  static/         style.css, highlight.js, theme.js (copied verbatim into output/)
  output/         generated HTML (gitignored; produced by the builder, deployed to Pages)
```

## How it works

1. `markdown.lin` parses each Markdown file into a JSON array of block records
   (`heading`, `paragraph`, `code`, `list`, `blockquote`, `hr`), then renders that
   model to an HTML fragment. Keeping the JSON model in the middle keeps the parser
   and the renderer independent.
2. `nav.lin` turns `content/nav.json` into the sidebar, marking the current page.
3. `main.lin` walks `content/`, renders each page's fragment, injects `title`/`nav`/
   `content` into the `.jinja` template via `std/template`'s `render`, and writes
   the result under `output/`. Static assets are copied last.

The templates use a Jinja layout (minijinja-backed): `page.jinja` and `home.jinja`
both `{% extends "base.jinja" %}` and fill `{% block %}`s, so the shared `<head>`,
nav, and scripts live in one place. Repeating *content* structure (nav, lists) is
still built as HTML strings in Lin and injected into the template's holes; the
fragments are passed through unescaped (the `.jinja` extension leaves minijinja
autoescaping off).

## Build locally

```bash
cargo build --workspace                                  # build the lin compiler
./target/debug/lin build docs-site/builder/main.lin -o docs-builder
./docs-builder                                           # writes docs-site/output/
# open docs-site/output/index.html
```

`LIN_DOCS_BASE` sets a deployment subpath prefix for all links and assets. It
defaults to `""` (root) for local builds; CI sets `LIN_DOCS_BASE=/Lin` because the
GitHub Pages project site is served from `https://lin-language.github.io/Lin/`.

## Deployment

`.github/workflows/docs.yml` builds the compiler, compiles and runs the generator,
and publishes `docs-site/output/` to GitHub Pages on every push to `master`.

See `KNOWN_ISSUES.md` for compiler bugs found (and worked around) while writing the
builder.
