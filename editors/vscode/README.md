# Kora Language for VS Code

Full editor support for [Kora](https://github.com/abhishektomar/kora-lang), the agent-native programming language.

## Features

- **Syntax highlighting** for `.ko` files (keywords, strings, f-strings, types, `@field` metadata, operators)
- **File icons** — a dedicated Kora file icon, enable via *File > Preferences > File Icon Theme > Kora Icons*
- **Run / Test buttons** in the editor toolbar, plus `Cmd/Ctrl+Shift+R` to run the current file
- **IntelliSense** via the `kora lsp` language server: diagnostics, hover, go-to-definition, outline, and completion
- **Debugging** via the `kora dap` debug adapter: breakpoints, step over/into/out, call stack, variables, watch expressions, and `print` output in the debug console
- **Cross-file imports** — `use "./lib/tax.ko" as tax` resolves for real: completion and diagnostics read the imported file, and go-to-definition on the alias opens it

## Requirements

The `kora` CLI must be on your `PATH` (or point `kora.serverPath` at it):

```bash
cargo install --path crates/kora-cli
```

## Settings

| Setting | Description | Default |
|---|---|---|
| `kora.serverPath` | Path to the `kora` binary used for the language server | `kora` |

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

## Commands

| Command | Keybinding |
|---|---|
| `Kora: Run Current File` | `Cmd/Ctrl+Shift+R` |
| `Kora: Test Current File` | — |
| `Kora: Debug Current File` | `F5` |
