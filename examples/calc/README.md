# calc — arithmetic expression interpreter

A small, self-contained interpreter for integer arithmetic expressions:
`tokenize → parse → evaluate`. Handles `+ - * /`, parentheses, standard
precedence and left-associativity, and reports division-by-zero and parse
errors as recoverable tagged results rather than aborting.

## What it demonstrates

- The parser threads a cursor through top-level recursive functions (local
  `val`s can't self-recurse); the lexer scans with `while` and `range(...).find`.
- **Named type aliases** for every record shape: `Token`, the recursive AST
  union `Ast = Num | BinOp`, and the tagged result `Evaluated = Success<Int32> | Failure`.
- **Tagged-union results** matched with `has { "type": "success", value }`.
- String/char handling (`charCode`, `substring`, `at`), string interpolation.
- Typed array boundaries: `tokenize` returns `Token[]`, `parse` takes `Token[]`.

## Structure

| File | What it is |
| --- | --- |
| `lexer.lin` | Source `String` → `Token[]` (`{ "kind", "text" }`). |
| `parser.lin` | Recursive-descent parser, `Token[]` → `Parsed` (AST or failure). |
| `eval.lin` | AST walker, `Parsed` → `Evaluated` (`Int32` or failure). |
| `result.lin` | Shared `Success<T>` / `Failure` shapes and `fail`. |
| `calc.lin` | The full pipeline as one `calc(src): Evaluated`. |
| `main.lin` | Prints a handful of example evaluations. |
| `lexer.test.lin` | Lexer unit tests (`String` → `Token[]`). |
| `parser.test.lin` | Parser unit tests (`Token[]` → AST). |
| `eval.test.lin` | Evaluator unit tests (AST → `Int32`). |
| `integration.test.lin` | End-to-end `calc(src)` pipeline tests. |

## Run / Test

```sh
lin run examples/calc/main.lin
lin test examples/calc/
```
