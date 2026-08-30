# The `kora` command

```bash
cargo install --path crates/kora-cli
```

---

## Commands

### `kora run <file.ko>`

Run a program. `kora <file.ko>` is the same thing.

| Flag | Effect |
|---|---|
| `--record` | call models, then save every call to a cassette |
| `--replay` | serve model calls from the cassette; never reach a provider |
| `--report` | print token usage and call count afterwards |
| `--trace` | record OpenTelemetry spans |
| `--durable` | journal every effect; survives being killed, and can suspend on `ask_human` |
| `--resume <run-id>` | continue a durable run that was interrupted |

```bash
kora run examples/00_basics.ko
kora run --record --report examples/01_expense_check.ko   # costs tokens
kora run --replay --report examples/01_expense_check.ko   # free, deterministic
```

Cassettes live in `cassettes/` beside the program and are meant to be
committed: they make a suite reproducible and free. A cassette is keyed on
call site, model, prompt, and input, so **changing the configured model
invalidates them** — re-record with `--record`.

### `kora check <file.ko>...`

Parse and check files without running them — the same analysis the editor
shows. Useful in CI, and the only way to check a file needing resources this
machine does not have.

```bash
kora check examples/*.ko
kora check --syntax draft.ko    # parse only, skip name resolution
```

Checking follows `use "./lib.ko"` imports, so a name that only exists in an
imported file resolves here, and a name no imported file defines is reported.
Problems *inside* an imported file belong to that file: check it directly.

It also catches Python habits that would otherwise only surface when the file
runs: `xs.append(v)` (Kora has no methods) and keyword arguments on a
user-defined function (only `analyze()` takes them) are both reported here,
not just at `kora run`.

Exits non-zero if anything fails to parse or resolve.

### `kora test <file.ko>`

Run the `test` blocks in a file. Model calls replay from the cassette, so a
suite costs nothing and gives the same answer every time. Exits non-zero if
anything fails.

```bash
kora test examples/07_tests.ko
```

### `kora audit <file.ko>`

List every place classified data is released, and to which sink. The list is
complete rather than best-effort, because every release goes through a
`declassify` block the parser can see. Imported files are part of the
program, so the audit covers every file the program imports and every
package it uses. A `declassify` inside a dependency releases the importing
program's data; an audit that could not see it would make adding a dependency
the way to hide one.

```bash
kora audit examples/03_salary_review.ko
kora audit --deps program.ko        # grouped by the package responsible
```

`--deps` answers a different question: not "where does this program release
data" but "whose code does the releasing". A dependency that declassifies is
doing it with the importing program's data.

```
  examples/03_salary_review.ko:28  declassify pay for local_model

1 declassification site
```

### `kora install <file.ko>`

Fetch the git dependencies the program actually uses. A dependency declared
and never imported is not downloaded, so a typo'd name never reaches the disk
at all.

```bash
kora install program.ko
kora install --jobs 4 program.ko
```

Sources land in `.kora/deps/<repository>@<commit>/`, `kora.lock` records what
was resolved, and `kora.sums` records what each commit contained the first
time it was seen. Both files are committed; `.kora/` is not. Fetching is IO-bound, so the default width is not the core
count; `[install] jobs` sets it.

Cold resolution is wave-shaped — what a package depends on is unknowable
until it is on disk — but once the lockfile exists the whole graph is known,
so a warm install is one flat fan-out. Deep chains cost only on the first
resolve, never in CI.

### `kora add` / `kora remove` / `kora update`

```bash
kora add program.ko receipts github.com/org/receipts --tag v0.3.1
kora add program.ko local ../local-package
kora remove program.ko receipts
kora update program.ko receipts --tag v0.4.0
```

Edits are format-preserving: comments and layout in `kora.toml` survive.
Adding does not fetch — a dependency nothing imports is not downloaded just
because it was named — and grants written by hand survive a re-add, so
`kora add` cannot quietly widen what a dependency may do.

