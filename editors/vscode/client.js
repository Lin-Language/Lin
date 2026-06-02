"use strict";

const fs = require("fs");
const path = require("path");
const os = require("os");
const cp = require("child_process");
const {
  workspace, window, commands, languages, Range, TextEdit, Uri,
  tests, TestRunRequest, TestRunProfileKind, TestMessage, Position,
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

// `lin fmt` only reformats files in place on disk and the editor buffer may
// hold unsaved edits, so we round-trip the in-memory text through a temp file:
// write it out, run the formatter, read the result back, and return it as a
// single full-document replacement. On any failure (parse error, missing
// binary) we surface the message and return no edits, leaving the buffer alone.
function makeFormattingProvider(linBin) {
  return {
    provideDocumentFormattingEdits(document) {
      let tmpDir;
      try {
        tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), "lin-fmt-"));
        const tmpFile = path.join(tmpDir, "buffer.lin");
        fs.writeFileSync(tmpFile, document.getText());
        cp.execFileSync(linBin, ["fmt", tmpFile]);
        const formatted = fs.readFileSync(tmpFile, "utf8");
        const fullRange = new Range(
          document.positionAt(0),
          document.positionAt(document.getText().length)
        );
        return [TextEdit.replace(fullRange, formatted)];
      } catch (err) {
        const detail = (err.stderr && err.stderr.toString()) || err.message;
        window.showErrorMessage(`Lin: formatting failed: ${detail}`);
        return [];
      } finally {
        if (tmpDir) {
          try { fs.rmSync(tmpDir, { recursive: true, force: true }); } catch (_) { /* best effort */ }
        }
      }
    },
  };
}

// --- Test Explorer integration (VSCode Testing API) ---------------------------
//
// Discovery is best-effort regex over *.test.lin: we never compile to enumerate
// tests. Running shells out to `lin test ... --reporter json`, which emits NDJSON
// (one record per test + one per file) on stdout; we stream it and map records
// back onto TestItems. Tests with interpolated/dynamic names aren't discovered
// statically — they're materialized on the fly from the `test` records at run time.

// Matches `test("name"` and `withFixture(..., "name",` style declarations. We only
// need the literal-string name; dynamic names simply won't match (handled at run time).
const TEST_DECL_RE = /\btest\s*\(\s*"((?:[^"\\]|\\.)*)"/g;
const WITHFIXTURE_DECL_RE = /\bwithFixture\s*\([^,]*,[^,]*,\s*"((?:[^"\\]|\\.)*)"/g;

function unescapeLinString(s) {
  // Best-effort inverse of the common Lin string escapes so the discovered id
  // matches the runtime `name` (which is the decoded string).
  return s
    .replace(/\\n/g, "\n")
    .replace(/\\t/g, "\t")
    .replace(/\\r/g, "\r")
    .replace(/\\"/g, '"')
    .replace(/\\\\/g, "\\");
}

function testItemId(filePath, name) {
  return `${filePath}::${name}`;
}

// (Re)build a file's child TestItems from its current text.
function refreshFileTests(controller, fileItem, text) {
  fileItem.children.replace([]);
  const seen = new Set();
  const lines = text.split("\n");
  const addMatch = (re) => {
    for (let i = 0; i < lines.length; i++) {
      re.lastIndex = 0;
      let m;
      while ((m = re.exec(lines[i])) !== null) {
        const name = unescapeLinString(m[1]);
        const id = testItemId(fileItem.uri.fsPath, name);
        if (seen.has(id)) continue;
        seen.add(id);
        const child = controller.createTestItem(id, name, fileItem.uri);
        const col = m.index >= 0 ? m.index : 0;
        child.range = new Range(new Position(i, col), new Position(i, col + name.length));
        fileItem.children.add(child);
      }
    }
  };
  addMatch(TEST_DECL_RE);
  addMatch(WITHFIXTURE_DECL_RE);
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

  const runHandler = (request, token) => {
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
    const wholeProject = !(request.include && request.include.length > 0);
    const args = ["test"];
    if (wholeProject && wsRoot) {
      args.push(wsRoot);
    } else {
      args.push(...targetFiles);
    }
    args.push("--reporter", "json");

    let child;
    try {
      child = cp.spawn(linBin, args, { cwd: wsRoot });
    } catch (err) {
      window.showErrorMessage(`Lin: failed to start test runner: ${err.message}`);
      run.end();
      return;
    }

    child.on("error", (err) => {
      window.showErrorMessage(`Lin: test runner error: ${err.message}`);
      run.end();
    });

    token.onCancellationRequested(() => {
      try { child.kill(); } catch (_) { /* already gone */ }
    });

    let buffer = "";
    const handleRecord = (rec) => {
      if (rec.event === "test") {
        const item = findOrCreateTestItem(controller, rec.file, rec.name);
        run.started(item);
        if (rec.status === "pass") {
          run.passed(item);
        } else {
          const msg = rec.message || "test failed";
          const tm = new TestMessage(msg);
          const ea = parseExpectedActual(msg);
          if (ea) {
            tm.expectedOutput = ea.expected;
            tm.actualOutput = ea.actual;
          }
          run.failed(item, tm);
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
      run.end();
    });
  };

  controller.createRunProfile("Run", TestRunProfileKind.Run, runHandler, true);
  // Expose the handler so the palette commands can drive a run directly.
  return { controller, runHandler };
}

function activate(context) {
  const lspBin = resolveBin(context, "lin-lsp");
  const linBin = resolveBin(context, "lin");

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
    terminal.sendText(`"${linBin}" ${subcommand} "${file}"`);
  };

  // Standard "Format Document" (Shift+Alt+F) and format-on-save go through this.
  context.subscriptions.push(
    languages.registerDocumentFormattingEditProvider(
      { scheme: "file", language: "lin" },
      makeFormattingProvider(linBin)
    )
  );

  // Test Explorer integration. Returns the controller + its run handler so the
  // palette commands below can trigger runs through it.
  const { controller: testController, runHandler: runTests } = setupTestController(context, linBin);
  // A no-op cancellation token for palette-initiated runs (the Testing UI's own
  // Stop button supplies a real token for gutter-initiated runs).
  const noopToken = { onCancellationRequested: () => ({ dispose() {} }) };

  context.subscriptions.push(
    commands.registerCommand("lin.build", () => runLinCommand("build")),
    commands.registerCommand("lin.run",   () => runLinCommand("run")),
    commands.registerCommand("lin.test",  () => {
      const editor = window.activeTextEditor;
      if (!editor) { window.showWarningMessage("Lin: No active file."); return; }
      const dir = path.dirname(editor.document.uri.fsPath);
      const terminal = window.createTerminal("Lin Test");
      terminal.show(true);
      terminal.sendText(`"${linBin}" test "${dir}"`);
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
      runTests(new TestRunRequest([fileItem]), noopToken);
    }),
    commands.registerCommand("lin.testProject", () => {
      runTests(new TestRunRequest(), noopToken);
    }),
  );
}

function deactivate() {
  if (client) {
    return client.stop();
  }
}

module.exports = { activate, deactivate };
