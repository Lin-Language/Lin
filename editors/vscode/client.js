"use strict";

const fs = require("fs");
const path = require("path");
const os = require("os");
const cp = require("child_process");
const { workspace, window, commands, languages, Range, TextEdit, Uri } = require("vscode");
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
  );
}

function deactivate() {
  if (client) {
    return client.stop();
  }
}

module.exports = { activate, deactivate };
