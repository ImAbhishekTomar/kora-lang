# Kora Language for VS Code

Full editor support for [Kora](https://github.com/abhishektomar/kora-lang), the agent-native programming language.

## Features

- **Syntax highlighting** for `.ko` files (keywords, strings, f-strings, types, `@field` metadata, operators)
- **File icons** — a dedicated Kora file icon, enable via *File > Preferences > File Icon Theme > Kora Icons*
- **Run / Test buttons** in the editor toolbar, plus `Cmd/Ctrl+Shift+R` to run the current file
- **IntelliSense** via the `kora lsp` language server: diagnostics, hover, go-to-definition, outline, and completion

## Requirements

The `kora` CLI must be on your `PATH` (or point `kora.serverPath` at it):

```bash
cargo install --path crates/kora-cli
```

## Settings

| Setting | Description | Default |
|---|---|---|
| `kora.serverPath` | Path to the `kora` binary used for the language server | `kora` |

## Commands

| Command | Keybinding |
|---|---|
| `Kora: Run Current File` | `Cmd/Ctrl+Shift+R` |
| `Kora: Test Current File` | — |
