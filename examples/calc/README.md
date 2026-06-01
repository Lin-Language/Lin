# calc — arithmetic expression interpreter

A small, self-contained interpreter for integer arithmetic expressions:
`tokenize → parse → evaluate`. Handles `+ - * /`, parentheses, standard
precedence and left-associativity, and reports division-by-zero and parse
errors as recoverable tagged results rather than aborting.

## What it demonstrates

- Top-level recursive functions threading an index/cursor (Lin has no `while`,
  and local `val`s can't self-recurse) — the idiomatic stateful-iteration shape.
- **Named type aliases** for every record shape: `Token`, the recursive AST
  union `Ast = Num | BinOp`, and the tagged result `CalcResult = Success | Failure`.
- **Tagged-union results** matched with `has { "type": "success", value }`.
- String/char handling (`charCode`, `substring`, `at`), string interpolation.
- Typed array boundaries: `tokenize` returns `Token[]`, `parse` takes `Token[]`.

## Structure

| File | What it is |
| --- | --- |
| `lexer.lin` | Source `String` → `Token[]` (`{ "kind", "text" }`). |
| `parser.lin` | Recursive-descent parser, `Token[]` → `Parsed` (AST or failure). |
| `eval.lin` | AST walker, `Parsed` → `Evaluated` (`Int32` or failure). |
| `calc.lin` | The full pipeline as one `calc(src): CalcResult`. |
| `main.lin` | Prints a handful of example evaluations. |
| `lexer.test.lin` | Lexer unit tests (`String` → `Token[]`). |
| `parser.test.lin` | Parser unit tests (`Token[]` → AST). |
| `eval.test.lin` | Evaluator unit tests (AST → `Int32`). |
| `integration.test.lin` | End-to-end `calc(src)` pipeline tests. |

Note: precise types annotate every public boundary; the intermediate
cursor/AST values inside the parser and evaluator stay `Json` because they are
unions read by index (`["pos"]`, `["left"]`) after a predicate guard, which the
checker can't narrow — the genuinely-dynamic case.

## Run / Test

```sh
lin run examples/calc/main.lin
lin test examples/calc/
```
