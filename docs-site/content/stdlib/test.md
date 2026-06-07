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
  "run": () -> Assertion[]
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
| `test` | `(String, () -> Assertion[]) -> Test` | Declare a test case |
| `run` | `(Suite) -> Null` | Execute a suite, print results, exit non-zero on failure |
| `report` | `(Suite) -> Int32` | Like `run`, but returns the failure count instead of exiting |
| `withFixture` | `(() -> Json, (Json) -> Null, String, (Json) -> Assertion[]) -> Test` | Per-test setup/teardown + injection |
| `expect` | `(Json) -> Asserter` | Begin an assertion chain |

---

### Basic usage

```lin
import { suite, test, run, expect } from "std/test"

val mathTests = suite("arithmetic", [
  test("addition", () => [
    expect(1 + 2).toBe(3)
  ]),
  test("subtraction", () => [
    expect(10 - 3).toBe(7)
  ])
])

run(mathTests)
```

Output:

```
arithmetic
  ok  addition
  ok  subtraction

2 passed
```

A test body **returns an array of assertions** (`Assertion[]`) — even a single
assertion is written `[ expect(...).toBe(...) ]`. This is enforced by the type
system, which is what guarantees every assertion is actually evaluated.

---

### Multiple assertions per test

```lin
test("string ops", () => [
  expect("hello".length()).toBe(5),
  expect("hello".toUpper()).toBe("HELLO"),
  expect("  hi  ".trim()).toBe("hi")
])
```

All assertions are evaluated; the test fails if any fail. When a test needs setup,
write the setup statements first and the assertion array as the final expression:

```lin
test("sorts ascending", () =>
  val sorted = [3, 1, 2].sort((a, b) => a - b)
  [ expect(sorted.toString()).toBe("[1, 2, 3]") ]
)
```

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
test("parse failure", () => [
  expect(tryParseInt32("bad")).toBeNull()
])

test("division result", () =>
  val result = divide(10.0, 0.0)
  [ expect(result).toFail() ]
)
```

---

### Running tests

`run` executes a suite, prints a summary, and calls `exit(1)` if any tests fail:

```lin
run(suite("unit", tests))
```

Exit code `0` = all passed; non-zero = at least one failed.

---

### Setup & teardown (lifecycle)

Lin's eager model needs no dedicated lifecycle keywords:

- **beforeAll** — a module-scope `val`/statement above the suite (test bodies run
  eagerly as the suite array is built, so it runs once before them).
- **afterAll** — statements after `report(suite)`. Unlike `run`, `report` returns
  the failure count instead of exiting, so cleanup runs even when a test fails:

```lin
val failures = report(suite("db", tests))
closeConnections()                    // always runs
if failures > 0 then exit(1) else null
```

- **beforeEach / afterEach** — `withFixture(setup, teardown, name, body)` builds a
  fixture, injects it into the body, and tears it down (failures are values, so
  teardown always runs). Compose it into a per-fixture helper with partial
  application:

```lin
val withDb = withFixture(openDb, closeDb,)

val tests = [
  withDb("inserts a row", (db) => [ expect(db.count()).toBe(1) ]),
  withDb("reads it back", (db) => [ expect(db.first().name).toBe("ada") ])
]
```

---

### Mocking with `replace`

A test-only `replace <name> = <expr>` statement overrides an imported export for
the whole test program — for isolating the unit under test from a sibling module or
a stdlib dependency:

```lin
import { readFile } from "std/fs"

replace readFile = (path: String): String | Error => "mock contents of ${path}"
```

The override applies to **every** caller of that export — the test file, the module
under test, and any transitive importer — because it swaps the export's single
compiled symbol. Highlights:

- **Stdlib is mockable** at the Lin-API level (`std/fs.readFile`, `std/time.now`,
  …). The polymorphic built-ins (`print`, `map`, `filter`, `reduce`, `for`,
  `length`, `toString`, the async family) are not.
- The mock body is **type-checked** against the export's real signature.
- Non-function `val` exports can be replaced too (`replace maxRetries = 99`).
- A **spy** is a mock closing over a module-level `var` cell to record calls,
  asserted after the run.
- `replace` is permitted **only in a `*.test.lin`** — a hard error elsewhere, so a
  shipped binary can never silently swap an import.

For worked examples, see `examples/processes/` (mock `exec`) and
`examples/web-server/` (mock `std/fs` in the `/route` solver, and `render`).
