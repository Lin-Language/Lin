"use strict";

const fs = require("fs");
const path = require("path");
const os = require("os");
const cp = require("child_process");
const {
  workspace, window, commands, Range, Uri,
  tests, TestRunRequest, TestRunProfileKind, TestMessage, Position,
  FileCoverage, StatementCoverage, CancellationTokenSource,
  tasks, Task, ShellExecution, ShellQuoting, TaskScope,
  debug,
} = require("vscode");
const { LanguageClient, TransportKind } = require("vscode-languageclient/node");

let client;

function getPlatformDir() {
  const os = process.platform;
  const arch = process.arch;
  if (os === "win32") return "win32-x64";
  if (os === "darwin") return arch === "arm64" ? "darwin-arm64" : "darwin-x64";
  return "linux-x64";
}

function exeName(name) {
  return process.platform === "win32" ? `${name}.exe` : name;
}

// Quote a single argument for an interactive terminal's shell so paths containing
// spaces or quotes survive intact. The Task provider uses ShellQuoting.Strong (VS
// Code quotes per-shell), but terminal.sendText() takes a raw command line, so we
// quote by hand here. POSIX shells: wrap in single quotes and escape any embedded
// single quote as '\''. Windows: the integrated terminal is usually PowerShell,
// where a literal " inside an arg is doubled ("") within a double-quoted string.
function shellQuote(arg) {
  if (process.platform === "win32") {
    return `"${String(arg).replace(/"/g, '""')}"`;
  }
  return `'${String(arg).replace(/'/g, "'\\''")}'`;
}

// Directory holding the bundled, co-located binaries (lin, lin-lsp,
// liblin_runtime.a). `lin build` finds liblin_runtime.a next to the `lin`
// executable, so this directory is what we expose on PATH. Returns null when
// running from a workspace build (no bundled bin/).
function bundledBinDir(context) {
  const dir = path.join(context.extensionPath, "bin", getPlatformDir());
  return fs.existsSync(path.join(dir, exeName("lin"))) ? dir : null;
}

function resolveBin(context, name) {
  const exe = exeName(name);

  // 1. Bundled binary (production VSIX install)
  const bundled = path.join(context.extensionPath, "bin", getPlatformDir(), exe);
  if (fs.existsSync(bundled)) return bundled;

  // 2. Workspace build (contributor workflow)
  const wsFolders = workspace.workspaceFolders;
  if (wsFolders) {
    const dev = path.join(wsFolders[0].uri.fsPath, "target", "debug", exe);
    if (fs.existsSync(dev)) return dev;
  }

  // 3. PATH fallback
  return name;
}

// Make `lin` available in VS Code's integrated terminal automatically. This is
// scoped to terminals VS Code spawns, applied on every activation (so it always
// points at the current version), and reverted by VS Code on uninstall/disable.
function addToIntegratedTerminalPath(context) {
  const binDir = bundledBinDir(context);
  if (!binDir) return; // workspace build: nothing bundled to expose
  const col = context.environmentVariableCollection;
  col.description = "Adds the bundled `lin` compiler to PATH";
  col.prepend("PATH", binDir + path.delimiter);
}

// Opt-in: symlink the bundled `lin` into a user-owned PATH directory so it works
// in any external shell, not just VS Code's terminal. Symlink resolution means
// liblin_runtime.a is still found beside the real binary.
async function installOnPath(context) {
  if (process.platform === "win32") {
    window.showWarningMessage(
      "Lin: 'Install on PATH' isn't supported on Windows yet. " +
      "Add this folder to PATH manually: " + (bundledBinDir(context) || "")
    );
    return;
  }
  const binDir = bundledBinDir(context);
  if (!binDir) {
    window.showWarningMessage(
      "Lin: no bundled compiler found (running from a workspace build?). " +
      "Nothing to install."
    );
    return;
  }

  const targetDir = path.join(process.env.HOME || "", ".local", "bin");
  const linkPath = path.join(targetDir, "lin");
  const realBin = path.join(binDir, "lin");

  try {
    fs.mkdirSync(targetDir, { recursive: true });
    try { fs.unlinkSync(linkPath); } catch (_) { /* no existing link */ }
    fs.symlinkSync(realBin, linkPath);
  } catch (err) {
    window.showErrorMessage(`Lin: failed to create symlink at ${linkPath}: ${err.message}`);
    return;
  }

  const onPath = (process.env.PATH || "")
    .split(path.delimiter)
    .includes(targetDir);
  if (onPath) {
    window.showInformationMessage(`Lin: \`lin\` installed at ${linkPath} — available in any terminal.`);
  } else {
    window.showInformationMessage(
      `Lin: \`lin\` installed at ${linkPath}, but ${targetDir} is not on your PATH. ` +
      `Add this to your shell profile:  export PATH="${targetDir}:$PATH"`
    );
  }
}

// Document formatting is provided exclusively by the LSP (lin-lsp advertises
// `document_formatting_provider`, and vscode-languageclient auto-registers it).
// We intentionally do NOT register a second formatter here — that previously
// caused VSCode's "multiple formatters configured" warning.

