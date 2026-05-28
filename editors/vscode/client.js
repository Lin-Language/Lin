"use strict";

const fs = require("fs");
const path = require("path");
const { workspace, window, commands, Uri } = require("vscode");
const { LanguageClient, TransportKind } = require("vscode-languageclient/node");

let client;

function getPlatformDir() {
  const os = process.platform;
  const arch = process.arch;
  if (os === "win32") return "win32-x64";
  if (os === "darwin") return arch === "arm64" ? "darwin-arm64" : "darwin-x64";
  return "linux-x64";
}

function resolveBin(context, name) {
  const exe = process.platform === "win32" ? `${name}.exe` : name;

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

function activate(context) {
  const lspBin = resolveBin(context, "lin-lsp");
  const linBin = resolveBin(context, "lin");

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
  );
}

function deactivate() {
  if (client) {
    return client.stop();
  }
}

module.exports = { activate, deactivate };
