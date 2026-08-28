// Kora VS Code extension.
//
// Two jobs: run the current file, and start the language server that provides
// diagnostics, hover, go-to-definition, outline, and completion.

const vscode = require("vscode");
const { LanguageClient, TransportKind } = require("vscode-languageclient/node");

let client;

function activate(context) {
  const runFile = vscode.commands.registerCommand("kora.runFile", () => {
    const editor = vscode.window.activeTextEditor;
    if (!editor || !editor.document.fileName.endsWith(".ko")) {
      vscode.window.showErrorMessage("Open a .ko file to run it.");
      return;
    }
    editor.document.save().then(() => {
      let terminal = vscode.window.terminals.find((t) => t.name === "kora");
      if (!terminal) {
        terminal = vscode.window.createTerminal("kora");
      }
      terminal.show(true);
      terminal.sendText(`kora run "${editor.document.fileName}"`);
    });
  });

  const testFile = vscode.commands.registerCommand("kora.testFile", () => {
    const editor = vscode.window.activeTextEditor;
    if (!editor || !editor.document.fileName.endsWith(".ko")) {
      vscode.window.showErrorMessage("Open a .ko file to test it.");
      return;
    }
    editor.document.save().then(() => {
      let terminal = vscode.window.terminals.find((t) => t.name === "kora");
      if (!terminal) {
        terminal = vscode.window.createTerminal("kora");
      }
      terminal.show(true);
      terminal.sendText(`kora test "${editor.document.fileName}"`);
    });
  });

  context.subscriptions.push(runFile, testFile);
  startLanguageServer();
}

function startLanguageServer() {
  const command = vscode.workspace
    .getConfiguration("kora")
    .get("serverPath", "kora");

  const serverOptions = {
    run: { command, args: ["lsp"], transport: TransportKind.stdio },
    debug: { command, args: ["lsp"], transport: TransportKind.stdio },
  };

  const clientOptions = {
    documentSelector: [{ scheme: "file", language: "kora" }],
    outputChannelName: "Kora Language Server",
  };

  client = new LanguageClient(
    "kora",
    "Kora Language Server",
    serverOptions,
    clientOptions
  );

  // A missing binary is the common case on a fresh checkout, so say what to
  // do about it rather than failing silently.
  client.start().catch((err) => {
    vscode.window.showWarningMessage(
      `Kora language server did not start (${err.message}). ` +
        `Install it with \`cargo install --path crates/kora-cli\`, ` +
        `or set "kora.serverPath".`
    );
  });
}

function deactivate() {
  return client ? client.stop() : undefined;
}

module.exports = { activate, deactivate };
