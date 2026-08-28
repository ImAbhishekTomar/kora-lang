// Kora VS Code extension.
//
// Three jobs: run the current file, start the language server that provides
// diagnostics, hover, go-to-definition, outline, and completion, and start the
// debug adapter that provides breakpoints, stepping, and the variables pane.
//
// All three are the same binary — `kora lsp`, `kora dap`, `kora run` — so
// there is one thing to install and one version to keep straight.

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

  const debugFile = vscode.commands.registerCommand("kora.debugFile", () => {
    const editor = vscode.window.activeTextEditor;
    if (!editor || !editor.document.fileName.endsWith(".ko")) {
      vscode.window.showErrorMessage("Open a .ko file to debug it.");
      return;
    }
    editor.document.save().then(() => {
      vscode.debug.startDebugging(
        vscode.workspace.getWorkspaceFolder(editor.document.uri),
        {
          type: "kora",
          request: "launch",
          name: "Debug current Kora file",
          program: editor.document.fileName,
        }
      );
    });
  });

  context.subscriptions.push(runFile, testFile, debugFile);
  registerDebugging(context);
  startLanguageServer();
}

// The debug adapter is `kora dap` speaking DAP over stdio.
//
// The configuration provider is what makes F5 work on a .ko file with no
// launch.json: without it VS Code asks the user to create one first, which is
// a poor first five minutes.
function registerDebugging(context) {
  const factory = {
    createDebugAdapterDescriptor() {
      const command = koraPath();
      return new vscode.DebugAdapterExecutable(command, ["dap"]);
    },
  };

  const provider = {
    resolveDebugConfiguration(folder, config) {
      if (!config.type && !config.request && !config.name) {
        const editor = vscode.window.activeTextEditor;
        if (!editor || !editor.document.fileName.endsWith(".ko")) {
          return vscode.window
            .showInformationMessage("Open a .ko file to debug it.")
            .then(() => undefined);
        }
        config.type = "kora";
        config.request = "launch";
        config.name = "Debug current Kora file";
        config.program = editor.document.fileName;
      }
      if (!config.program) {
        return vscode.window
          .showErrorMessage("Set `program` in the launch configuration.")
          .then(() => undefined);
      }
      return config;
    },
  };

  context.subscriptions.push(
    vscode.debug.registerDebugAdapterDescriptorFactory("kora", factory),
    vscode.debug.registerDebugConfigurationProvider("kora", provider)
  );
}

function koraPath() {
  return vscode.workspace.getConfiguration("kora").get("serverPath", "kora");
}

function startLanguageServer() {
  const command = koraPath();

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
