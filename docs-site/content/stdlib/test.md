# std/test

std/test — a lightweight test framework. Tests are plain Lin values — no magic, no macros.

  import { suite, test, run, expect } from "std/test"

A test body RETURNS an array of assertions (`Assertion[]`) — even a single assertion is written
`[ expect(...).toBe(...) ]`. The array requirement is type-enforced, which is what guarantees
every assertion is actually evaluated. Build tests with `test(name, () => [...])`, group them
with `suite(name, tests)`, then run them: `run` prints a summary and `exit(1)`s if any test
fails (the CI default), while `report` returns the failure count instead of exiting — the
building block for guaranteed afterAll teardown.

Assertions begin with `expect(value)` and chain a matcher: `.toBe(expected)` (deep equality),
`.toBeNull()`, `.toSatisfy(pred)`, `.toSucceed()` / `.toFail()` / `.toFailWith(msg)` (shape
checks on `{ "type": "success" | "failure" }` values).

Lifecycle without keywords: a module-scope `val` above the suite is "beforeAll" (test bodies run
eagerly as the suite array is built); statements after `report(suite)` are "afterAll";
`withFixture(setup, teardown, name, body)` is per-test "beforeEach/afterEach" with dependency
injection — compose it via partial application, e.g. `val withDb = withFixture(openDb, closeDb,)`.
Mocking is the test-only `replace <name> = <expr>` statement (see std-level docs / the Testing
tutorial), permitted only in a `*.test.lin`.

## Reference

#### `expect`

```lin
val expect = (value: Json): Json
```

Begin an assertion by wrapping `value` in an asserter for a matcher to inspect.
- **`value`** — the value under test.
- **Returns** an asserter `{ "value": value }`; chain a matcher via dot-syntax, e.g.
  `expect(x).toBe(42)`.

#### `toBe`

```lin
val toBe = (asserter: Json, expected: Json): Json
```

Assert the asserter's value equals `expected` (structural `==`).
- **`asserter`** — the asserter from `expect`.
- **`expected`** — the value it should equal.
- **Returns** a passing assertion, or a failing one carrying the expected/actual pair.
- **Example:** expect(1 + 2).toBe(3)
- **Example:** expect("hi".toUpper()).toBe("HI")

#### `toBeNull`

```lin
val toBeNull = (asserter: Json): Json
```

Assert the asserter's value is `null`.
- **`asserter`** — the asserter from `expect`.
- **Returns** a passing assertion, or a failing one carrying the actual value.

#### `toSatisfy`

```lin
val toSatisfy = (asserter: Json, pred: Function): Json
```

Assert the asserter's value satisfies a predicate.
- **`asserter`** — the asserter from `expect`.
- **`pred`** — a `(Json) => Boolean` predicate the value must satisfy.
- **Returns** a passing assertion if `pred(value)` is true, otherwise a failing one.

#### `toSucceed`

```lin
val toSucceed = (asserter: Json): Json
```

Assert the asserter's value is a success result (`{ "type": "success" }`).
- **`asserter`** — the asserter from `expect`.
- **Returns** a passing assertion if it is a success, otherwise a failing one.

#### `toFail`

```lin
val toFail = (asserter: Json): Json
```

Assert the asserter's value is a failure result (`{ "type": "failure" }`).
- **`asserter`** — the asserter from `expect`.
- **Returns** a passing assertion if it is a failure, otherwise a failing one.

#### `toFailWith`

```lin
val toFailWith = (asserter: Json, message: String): Json
```

Assert the asserter's value is a failure result whose `error` equals `message`.
- **`asserter`** — the asserter from `expect`.
- **`message`** — the expected failure error message.
- **Returns** a passing assertion if it is a failure with that message, otherwise a failing one
  carrying the expected/actual pair.

#### `test`

```lin
val test = (name: String, body: ()
```

Define a test: eagerly run its body and store the result alongside the name.
- **`name`** — the test name (also used by `--filter-test` selection).
- **`body`** — a thunk returning an `Assertion[]` (use the `[ expect(...).toBe(...), ... ]` form,
  even for a single assertion); every assertion is evaluated and the test fails if any fails.
- **Returns** a result record. When `--filter-test` (via LIN_TEST_ONLY) did not select this test, the
  body is NOT evaluated (no side effects, no fixture setup/teardown) and a `{ "type": "skip" }`
  sentinel is produced, which `report` counts as neither pass nor fail.

#### `withFixture`

```lin
val withFixture = (setup: ()
```

Run a test with per-test setup/teardown and dependency injection (ADR-046): build a fixture,
inject it into the body, and tear it down — all within a single `test`. The functional
alternative to keyword beforeEach/afterEach; compose via partial application into a per-fixture
helper, e.g. `val withDb = withFixture(openDb, closeDb,)`, then `withDb("name", db => [ ... ])`.
- **`setup`** — builds the fixture value.
- **`teardown`** — releases the fixture; always runs, even when the body's assertions fail (because
  assertion failures are VALUES, not exceptions).
- **`name`** — the test name.
- **`body`** — receives the fixture and returns an `Assertion[]`.
- **Returns** the test result record (same as `test`).

#### `suite`

```lin
val suite = (name: String, tests: Json[]): Json
```

Group test results into a named suite for `report`/`run`.
- **`name`** — the suite name.
- **`tests`** — the array of test result records (from `test`/`withFixture`).
- **Returns** a suite record `{ "name", "tests" }`.

#### `report`

```lin
val report = (s: Json): Int32
```

Print a suite's results and return the failure count. Unlike `run`, does NOT call `exit`, so
statements after it run regardless of outcome — the building block for guaranteed afterAll
teardown: `val failures = report(s); cleanup(); if failures > 0 then exit(1)`.
- **`s`** — the suite from `suite`.
- **Returns** the number of failed tests (0 = all passed). Skipped tests (from `--filter-test`) are
  excluded from both pass and fail counts. When LIN_TEST_JSON is set, the human-readable lines
  are suppressed and one NDJSON record is emitted per test (for `lin test --reporter json`); the
  return value is identical in both modes.

#### `run`

```lin
val run = (s: Json): Null
```

Run a suite: print results and `exit(1)` if any test failed (the common case — a non-zero status
fails CI). For guaranteed post-run teardown even on failure, use `report` instead.
- **`s`** — the suite from `suite`.
