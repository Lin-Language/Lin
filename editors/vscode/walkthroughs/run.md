# Run and test your code

With a `.lin` file open:

- Click the **▶ Run** button in the editor title bar, or run **Lin: Run** from
  the Command Palette, to compile and execute it.
- Use **Lin: Build** to produce a standalone native binary.
- Write tests in `*.test.lin` files using `std/test` and run them from the
  Testing view — each `test("...")` gets a gutter ▶ and a **Run Test** CodeLens.

```lin
import { expect, toBe, test, suite, run } from "std/test"

val s = suite("math", [
  test("adds", () => [
    expect(1 + 1).toBe(2)
  ])
])

run(s)
```
