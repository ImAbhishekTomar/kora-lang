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
