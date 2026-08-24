"use strict";

// Standalone unit test for the auto-indent rules shipped in
// `language-configuration.json`. Runs under plain Node
// (`node test/indent.test.js`) with no VS Code host.
//
// The patterns are read out of the real config file (not copied here) so this
// test guards what actually ships. What *is* reimplemented below is the small
// slice of VS Code's `getIndentActionForType` / `getInheritIndentForLine`
// algorithm that decides where a just-typed `else` lands:
//
//   1. On type, if the line's text *newly* matches `decreaseIndentPattern`
//      (it didn't before the keystroke, it does after), VS Code asks
//      `getInheritIndentForLine(..., honorIntentialIndent = false, ...)`.
//   2. That scans *backwards* from the preceding non-blank line. For each line,
//      in order: `shouldIncrease` -> return { indentation: <that line's
//      indent>, action: Indent }; else `shouldIndentNextLine` (Lin defines no
//      `indentNextLinePattern`, so never); else `shouldDecrease` -> return
//      { indentation: <that line's indent>, action: null }. Otherwise keep
//      walking up. Falling off the top yields the first line's indent, null.
//   3. The caller then uses `indentation` as-is when `action === Indent`, and
//      `unshiftIndent(indentation)` otherwise.
//
// Note step 3 is why `then`/`else` must live in `increaseIndentPattern`: a line
// ending in one of them opens a block, so the backward scan has to leave via
// the *Indent* branch and keep the indent as-is. If it leaves via the decrease
// branch instead, the indent gets unshifted and `else` is yanked a level out
// (to column 0 in the common case) — the bug this test pins down.

const assert = require("assert");
const fs = require("fs");
const path = require("path");

const CONFIG_PATH = path.join(__dirname, "..", "language-configuration.json");
const config = JSON.parse(fs.readFileSync(CONFIG_PATH, "utf8"));
const rules = config.indentationRules;

const increase = new RegExp(rules.increaseIndentPattern);
const decrease = new RegExp(rules.decreaseIndentPattern);
const onEnterRules = config.onEnterRules || [];

const INDENT_SIZE = 2;

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

// --- The VS Code algorithm slice ---------------------------------------------

function leadingWhitespace(line) {
  return /^[ \t]*/.exec(line)[0];
}

function unshiftIndent(indentation) {
  return indentation.slice(0, Math.max(0, indentation.length - INDENT_SIZE));
}

function shiftIndent(indentation) {
  return indentation + " ".repeat(INDENT_SIZE);
}

// `getInheritIndentForLine(..., honorIntentialIndent = false, ...)`, reduced to
// the branches Lin's config can reach (no `indentNextLinePattern`, no
// `unIndentedLinePattern`).
function inheritIndent(precedingLines) {
  for (let i = precedingLines.length - 1; i >= 0; i--) {
    const line = precedingLines[i];
    if (line.trim() === "") continue; // blank lines are skipped
    if (increase.test(line)) {
      return { indentation: leadingWhitespace(line), action: "Indent" };
    }
    // `shouldIndentNextLine` would go here; Lin defines no indentNextLinePattern.
    if (decrease.test(line)) {
      return { indentation: leadingWhitespace(line), action: null };
    }
  }
  const first = precedingLines.find((l) => l.trim() !== "");
  return { indentation: first === undefined ? "" : leadingWhitespace(first), action: null };
}

/**
 * Where does the caret line end up, in columns, after typing `typedText`?
 *
 * `lines` is the buffer; its LAST element is the caret line and holds only the
 * whitespace the caret currently sits behind (so a test can vary where the user
 * started typing). Everything before it is context.
 *
 * Returns the resulting indent column. When the keystroke doesn't newly match
 * `decreaseIndentPattern`, VS Code does nothing and the caret keeps its column.
 */
function indentForTypedLine(lines, typedText) {
  const currentIndent = lines[lines.length - 1];
  const before = currentIndent;
  const after = currentIndent + typedText;

  if (!decrease.test(after) || decrease.test(before)) {
    return currentIndent.length; // no on-type reindent fires
  }

  const r = inheritIndent(lines.slice(0, -1));
  const indentation = r.action === "Indent" ? r.indentation : unshiftIndent(r.indentation);
  return indentation.length;
}

