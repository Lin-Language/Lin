"use strict";

// Standalone unit test for the pure discovery/unescape helpers in client.js.
// Runs under plain Node (`node test/discovery.test.js`) with no VS Code host —
// we stub the `vscode` module in the require cache before loading client.js so
// its top-level `require("vscode")` resolves. This is our best validation of the
// hardened test-discovery regex + unescapeLinString without a live editor.

const Module = require("module");
const assert = require("assert");
const path = require("path");

// --- Stub host-provided modules so requiring client.js doesn't throw ----------
// client.js requires `vscode` (injected by the host at runtime) and
// `vscode-languageclient/node` (a bundled dependency). Neither is needed to
// exercise the pure helpers, and node_modules may be absent, so we return a Proxy
// that satisfies any destructure/`new`/property access at load time.
const STUBBED = new Set(["vscode", "vscode-languageclient/node"]);
function makeStub() {
  const Cls = class {};
  return new Proxy(Cls, { get: () => Cls });
}
const origLoad = Module._load;
Module._load = function (request, ...rest) {
  if (STUBBED.has(request)) return makeStub();
  return origLoad.call(this, request, ...rest);
};

const { _test } = require(path.join(__dirname, "..", "client.js"));
const { discoverLine, unescapeLinString, stripLineComment, firstStringArg, discoverFileStructure } = _test;

let failures = 0;
function check(label, fn) {
  try {
    fn();
    console.log(`  ok   ${label}`);
  } catch (err) {
    failures++;
    console.log(`  FAIL ${label}: ${err.message}`);
  }
}

console.log("unescapeLinString:");

check("basic escapes \\n \\t \\r \\\" \\\\", () => {
  assert.strictEqual(unescapeLinString("a\\nb\\tc\\rd\\\"e\\\\f"), 'a\nb\tc\rd"e\\f');
});

check("\\$ decodes to $", () => {
  assert.strictEqual(unescapeLinString("price\\$5"), "price$5");
});

check("\\0 decodes to NUL", () => {
  assert.strictEqual(unescapeLinString("a\\0b"), "a\0b");
});

check("\\u{...} unicode escape decodes (snowman)", () => {
  assert.strictEqual(unescapeLinString("snow \\u{2603} man"), "snow ☃ man");
});

check("\\u{...} astral plane (emoji)", () => {
  assert.strictEqual(unescapeLinString("\\u{1F600}!"), "\u{1F600}!");
});

check("does not re-decode an already-decoded backslash (\\\\n stays literal)", () => {
  // Source `\\n` is an escaped backslash followed by literal n — NOT a newline.
  assert.strictEqual(unescapeLinString("x\\\\n"), "x\\n");
});

check("unknown escape \\q decodes to q", () => {
  assert.strictEqual(unescapeLinString("a\\qb"), "aqb");
});

check("malformed \\u{ZZ} drops the char (matches lexer)", () => {
  assert.strictEqual(unescapeLinString("a\\u{ZZ}b"), "ab");
});

console.log("stripLineComment:");

check("removes a trailing line comment", () => {
  assert.strictEqual(stripLineComment('test("x") // hi').trimEnd(), 'test("x")');
});

check("keeps // inside a string literal", () => {
  assert.strictEqual(stripLineComment('val u = "http://x"'), 'val u = "http://x"');
});

console.log("firstStringArg:");

check("finds first string after a brace-comma fixture object", () => {
  const line = '  withFixture({ a, b }, "the name", body)';
  const at = line.indexOf("(", line.indexOf("withFixture")) + 1;
  const r = firstStringArg(line, at);
  assert.ok(r, "expected a match");
  assert.strictEqual(r.raw, "the name");
});

check("does not pick a string nested inside the fixture object", () => {
  const line = 'withFixture({ "key": "ignored" }, "real", body)';
  const at = line.indexOf("(", line.indexOf("withFixture")) + 1;
  const r = firstStringArg(line, at);
  assert.ok(r);
  assert.strictEqual(r.raw, "real");
});

console.log("discoverLine:");

check("discovers a plain test declaration", () => {
  const r = discoverLine('  test("adds two numbers", () => [])');
  assert.deepStrictEqual(r.map((x) => x.name), ["adds two numbers"]);
});

check("ignores a commented-out test declaration", () => {
  const r = discoverLine('  // test("not a real test")');
  assert.deepStrictEqual(r, []);
});

check("ignores test( inside a string literal", () => {
  const r = discoverLine('val s = "this has test(\\"x\\") inside"');
  assert.deepStrictEqual(r, []);
});

check("discovers withFixture with a brace-comma fixture arg", () => {
  const r = discoverLine('  withFixture({ a, b }, "fixture test", (f) => [])');
  assert.deepStrictEqual(r.map((x) => x.name), ["fixture test"]);
});

check("discovers a test whose name uses a \\u{...} escape", () => {
  const r = discoverLine('  test("emoji \\u{1F600} test", () => [])');
  assert.deepStrictEqual(r.map((x) => x.name), ["emoji \u{1F600} test"]);
  // This is the decoded form the runtime emits, so --filter-test will match it.
});

