# std/test

A lightweight test framework. Tests are plain Lin values — no magic, no macros.

```lin
import { suite, test, run, expect } from "std/test"
```

## Types

```lin
type Assertion =
  | { "type": "pass" }
  | { "type": "fail", "message": String }

type Test = {
  "name": String,
  "run": () -> Assertion | Assertion[]
}

type Suite = {
  "name": String,
  "tests": Test[]
}
```

## Function reference

| Function | Signature | Description |
| --- | --- | --- |
| `suite` | `(String, Test[]) -> Suite` | Group tests under a name |
| `test` | `(String, () -> Assertion \| Assertion[]) -> Test` | Declare a test case |
| `run` | `(Suite[]) -> Null` | Execute all suites, print results, exit non-zero on failure |
| `expect` | `(Json) -> Asserter` | Begin an assertion chain |

---

### Basic usage

```lin
import { suite, test, run, expect } from "std/test"

val mathTests = suite("arithmetic", [
  test("addition", () =>
    expect(1 + 2).toBe(3)
  ),
  test("subtraction", () =>
    expect(10 - 3).toBe(7)
  )
])

run([mathTests])
```

Output:

```
arithmetic
  ok  addition
  ok  subtraction

2 passed
```

---

### Multiple assertions per test

```lin
test("string ops", () =>
  expect(length("hello")).toBe(5)
  expect(toUpper("hello")).toBe("HELLO")
  expect(trim("  hi  ")).toBe("hi")
)
```

All assertions are evaluated; the test fails if any fail.

---

### `expect` assertion methods

| Method | Passes when |
| --- | --- |
| `.toBe(expected)` | Value is deeply equal to `expected` |
| `.toBeNull()` | Value is `null` |
| `.toSatisfy(pred)` | `pred(value)` returns `true` |
| `.toSucceed()` | Value has shape `{ "type": "success", ... }` |
| `.toFail()` | Value has shape `{ "type": "failure", ... }` |
| `.toFailWith(msg)` | Value has `{ "type": "failure", "error": msg }` |

---

### Testing error cases

```lin
test("parse failure", () =>
  expect(tryParseInt32("bad")).toBeNull()
)

test("division result", () =>
  val result = divide(10.0, 0.0)
  expect(result).toFail()
)
```

---

### Running tests

`run` executes all suites, prints a summary, and calls `exit(1)` if any tests fail:

```lin
run([unitTests, integrationTests])
```

Exit code `0` = all passed; non-zero = at least one failed.