// --- The Enter path -----------------------------------------------------------
//
// Pressing Enter is a *different* code path from typing, and the indentation
// patterns alone cannot fix it. `TypeOperations._enter` does:
//
//   1. `getEnterAction` — walk `onEnterRules` in order, matching each rule's
//      `beforeText` against the text left of the caret and `afterText` against
//      the text right of it. The first match wins and SHORT-CIRCUITS step 2; the
//      resulting indent is `getLeadingWhitespace(beforeText)` (plus a shift for
//      `indent: "indent"`).
//   2. `getIndentForEnter` — otherwise, build a virtual model where the caret
//      line is replaced by just the text left of the caret, then run
//      `getInheritIndentForLine(..., honorIntentialIndent = true, ...)` for the
//      line below it. `getPrecedingValidLine` skips blank and whitespace-only
//      lines, so blank-line separation is invisible to this path *by design*.
//
// Step 2 is what makes Enter at column 0 of a top-level line come back indented:
// the scan skips the emptied caret line and the blank line above it, lands on
// the last indented line of the preceding block, and inherits its indent. Only
// an `onEnterRules` entry can intercept that.

// `getInheritIndentForLine` with honorIntentialIndent = true, reduced to the
// branches Lin's config can reach.
function inheritIndentForEnter(virtualLines, lineIndex) {
  // getPrecedingValidLine: nearest preceding line that is neither empty nor
  // whitespace-only.
  let preceding = -1;
  for (let i = lineIndex - 1; i >= 0; i--) {
    const text = virtualLines[i];
    if (text === "" || /^\s+$/.test(text)) continue;
    preceding = i;
    break;
  }
  if (preceding < 0) return { indentation: "", action: null };

  const content = virtualLines[preceding];
  if (increase.test(content)) return { indentation: leadingWhitespace(content), action: "Indent" };
  // `shouldDecrease` and "neither" both yield the line's own indent with no
  // action once honorIntentialIndent is true.
  return { indentation: leadingWhitespace(content), action: null };
}

/**
 * Where does the pushed-down line land, in columns, after pressing Enter with
 * the caret at `column` of `lines[lineIndex]`?
 */
function indentForEnter(lines, lineIndex, column) {
  const currentLine = lines[lineIndex];
  const beforeEnterText = currentLine.slice(0, column);
  const afterEnterText = currentLine.slice(column);
  const previousLineText = lineIndex > 0 ? lines[lineIndex - 1] : "";

  // Stage 1: onEnterRules.
  for (const rule of onEnterRules) {
    if (rule.beforeText && !new RegExp(rule.beforeText).test(beforeEnterText)) continue;
    if (rule.afterText && !new RegExp(rule.afterText).test(afterEnterText)) continue;
    if (rule.previousLineText && !new RegExp(rule.previousLineText).test(previousLineText)) continue;
    const indent = rule.action && rule.action.indent;
    const base = leadingWhitespace(beforeEnterText);
    if (indent === "none") return base.length;
    if (indent === "indent") return shiftIndent(base).length;
    throw new Error(`unmodelled onEnterRule action: ${JSON.stringify(rule.action)}`);
  }

  // Stage 2: getIndentForEnter, over a virtual model whose caret line is
  // truncated to the text left of the caret.
  const virtualLines = lines.slice();
  virtualLines[lineIndex] = beforeEnterText;

  const r = inheritIndentForEnter(virtualLines, lineIndex + 1);
  let afterEnterIndent = r.indentation;
  if (r.action === "Indent") afterEnterIndent = shiftIndent(afterEnterIndent);
  if (decrease.test(afterEnterText)) afterEnterIndent = unshiftIndent(afterEnterIndent);
  return afterEnterIndent.length;
}

// --- The reported bug --------------------------------------------------------

console.log("indentForTypedLine — binary search repro (the reported bug):");

// The `else` under an `else if ... then` must align with the `if` chain at
// indent 2, not be flung out to column 0.
function binarySearchBuffer(caretIndent) {
  return [
    "val find = (xs: Int32[], target: Int32): Int32 =>",
    "  val midIndex = (xs.length() / 2).toInt32()",
    "",
    "  if (xs[midIndex] == target) then",
    "    midIndex",
    "  else if (xs[midIndex] > target) then",
    "    find(xs.slice(midIndex + 1), target)",
    " ".repeat(caretIndent),
  ];
}

check("typing `else` with the caret at indent 2 lands at 2", () => {
  assert.strictEqual(indentForTypedLine(binarySearchBuffer(2), "else"), 2);
});

check("typing `else` with the caret at indent 4 lands at 2", () => {
  assert.strictEqual(indentForTypedLine(binarySearchBuffer(4), "else"), 2);
});

