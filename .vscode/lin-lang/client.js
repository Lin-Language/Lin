"use strict";

const path = require("path");
const { workspace, window } = require("vscode");
const { LanguageClient, TransportKind } = require("vscode-languageclient/node");

let client;

function activate(context) {
  // Resolve the lin-lsp binary relative to the workspace root.
  // Falls back to searching PATH if the workspace folder isn't available.
  const workspaceFolders = workspace.workspaceFolders;
  const workspaceRoot = workspaceFolders ? workspaceFolders[0].uri.fsPath : null;

  const serverBin = workspaceRoot
    ? path.join(workspaceRoot, "target", "debug", "lin-lsp")
    : "lin-lsp";

  const serverOptions = {
    command: serverBin,
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
      `Run 'cargo build -p lin-lsp' in the workspace root first.`
    );
  });
}

function deactivate() {
  if (client) {
    return client.stop();
  }
}

module.exports = { activate, deactivate };