// --- Test Explorer integration (VSCode Testing API) ---------------------------
//
// Discovery is best-effort regex over *.test.lin: we never compile to enumerate
// tests. Running shells out to `lin test ... --reporter json`, which emits NDJSON
// (one record per test + one per file) on stdout; we stream it and map records
// back onto TestItems. Tests with interpolated/dynamic names aren't discovered
// statically — they're materialized on the fly from the `test` records at run time.

// Matches `test("name"` and `withFixture(..., "name",` style declarations. We only
// need the literal-string name; dynamic names simply won't match (handled at run time).
// The NDJSON schema version this extension was written against. The CLI emits a leading
// `{"event":"meta","schema":N}` record; if N is newer than this we warn (once) that the
// extension may not understand the stream, but keep parsing best-effort.
const SUPPORTED_SCHEMA = 2;

const TEST_DECL_RE = /\btest\s*\(\s*"((?:[^"\\]|\\.)*)"/g;
const WITHFIXTURE_DECL_RE = /\bwithFixture\s*\(/g;

// Strip an unquoted `//` line-comment from a single source line. We walk the line
// tracking whether we're inside a double-quoted string literal (respecting `\`
// escapes) so a `//` *inside* a string is preserved, and only a genuine comment
// is removed. Lin has no block comments (see the lexer), so this is sufficient.
//
// Residual limit: this is line-based, so it cannot see a string literal that opens
// on one line and closes on another. The Lin lexer does allow a string to span
// lines, so a `//` or `test(` on a *continuation* line of such a literal could be
// mis-handled. Multi-line string literals that contain the literal text
// `test("...")` are rare enough that a full lexer isn't worth it — discovery is
// best-effort, and any miss is corrected at run time from the NDJSON `test` records.
function stripLineComment(line) {
  let inStr = false;
  for (let i = 0; i < line.length; i++) {
    const ch = line[i];
    if (inStr) {
      if (ch === "\\") { i++; continue; } // skip escaped char
      if (ch === '"') inStr = false;
    } else if (ch === '"') {
      inStr = true;
    } else if (ch === "/" && line[i + 1] === "/") {
      return line.slice(0, i);
    }
  }
  return line;
}

// True if character offset `idx` in `line` lies inside a double-quoted string
// literal — i.e. a `test(`/`withFixture(` token there is text, not a declaration.
function isInsideString(line, idx) {
  let inStr = false;
  for (let i = 0; i < idx && i < line.length; i++) {
    const ch = line[i];
    if (inStr) {
      if (ch === "\\") { i++; continue; }
      if (ch === '"') inStr = false;
    } else if (ch === '"') {
      inStr = true;
    }
  }
  return inStr;
}

// Extract the first top-level double-quoted string-literal argument of a call,
// scanning from `from` (the index just after the opening `(`). Used for
// withFixture, whose test name is the first string-literal argument — robust to
// earlier arguments that contain commas inside braces/brackets/quotes (e.g. a
// `{ a, b }` fixture object). Skips balanced `{}`/`[]`/`()` groups so a `"name"`
// nested inside an object isn't mistaken for the test name, and stops if the call
// closes before any string is seen. Returns { raw, index } or null (raw is the
// still-escaped literal body; index is the offset of its opening quote).
function firstStringArg(line, from) {
  let depth = 0;
  for (let i = from; i < line.length; i++) {
    const ch = line[i];
    if (ch === "{" || ch === "[" || ch === "(") {
      depth++;
    } else if (ch === "}" || ch === "]" || ch === ")") {
      if (depth === 0) return null; // call closed before any string argument
      depth--;
    } else if (ch === '"' && depth === 0) {
      let raw = "";
      let j = i + 1;
      for (; j < line.length; j++) {
        if (line[j] === "\\") { raw += line[j] + (line[j + 1] || ""); j++; continue; }
        if (line[j] === '"') break;
        raw += line[j];
      }
      return { raw, index: i };
    }
  }
  return null;
}

function unescapeLinString(s) {
  // Inverse of the Lin lexer's string escapes (crates/lin-lex/src/lexer.rs
  // `lex_string`) so the discovered id matches the runtime `name` (the decoded
  // string). The lexer supports: \n \r \t \0 \" \\ \$ \u{HEX}; any other \<c>
  // decodes to the literal <c>. We mirror that exact set. Driven by a single
  // left-to-right scan so an already-decoded backslash is never re-processed (a
  // chain of .replace() would, e.g., turn the source "\\n" into a newline).
  let out = "";
  for (let i = 0; i < s.length; i++) {
    if (s[i] !== "\\") { out += s[i]; continue; }
    const c = s[i + 1];
    if (c === undefined) { out += "\\"; break; } // trailing backslash, leave as-is
    switch (c) {
      case "n": out += "\n"; i++; break;
      case "r": out += "\r"; i++; break;
      case "t": out += "\t"; i++; break;
      case "0": out += "\0"; i++; break;
      case '"': out += '"'; i++; break;
      case "\\": out += "\\"; i++; break;
      case "$": out += "$"; i++; break;
      case "u": {
        // \u{HEX} (1-6 hex digits). The lexer reads to the closing `}`; on a
        // valid code point it pushes the char, on a malformed one it pushes
        // nothing but still consumes the braces. Mirror both behaviours.
        if (s[i + 2] === "{") {
          const close = s.indexOf("}", i + 3);
          if (close !== -1) {
            const hex = s.slice(i + 3, close);
            const code = parseInt(hex, 16);
            if (/^[0-9a-fA-F]+$/.test(hex) && Number.isFinite(code) && code <= 0x10ffff) {
              try { out += String.fromCodePoint(code); } catch (_) { /* invalid: drop */ }
            }
            i = close;
            break;
          }
        }
        // No `{...}` — lexer's `continue` leaves following chars in place; emit nothing.
        i++;
        break;
      }
      default: out += c; i++; break; // \<c> → <c>
    }
  }
  return out;
}

function testItemId(filePath, name) {
  return `${filePath}::${name}`;
}

// Discover every `test(...)` / `withFixture(...)` declaration on a single source
// line, returning [{ name, col }] (name decoded, col = the declaration's start).
// Comments are stripped and matches that begin inside a string literal are skipped,
// so `// test("x")` and a string containing `test("y")` don't produce phantom items.
function discoverLine(line) {
  const code = stripLineComment(line);
  const found = [];

  // `test("name"` — the captured group is the (still-escaped) literal name. We
  // only accept the match if the `test` keyword itself is not inside a string.
  TEST_DECL_RE.lastIndex = 0;
  let m;
  while ((m = TEST_DECL_RE.exec(code)) !== null) {
    if (isInsideString(code, m.index)) continue;
    found.push({ name: unescapeLinString(m[1]), col: m.index });
  }

  // `withFixture(...)` — the name is the FIRST top-level string-literal argument,
  // which is robust to a fixture object/array containing commas. We anchor on the
  // call keyword (not a fixed two-comma shape) and then scan its arguments.
  WITHFIXTURE_DECL_RE.lastIndex = 0;
  while ((m = WITHFIXTURE_DECL_RE.exec(code)) !== null) {
    if (isInsideString(code, m.index)) continue;
    const arg = firstStringArg(code, m.index + m[0].length);
    if (arg) found.push({ name: unescapeLinString(arg.raw), col: m.index });
  }

  return found;
}

// (Re)build a file's child TestItems from its current text.
function refreshFileTests(controller, fileItem, text) {
  fileItem.children.replace([]);
  const seen = new Set();
  const lines = text.split("\n");
  for (let i = 0; i < lines.length; i++) {
    for (const { name, col } of discoverLine(lines[i])) {
      const id = testItemId(fileItem.uri.fsPath, name);
      if (seen.has(id)) continue;
      seen.add(id);
      const child = controller.createTestItem(id, name, fileItem.uri);
      const startCol = col >= 0 ? col : 0;
      child.range = new Range(new Position(i, startCol), new Position(i, startCol + name.length));
      fileItem.children.add(child);
    }
  }
}

function getOrCreateFileItem(controller, uri) {
  const id = uri.fsPath;
  let item = controller.items.get(id);
  if (!item) {
    item = controller.createTestItem(id, path.basename(uri.fsPath), uri);
    item.canResolveChildren = true;
    controller.items.add(item);
  }
  return item;
}

async function discoverAllTestFiles(controller) {
  const files = await workspace.findFiles("**/*.test.lin");
  for (const uri of files) {
    getOrCreateFileItem(controller, uri);
  }
}

async function resolveFileChildren(controller, fileItem) {
  try {
    const doc = await workspace.openTextDocument(fileItem.uri);
    refreshFileTests(controller, fileItem, doc.getText());
  } catch (_) {
    // Unreadable file: leave it childless (best-effort discovery).
  }
}

// Map a requested set of TestItems to the file paths we should pass to `lin test`.
// A child item resolves to its parent file. An undefined include means "everything".
function collectTargetFiles(controller, request) {
  const files = new Set();
  const items = [];
  if (request.include && request.include.length > 0) {
    for (const it of request.include) items.push(it);
  } else {
    controller.items.forEach((it) => items.push(it));
  }
  for (const it of items) {
    // A test child has id `<file>::<name>`; a file item's id IS the fsPath.
    const fsPath = it.parent ? it.parent.uri.fsPath : it.uri.fsPath;
    files.add(fsPath);
  }
  return [...files];
}

// Build the `lin test` argument vector for a run/coverage request. Whole-project runs (no
// include) pass the workspace root so the CLI also discovers files we never enumerated. When
// the request targets INDIVIDUAL test items (gutter arrow on a single test), we additionally
// pass `--filter-test "<name>"` per selected child so the CLI runs only those tests, not the
// whole file. File-level selections (and a child mixed with its own file) run the whole file.
function buildTestArgs(request, targetFiles, wsRoot) {
  const wholeProject = !(request.include && request.include.length > 0);
  const args = ["test"];
  if (wholeProject && wsRoot) {
    args.push(wsRoot);
  } else {
    args.push(...targetFiles);
  }
  args.push("--reporter", "json");

  if (!wholeProject) {
    // Files explicitly included as whole files — don't narrow those by name.
    const wholeFiles = new Set();
    for (const it of request.include) {
      if (!it.parent) wholeFiles.add(it.uri.fsPath);
    }
    // Child test items whose file isn't being run wholesale → filter by name.
    for (const it of request.include) {
      if (it.parent && !wholeFiles.has(it.parent.uri.fsPath)) {
        args.push("--filter-test", it.label);
      }
    }
  }
  return args;
}

// Find the TestItem for a `<file>::<name>` pair, creating it under the file item
// if it wasn't statically discovered (e.g. an interpolated/dynamic test name).
function findOrCreateTestItem(controller, file, name) {
  const fileUri = Uri.file(file);
  const fileItem = getOrCreateFileItem(controller, fileUri);
  const id = testItemId(file, name);
  let child = fileItem.children.get(id);
  if (!child) {
    child = controller.createTestItem(id, name, fileUri);
    fileItem.children.add(child);
  }
  return child;
}

// Render a structured expected/actual JSON value for TestMessage.expectedOutput /
// actualOutput, which require strings. Strings pass through verbatim (so a quoted value isn't
// double-quoted); everything else (objects/arrays/numbers/booleans/null) is pretty-printed with
// 2-space indent for a readable diff.
function toDiffString(value) {
  if (typeof value === "string") return value;
  try {
    return JSON.stringify(value, null, 2);
  } catch (_) {
    return String(value);
  }
}

// Split a `toBe`-style failure message into expected/actual for a richer diff in
// the Test Explorer. The runner emits `expected: X\n    actual:   Y`.
function parseExpectedActual(message) {
  const expMatch = message.match(/expected:\s*([\s\S]*?)(?:\n\s*actual:|$)/);
  const actMatch = message.match(/actual:\s*([\s\S]*)$/);
  if (expMatch && actMatch) {
    return { expected: expMatch[1].trim(), actual: actMatch[1].trim() };
  }
  return null;
}

// Minimal lcov parser. lcov groups per-file sections:
//   SF:<path>            begins a file section
//   DA:<line>,<hits>     one record per instrumented line (hits = execution count)
//   end_of_record        ends the section
// Returns an array of { file, lines: [{ line, hits }] }.
function parseLcov(text) {
  const files = [];
  let current = null;
  for (const raw of text.split("\n")) {
    const line = raw.trim();
    if (line.startsWith("SF:")) {
      current = { file: line.slice(3), lines: [] };
    } else if (line.startsWith("DA:") && current) {
      const [lineNoStr, hitsStr] = line.slice(3).split(",");
      const lineNo = parseInt(lineNoStr, 10);
      const hits = parseInt(hitsStr, 10);
      if (Number.isFinite(lineNo) && Number.isFinite(hits)) {
        current.lines.push({ line: lineNo, hits });
      }
    } else if (line === "end_of_record" && current) {
      files.push(current);
      current = null;
    }
  }
  if (current) files.push(current);
  return files;
}

// Per-run cache mapping a file uri string → its parsed line records, so the profile's
// loadDetailedCoverage resolver can materialize StatementCoverage on demand.
const coverageDetailCache = new WeakMap();

// Parse an lcov file and attach a FileCoverage per source file to the TestRun. The detailed
// per-line StatementCoverage is materialized lazily via the profile's loadDetailedCoverage.
function attachLcovCoverage(run, lcovFile) {
  let text;
  try {
    text = fs.readFileSync(lcovFile, "utf8");
  } catch (_) {
    return; // No lcov produced (e.g. llvm tools missing) — best effort.
  }
  const detail = new Map();
  for (const fileCov of parseLcov(text)) {
    const uri = Uri.file(fileCov.file);
    const total = fileCov.lines.length;
    const covered = fileCov.lines.filter((l) => l.hits > 0).length;
    const fc = new FileCoverage(uri, { covered, total });
    detail.set(uri.toString(), fileCov.lines);
    run.addCoverage(fc);
  }
  coverageDetailCache.set(run, detail);
}

function setupTestController(context, linBin) {
  const controller = tests.createTestController("lin", "Lin Tests");
  context.subscriptions.push(controller);

  controller.resolveHandler = async (item) => {
    if (!item) {
      await discoverAllTestFiles(controller);
    } else {
      await resolveFileChildren(controller, item);
    }
  };

  // Keep discovery fresh as files change / open / get created.
  const watcher = workspace.createFileSystemWatcher("**/*.test.lin");
  context.subscriptions.push(watcher);
  watcher.onDidCreate((uri) => getOrCreateFileItem(controller, uri));
  watcher.onDidChange((uri) => {
    const item = controller.items.get(uri.fsPath);
    if (item) resolveFileChildren(controller, item);
  });
  watcher.onDidDelete((uri) => controller.items.delete(uri.fsPath));
  context.subscriptions.push(
    workspace.onDidOpenTextDocument((doc) => {
      if (doc.uri.fsPath.endsWith(".test.lin")) {
        const item = getOrCreateFileItem(controller, doc.uri);
        refreshFileTests(controller, item, doc.getText());
      }
    })
  );

  // Kick off initial discovery (don't block activation on it).
  discoverAllTestFiles(controller);

  // Shared run/coverage session. Spawns `lin test ... --reporter json` (plus coverage flags
  // when `opts.coverage` is set), streams NDJSON, and maps records onto TestItems. On close, if
  // a coverage lcov file was requested, parses it and attaches coverage to the TestRun.
  const runTestSession = (request, token, opts, onDone) => {
    opts = opts || {};
    const run = controller.createTestRun(request);
    const targetFiles = collectTargetFiles(controller, request);

    // Mark requested items enqueued so the UI shows them as pending.
    const requested = [];
    if (request.include && request.include.length > 0) {
      request.include.forEach((it) => requested.push(it));
    } else {
      controller.items.forEach((it) => requested.push(it));
    }
    for (const it of requested) {
      run.enqueued(it);
      it.children.forEach((c) => run.enqueued(c));
    }

    // Whole-project run: pass the workspace root rather than enumerating files,
    // so `lin test` also picks up files we never discovered.
    const wsRoot = workspace.workspaceFolders && workspace.workspaceFolders[0]
      ? workspace.workspaceFolders[0].uri.fsPath
      : undefined;
    const args = buildTestArgs(request, targetFiles, wsRoot);

    // For a coverage run, write lcov to an OS temp file and ask the CLI to emit it.
    let lcovFile;
    if (opts.coverage) {
      lcovFile = path.join(os.tmpdir(), `lin-cov-${Date.now()}-${Math.random().toString(36).slice(2)}.lcov`);
      args.push("--coverage", "--format", "llvm-cov", "--output", lcovFile);
    }

    // End the run exactly once and notify the caller (palette runs use this to dispose their
    // CancellationTokenSource). Guarded so multiple termination paths (spawn error, child error,
    // normal close) don't double-end.
    let finished = false;
    const finish = () => {
      if (finished) return;
      finished = true;
      run.end();
      if (typeof onDone === "function") onDone();
    };

    let child;
    try {
      child = cp.spawn(linBin, args, { cwd: wsRoot });
    } catch (err) {
      window.showErrorMessage(`Lin: failed to start test runner: ${err.message}`);
      finish();
      return;
    }

    child.on("error", (err) => {
      window.showErrorMessage(`Lin: test runner error: ${err.message}`);
      finish();
    });

    token.onCancellationRequested(() => {
      try { child.kill(); } catch (_) { /* already gone */ }
    });

    let buffer = "";
    let warnedSchema = false;
    const handleRecord = (rec) => {
      if (rec.event === "meta") {
        if (typeof rec.schema === "number" && rec.schema > SUPPORTED_SCHEMA && !warnedSchema) {
          warnedSchema = true;
          window.showWarningMessage(
            `Lin: test reporter schema v${rec.schema} is newer than this extension supports ` +
            `(v${SUPPORTED_SCHEMA}). Update the Lin extension; results may be incomplete.`
          );
        }
      } else if (rec.event === "output") {
        // User `print(...)` output forwarded by the CLI. VSCode's appendOutput requires CRLF
        // line endings, so normalize "\n" → "\r\n". A trailing CRLF separates it from later lines.
        if (typeof rec.text === "string" && rec.text.length > 0) {
          const header = rec.file ? `--- ${rec.file} (stdout) ---\r\n` : "";
          run.appendOutput(header + rec.text.replace(/\r?\n/g, "\r\n") + "\r\n");
        }
      } else if (rec.event === "test") {
        const item = findOrCreateTestItem(controller, rec.file, rec.name);
        run.started(item);
        const durationMs = typeof rec.durationMs === "number" ? rec.durationMs : undefined;
        // Always record a per-test summary line in the output tab (pass AND fail) so the run
        // produces output even without any user prints — this is what stops VSCode showing
        // "Test run did not record any output". CRLF-terminated as appendOutput requires.
        const mark = rec.status === "pass" ? "✓" : "✗";
        const durSuffix = typeof durationMs === "number" ? ` (${durationMs}ms)` : "";
        run.appendOutput(`${mark} ${rec.name}${durSuffix}\r\n`, undefined, item);
        if (rec.status === "pass") {
          run.passed(item, durationMs);
        } else {
          const msg = rec.message || "test failed";
          // Surface the failure message (indented) in the output tab too.
          run.appendOutput(`    ${msg.replace(/\r?\n/g, "\r\n    ")}\r\n`, undefined, item);
          const tm = new TestMessage(msg);
          // Prefer the STRUCTURED expected/actual the runner now attaches (equality-style
          // failures). They may be any JSON shape, but TestMessage wants strings, so non-strings
          // are pretty-printed. Fall back to regex-scraping the human message only when the
          // structured fields are absent (older runner / matchers without a pair).
          if (rec.expected !== undefined || rec.actual !== undefined) {
            tm.expectedOutput = toDiffString(rec.expected);
            tm.actualOutput = toDiffString(rec.actual);
          } else {
            const ea = parseExpectedActual(msg);
            if (ea) {
              tm.expectedOutput = ea.expected;
              tm.actualOutput = ea.actual;
            }
          }
          run.failed(item, tm, durationMs);
        }
      } else if (rec.event === "file") {
        if (rec.status === "compile_error" || rec.status === "timeout") {
          const fileItem = getOrCreateFileItem(controller, Uri.file(rec.file));
          const tm = new TestMessage(rec.message || rec.status);
          // Mark the file item and any known children as errored so the failure
          // is visible even when no per-test records were produced.
          run.errored(fileItem, tm);
          fileItem.children.forEach((c) => run.errored(c, tm));
        }
      }
    };

    child.stdout.on("data", (chunk) => {
      buffer += chunk.toString();
      let nl;
      while ((nl = buffer.indexOf("\n")) !== -1) {
        const line = buffer.slice(0, nl).trim();
        buffer = buffer.slice(nl + 1);
        if (!line) continue;
        try {
          handleRecord(JSON.parse(line));
        } catch (_) {
          // Non-JSON line (stray output) — ignore.
        }
      }
    });

    child.on("close", () => {
      if (buffer.trim()) {
        try { handleRecord(JSON.parse(buffer.trim())); } catch (_) { /* ignore */ }
      }
      if (lcovFile) {
        attachLcovCoverage(run, lcovFile);
        try { fs.unlinkSync(lcovFile); } catch (_) { /* best effort */ }
      }
      finish();
    });
  };

  const runHandler = (request, token, onDone) => runTestSession(request, token, { coverage: false }, onDone);
  const coverageHandler = (request, token) => runTestSession(request, token, { coverage: true });

  controller.createRunProfile("Run", TestRunProfileKind.Run, runHandler, true);
  const coverageProfile = controller.createRunProfile(
    "Run with Coverage", TestRunProfileKind.Coverage, coverageHandler, false
  );
  // Resolve per-file detailed coverage lazily from the lines cached during the run.
  coverageProfile.loadDetailedCoverage = async (testRun, fileCoverage) => {
    const detail = coverageDetailCache.get(testRun);
    const lines = detail && detail.get(fileCoverage.uri.toString());
    if (!lines) return [];
    // lcov line numbers are 1-based; VSCode Positions are 0-based.
    return lines.map((l) => new StatementCoverage(l.hits, new Position(Math.max(0, l.line - 1), 0)));
  };
  // Expose the handler so the palette commands can drive a run directly.
  return { controller, runHandler };
}

// --- Task provider ------------------------------------------------------------
//
// Surfaces `lin build|run|test` as VS Code tasks. The `lin` problem matcher
// (contributed in package.json) parses the Ariadne-style compiler diagnostics
// (`[path:line:col]`) into the Problems panel. Tasks invoke the bundled `lin`
// binary via a ShellExecution so output is visible in the terminal.
function buildLinTask(linBin, def) {
  const file = def.file && def.file.length > 0 ? def.file : "${file}";
  const argv = [def.command, file, ...(Array.isArray(def.args) ? def.args : [])];
  // Quote each argv element for the shell; ShellExecution handles the rest.
  const exec = new ShellExecution(linBin, argv.map((a) => ({ value: a, quoting: ShellQuoting.Strong })));
  const task = new Task(
    def,
    def.scope || TaskScope.Workspace,
    `lin ${def.command}`,
    "lin",
    exec,
    "$lin"
  );
  return task;
}

function makeTaskProvider(linBin) {
  return {
    // Default tasks offered in the "Run Task" picker. They use the ${file}
    // predefined variable so they operate on whatever .lin file is active.
    provideTasks() {
      const defs = [
        { type: "lin", command: "build" },
        { type: "lin", command: "run" },
        { type: "lin", command: "test" },
      ];
      return defs.map((d) => buildLinTask(linBin, d));
    },
    // Resolve a task authored in tasks.json (definition already known).
    resolveTask(task) {
      const def = task.definition;
      if (!def || def.type !== "lin" || !def.command) return undefined;
      return buildLinTask(linBin, { ...def, scope: task.scope });
    },
  };
}

// --- Debug configuration provider --------------------------------------------
//
// Lin native debugging delegates to CodeLLDB (vadimcn.vscode-lldb): we compile the
// `.lin` file with `lin build --debug` (emitting DWARF line tables) and then hand a
// `type: "lldb"` launch config to CodeLLDB, which loads the binary and maps DWARF
// back to `.lin` source lines for breakpoints/stepping.
//
// The provider accepts a `type: "lin"` launch config (authored in launch.json or the
// auto-supplied initialConfiguration). It resolves `program` (the compiled binary,
// defaulting to the source stem next to the file), builds it with --debug, then returns
// the CodeLLDB config. Returning a config with a different `type` reroutes the session to
// that adapter — the documented VS Code mechanism for a "delegating" debugger.
// Absolute path to the lldb pretty-printer script shipped with the extension. Resolved
// relative to the extension install dir so it works whether the extension is bundled
// (production VSIX) or running from source (contributor workflow) -- the script lives at
// `<extensionPath>/formatters/lin_formatters.py` in both layouts.
function formattersScriptPath(context) {
  return path.join(context.extensionPath, "formatters", "lin_formatters.py");
}

// Phase 2 (data formatters): the returned CodeLLDB config carries `initCommands` that import
// the bundled lldb pretty-printer script. On import the script self-registers `type summary`/
// `type synthetic` providers (via __lldb_init_module) that decode Lin's boxed runtime values,
// so the Variables/Watch panels show logical Lin values instead of raw boxed structs.
function makeDebugConfigProvider(linBin, context) {
  return {
    // Run when launch.json has no Lin config yet (F5 with a .lin file open): synthesize one.
    provideDebugConfigurations() {
      return [
        {
          type: "lin",
          request: "launch",
          name: "Debug Lin file",
          source: "${file}",
          program: "${fileDirname}/${fileBasenameNoExtension}",
          cwd: "${workspaceFolder}",
          args: [],
        },
      ];
    },
    // Resolve after VS Code has substituted ${...} variables. We build here (async) and
    // return the CodeLLDB config; returning undefined aborts the session.
    async resolveDebugConfigurationWithSubstitutedVariables(folder, config) {
      // Bare F5 with no config: fill from the active editor.
      const editor = window.activeTextEditor;
      let source = config.source;
      if (!source && editor && editor.document.uri.fsPath.endsWith(".lin")) {
        source = editor.document.uri.fsPath;
      }
      if (!source) {
        window.showErrorMessage("Lin debug: no .lin source to build. Open a .lin file or set `source` in launch.json.");
        return undefined;
      }
      const program =
        config.program ||
        path.join(path.dirname(source), path.basename(source, ".lin"));

      // Build with --debug so the binary carries DWARF line tables. We run the build as a
      // VS Code task (so the `lin` problem matcher surfaces compile errors) and await it.
      const argv = ["build", source, "--debug", "-o", program];
      const exec = new ShellExecution(
        linBin,
        argv.map((a) => ({ value: a, quoting: ShellQuoting.Strong }))
      );
      const buildTask = new Task(
        { type: "lin", command: "build", file: source, debug: true },
        folder || TaskScope.Workspace,
        "lin build --debug",
        "lin",
        exec,
        "$lin"
      );
      const ok = await new Promise((resolve) => {
        let disposed = false;
        const end = tasks.onDidEndTaskProcess((e) => {
          if (e.execution.task === buildTask || e.execution.task.name === buildTask.name) {
            if (!disposed) {
              disposed = true;
              end.dispose();
              resolve(e.exitCode === 0);
            }
          }
        });
        tasks.executeTask(buildTask).then(undefined, () => {
          if (!disposed) {
            disposed = true;
            end.dispose();
            resolve(false);
          }
        });
      });
      if (!ok) {
        window.showErrorMessage("Lin debug: `lin build --debug` failed; see the terminal/Problems panel.");
        return undefined;
      }

      // Hand off to CodeLLDB. The session's effective type becomes "lldb".
      return {
        type: "lldb",
        request: "launch",
        name: config.name || `Debug ${path.basename(source)}`,
        program,
        args: Array.isArray(config.args) ? config.args : [],
        cwd: config.cwd || (folder ? folder.uri.fsPath : path.dirname(source)),
        stopOnEntry: !!config.stopOnEntry,
        // Surface the source path so CodeLLDB resolves relative DWARF file names if needed.
        sourceLanguages: ["lin"],
        // Auto-load the Lin lldb pretty-printers so boxed runtime values render as logical Lin
        // values in the Variables/Watch panels. The script self-registers its summary/synthetic
        // providers on import (__lldb_init_module). Preserve any user-supplied initCommands.
        initCommands: [
          `command script import ${formattersScriptPath(context)}`,
          ...(Array.isArray(config.initCommands) ? config.initCommands : []),
        ],
      };
    },
  };
}

function activate(context) {
  const lspBin = resolveBin(context, "lin-lsp");
  const linBin = resolveBin(context, "lin");

  // Register the task provider so `lin build|run|test` appear in Run Task and
  // can be authored in tasks.json with the `lin` problem matcher.
  context.subscriptions.push(
    tasks.registerTaskProvider("lin", makeTaskProvider(linBin))
  );

  // Register the Lin debug configuration provider: builds with `lin build --debug` and
  // delegates the actual debug session to CodeLLDB (see makeDebugConfigProvider).
  context.subscriptions.push(
    debug.registerDebugConfigurationProvider("lin", makeDebugConfigProvider(linBin, context))
  );

  // `lin` is available in VS Code's integrated terminal out of the box.
  addToIntegratedTerminalPath(context);

  const serverOptions = {
    command: lspBin,
    transport: TransportKind.stdio,
  };

  const clientOptions = {
    documentSelector: [{ scheme: "file", language: "lin" }],
    synchronize: {
      fileEvents: workspace.createFileSystemWatcher("**/*.lin"),
    },
    outputChannelName: "Lin Language Server",
  };

  client = new LanguageClient("lin-lsp", "Lin Language Server", serverOptions, clientOptions);

  client.start().catch((err) => {
    window.showErrorMessage(
      `Lin LSP failed to start: ${err.message}. ` +
      `Install the extension from the marketplace or run 'cargo build -p lin-lsp'.`
    );
  });

  // Register Lin compiler commands — run in a terminal so output is visible.
  const runLinCommand = (subcommand) => {
    const editor = window.activeTextEditor;
    if (!editor) {
      window.showWarningMessage("Lin: No active file.");
      return;
    }
    const file = editor.document.uri.fsPath;
    if (!file.endsWith(".lin")) {
      window.showWarningMessage("Lin: Active file is not a .lin file.");
      return;
    }
    const terminal = window.createTerminal("Lin");
    terminal.show(true);
    terminal.sendText(`${shellQuote(linBin)} ${subcommand} ${shellQuote(file)}`);
  };

  // "Format Document" (Shift+Alt+F), format-on-save, and the `lin.format` command
  // below all route to the LSP's single formatter (auto-registered by the language
  // client). We deliberately register no formatter here to avoid a duplicate.

  // Test Explorer integration. Returns the controller + its run handler so the
  // palette commands below can trigger runs through it.
  const { controller: testController, runHandler: runTests } = setupTestController(context, linBin);

  // Palette-initiated runs (gutter runs get a real token from the Testing UI's Stop button, but
  // palette commands construct their own request). We give each a real CancellationTokenSource so
  // the run handler's `token.onCancellationRequested(() => child.kill())` can actually fire.
  // Re-invoking the same command cancels the previous in-flight run for that scope (so a re-run
  // supersedes it), and we dispose the source when the run completes to free resources.
  const activeCts = new Map();
  const runWithFreshToken = (scopeKey, request) => {
    const prior = activeCts.get(scopeKey);
    if (prior) {
      prior.cancel();
      prior.dispose();
    }
    const cts = new CancellationTokenSource();
    activeCts.set(scopeKey, cts);
    // Cancel + dispose once this run finishes, so the source doesn't linger and a later Stop on a
    // stale source is a no-op.
    cts.token.onCancellationRequested(() => {
      if (activeCts.get(scopeKey) === cts) activeCts.delete(scopeKey);
    });
    runTests(request, cts.token, () => {
      if (activeCts.get(scopeKey) === cts) {
        activeCts.delete(scopeKey);
        cts.dispose();
      }
    });
  };
  context.subscriptions.push({
    dispose() {
      for (const cts of activeCts.values()) {
        cts.cancel();
        cts.dispose();
      }
      activeCts.clear();
    },
  });

  context.subscriptions.push(
    commands.registerCommand("lin.build", () => runLinCommand("build")),
    commands.registerCommand("lin.run",   () => runLinCommand("run")),
    commands.registerCommand("lin.test",  () => {
      const editor = window.activeTextEditor;
      if (!editor) { window.showWarningMessage("Lin: No active file."); return; }
      const dir = path.dirname(editor.document.uri.fsPath);
      const terminal = window.createTerminal("Lin Test");
      terminal.show(true);
      terminal.sendText(`${shellQuote(linBin)} test ${shellQuote(dir)}`);
    }),
    // Route through the provider above so it behaves identically to Format Document.
    commands.registerCommand("lin.format", () => {
      const editor = window.activeTextEditor;
      if (!editor) {
        window.showWarningMessage("Lin: No active file.");
        return;
      }
      if (!editor.document.uri.fsPath.endsWith(".lin")) {
        window.showWarningMessage("Lin: Active file is not a .lin file.");
        return;
      }
      commands.executeCommand("editor.action.formatDocument");
    }),
    commands.registerCommand("lin.installOnPath", () => installOnPath(context)),
    // Palette entry points that drive the TestController (discoverability beyond
    // the Testing view's gutter icons). They construct a TestRunRequest and run
    // it through the same handler the gutter "Run" profile uses.
    commands.registerCommand("lin.testFile", async () => {
      const editor = window.activeTextEditor;
      if (!editor || !editor.document.uri.fsPath.endsWith(".test.lin")) {
        window.showWarningMessage("Lin: Active file is not a .test.lin file.");
        return;
      }
      const fileItem = getOrCreateFileItem(testController, editor.document.uri);
      await resolveFileChildren(testController, fileItem);
      runWithFreshToken(`file:${editor.document.uri.fsPath}`, new TestRunRequest([fileItem]));
    }),
    commands.registerCommand("lin.testProject", () => {
      runWithFreshToken("project", new TestRunRequest());
    }),
    // CodeLens-driven single-test run. The LSP emits CodeLenses with this fixed
    // command id and argument order: [documentUri: string, testName: string].
    // We run only that test by constructing a TestRunRequest for the matching
    // child TestItem (created if it wasn't statically discovered), which the run
    // handler narrows via `--filter-test "<name>"`.
    commands.registerCommand("lin.runTest", (documentUri, testName) => {
      if (typeof documentUri !== "string" || typeof testName !== "string") {
        window.showWarningMessage("Lin: Run Test invoked without a document/test name.");
        return;
      }
      const fsPath = Uri.parse(documentUri).fsPath;
      const child = findOrCreateTestItem(testController, fsPath, testName);
      runWithFreshToken(`runTest:${fsPath}::${testName}`, new TestRunRequest([child]));
    }),
  );
}

function deactivate() {
  if (client) {
    return client.stop();
  }
}

module.exports = {
  activate,
  deactivate,
  // Exposed for the standalone discovery/unescape unit test (test/discovery.test.js).
  // These are pure (no VS Code API) and safe to call directly.
  _test: { discoverLine, unescapeLinString, stripLineComment, isInsideString, firstStringArg },
};
