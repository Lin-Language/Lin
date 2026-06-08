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
const { discoverLine, unescapeLinString, stripLineComment, firstStringArg } = _test;

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

console.log(failures === 0 ? "\nALL PASS" : `\n${failures} FAILURE(S)`);
process.exit(failures === 0 ? 0 : 1);
