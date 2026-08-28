# The Kora language

A reference, not a tutorial. For a guided tour, read the numbered programs in
[`examples/`](../examples).

Kora reads like Python. This page covers what is the same, what is different,
and the constructs Python has no equivalent for.

---

## Contents

- [Values and types](#values-and-types)
- [Control flow](#control-flow)
- [Functions, agents, and tools](#functions-agents-and-tools)
- [Model calls](#model-calls)
- [Outcomes and `match`](#outcomes-and-match)
- [Parallelism](#parallelism)
- [Budgets](#budgets)
- [Classified data](#classified-data)
- [Durability](#durability)
- [Modules](#modules)
- [Tests](#tests)
- [Built-in functions](#built-in-functions)
- [Differences from Python](#differences-from-python)

---

## Values and types

```python
count = 42                  # int
ratio = 3.14                # float
name = "ada"                # str
ready = True                # bool
nothing = None
items = [1, 2, 3]           # list
lookup = {"a": 1, "b": 2}   # dict, string keys only
```

Annotations are optional on assignment and checked at runtime:

```python
total: int = 0
label: str = "high"
```

### Declared types

```python
type Employee:
    name: str
    role: str
    salary: int
```

Constructed positionally, in declaration order:

```python
e = Employee("Ada", "staff engineer", 165)
print(e.name)
e.salary = 170
```

Field types may be `str`, `int`, `float`, `bool`, `list[str]`, or another
declared type.

### Strings

```python
greeting = f"hello, {name}"
first = name[0]
part = name[1:3]
joined = "a" + "b"
repeated = "ab" * 3
```

Escapes: `\n`, `\t`, `\r`, `\\`, `\'`, `\"`, `\0`.

---

## Control flow

```python
if score > 90:
    grade = "A"
elif score > 80:
    grade = "B"
else:
    grade = "C"

for item in items:
    print(item)

while remaining > 0:
    remaining -= 1

for i in range(10):
    if i == 3:
        continue
    if i > 6:
        break
```

`in` works on lists, dicts (keys), and strings. Blocks are indentation-based;
**tabs are rejected** rather than silently mixed with spaces.

---

## Functions, agents, and tools

Three keywords, one shape. They differ in what they may do, not how they look.

```python
def add(a: int, b: int) -> int:
    return a + b
```

An **agent** may call models and suspend, and may carry a budget:

```python
agent triage(raw: str) -> str:
    budget: max_tokens = 4000, max_steps = 5
    ...
```

A **tool** is exposed to models. Its signature becomes the schema and its
docstring becomes the description, so there is no boilerplate to write:

```python
tool priority_for(severity: str) -> int:
    "Map a severity label to the on-call priority number."
    if severity == "high":
        return 1
    return 3
```

Tool parameters must be typed — a model cannot be told what to pass otherwise.

`main()` runs automatically if defined.

---

## Model calls

```python
result: Insight = analyze(data, "find revenue anomalies by region")
```

The declared type becomes a JSON schema the model must satisfy, so the result
is ordinary typed data. **The annotation is required**: without it there is no
schema to constrain the model.

With tools the model may call:

```python
t: Ticket = analyze(raw, "classify this ticket", tools=[priority_for])
```

---

## Outcomes and `match`

A model call returns one of three things. Failure is a value, never an
exception:

```python
match result:
    case Ok(value):
        print(value.summary)
    case Uncertain(reason):
        print(f"the model declined: {reason}")
    case Exhausted(meter):
        print(f"budget ran out of {meter}")
```

Patterns:

| Pattern | Matches |
|---|---|
| `case Ok(v):` | a constructor, binding its payload |
| `case 3:` `case "high":` `case True:` | a literal |
| `case name:` | anything, binding it |
| `case _:` | anything |

An unmatched value is an error, so adding a case to a type does not silently
fall through. Stdlib functions use `Ok` / `Err` in the same shape.

You can construct outcomes yourself — useful in tests:

```python
x = Ok(Ticket("high", "down"))
y = Err("not found")
```

---

## Parallelism

Real OS threads. No GIL, no `async`, no `await`.

```python
results = parallel for ticket in tickets:
    return triage(ticket)
```

Each branch is an isolated agent with its own heap, seeded with a **copy** of
what it needs. Nothing is shared, so there is no lock to take and no data race
to reason about. Results come back in input order, so a parallel run reads
like a sequential one.

Mutating a captured value inside a branch changes only that branch's copy.

> Running many branches against one local model is slower than it looks:
> they contend for the same GPU. Cassettes make repeat runs free.

---

## Budgets

Denominated in tokens, because tokens are what the runtime can measure.
Money is a display layer, never enforcement.

```python
agent triage(raw: str) -> str:
    budget: max_tokens = 20_000, max_calls = 10, max_steps = 5
```

```python
with budget(max_tokens = 500_000):
    results = parallel for e in emails:
        return summarize(e)
```

Budgets nest, and a child may only **tighten**. A `parallel for` shares one
pot, so concurrent agents stop collectively.

Introspection makes degrade-gracefully logic an ordinary `if`:

```python
if tokens_remaining() < 1000:
    break
print(f"spent {tokens_spent()} over {calls_spent()} calls")
```

Budgets are opt-in: a program without one runs unbounded.

---

## Classified data

Two independent labels. **Confidentiality** runs outward (secrets must not
leave); **integrity** runs inward (untrusted data must not be acted on).

```python
type Employee:
    name: str
    classified salary: int

classified api_key = "sk-..."
```

A classified value cannot reach a model, a file, a serializer, or a database
without an explicit release:

```python
declassify emp.salary as pay for local_model:
    a: Assessment = analyze({"pay": pay}, "assess against market")
```

The block is the exposure: the binding does not escape it, and the value is
released **to that sink only** — it keeps its label and records what it was
approved for, so a secret released to a model still cannot be written to a
file inside the same block. Which sinks accept
which labels lives in `kora.toml`:

```toml
[sinks]
local_model = { allow = ["classified"] }
openai      = { allow = ["internal"], deny = ["classified"] }
```

The label is **transitive** — it survives slicing, arithmetic, f-strings,
containers, function returns, and the copy between agents:

```python
disguised = f"the value is {ssn}"
analyze(disguised, "...")   # still refused
```

`redact()` is the easy path when the model needs shape, not values. It
replaces sensitive leaves with placeholders (`<NUM_1>`), so nothing sensitive
leaves and no declassification is needed.

Data entering from outside — file contents, HTTP bodies, parsed JSON, model
output — is **unverified** and cannot reach a dangerous sink until narrowed:

```python
contents = fs.read("config.txt")
fs.read(contents)             # refused: a path from outside the program
sql.query(db, contents)       # refused: a statement from outside the program
```

`kora audit <file.ko>` lists every declassification site in a program.

---

## Durability

```python
decision = ask_human("approve this refund?", details)
```

The program stops, the process may exit, and it resumes on the next line when
the answer arrives — with everything already computed intact, and without
re-paying for model calls.

```bash
kora run --durable program.ko
kora runs program.ko
kora answer program.ko <run-id> yes
```

Durability is replay-based: every effect is journaled, and a resumed run
re-executes from the top with those effects served from the journal. **Code
between effects must be deterministic.** Anything nondeterministic must go
through the journal — `time.now()` already does.

A killed process resumes with `kora run --durable --resume <run-id>`.

---

## Modules

```python
use json
use json as j
```

Eight modules: `json`, `csv`, `http`, `sql`, `fs`, `env`, `time`, `re`.
Every fallible call returns `Ok` / `Err`. See
[the standard library reference](stdlib.md).

### MCP servers

```python
use mcp github as gh

t: Ticket = analyze(issue, "triage this", tools=gh.tools)
```

`gh.tools` is every tool the server offers; `gh.search_issues` offers one.
How to launch a server lives in `kora.toml`, so credentials stay out of
source:

```toml
[mcp.github]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-github"]
env = { GITHUB_PERSONAL_ACCESS_TOKEN = "$GITHUB_TOKEN" }
```

A server runs in its own process, so it is **a sink of its own**. Releasing a
secret to the model does not release it to the server:

```python
declassify salary as pay for local_model:
    analyze(pay, "...", tools=gh.tools)
# error: classified data cannot reach MCP server `github`
```

### Python

```python
use python statistics as stats
use python os.path as pypath      # dotted names need an alias

match stats.mean([1, 2, 3, 4]):
    case Ok(m):      print(m)
    case Err(why):   print(why)   # ValueError: math domain error
```

Python runs in its own process. Values cross as JSON — data in, data out —
so there are no live Python objects on this side and no callbacks back into
Kora. That boundary is what keeps the rest of the language intact: no GIL,
durable runs still resumable, labels still meaningful.

The cost, accepted knowingly: per-call serialization, and no
`df.groupby().apply(lambda ...)`.

A Python exception is `Err`, not a crash. Results are `unverified`. Python is
its own sink, so a secret released to a model has not been released to
Python. Set `[python] command` to point at a virtualenv's interpreter.

---

## Tests

```python
test "a high severity ticket routes to P1":
    with mock analyze -> Ok(Ticket("high", "everything is down")):
        result = triage("HELP")
        assert result == "P1 everything is down", f"got: {result}"
```

```bash
kora test program.ko
```

Mocks are checked against the declared result type, so one that drifts from
reality fails instead of passing. Model calls replay from the cassette, so a
suite costs nothing. Under `kora run`, `test` blocks are inert.

---

## Built-in functions

| | |
|---|---|
| `print(...)` | write a line |
| `len(x)` | length of a list, string, or dict |
| `range(n)` / `range(a, b)` | a list of integers |
| `str(x)` `int(x)` `float(x)` `bool(x)` | conversions |
| `abs` `min` `max` `sum` `sorted` | arithmetic and ordering |
| `append(xs, v)` | add to a list |
| `keys(d)` `values(d)` | dict access |
| `redact(x)` | mask sensitive leaves |
| `tokens_spent()` `tokens_remaining()` `calls_spent()` | budget state |
| `ask_human(question, context)` | suspend for a person |

---

## Differences from Python

**Deliberately the same:** indentation blocks, `def`, `if`/`elif`/`else`,
`for`/`while`, f-strings, list and dict literals, slicing, `in`, `match`.

**Deliberately different:**

| | |
|---|---|
| No GIL | `parallel for` uses real threads |
| No `async`/`await` | there is no function-colour split |
| No exceptions | failure is a value; `assert` is the only raise |
| Tabs rejected | rather than silently mixed with spaces |
| Dict keys are strings | no arbitrary hashable keys |
| No classes | `type` blocks hold data; functions act on it |
| No comprehensions yet | use a `for` loop |
| No `import` of other `.ko` files yet | one file per program |
| Methods are functions | `append(xs, v)`, not `xs.append(v)` |
| Type annotations are checked | not hints |

**Not yet built:** classes, list comprehensions, multi-file programs,
user-defined generics, `try`/`except`, keyword arguments on user functions
(only `analyze` takes them), `*args`/`**kwargs`, integer keys in dicts.
