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
  Prompt-injection-relevant flows become compile errors.
- **Native budgets** — token-denominated, lexically scoped, shared across
  parallel work. Exhaustion is a value, not a crash; partial work survives.
- **Typed model calls** — `analyze(data, "prompt")` returns your declared
  type or an explicit `Uncertain`. No raw-string parsing, no confidence theater.
- **Record/replay + OpenTelemetry built into the runtime** — deterministic
  CI with zero tokens; every agent and call is a span.

## Try it

```bash
cargo build
./target/debug/kora run examples/00_basics.ko          # deterministic core
./target/debug/kora run --replay examples/01_expense_check.ko   # model call, from cassette
```

Model calls need either an `OPENAI_API_KEY` or a running [Ollama](https://ollama.com).
Point `[models] default` in `kora.toml` at whichever you have, then:

```bash
kora run --record --report examples/01_expense_check.ko   # call the model, save a cassette
kora run --replay --report examples/01_expense_check.ko   # re-run free and deterministic
```

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
  and `..` in a path is refused
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
    case Ok(text):  ...
    case Err(why):  print(why)
```

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
failure paths nobody tests today (`Uncertain`, `Exhausted`) are forceable,
because they are ordinary values.

## Editor support

The VS Code extension in `editors/vscode` gives syntax highlighting, run and
test buttons, and a language server providing:

- **Diagnostics** as you type — parse errors, undefined names, undeclared
  types, unknown modules and module functions, each with the same hint the
  compiler would print
- **Hover** — signatures and docstrings, including `classified` markers on
  type fields
- **Go to definition** for functions, agents, tools, and types
- **Outline** — agents, tools, types, and tests, in file order
- **Completion** — keywords, builtins, declared symbols, and module functions
  after `json.`

Install the binary, then link the extension:

```bash
cargo install --path crates/kora-cli
ln -s "$PWD/editors/vscode" ~/.vscode/extensions/kora-lang
```

The editor and the compiler share one analysis pass, so their answers cannot
disagree.

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

Early development, pre-alpha. Built for personal use first. See
[DECISIONS.md](DECISIONS.md) for the frozen design and phase plan.

## Layout

```
crates/kora-syntax    lexer, parser, AST
crates/kora-types     type checker + classified labels
crates/kora-runtime   interpreter, agents, scheduler, budgets, cassettes
crates/kora-models    OpenAI + Ollama clients, schema-constrained output
crates/kora-lsp       language server
crates/kora-cli       the `kora` binary
editors/vscode        VS Code extension
examples/             sample .ko programs
```

## Build

```bash
cargo build
./target/debug/kora --version
```

## License

MIT
