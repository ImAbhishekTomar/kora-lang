# Kora

[![CI](https://github.com/ImAbhishekTomar/kora-lang/actions/workflows/ci.yml/badge.svg)](https://github.com/ImAbhishekTomar/kora-lang/actions/workflows/ci.yml)

An agent-first programming language. Python-like syntax; agents, model calls,
budgets, and data-flow security as native language constructs — not libraries.

```python
type Expense:
    merchant: str
    amount: float
    policy_violation: bool

def main():
    rows = load_csv("expenses.csv")
    for row in rows:
        e: Expense = analyze(row, "extract expense details, flag violations")
        if e.policy_violation:
            print(f"FLAG: {e.merchant} ${e.amount}")
```

## The thesis

**The agent is the unit of execution.**

- **Durable by default** — programs checkpoint at every model call, tool call,
  and `ask_human`. Kill the process; it resumes where it stopped.
- **Real parallelism** — no GIL, no async/await coloring. `parallel for`
  fans out across all cores. Safe because agents share nothing.
- **`classified` / `declassify`** — sensitive data cannot reach a model
  unless explicitly declassified for a named sink, checked at compile time.
  This controls data disclosure; it does not make untrusted text safe to
  follow as instructions.
- **Native budgets** — token-denominated, lexically scoped, shared across
  parallel work. Exhaustion is a value, not a crash; partial work survives.
- **Lexical context policies** — `with context(...)` bounds model input
  independently of spend budgets, keeps only whole recent tool exchanges, and
  fails rather than silently truncating base prompt or data.
- **Typed model calls** — `analyze(data, "prompt")` returns your declared
  type or an explicit `Uncertain`. No raw-string parsing, no confidence theater.
- **An outage is a value too** — model calls retry with backoff, and a
  provider that still does not answer comes back as `Failed(reason)` rather
  than taking the run down with it. So does an MCP tool server: every request
  has a deadline, and one that goes quiet ends the call the same way. Tool
  calls are never retried, because a timeout is exactly when whether the
  effect ran is unknown.
- **Record/replay + OpenTelemetry built into the runtime** — deterministic
  CI with zero tokens; every agent and call is a span.
- **Source-derived packages** — only packages a program imports are fetched
  or shipped, with a lockfile, checksums, and per-package capability grants.

## Install

```bash
curl -fsSL https://raw.githubusercontent.com/ImAbhishekTomar/kora-lang/main/scripts/install.sh | sh
```

Or pick a package manager:

```bash
brew install ImAbhishekTomar/tap/kora   # Homebrew, macOS/Linux
cargo install kora-cli                  # crates.io
npm install -g @abhishektomar/kora-cli  # npm (downloads the release binary)
```

Prebuilt archives for Linux, macOS, and Windows are also on the
[releases page](https://github.com/ImAbhishekTomar/kora-lang/releases).
Building from source needs a Rust toolchain (`cargo install --path crates/kora-cli`,
run from a clone of this repo).

## Try it

Clone the repo to get the example programs, then:

```bash
kora run examples/00_basics.ko                    # the deterministic core
kora run --replay examples/01_expense_check.ko    # a model call, from a cassette
kora test examples/07_tests.ko                    # the test runner
```

Those work with no API key and no model running: the model calls replay from
committed cassettes.

To call a model for real you need either an `OPENAI_API_KEY` or a running
[Ollama](https://ollama.com). Point `[models] default` in `kora.toml` at
whichever you have:

```bash
kora run --record --report examples/01_expense_check.ko   # calls the model, saves a cassette
kora run --replay --report examples/01_expense_check.ko   # free and deterministic
```

Changing the configured model invalidates existing cassettes, since a
cassette is keyed on the model as well as the prompt. Re-record with
`--record`.

## How it fits together

One program, all four pillars:

```python
use mcp github as gh

type Assessment:
    risk: str
    rationale: str

agent review(customer: Customer) -> str:
    budget: max_tokens = 4000                    # bounded, and shared

    declassify customer.account as acct for local_model:
        a: Assessment = analyze(                 # typed result, or Uncertain
            {"account": acct},
            "assess refund risk",
            tools=gh.tools                       # refused: gh is its own sink
        )

    match a:
        case Ok(assessment):
            if assessment.risk != "low":
                # stops here for hours or days; the process may exit
                decision = ask_human("approve?", assessment.rationale)
                return decision
            return "auto-approved"
        case Uncertain(why):
            return f"needs a human: {why}"
        case Exhausted(meter):
            return f"out of {meter}"
        case Failed(why):
            return f"the provider did not answer: {why}"

def main():
    with budget(max_tokens = 50000):
        results = parallel for c in customers:   # real threads, isolated heaps
            return review(c)
```

Each guarantee comes from a different layer, and they compose: the budget is
atomic across the fan-out, the account number can reach the on-box model but
not GitHub, and killing the process mid-`ask_human` loses nothing.

## Documentation

| | |
|---|---|
| [Language reference](docs/language.md) | syntax, semantics, and how it differs from Python |
| [Standard library](docs/stdlib.md) | the eight modules and the defect each one fixes |
| [CLI reference](docs/cli.md) | commands, flags, `kora.toml`, editor setup |
| [Decisions](DECISIONS.md) | why the language is the way it is, and what was traded away |
| [DECISIONS.md](DECISIONS.md) | the frozen design and why each call was made |
| [AGENTS.md](AGENTS.md) | contributing: what a language change has to touch |
| [examples/](examples) | thirteen runnable programs, in order |

## Agents, tools, and budgets

```python
tool priority_for(severity: str) -> int:
    "Map a severity label to the on-call priority number."
    if severity == "high":
        return 1
    return 3

agent triage(raw: str) -> str:
    budget: max_tokens = 4000, max_steps = 4
    t: Ticket = analyze(raw, "classify this ticket", tools=[priority_for])
    ...

def main():
    with budget(max_tokens = 20000):
        results = parallel for t in tickets:
            return triage(t)
```

Each branch of `parallel for` runs on its own thread with its own heap, so
there is no shared mutable state to guard. All branches draw from one token
budget, and results come back in input order.

## Images

Most of what people hand a model is a picture — a receipt, a screenshot, a
scanned form. So an image is an ordinary value, not an integration:

```python
use fs
use csv

agent classify(path: str) -> Row:
    match fs.image(path):
        case Ok(picture):
            r: Receipt = analyze(picture, "read this receipt", model="vision")
            ...

def main():
    match fs.glob("dataset/*.png"):
        case Ok(paths):
            rows = parallel for p in paths:
                return classify(p)
```

No base64, no hand-built request body, no sidecar. The type comes from the
file's magic bytes rather than its extension, `model="vision"` names a role
that `kora.toml` fills, and the cassette is keyed on the image bytes — so
editing a receipt re-asks the model while an untouched one replays for free.

## Classified data

Sensitive values cannot reach a model unless a scoped `declassify` block
releases them to a sink the project policy allows:

```python
type Employee:
    name: str
    classified salary: int

agent review(emp: Employee) -> str:
    declassify emp.salary as pay for local_model:
        a: Assessment = analyze({"pay": pay}, "assess against market")
```

The label is transitive: slicing, arithmetic, f-strings, containers, and
function returns all carry it, so laundering does not work.

```
error: classified data cannot reach model sink `local_model` (no declassify in scope)
  --> payroll.ko:12:20
   |
12 |     r: R = analyze(f"value is {ssn}", "anything")
   |                    ^
   = hint: wrap it in `declassify <value> for local_model:`
```

Sink policy lives in `kora.toml`, so salary data can reach the on-box model
and never a vendor API:

```toml
[sinks]
local_model = { allow = ["classified"] }
openai      = { allow = ["internal"], deny = ["classified"] }
```

`redact()` is the easy path when the model only needs shape, not values:
it replaces sensitive leaves with placeholders (`<NUM_1>`), so nothing
sensitive leaves the process and no declassification is needed.

`kora audit <file.ko>` lists every declassification site in a program.
The list is complete, not best-effort, because every release goes through a
`declassify` block the parser can see.

## Durable execution

A program can stop and wait for a person. The process exits; the run survives.

```python
a: Assessment = analyze(request, "assess refund risk")

if assessment.risk != "low":
    # The program sleeps here -- hours or days. The process may exit.
    decision = ask_human("approve this refund?", assessment.reason)
```

```bash
kora run --durable examples/04_durable_approval.ko   # 30s of model work, then parks
kora runs examples/04_durable_approval.ko            # see what is waiting
kora answer examples/04_durable_approval.ko <id> yes # resumes in 0.02s
```

Resuming does not re-pay for work already done, and does not reprint what you
already saw. Kill the process with `SIGKILL` mid-run and
`kora run --durable --resume <id>` picks up from the last completed effect.

Durability is replay-based: every effect is journaled, and a resumed run
re-executes with those effects served from the journal. The contract is that
code between effects is deterministic — the same one Temporal makes.

## Standard library

```python
use json
use fs
use time
use re
```

Eight modules, each native (Rust-backed) and each fixing a specific, known
defect rather than reimplementing it:

- **`http`** — a timeout always exists (you can change it, not omit it); a
  non-2xx is `Err`, not a response object that flows onward; retries with
  backoff are built in, and only for idempotent methods; private address
  ranges are refused, so a stray URL cannot reach the cloud metadata service
- **`json`** — errors quote the offending text and name the path
  (`$.users.0.email: not found`), instead of a byte offset on one-line JSON
- **`csv`** — you declare the row type; nothing is guessed, so a zip code
  keeps its leading zero, and a bad field names the row and column
- **`sql`** — parameters only. A value from outside cannot become query text
  at all, so injection is unavailable rather than discouraged
- **`fs`** — writes are atomic (temp + rename), missing files name the path,
  and `..` in a path is refused; `fs.glob` and `fs.list` are sorted, because
  filesystem order differs between machines and an agent program fans that
  list out across threads
- **`env`** — a variable whose name looks like a credential comes back
  `classified`, so it cannot reach a log line by accident
- **`re`** — linear-time engine, so `(a+)+$` against hostile input answers
  instead of hanging
- **`time`** — instants are absolute; there is no naive type to misuse, and
  `now()` is journaled so durable replay stays correct

Two rules hold across all of them. Data that enters from outside is
`unverified` and cannot reach a dangerous sink until it is narrowed:

```python
contents = fs.read("config.txt")   # unverified: it came from outside
fs.read(contents)                  # error: a path that came from outside

sql.query(db, f"select * from t where id = {user_input}")
# error: a statement built from outside data
# hint:  pass the value as a parameter instead
```

And failure is a value, never a silent `None` or a forgotten exception:

```python
match fs.read(path):
    case Ok(text):
        ...
    case Err(why):
        print(why)
```

## Splitting a program across files

```python
# lib/tax.ko
RATE = 0.2

def with_tax(amount: float) -> float:
    return amount * (1.0 + RATE)
```

```python
# main.ko
use "./lib/tax.ko" as tax

def main():
    print(tax.with_tax(100.0))
```

A quoted path is a file, a bare word is a stdlib module, so the two can never
be confused. Paths resolve relative to the file that writes them, not the
working directory: a program is a directory, and it moves whole.

Each file keeps its own top-level names, so importing a module can never
change what the code inside it means. A file's top level runs once per run
however many files import it. Cycles are an error with the chain that caused
them, not a half-built module.

Budgets, labels, and the journal cross file boundaries unchanged: an imported
agent spends from the same budget, `classified` still propagates, and
`kora audit` covers every imported file.

## MCP

```python
use mcp github as gh

t: Ticket = analyze(issue, "triage this", tools=gh.tools)
```

Kora already has `tool` as a first-class construct, and an MCP server is a bag
of tools, so one implementation reaches hundreds of maintained servers rather
than adding integrations one at a time. Tool schemas and descriptions come
from the server; how to launch it lives in `kora.toml`, so credentials stay
out of source.

A server runs in its own process, so it is a sink of its own. Releasing a
secret to the model does not release it to the server.

## Python

```python
use python statistics as stats

match stats.mean(readings):
    case Ok(m):
        print(m)
    case Err(why):
        print(why)
```

The long-tail escape hatch, as a sidecar rather than an embed. Python runs in
its own process and values cross as JSON, so Kora keeps no GIL, durable runs
stay resumable, and labels stay meaningful. A Python exception is `Err`.

Chosen over embedding CPython deliberately — see
[DECISIONS.md](DECISIONS.md#ecosystem-strategy) for why embedding would break
three of the four thesis pillars.

## Testing

```bash
kora test examples/07_tests.ko
```

```python
test "a high severity ticket routes to P1":
    with mock analyze -> Ok(Ticket("high", "everything is down")):
        result = triage("HELP")
        assert result == "P1 everything is down", f"got: {result}"

test "an uncertain result does not crash":
    with mock analyze -> Uncertain("too vague"):
        assert triage("hello?") == "needs a human: too vague"
```

Model calls replay from the cassette, so a suite costs nothing and gives the
same answer every time. Mocks are checked against the declared result type:

```
error: the mock returns `Other`, but this call site declares `Ticket`
error: the mock is missing field `summary`
```

An untyped mocking framework cannot catch that — it has no idea what the call
site expected, so a mock that drifts from reality keeps passing. And the
failure paths nobody tests today (`Uncertain`, `Exhausted`, `Failed`) are
forceable, because they are ordinary values.

## Editor support

The VS Code extension in `editors/vscode` gives syntax highlighting, run and
test buttons, a debugger, and a language server providing:

- **Diagnostics** as you type — parse errors, undefined names, undeclared
  types, unknown modules and module functions, each with the same hint the
  compiler would print
- **Hover** — signatures and docstrings, including `classified` markers on
  type fields
- **Go to definition** for functions, agents, tools, and types
- **Outline** — agents, tools, types, and tests, in file order
- **Completion** — keywords, builtins, declared symbols, and module functions
  after `json.`

Install the binary, then link the extension and restart VS Code — extension
contributions are read once, at startup:

```bash
cargo install --path crates/kora-cli
ln -s "$PWD/editors/vscode" ~/.vscode/extensions/kora-lang.kora-lang-0.3.1
```

Set `kora.serverPath` to an absolute path if the `kora` first on your `PATH` is
an older release: `kora | grep "kora dap"` prints nothing when it cannot debug.

The editor and the compiler share one analysis pass, so their answers cannot
disagree.

### Debugging

Press F5 on a `.ko` file. Breakpoints in the gutter, step over / into / out,
the call stack, and a variables pane where lists, dicts, and typed objects
expand. `classified` values are labelled as such, so it is visible at a glance
which data is under a flow restriction.

The adapter is `kora dap`, the same binary, speaking the Debug Adapter
Protocol — so any DAP-capable editor can drive it, not only VS Code. Setting
`"replay": true` in the launch configuration serves model calls from the
cassette, which makes stepping through agent code free and repeatable.

The debugger reads a snapshot of each frame rather than reaching into a live
interpreter, which is why inspecting a paused program cannot perturb it: there
is no way for the variables pane to run code.

## Tracing

```bash
kora run --trace examples/03_salary_review.ko
kora trace examples/03_salary_review.ko
```

```
review                                 812ms
  declassify                             0ms
  analyze Assessment                   806ms
review                                 794ms
  declassify                             0ms
  analyze Assessment                   790ms
```

Spans come from the runtime, not from hand-written instrumentation, so they
cannot drift from the code. Model calls follow the OpenTelemetry GenAI
semantic conventions, so existing dashboards read them without translation.
Point `[telemetry] exporter = "otlp"` at a collector when you have one.

The exporter is a labeled sink, which is the part worth knowing: a
`classified` value cannot become a span attribute, so prompt text and secrets
cannot leak into an observability vendor by accident.

## Status

Early development, pre-alpha, built for personal use first. Everything
documented here works and is covered by tests; the test suite never touches
the network.

Not built yet: classes, list comprehensions, documents (PDF) as values, and
`try`/`except`. See
[DECISIONS.md](DECISIONS.md) for what is planned and what is deliberately
excluded.

## Layout

```
crates/kora-syntax    lexer, parser, AST
crates/kora-types     name resolution and editor checks
crates/kora-runtime   interpreter, agents, budgets, labels, journal, stdlib
crates/kora-models    OpenAI + Ollama clients, schema-constrained output
crates/kora-lsp       language server (diagnostics, hover, definition)
crates/kora-dap       debug adapter (breakpoints, stepping, variables)
crates/kora-mcp       Model Context Protocol client
crates/kora-python    Python sidecar worker
crates/kora-cli       the `kora` binary
editors/vscode        VS Code extension
examples/             runnable .ko programs
benches/              performance benchmarks and their baseline
docs/                 language, stdlib, and CLI references
site/                 the public documentation site
scripts/              documentation checks, benchmarks, packaging
```

## Build

```bash
cargo build
cargo test --workspace
./target/debug/kora --version
```

Before opening a pull request, the checks CI runs:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
python3 scripts/check_docs.py     # the docs still describe the language
kora check examples/*.ko
```

The documentation site lives in `site/` and is its own project:

```bash
cd site
pnpm install
pnpm dev
```

## Performance

The interpreter is a tree walker today; a bytecode VM is the next stage. That
is a published number, not an opinion:

```bash
cargo build --release -p kora-cli
python3 scripts/bench.py                    # measure this build
python3 scripts/bench.py --against main     # A/B, same machine, same run
```

Twelve programs cover arithmetic, calls, collections, strings, the stdlib
modules, `parallel for` against its sequential twin, and `--durable` against
an unjournaled run. CI compares every pull request against its base commit on
one runner and fails a benchmark more than 1.25x slower. Current numbers and
how to read them: [benches/README.md](benches/README.md).

## License

MIT
