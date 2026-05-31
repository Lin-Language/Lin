# result

A **form-validation pipeline** built on a generic `Result<T, E>` tagged union. Field
validators return a `Result`, and the registration validator composes them so the
chain short-circuits on the first failing field — the canonical "errors as values"
pattern.

## What it demonstrates

- A generic `Result<T, E>` whose discriminant `"type"` uses **singleton string-literal
  types** (`"success"` / `"failure"`) — checked at compile time (spec §18).
- Composable Result combinators: `ok` / `err` / `andThen` (chain a fallible step) /
  `map` (transform a success) / `unwrapOr`.
- Short-circuiting validation: `andThen` runs the next step only on success, so the
  first failing field's error propagates and later checks are skipped.
- Dot-syntax chaining of `andThen` to build a readable pipeline.

## Structure

- **`result.lin`** — the generic `Result<T, E>` type and its combinators (the reusable core).
- **`validate.lin`** — field validators (`nonEmpty`, `email`, `intInRange`) returning `Result`.
- **`form.lin`** — composes the validators into `validateForm` (RawForm → Registration | error).
- **`main.lin`** — validates a few sample forms and prints the outcome.
- **`result.test.lin`** — unit tests for the combinators.
- **`validate.test.lin`** — unit tests for each field validator.
- **`integration.test.lin`** — end-to-end: the full pipeline, incl. first-error short-circuit.

## Run / Test

```bash
lin run examples/result/main.lin
lin test examples/result/
```
