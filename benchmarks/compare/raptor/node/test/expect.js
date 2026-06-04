import assert from "node:assert";
import { deepEqual } from "./util.js";

/**
 * Minimal vitest-like `expect` over node:assert, so spec files can mirror the reference verbatim.
 */
export function expect(actual) {
  return {
    toBe(expected) {
      assert.strictEqual(actual, expected);
    },
    toEqual(expected) {
      assert.ok(
        deepEqual(actual, expected),
        `deep equality failed:\nactual:   ${stringify(actual)}\nexpected: ${stringify(expected)}`
      );
    }
  };
}

function stringify(value) {
  return JSON.stringify(value, (_key, v) => (v instanceof Set ? [...v] : v));
}
