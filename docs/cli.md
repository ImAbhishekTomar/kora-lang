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
`declassify` block the parser can see.

```bash
kora audit examples/03_salary_review.ko
```

```
  examples/03_salary_review.ko:28  declassify pay for local_model

1 declassification site
```

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

---

## Configuration

`kora.toml`, found by walking up from the program file. Every section is
optional.

```toml
[models]
default = "local:qwen3:8b"          # Ollama
smart   = "openai:gpt-4o"           # needs OPENAI_API_KEY

[models.openai]
max_output_tokens = 4096            # bounds worst-case budget reservation

[models.local]
endpoint = "http://localhost:11434"

[sinks]                             # which labels may reach which sink
local_model = { allow = ["classified"] }
openai      = { allow = ["internal"], deny = ["classified"] }

[http]
allow_private = false               # loopback and private ranges refused
timeout_secs = 30                   # 0 is clamped, not honoured

[python]
command = "python3"                 # or a virtualenv's interpreter

[mcp.github]                        # how to launch a server
command = "npx"
args = ["-y", "@modelcontextprotocol/server-github"]
env = { GITHUB_PERSONAL_ACCESS_TOKEN = "$GITHUB_TOKEN" }

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

---

## Exit codes

| Code | Meaning |
|---|---|
| `0` | success |
| `1` | the program failed, or a test failed |
| `2` | the command line was wrong |
| `3` | a durable run suspended, waiting on a person |
