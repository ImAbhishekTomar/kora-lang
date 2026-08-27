// Kora VS Code extension — Phase 1: run command.
// The LSP client (squiggles, hover, go-to-def) arrives in Phase 6.

const vscode = require("vscode");

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
  context.subscriptions.push(runFile);
}

function deactivate() {}

module.exports = { activate, deactivate };