check("discovers two tests on one line, in order", () => {
  const r = discoverLine('test("a", () => []) ; test("b", () => [])');
  assert.deepStrictEqual(r.map((x) => x.name), ["a", "b"]);
});

check("trailing comment after a real test still discovers the test", () => {
  const r = discoverLine('test("kept", () => []) // ran this');
  assert.deepStrictEqual(r.map((x) => x.name), ["kept"]);
});

console.log("discoverFileStructure:");

check("file with one suite wrapping tests → one suite group, no loose tests", () => {
  const text = [
    'import { suite, test } from "std/test"',
    'val s = suite("MyGroup", [',
    '  test("alpha", () => [])',
    '  test("beta", () => [])',
    '])',
  ].join("\n");
  const result = discoverFileStructure(text);
  assert.strictEqual(result.suites.length, 1, `expected 1 suite, got ${result.suites.length}`);
  assert.strictEqual(result.suites[0].name, "MyGroup");
  assert.deepStrictEqual(result.suites[0].tests.map((t) => t.name), ["alpha", "beta"]);
  assert.strictEqual(result.looseTests.length, 0, `expected 0 loose tests, got ${result.looseTests.length}`);
});

check("file mixing a suite + a loose top-level test", () => {
  const text = [
    'val s = suite("Suite1", [',
    '  test("inside", () => [])',
    '])',
    'val t = test("outside", () => [])',
  ].join("\n");
  const result = discoverFileStructure(text);
  assert.strictEqual(result.suites.length, 1);
  assert.strictEqual(result.suites[0].name, "Suite1");
  assert.deepStrictEqual(result.suites[0].tests.map((t) => t.name), ["inside"]);
  assert.strictEqual(result.looseTests.length, 1);
  assert.strictEqual(result.looseTests[0].name, "outside");
});

check("sequential suites each collect their own tests", () => {
  const text = [
    'val a = suite("A", [',
    '  test("a1", () => [])',
    '  test("a2", () => [])',
    '])',
    'val b = suite("B", [',
    '  test("b1", () => [])',
    '])',
  ].join("\n");
  const result = discoverFileStructure(text);
  assert.strictEqual(result.suites.length, 2, `expected 2 suites, got ${result.suites.length}`);
  const byName = Object.fromEntries(result.suites.map((s) => [s.name, s]));
  assert.deepStrictEqual(byName["A"].tests.map((t) => t.name), ["a1", "a2"]);
  assert.deepStrictEqual(byName["B"].tests.map((t) => t.name), ["b1"]);
  assert.strictEqual(result.looseTests.length, 0);
});

check("service.test.lin structure: 1 suite 'Service' with 7 tests, 0 loose", () => {
  // Inline the service.test.lin content (abbreviated) — just enough to verify structure parsing.
  const text = [
    'import { expect, toBe, test, suite, run } from "std/test"',
    'val s = suite("Service", [',
    '  test("checks the start date", () => [])',
    '  test("checks the end date", () => [])',
    '  test("checks dates within range", () => [])',
    '  test("checks the day of the week (DayOfWeek integer literal union key)", () => [])',
    '  test("checks Sunday runs when all days active", () => [])',
    '  test("checks include dates override", () => [])',
    '  test("checks exclude dates override", () => [])',
    '])',
    'run(s)',
  ].join("\n");
  const result = discoverFileStructure(text);
  assert.strictEqual(result.suites.length, 1, `expected 1 suite, got ${result.suites.length}`);
  assert.strictEqual(result.suites[0].name, "Service");
  assert.strictEqual(result.suites[0].tests.length, 7, `expected 7 tests, got ${result.suites[0].tests.length}`);
  assert.strictEqual(result.looseTests.length, 0);
});

// --- findDescendantById: must find a test nested under a suite group ----------
// A minimal TestItem/collection fake mirroring the bits findDescendantById touches:
// `children.get(id)` (direct only) and `children.forEach(cb)`.
console.log("findDescendantById:");
function fakeItem(id) {
  const map = new Map();
  return {
    id,
    children: {
      get: (k) => map.get(k),
      forEach: (cb) => map.forEach((v) => cb(v)),
      add: (it) => map.set(it.id, it),
    },
  };
}

check("finds a test nested two levels deep (file → suite → test)", () => {
  const { findDescendantById } = _test;
  const file = fakeItem("/f.test.lin");
  const suite = fakeItem("/f.test.lin::suite::S");
  const test = fakeItem("/f.test.lin::checks a thing");
  suite.children.add(test);
  file.children.add(suite);
  const found = findDescendantById(file, "/f.test.lin::checks a thing");
  assert.ok(found, "should find the under-suite test");
  assert.strictEqual(found.id, "/f.test.lin::checks a thing");
});

check("returns undefined for an unknown id (so caller creates it)", () => {
  const { findDescendantById } = _test;
  const file = fakeItem("/f.test.lin");
  const suite = fakeItem("/f.test.lin::suite::S");
  file.children.add(suite);
  assert.strictEqual(findDescendantById(file, "/f.test.lin::nope"), undefined);
});

console.log(failures === 0 ? "\nALL PASS" : `\n${failures} FAILURE(S)`);
process.exit(failures === 0 ? 0 : 1);
