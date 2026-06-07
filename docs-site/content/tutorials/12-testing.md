# Testing

Lin ships with a test framework as an ordinary stdlib module, `std/test`. Tests are
plain Lin values, run by the `lin test` command. This tutorial covers writing tests,
structuring a project's suites, setup and teardown, mocking dependencies, and
measuring coverage.

## Your first test

A test file ends in `.test.lin`. It builds a suite of tests and hands it to `run`:

```lin
import { suite, test, run, expect, toBe } from "std/test"

val tests = [
  test("adds two numbers", () => [
    expect(2 + 3).toBe(5)
  ]),
  test("multiplies", () => [
    expect(4 * 4).toBe(16)
  ])
]

run(suite("arithmetic", tests))
```

Run it:

```bash
lin test arithmetic.test.lin
```

```
arithmetic
  ok  adds two numbers
  ok  multiplies

2 passed
```

`run` prints a summary and exits non-zero if any test failed, so it doubles as a CI
gate. Point `lin test` at a directory to run every `*.test.lin` under it:

```bash
lin test src/          # all suites in the project
```

## A test body returns an array of assertions

Every test body returns an `Assertion[]` — even a single assertion is wrapped in
`[ ... ]`:

```lin
test("one assertion", () => [
  expect(answer()).toBe(42)
])

test("several assertions", () => [
  expect(name()).toBe("Ada"),
  expect(age()).toBe(36),
  expect(active()).toBe(true)
])
```

This is enforced by the type system: a bare assertion or a sequence of bare
assertion statements is a compile error. That guarantee is the point — **every**
assertion in the array is evaluated, so none is silently skipped.

When a test needs setup, write the statements first and the array as the final
expression:

```lin
test("sorts ascending", () =>
  val sorted = [3, 1, 2].sort((a, b) => a - b)
  [ expect(sorted.toString()).toBe("[1, 2, 3]") ]
)
```

## Matchers

`expect(value)` begins an assertion chain. The available matchers:

| Matcher | Passes when |
| --- | --- |
| `.toBe(expected)` | `value` is deeply equal to `expected` (objects order-independent, arrays ordered) |
| `.toBeNull()` | `value` is `null` |
| `.toSatisfy(pred)` | `pred(value)` returns `true` |
| `.toSucceed()` | `value` has shape `{ "type": "success", ... }` |
| `.toFail()` | `value` has shape `{ "type": "failure", ... }` |
| `.toFailWith(msg)` | `value` is `{ "type": "failure", "error": msg }` |

`toSatisfy` is the escape hatch for anything else — pass a predicate, including one
that uses a `has` pattern to inspect a union shape:

```lin
test("returns a success result", () => [
  expect(parse("42")).toSatisfy(r => r has { "type": "success", value })
])
```

## How to structure a project's tests

The convention used throughout the `examples/` projects:

- **One `<module>.test.lin` per source file** — unit tests for that module's
  exported functions. `schema.lin` → `schema.test.lin`.
- **One `integration.test.lin` per project** — end-to-end tests that drive the whole
  program across modules on realistic input and assert the final output.

```
myproject/
  loader.lin
  loader.test.lin          # unit tests for loader.lin
  schema.lin
  schema.test.lin          # unit tests for schema.lin
  main.lin
  integration.test.lin     # end-to-end flow
```

Each `*.test.lin` is compiled and run as its own program, so suites are fully
isolated from one another.

## Setup and teardown

Lin's eager evaluation means most lifecycle needs are met without special keywords.

**beforeAll** — a module-scope binding above the suite. Test bodies run as the suite
array is built, so this runs once before them:

```lin
val db = openInMemoryDb()        // runs once, before any test

val tests = [
  test("query", () => [ expect(db.count()).toBe(0) ])
]
```

**afterAll** — `run` calls `exit(1)` on failure, so a statement after it would not
run when a test fails. For teardown that must always happen, use `report`, which
prints results and **returns the failure count** instead of exiting:

