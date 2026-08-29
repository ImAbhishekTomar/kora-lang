# Examples

In order. Each one adds a layer, so reading them top to bottom is a tour of
the language.

Model calls replay from the committed cassettes, so everything here runs with
no API key and no model installed.

| | | |
|---|---|---|
| [00_basics.ko](00_basics.ko) | the deterministic core: types, functions, loops, f-strings | `kora run` |
| [01_expense_check.ko](01_expense_check.ko) | a typed model call, and handling `Uncertain` | `kora run --replay` |
| [02_triage.ko](02_triage.ko) | agents, a tool, `parallel for`, token budgets | `kora run --replay` |
| [03_salary_review.ko](03_salary_review.ko) | classified data reaching only the local model | `kora run --replay` |
| [04_durable_approval.ko](04_durable_approval.ko) | stopping for a person, and resuming days later | `kora run --durable` |
| [05_stdlib.ko](05_stdlib.ko) | `json`, `fs`, `time`, `re` and the defects they fix | `kora run` |
| [06_stdlib_safety.ko](06_stdlib_safety.ko) | `csv` and `http`, and the attacks that are refused | `kora run` |
| [07_tests.ko](07_tests.ko) | `test` blocks and typed mocks | `kora test` |
| [08_mcp.ko](08_mcp.ko) | a model calling tools from a real MCP server | `kora run` |
| [09_python.ko](09_python.ko) | calling Python through the sidecar | `kora run` |
| [10_modules.ko](10_modules.ko) | splitting a program across files | `kora run` |
| [11_receipt_classifier.ko](11_receipt_classifier.ko) | reading a receipt and extracting a typed expense record | `kora run --replay` |
| [12_receipt_images.ko](12_receipt_images.ko) | the same job from the *pictures*: `fs.glob`, `fs.image`, a vision model | `kora run --replay` |
| [13_packages.ko](13_packages.ko) | naming a dependency, and letting the source decide what is used | `kora run` |

## The pattern set

[`patterns/`](patterns) is a second, parallel tour: the seven agent and
workflow patterns from LangGraph's
[Workflows and agents](https://docs.langchain.com/oss/python/langgraph/workflows-agents)
guide, written in Kora so the two can be read side by side. Prompt chaining,
parallelization, routing, orchestrator-worker, evaluator-optimizer, and the
agent loop.

Every one runs its own tests with no model, no API key, and no cassette:

```bash
kora test examples/patterns/07_agent.ko
```

Its [README](patterns/README.md) also says what is *not* good yet — nested
schemas, the closed tool loop, single-valued mocks — which is the honest half
of a comparison.

```bash
kora run examples/00_basics.ko
kora run --replay examples/03_salary_review.ko
kora test examples/07_tests.ko
kora run --replay examples/11_receipt_classifier.ko
kora run --replay examples/12_receipt_images.ko
```

## The image one

`12_receipt_images.ko` classifies the PNGs in [`receipts/`](receipts) rather
than a text transcript of them. It is the only example that needs a vision
model to re-record, and `kora.toml` names one under `vision`:

```toml
[models]
vision = "local:gemma4:12b"
timeout_secs = 900
```

Replay needs neither. The cassette is keyed on the image bytes, so editing a
receipt re-asks the model while the untouched one stays free.

## The module one

`10_modules.ko` imports [`lib/payroll.ko`](lib/payroll.ko). Run it from
anywhere: the path is resolved against the importing file, not the working
directory.

## The durable one

`04_durable_approval.ko` is the only one that does not simply run to
completion — it stops and waits for a person, which is the point:

```bash
kora run --durable examples/04_durable_approval.ko
kora runs examples/04_durable_approval.ko
kora answer examples/04_durable_approval.ko <run-id> yes
```

It needs a real model, since it is not backed by a cassette. The resume costs
nothing: the model work it already did is served from the journal.

## The MCP one

`08_mcp.ko` starts a real MCP server (`npx @modelcontextprotocol/server-filesystem`)
and lets a model drive its tools. It needs Node and a real model, and takes
about a minute — most of it the local model thinking.

## Re-recording

A cassette is keyed on call site, model, prompt, and input, so changing
`[models] default` in `kora.toml` invalidates them:

```bash
kora run --record examples/01_expense_check.ko
```

Cassettes are committed on purpose. They are what makes the suite
reproducible and free.