check("typing `else` with the caret at indent 0 lands at 2", () => {
  assert.strictEqual(indentForTypedLine(binarySearchBuffer(0), "else"), 2);
});

console.log("indentForTypedLine — other shapes:");

check("simple two-line if/then: `else` lands at 0", () => {
  const lines = ["if x > 0 then", "  1", "  "];
  assert.strictEqual(indentForTypedLine(lines, "else"), 0);
});

check("`else` of an inner if nested inside an outer else block lands at 4", () => {
  const lines = [
    "val f = (x: Int32): Int32 =>",
    "  if x > 0 then",
    "    1",
    "  else",
    "    if x < -5 then",
    "      2",
    "      ", // caret over-indented at 6
  ];
  assert.strictEqual(indentForTypedLine(lines, "else"), 4);
});

check("`else` closing a lambda-nested if lands at the lambda body level", () => {
  const lines = [
    "val classify = (n: Int32): String =>",
    "  if n == 0 then",
    '    "zero"',
    "        ", // caret wildly over-indented
  ];
  assert.strictEqual(indentForTypedLine(lines, "else"), 2);
});

// (A line-initial `then` is kept in `decreaseIndentPattern` for safety but does
// not occur anywhere in stdlib/ or examples/, so there is no canonical layout to
// pin an on-type column against. Only its pattern-level behaviour is asserted.)

check("typing a non-dedent word leaves the caret column alone", () => {
  const lines = ["if x > 0 then", "  1", "    "];
  assert.strictEqual(indentForTypedLine(lines, "val y = 1"), 4);
});

// --- The Enter-at-column-0 bug -----------------------------------------------

console.log("indentForEnter — top-level Enter repro (the reported bug):");

// Pressing Enter at column 0 of a top-level line must leave that line at
// column 0, not inherit the indent of the block that ended two lines above.
const TOP_LEVEL_BUFFER = [
  "export val solve = (xs: Int32[], target: Int32): Int32 =>",
  "  val midIndex = xs.length() / 2",
  "",
  "  if (xs[midIndex] == target) then",
  "    midIndex",
  "  else if (xs[midIndex] > target) then",
  "    solve(xs.slice(midIndex + 1), target)",
  "  else",
  "    solve(xs.slice(0, midIndex - 1), target)",
  "",
  "val find = () =>",
  "  -1",
];

check("Enter at column 0 of a top-level line keeps it at column 0", () => {
  assert.strictEqual(indentForEnter(TOP_LEVEL_BUFFER, 10, 0), 0);
});

check("Enter at column 0 of the very first line keeps it at column 0", () => {
  assert.strictEqual(indentForEnter(TOP_LEVEL_BUFFER, 0, 0), 0);
});

console.log("indentForEnter — the afterText guard (rule must not over-fire):");

// `afterText: "^\\S"` is what keeps the rule from firing at column 0 of an
// *indented* line, where `beforeText` is also empty but inheriting the block
// indent is the correct behaviour.
check("Enter at column 0 of an indented line still inherits the block indent", () => {
  const lines = ["val f = () =>", "  val a = 1", "  val b = 2"];
  assert.strictEqual(indentForEnter(lines, 2, 0), 2);
});

check("Enter at column 0 of a deeply indented line inherits that depth", () => {
  const lines = ["val f = (x: Int32): Int32 =>", "  if x > 0 then", "    midIndex"];
  assert.strictEqual(indentForEnter(lines, 2, 0), 4);
});

check("Enter at column 0 of a whitespace-only line does not fire the rule", () => {
  // afterText is "    ", which does not match ^\S, so this falls through to the
  // inherit path and is unaffected by the fix.
  const lines = ["val f = () =>", "  val a = 1", "    "];
  assert.strictEqual(indentForEnter(lines, 2, 0), 2);
});

check("Enter at the end of a whitespace-only line inside a block is unchanged", () => {
  // beforeText is "    ", which does not match ^$, so the rule never applies.
  const lines = ["val f = () =>", "  val a = 1", "    "];
  assert.strictEqual(indentForEnter(lines, 2, 4), 2);
});

console.log("indentForEnter — increaseIndentPattern through the Enter path:");

