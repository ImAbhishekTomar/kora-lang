# Kora Language for VS Code

Full editor support for [Kora](https://github.com/abhishektomar/kora-lang), the agent-native programming language.

## Features

- **Syntax highlighting** for `.ko` files (keywords, strings, f-strings, types, `@field` metadata, operators)
- **File icons** — a dedicated Kora file icon, enable via *File > Preferences > File Icon Theme > Kora Icons*
- **Run / Test buttons** in the editor toolbar, plus `Cmd/Ctrl+Shift+R` to run the current file
- **IntelliSense** via the `kora lsp` language server: diagnostics, hover, go-to-definition, outline, and completion
- **Debugging** via the `kora dap` debug adapter: breakpoints, step over/into/out, call stack, variables, watch expressions, and `print` output in the debug console
- **Cross-file imports** — `use "./lib/tax.ko" as tax` resolves for real: completion and diagnostics read the imported file, and go-to-definition on the alias opens it
- **Package-aware syntax** — `use pkg receipts as r` is highlighted alongside local, Python, and MCP imports; package resolution and diagnostics come from the `kora` language server
- **Images in the debugger** — an image value from `fs.image` expands in the variables pane to its source, type, and size, never a wall of bytes

## Requirements

The `kora` CLI provides the language server and the debug adapter, so the
extension needs a binary it can run:

```bash
cargo install --path crates/kora-cli
```

Check that the binary the extension will use is new enough to debug:

```bash
kora | grep "kora dap"
```

No output means the `kora` first on your `PATH` predates the debugger — a
released build, say, while the feature is still unpublished. Point the
extension at the one you just built rather than reordering your `PATH`:

```json
"kora.serverPath": "/absolute/path/to/.cargo/bin/kora"
```

The grammar recognizes every keyword the lexer does, plus the contextual
words that are language surface rather than identifiers: `analyze`,
`ask_human`, and the handler forms `on token(...)`, `on tool_call(...)`, and
`stream`. A context policy, a budget, a streamed answer and a tool loop are
all highlighted alike.

## Settings

| Setting | Description | Default |
|---|---|---|
| `kora.serverPath` | Path to the `kora` binary used for the language server and the debug adapter | `kora` |

## Debugging

Open a `.ko` file and press **F5**. No `launch.json` is required — the
extension supplies a configuration for the active file.

Write one to pin the options:

```json
{
  "version": "0.2.0",
  "configurations": [
    {
      "type": "kora",
      "request": "launch",
      "name": "Debug current Kora file",
      "program": "${file}",
      "stopOnEntry": false,
      "replay": true
    }
  ]
}
```

| Option | What it does |
|---|---|
| `program` | The `.ko` file to debug. Required. |
| `stopOnEntry` | Stop before the first statement runs. |
| `replay` | Serve model calls from the cassette, so the session is free and repeatable. |
| `record` | Call models for real and save the calls to the cassette. |

A breakpoint on a blank line or a comment moves to the next statement, and the
gutter marker moves with it. Breakpoints inside a `parallel for` body do not
fire: its branches run on worker threads with no debugger attached.

A debug session is never durable. There is no `durable` option and no way to
resume a run under the debugger, which is deliberate rather than missing: a
durable run's journal is a record of what really happened, and stepping
through one would write stepping into that record. Debug against `replay`
instead, then run the real thing with `kora run --durable`.

## Troubleshooting

**"You don't have an extension for debugging Kora"**, or the gutter refuses a
breakpoint on a `.ko` file. VS Code reads an extension's contributions once, at
startup, and this extension is what tells it that `.ko` files are debuggable.
Quit VS Code completely — **Cmd/Ctrl+Q, not Reload Window** — and reopen. A
reload does not rebuild that registry.

**The debug session fails to start.** The adapter is `kora dap`. Run it by hand:

```bash
kora dap
```

It should sit waiting for protocol input rather than printing an error. If it
reports an unknown command, the binary is older than the debugger — see
Requirements above.

**Installing from source.** Link the extension directory and name the link for
the version in its `package.json`, so VS Code's extension scanner does not
serve a manifest it cached under an older version:

```bash
ln -s "$PWD/editors/vscode" ~/.vscode/extensions/kora-lang.kora-lang-0.2.0
```

Restart VS Code after linking, and again after any change to `package.json`.
Changes to `src/extension.js` alone need only a window reload.

## Commands

| Command | Keybinding |
|---|---|
| `Kora: Run Current File` | `Cmd/Ctrl+Shift+R` |
| `Kora: Test Current File` | — |
| `Kora: Debug Current File` | `F5` |
