# Kora

[![CI](https://github.com/ImAbhishekTomar/kora-lang/actions/workflows/ci.yml/badge.svg)](https://github.com/ImAbhishekTomar/kora-lang/actions/workflows/ci.yml)

> **Pre-alpha:** Kora is an experimental language. It is not ready for
> production workloads, and it has not yet earned independent security review
> or adoption evidence.

Kora is a small language for policy-safe, replayable AI workflows over
sensitive documents. Its narrow goal is to make extraction and review jobs for
claims, invoices, compliance, and internal operations easier to inspect and
harder to run outside policy.

It is useful only if you need several of these guarantees together:

- declared input and output shapes checked before execution;
- explicit outcomes for model uncertainty, budget exhaustion, and outages;
- information-flow labels and named declassification sinks;
- record/replay tests with no live model call;
- opt-in durable execution for effects and human approval;
- one shared budget across parallel work.

If you only need to call a model, use Python or TypeScript. They have mature
ecosystems and far lower adoption cost.

## A real Kora workflow

```kora
type Employee:
    name: str
    classified salary: int

type Assessment:
    band: str
    rationale: str

def review(employee: Employee) -> str:
    declassify employee as approved for local_model:
        result: Assessment = analyze(
            approved,
            "assess salary band and explain the result"
        )

    match result:
        case Ok(assessment):
            return f"{employee.name}: {assessment.band}"
        case Uncertain(reason):
            return f"human review: {reason}"
        case Exhausted(meter):
            return f"budget exhausted: {meter}"
        case Failed(why):
            return f"provider failed: {why}"

def main():
    print(review(Employee("Maya", 142000)))
```

The declassification is valid only when the project policy names the same
sink:

```toml
[models]
default = { name = "qwen2.5:7b", endpoint = "http://localhost:11434", api = "ollama" }

[sinks]
local_model = { allow = ["classified"] }
```

Then check and replay the committed examples:

```bash
kora check examples/03_salary_review.ko
kora run --replay examples/03_salary_review.ko
kora test examples/07_tests.ko
```

`kora run` checks the entry file before starting effects. `kora check`
currently checks local declared types, calls, constructors, field access,
control-flow placement, and direct classified flow into `analyze`. Unknown
values from dynamic integrations stay runtime-checked. Sink policy is enforced
again at runtime.

## What is real today

- A parser, static checker, interpreter, CLI, language server, debugger, and
  VS Code extension.
- Typed model results validated against Kora declarations.
- `Ok`, `Uncertain`, `Exhausted`, and `Failed` outcomes.
- Scoped token, call, step, time, and context budgets.
- Classified values, named sinks, runtime enforcement, and audit output.
- Record and replay cassettes for deterministic model tests.
- `parallel for` with isolated worker values and shared budget accounting.
- Durable journals for model calls, tools, writes, reads, output, and
  `ask_human` when `--durable` is requested.
- Standard modules for JSON, CSV, files, HTTP, SQL, environment, time, regex,
  notes, PDF, YAML, and XML.
- Source packages with lockfiles, checksums, capability grants, and no install
  scripts or in-process native libraries.

## What is not proven yet

- No production users or published design-partner case study.
- No independent security audit.
- No stable language or package compatibility promise.
- A tree-walking interpreter, not a high-performance general-purpose runtime.
- A small ecosystem with fewer integrations than established frameworks.
- Durable execution is opt-in, not automatic.
- Static checking is conservative, not whole-program verification across
  Python, MCP, helpers, or network responses.

These are release gates, not footnotes. See the [roadmap](https://kora-lang.vercel.app/roadmap)
for the validation plan.

## Install

```bash
curl -fsSL https://raw.githubusercontent.com/ImAbhishekTomar/kora-lang/main/scripts/install.sh | sh
```

Or use a package manager:

```bash
brew install ImAbhishekTomar/tap/kora
cargo install kora-cli
npm install -g @abhishektomar/kora-cli
```

Prebuilt archives for Linux, macOS, and Windows are on the
[releases page](https://github.com/ImAbhishekTomar/kora-lang/releases).

## Start with evidence

The repository includes runnable programs and recorded model responses:

```bash
kora run examples/00_basics.ko
kora run --replay examples/01_expense_check.ko
kora run --durable examples/19_durable_pipeline.ko
```

The first two require no API key. The durable example performs real local file
effects, so run it in a disposable working directory when evaluating it.

## Documentation

| Resource | Purpose |
| --- | --- |
| [Start here](https://kora-lang.vercel.app/start-here) | Install and run the first checked program |
| [Language reference](docs/language.md) | Syntax and semantics |
| [Standard library](docs/stdlib.md) | Built-in modules and safety behavior |
| [CLI reference](docs/cli.md) | Commands, config, replay, and durability |
| [Decisions](DECISIONS.md) | Design choices and accepted trade-offs |
| [Comparison](https://kora-lang.vercel.app/comparison) | When Kora is and is not a sensible choice |
| [Examples](examples) | Runnable workflows and tests |

## Contributing

Read [AGENTS.md](AGENTS.md) before changing the language. A construct is not
finished until its compiler, runtime, editor, docs, examples, and tests agree.

```bash
cargo fmt --all
cargo clippy --all-targets --all-features
cargo test
cargo build && python3 scripts/check_docs.py --kora ./target/debug/kora
```

## License

Apache-2.0