const ENTER_INDENTS = [
  ["a line ending in `=>`", ["val f = (x: Int32): Int32 =>"], 2],
  ["a line ending in `then`", ["val f = () =>", "  if x > 0 then"], 4],
  ["a line ending in `else`", ["val f = () =>", "  else"], 4],
  ["a line-initial `match`", ["val f = () =>", "  match value"], 4],
  ["a nested match arm arrow", ["val f = () =>", "  match value", "    is Error =>"], 6],
];
for (const [label, lines, expected] of ENTER_INDENTS) {
  check(`Enter at the end of ${label} indents one level`, () => {
    const last = lines.length - 1;
    assert.strictEqual(indentForEnter(lines, last, lines[last].length), expected);
  });
}

check("Enter at the end of an ordinary line keeps the current indent", () => {
  const lines = ["val f = (x: Int32): Int32 =>", "  if x > 0 then", "    midIndex"];
  assert.strictEqual(indentForEnter(lines, 2, lines[2].length), 4);
});

// --- The raw patterns on representative lines --------------------------------

console.log("increaseIndentPattern:");

const INCREASE_YES = [
  ["lambda arrow at end of line", "val f = (x: Int32): Int32 =>"],
  ["`then` at end of line", "  if (xs[i] == target) then"],
  ["`else if ... then` at end of line", "  else if (xs[i] > target) then"],
  ["bare `else` at end of line", "  else"],
  ["line-initial `match`", "  match value"],
  ["match arm arrow", "    is Error => "],
];
for (const [label, line] of INCREASE_YES) {
  check(`matches: ${label}`, () => assert.ok(increase.test(line), `expected to match: ${line}`));
}

const INCREASE_NO = [
  ["mid-line lambda arrow", "  xs.map(x => x + 1)"],
  ["inline if/then/else expression", "  val y = if c then a else b"],
  ["`match` not at line start", "  val m = x.match(re)"],
  ["identifier starting with `match`", "  val matches = find(a, b)"],
  ["identifier starting with `else`", "  val elsewhere = 1"],
  ["identifier starting with `then`", "  val thenable = 1"],
  ["ordinary call", '  print("hello")'],
];
for (const [label, line] of INCREASE_NO) {
  check(`does not match: ${label}`, () => assert.ok(!increase.test(line), `expected NOT to match: ${line}`));
}

console.log("decreaseIndentPattern:");

const DECREASE_YES = [
  ["bare `else`", "    else"],
  ["`else if ... then`", "    else if (x > 0) then"],
  ["line-initial `then`", "    then"],
];
for (const [label, line] of DECREASE_YES) {
  check(`matches: ${label}`, () => assert.ok(decrease.test(line), `expected to match: ${line}`));
}

const DECREASE_NO = [
  // The `(?!\s*=>)` guard: a match arm's `else =>` is a sibling of the `is X =>`
  // arms above it and must NOT be dedented by Reindent Lines / format-on-paste.
  // (It cannot help mid-typing — when the final `e` of `else` is typed the `=>`
  // isn't there yet. That is inherent and accepted.)
  ["match arm `else =>`", "    else => 0"],
  ["match arm `else =>` with a block body", "    else =>"],
  ["inline if/then/else expression", "  val y = if c then a else b"],
  ["identifier starting with `else`", "  elsewhere(1)"],
  ["identifier starting with `then`", "  thenable = 1"],
  ["`then` not at line start", "  if c then a"],
];
for (const [label, line] of DECREASE_NO) {
  check(`does not match: ${label}`, () => assert.ok(!decrease.test(line), `expected NOT to match: ${line}`));
}

// --- The two tracked copies of the config must stay byte-identical -----------

console.log("onEnterRules:");

check("the column-0 rule is present and correctly shaped", () => {
  const rule = onEnterRules.find((r) => r.beforeText === "^$");
  assert.ok(rule, "expected an onEnterRule with beforeText ^$");
  // The JSON contribution form is `indent`, not `indentAction` (the API-side
  // name). Getting this wrong makes the rule silently invalid.
  assert.strictEqual(rule.action.indent, "none");
  assert.strictEqual(rule.afterText, "^\\S", "the afterText guard must not be dropped");
});

console.log("config copies:");

check(".vscode/lin-lang/language-configuration.json is byte-identical", () => {
  const tracked = path.join(__dirname, "..", "..", "..", ".vscode", "lin-lang", "language-configuration.json");
  if (!fs.existsSync(tracked)) return; // not present in a packaged extension
  assert.strictEqual(fs.readFileSync(tracked, "utf8"), fs.readFileSync(CONFIG_PATH, "utf8"));
});

console.log(failures === 0 ? "\nALL PASS" : `\n${failures} FAILURE(S)`);
process.exit(failures === 0 ? 0 : 1);
