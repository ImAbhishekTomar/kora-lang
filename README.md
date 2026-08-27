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