```lin
import { suite, test, run, report, expect, toBe } from "std/test"
import { exit } from "std/io"

val failures = report(suite("db", tests))
closeDb()                              // always runs, even on failure
if failures > 0 then exit(1) else null
```

**beforeEach / afterEach** — `withFixture` runs setup and teardown around each test
body and injects the fixture. Because assertion failures are values (not
exceptions), teardown always runs:

```lin
import { withFixture } from "std/test"

val openConn = (): Json => { "rows": [] }
val closeConn = (c: Json): Null => null

// Partial application builds a reusable per-fixture helper:
val withConn = withFixture(openConn, closeConn,)

val tests = [
  withConn("starts empty", (c) => [ expect(c["rows"].length()).toBe(0) ]),
  withConn("isolated per test", (c) => [ expect(c["rows"].length()).toBe(0) ])
]
```

## Mocking dependencies with `replace`

A unit test should not hit the real filesystem, clock, or network. The `replace`
statement (test files only) swaps an imported export for the whole test program:

```lin
import { suite, test, run, expect, toBe } from "std/test"
import { readFile } from "std/fs"

replace readFile = (path: String): Json => "mock contents of ${path}"

val tests = [
  test("reads the (mocked) file", () => [
    expect(readFile("/etc/config")).toBe("mock contents of /etc/config")
  ])
]

run(suite("config", tests))
```

The key property: the override applies to **every** caller of that export. If the
module under test calls `readFile` internally, it sees the mock — without any change
to that module. You mock the *dependency*, and the code that uses it is none the
wiser.

This works for sibling modules and stdlib wrappers alike (`std/fs.readFile`,
`std/time.now`, …). The polymorphic built-ins (`print`, `map`, `filter`, `reduce`,
`for`, `length`, `toString`, the async family) are not replaceable. The mock body is
type-checked against the export's real signature, so a drifting mock is a compile
error. Non-function `val` exports can be replaced too:

```lin
import { maxRetries } from "./config"
replace maxRetries = 1
```

### Spies

A mock that closes over a module-level `var` cell can record how it was called —
then you assert on it after the run, no extra framework needed:

```lin
import { appendFile } from "std/fs"

var writes = 0
var lastLine = ""

replace appendFile = (path: String, content: String): Json =>
  writes = writes + 1
  lastLine = content
  { "type": "success" }

val tests = [
  test("logging appends one line", () =>
    val _ = logEvent("started")
    [ expect(writes).toBe(1), expect(lastLine).toBe("started\n") ]
  )
]
```

`replace` is permitted **only** in a `*.test.lin` file — using it in a program built
with `lin build`/`lin run` is a hard compile error, so a shipped binary can never
silently swap a real import. For worked examples, see `examples/processes/` (mocks
`std/process.exec` so the task-runner tests are deterministic and hermetic) and
`examples/web-server/` (mocks `std/fs` read/write to test the file-driven `/route`
Dijkstra solver, and `std/template.render` to decouple the routing tests from a view
file on disk).

## Coverage

`lin test --coverage` instruments the code and reports which lines ran:

```bash
lin test src/ --coverage
```

A console summary is printed by default. For machine-readable output (to feed an
external viewer), choose the llvm-cov format and an output path:

```bash
lin test src/ --coverage --format llvm-cov --output coverage.profdata
```

Only your own modules (the suites and the non-stdlib code they exercise) are
instrumented — stdlib internals are excluded from the report.

## Useful flags

| Flag | Effect |
| --- | --- |
| `--filter <substr>` | Only run test files whose path contains `<substr>` |
| `--parallel <n>` | Number of parallel test runners (default: CPU count) |
| `--timeout <secs>` | Kill a test binary after this many seconds (default: 30) |
| `--verbose` | Show stdout/stderr from passing tests too |
| `--coverage` | Enable coverage instrumentation |

## What's next?

- [std/test reference](/stdlib/test) — the full API.
- The `examples/` projects — every one ships a unit + integration test suite that
  exercises the patterns here.