`kora update` is the one command that deliberately moves past the lockfile,
so it is where a new version's authority is examined. It refuses when the new
version asks for capabilities the old one did not, or declassifies in more
places, until `--accept-new-authority` says a person has looked:

```
  greet: v2.0.0 -> v3.0.0

this version of `greet` does more than the one it replaces:
  it now requires net
  it now requires sink `telemetry`
  it declassifies in 1 place, up from 0
```

The warning is advisory; the program still cannot run until the new authority
is granted. Two independent gates — the update warns, the runtime enforces.

### `kora vendor <file.ko>`

Copy the packages a shipped program needs into `vendor/`, so the project
builds with no network at all.

```bash
kora vendor program.ko
kora vendor --include-tests program.ko
```

Distinct from `.kora/deps`, which is a cache: `vendor/` is deliberate and
committed. Test-only packages are excluded by default, since they are not
part of what ships. `.git` and `.kora` are never copied.

### `kora tree <file.ko>`

The packages the program actually uses. Kora derives the graph from the
source, so this is what would be fetched and shipped, not what `kora.toml`
declares.

```bash
kora tree examples/13_packages.ko
```

```
examples/13_packages.ko
  greet 0.1.0
  fixtures 2.0.0  (dev — reached only through test blocks)
  unused — declared by this program, never imported
```

A package reached only through `test` blocks is dev-only and stays out of a
shipped program. Nothing declares that — there is no `[dev-dependencies]`
table — and the line says which rule produced the classification.

Each package's line is followed by the authority it holds:

```
  receipts 0.3.1
      grants: net, sink:stripe
```

`kora check` reports the same unused entries as warnings, and errors on a
`use pkg` naming something no manifest declares, on a package requiring
authority nobody granted it, and on one package granted two different ways
by two importers.

### Durable runs

```bash
kora run --durable program.ko          # runs until it needs a person
kora runs program.ko                   # what is waiting, and for what
kora answer program.ko <run-id> yes    # resume with an answer
kora run --durable --resume <run-id> program.ko   # resume after a crash
```

Journals live in `.kora/runs/` beside the program and are git-ignored.

### `kora trace <file.ko>`

Show the spans from the most recent traced run.

```bash
kora run --trace program.ko
kora trace program.ko
```

```
review                                 812ms
  declassify                             0ms
  analyze Assessment                   806ms
```

### `kora lsp`

Run the language server over stdio. Editors start this; you do not.

| | |
|---|---|
| Diagnostics | parse errors and checker findings, pushed on open and on every change |
| Hover | signatures and docstrings for names under the cursor |
| Go to definition | jumps to where a name is declared |
| Outline | document symbols — functions, agents, and top-level names |
| Completion | triggered on `.`, plus general name completion |

No rename, find-references, code actions, or formatting yet. Run/test
buttons in the VS Code extension shell out to `kora run`/`kora test`
directly; they are not LSP requests.

### `kora dap`

Run the debug adapter over stdio. Editors start this; you do not.

It speaks the Debug Adapter Protocol, so any editor that implements DAP can
drive it. What it supports:

| | |
|---|---|
| Breakpoints | set per file; a breakpoint on a blank line or a comment moves to the next statement, and the editor is told where it landed |
| Stepping | step over, step into, step out, continue, and pause |
| Call stack | one frame per function call and per file's top level, each naming its own file |
| Variables | locals for the selected frame, plus that file's top-level names; lists, dicts, and objects expand |
| Watch and hover | names and field paths — `total`, `employee.salary`, `rows.0` |
| Output | `print` reaches the debug console as it happens |

The launch configuration takes `program` (required), `stopOnEntry`, and
`replay` or `record`, which do what the same flags do on `kora run`. Debugging
with `replay` costs nothing and gives the same answer every time, which is
usually what you want when stepping through code that calls a model.

