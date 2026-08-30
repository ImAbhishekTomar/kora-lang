# Agent and workflow patterns

The patterns from LangGraph's
[Workflows and agents](https://docs.langchain.com/oss/python/langgraph/workflows-agents)
guide, written in Kora. Same problems, same order, so the two can be read side
by side.

A workflow has a predetermined path. An agent decides its own. The first six
here are workflows; the last is an agent.

| | Pattern | What it shows |
|---|---|---|
| [01_augmented_llm.ko](01_augmented_llm.ko) | augmented LLM | a declared result type *is* the schema; a `tool` signature *is* the tool |
| [02_prompt_chaining.ko](02_prompt_chaining.ko) | prompt chaining | calls in sequence with a gate, each one degrading to the last good result |
| [03_parallelization.ko](03_parallelization.ko) | parallelization | `parallel for` on real threads, one shared budget, results in input order |
| [04_routing.ko](04_routing.ko) | routing | a constrained route field, a `match`, and a guard that asks a person |
| [05_orchestrator_worker.ko](05_orchestrator_worker.ko) | orchestrator-worker | a plan computed at runtime, fanned out one worker per subtask |
| [06_evaluator_optimizer.ko](06_evaluator_optimizer.ko) | evaluator-optimizer | generate, grade, repeat — bounded by the budget rather than a hop count |
| [07_agent.ko](07_agent.ko) | agent | a model looping over tools until it has an answer |

Every file runs its own tests with no model, no API key, and no cassette:

```bash
kora test examples/patterns/07_agent.ko
```

Each also has a `main()`, so it runs against a real model once `kora.toml`
points at one:

```bash
kora run examples/patterns/07_agent.ko
```

## What is different from the LangGraph versions

**There is no graph.** No `StateGraph`, no `add_node`, no `add_edge`, no
shared `State` TypedDict. Control flow is the graph: `if` is the conditional
edge, `while` is the loop back, `parallel for` is the fan-out and the fan-in.
The pattern that shrinks the most is orchestrator-worker, which needs
LangGraph's `Send` API, a second `WorkerState`, and an
`Annotated[list, operator.add]` reducer, and here is a `parallel for` over the
planned list.

**Failure is a value, in four shapes.** Every model call returns `Ok`,
`Uncertain` (the model declined), `Exhausted` (the budget ran out), or
`Failed` (the provider did not answer, after retrying). None of them raise, so
"what happens when this step fails" is written down in every pattern above
rather than left to a `try` somewhere else. The `else` form handles all three
failures in one line; `match` separates them when the difference matters.

**Budgets are shared and enforced.** `budget: max_tokens = 20000` on an agent,
or `with budget(...)` around a fan-out, is one pot that concurrent work draws
down together. Exhaustion arrives as a value, so partial work survives. There
is no equivalent in the LangGraph versions at any price.

**The failure paths are tested.** Every file here forces `Uncertain`,
`Exhausted`, and `Failed` with typed mocks — the paths that normally go
untested because provoking them means making a real model misbehave.

## What is not good yet

Reading these should show the rough edges as well as the good parts.

- **The tool loop is closed.** `07_agent.ko` cannot approve, log, or rewrite
  an individual tool call before it runs. LangGraph's `ToolNode` and its
  `ToolRuntime` state injection have no equivalent yet.
- **A mock is one value.** `mock analyze -> Ok(...)` is checked against every
  call site it reaches, so a flow calling `analyze` for two different types
  can only have its failure paths tested — see the note in
  `06_evaluator_optimizer.ko`. Failure mocks carry no type, so those work
  everywhere.
- **An agent is not a tool.** `tools=[some_agent]` is refused, so the
  supervisor pattern — one agent delegating to specialists — has to be spelled
  as a `tool` that wraps the call, and the wrapper cannot carry its own
  budget.
- **No streaming.** There is no equivalent of `stream_events`, so nothing here
  can render tokens as they arrive.