Two limits worth knowing. A `parallel for` body runs on worker threads that
have no debugger attached, so breakpoints inside one do not fire — its output
arrives when the run ends. And a watch expression is a lookup, not an
evaluation: a debugger that could call a model or write a file while you hover
over a variable is not inspecting the program, it is changing it.

---

## Configuration

`kora.toml`, found by walking up from the program file. Every section is
optional.

```toml
[models]
default = "local:qwen3:8b"          # Ollama
smart   = "openai:gpt-4o"           # needs OPENAI_API_KEY
vision  = "local:moondream"         # named by `analyze(..., model="vision")`

timeout_secs = 600                  # one model call; 0 is clamped, not honoured
max_retries = 2                     # three attempts; 0 turns retrying off

[models.openai]
max_output_tokens = 4096            # bounds worst-case budget reservation

[models.local]
endpoint = "http://localhost:11434"

[sinks]                             # which labels may reach which sink
local_model = { allow = ["classified"] }
openai      = { allow = ["internal"], deny = ["classified"] }

[package]                           # only when this project *is* a package
name    = "receipts"
version = "0.1.0"
entry   = "src/lib.ko"              # the default

[install]
jobs = 16                           # parallel fetches; IO-bound, so not the
                                    # core count. Default max(8, cores * 2)

[dependencies.receipts]             # where a package comes from. Whether it
path = "./receipts"                 # is used is decided by the source, so
                                    # declaring one costs nothing until
                                    # something writes `use pkg receipts`
grants = { net = true, sinks = ["stripe"] }   # and what it may do. Absent
                                    # means nothing: a dependency never
                                    # given the network cannot reach it

[http]
allow_private = false               # loopback and private ranges refused
timeout_secs = 30                   # 0 is clamped, not honoured

[python]
command = "python3"                 # or a virtualenv's interpreter

[mcp]                               # defaults for every server below
timeout_secs = 60                   # one request; 0 is clamped, not honoured
max_retries = 2                     # starting a server only, never a tool call

[mcp.github]                        # how to launch a server
command = "npx"
args = ["-y", "@modelcontextprotocol/server-github"]
env = { GITHUB_PERSONAL_ACCESS_TOKEN = "$GITHUB_TOKEN" }
timeout_secs = 120                  # this one reaches a slow API

[telemetry]
exporter = "file"                   # "file" | "otlp" | omit for none
path = ".kora/last.trace.json"
# endpoint = "http://localhost:4318"  # for exporter = "otlp"
level = "calls"                     # off | agents | calls | full
redact = true                       # labeled values never reach the exporter
```

Model references are `provider:model`. `local:` and `ollama:` mean Ollama;
`openai:` means the API. Everything after the first colon is the model name,
so tags like `local:llama3.1:8b` survive intact.

---

## Editor support

```bash
cargo install --path crates/kora-cli
ln -s "$PWD/editors/vscode" ~/.vscode/extensions/kora-lang
```

Restart VS Code. You get diagnostics as you type, hover signatures and
docstrings, go-to-definition, an outline, completion, and run/test buttons.
Set `kora.serverPath` if the binary is not on `PATH`.

Press F5 on a `.ko` file to debug it — breakpoints in the gutter, the call
stack, and a variables pane.

Two things catch people out, both of them VS Code's rules rather than Kora's:

- **Quit and reopen VS Code after installing or updating the extension.**
  Contributions are read once, at startup. Until that happens the gutter
  refuses breakpoints on `.ko` files and F5 offers to find a Kora extension in
  the Marketplace.
- **Check the binary the editor will use has a `dap` subcommand.** Run
  `kora | grep "kora dap"`. If it prints nothing, the `kora` first on your
  `PATH` predates the debugger; set `kora.serverPath` to an absolute path
  rather than reordering `PATH`.

No `launch.json` is needed for the common case; write one to pin the options:

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

---

## Exit codes

| Code | Meaning |
|---|---|
| `0` | success |
| `1` | the program failed, or a test failed |
| `2` | the command line was wrong |
| `3` | a durable run suspended, waiting on a person |
