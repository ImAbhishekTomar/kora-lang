# The Kora language

A reference, not a tutorial. For a guided tour, read the numbered programs in
[`examples/`](../examples).

Kora reads like Python. This page covers what is the same, what is different,
and the constructs Python has no equivalent for.

---

## Contents

- [Values and types](#values-and-types)
- [Images](#images)
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

### Field metadata and constraints

Fields may carry native metadata. Use the indented form when a field has
several entries:

```python
type Expense:
    merchant: str
        description: "Merchant identifier, exactly 12 alphanumeric characters"
        pattern: "^[A-Za-z0-9]{12}$"
    amount: float
```

For a short declaration, use the equivalent inline form:

```python
type Expense:
    merchant: str @description("Merchant identifier") @pattern("^[A-Za-z0-9]{12}$")
```

Both forms have identical behavior and can be mixed in one type. `description`
becomes model and editor guidance. `pattern` is a regular expression for a
`str` field and is enforced when Kora constructs an object, assigns that field,
or accepts a model response. Fields without metadata keep their existing
behavior. The native metadata available today is `description` and `pattern`.

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

## Images

An image is an ordinary value. `fs.image` loads one the way `fs.read` loads
text, and it goes into `analyze` like any other data:

```python
use fs

match fs.image("receipts/0.png"):
    case Ok(picture):
        receipt: Receipt = analyze(picture, "read this receipt", model="vision")
    case Err(why):
        print(why)      # no such file: receipts/0.png
```

PNG, JPEG, GIF, and WebP. The type is read from the file's magic bytes rather
than its extension, so a JPEG named `.png` is still sent as a JPEG. Contents
are `unverified` like any other file, and an image cannot be serialized —
`json.stringify` refuses it rather than emitting a megabyte of base64.

Images may sit anywhere in the data argument. They are extracted in the order
they appear, and the JSON the model sees carries an `<image>` marker in each
one's place:

```python
r: Comparison = analyze(
    {"front": front, "back": back, "claim": claim_id},
    "do these two photos show the same package?",
    model="vision"
)
```

`print` and the debugger show a summary — source, type, size — never the
bytes.

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

### Choosing a model

By default a call uses `[models] default` from `kora.toml`. A call that needs
a different one names a **role**, and the config says which model fills it:

```python
r: Receipt = analyze(picture, "read this receipt", model="vision")
```

```toml
[models]
default = "local:qwen3:8b"
vision  = "local:gemma4:12b"
```

`model=` takes a name declared in `[models]`, never a provider spec like
`"openai:gpt-4o"`. A vendor's model name in a source file is how a program
ends up needing an environment variable to choose between two providers. A
name that came from outside the program is refused: the model is a
destination, and model output should not be able to redirect the call.

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

### Your own files

Split a program across files and import one from another:

```python
# lib/tax.ko
RATE = 0.2

type Money:
    amount: float
    currency: str

def with_tax(amount: float) -> float:
    return amount * (1.0 + RATE)
```

```python
# main.ko
use "./lib/tax.ko" as tax

def main():
    print(tax.with_tax(100.0))
    m = tax.Money(12.5, "USD")
```

A quoted path is a file; a bare word is a stdlib module. The two can never be
confused for one another, and a path always needs `as <name>` — a path has no
natural bare name, so Kora does not invent one.

Rules worth knowing:

- **Paths resolve relative to the file that writes them**, never the working
  directory. A program is a directory, and it can be moved or vendored whole.
  Inside `lib/deep.ko`, `use "./inner.ko"` means `lib/inner.ko`.
- **Every top-level name is exported.** Functions, agents, tools, types, and
  top-level variables are all reachable as `alias.name`. There is no `export`
  keyword and no privacy marker yet.
- **A file reads its own top level, not its importer's.** Two files may bind
  `RATE` to different values; each one's functions see their own. Importing a
  module can never change what the code inside it means.
- **A file's top level runs once per run**, no matter how many files import
  it. Two importers get the same module, not two copies with separate state.
- **Types are shared across files.** A `Money` built in one file is the same
  `Money` everywhere, so declaring the same type name differently in two files
  is an error rather than two unrelated types.
- **Cycles are an error**, reported with the chain that produced them. Move
  the shared code into a third file both can import.
- **`.ko` is required.** Any other extension is refused.

Budgets, labels, and the journal do not stop at a file boundary. An imported
agent spends from the same budget, `classified` still propagates, and
`kora audit` lists declassification sites across every imported file.

### Packages

A package is a directory with a `kora.toml` and an entry file. Importers name
it rather than pathing into it:

```python
use pkg receipts as r

def main():
    print(r.describe("coffee", 4.5))
```

Where it comes from lives in `kora.toml`:

```toml
[dependencies]
receipts = { path = "./receipts" }
```

The package's own manifest names it and points at its entry, which defaults
to `src/lib.ko`:

```toml
[package]
name = "receipts"
version = "0.1.0"
entry = "src/lib.ko"
```

`as` is optional, unlike a file path: a package name is a Kora identifier and
so has a natural binding. Names are lowercase, digits, and underscores.

**A dependency is used when the source says so.** `[dependencies]` says where
a package comes from; the `use pkg` statements decide whether it is needed at
all. Declaring a hundred and importing four resolves four, and the pruning is
transitive — a dependency's own unused entries are never resolved either.
This is exact rather than a guess: a package name is always a literal token,
and Kora has no dynamic import, so scanning the source cannot miss one.

```bash
kora tree program.ko     # the packages actually used
kora check program.ko    # warns about anything declared and never imported
```

**Names resolve against the manifest that wrote them.** A `use pkg` inside a
package is answered by that package's `[dependencies]`, never the importing
program's. So a package may depend on something its consumer has never heard
of, two packages may bind the same bare name to different sources, and a
program cannot reach its dependencies' dependencies.

**Types belong to their package.** Two dependencies may both declare
`Config`, and they are different types:

```python
use pkg left as l
use pkg right as r

a = l.Config("left.example", 80)     # left's Config
b = r.Config("right", 3)             # right's, with different fields
```

Types are still shared across the *files* of one package, exactly as before,
so splitting a program across files is unchanged. What is new is that the
sharing stops at the package boundary — otherwise two dependencies declaring
the same type name would be an error the consumer could not fix, owning
neither of them.

A type from a package does not satisfy a same-named annotation here, and the
error says which is which rather than `expected Config, got Config`:

```
error: expected `Config`, got `Config`
   = hint: `Config` from package `left` is not `Config` in this program;
           they are different types with the same name
```

**A fetched package is named by its repository, and pinned by its commit:**

```toml
[dependencies.receipts]
git = "github.com/org/receipts"
tag = "v0.3.1"
grants = { net = true }
```

```bash
kora install program.ko    # fetch what the source imports
```

Identity is the full repository path, never a short name — there is no flat
namespace to squat in, which is where dependency-confusion attacks begin.

`kora.lock` records the reference you wrote, the commit it resolved to, and a
hash of the tree's contents. It is generated, committed, and never
hand-edited. **Once a repository is locked, its commit is what gets fetched —
never the tag again.** A maintainer account taken over and a tag force-pushed
to a backdoor changes nothing about what runs, including on a machine with a
cold cache, which is where re-resolving the tag would otherwise land it.

Hashes are checked on every `run`, `test`, and `check`, not only at install:
a cached dependency edited on disk must not run just because the directory is
there.

Fetched sources live in `.kora/deps/<repository>@<commit>/`, keyed by commit
so a moved tag gets its own directory instead of reusing bytes already there.
The directory is tool-owned and reproducible from the lockfile; `kora.lock`
is what belongs in version control.

**A dependency has no ambient authority.** It reaches the network, the
filesystem, a database, the environment, a Python worker, or an MCP server
only where the importing program said so:

```toml
[dependencies.receipts]
path = "./receipts"
grants = { net = true, sinks = ["stripe"] }
```

Note the table form. TOML forbids extending an inline table, so
`receipts = { path = "..." }` followed by `[dependencies.receipts.grants]` is
not valid TOML.

Ungranted, a call is refused where it is written:

```
error: package `reader` is not allowed to use `fs`: no `fs` capability
   = hint: grant it in kora.toml: `[dependencies.reader]` with `grants = { fs = true }`
```

The capabilities are `net`, `fs`, `sql`, `env`, and `python`, plus `sinks`
and `mcp` as lists of names and `declassify` as a flag. `json`, `csv`, `re`,
and `time` need no grant: they compute over values the caller already holds.

Three rules make this hold up:

- **Confinement follows execution, not the call site.** A package cannot shed
  it by spawning a `parallel for`, by being reached through a `tool` a model
  called, or by handing the work to a dependency of its own.
- **A parent may only pass on what it holds.** Granting `fs` to a dependency
  you were never given `fs` yourself grants nothing. Compromising a leaf of
  the graph therefore gains an attacker nothing that every link above it
  lacked.
- **`declassify` needs two grants**, the permission and the named sink, and
  both are off by default. Adding a dependency must not become the way to
  launder a secret out of a program.

A package states what it needs, and a shortfall is reported before the run
rather than at whichever call first needs it:

```toml
[package.requires]
net = true
sinks = ["stripe"]
```

```
error: package `receipts` requires net, sink `stripe`, but was granted fs
```

The root program is unrestricted, bounded by its own `kora.toml` and nothing
else. Capabilities are coarse today — `net = true`, not a list of hosts.

**Test-only packages are derived, not declared.** A package reached only
through `test` blocks is dev-only and stays out of a shipped program:

```python
use pkg receipts as r          # runtime

test "it parses a row":
    use pkg fixtures as f      # dev-only
    assert f.fake_row() == "fake", "bad fixture"
```

There is no `[dev-dependencies]` table to put something in the wrong half of.
A package reached by both a test path and a runtime path is a runtime
dependency — one runtime path anywhere is enough. And a dependency's *own*
`test` blocks are never roots for its consumer, since only the root program's
tests run, so you do not inherit a package's test helpers.

Budgets, labels, and the journal cross a package boundary exactly as they
cross a file boundary. `kora audit` lists declassification sites inside
dependencies too — a `declassify` in a package releases the importing
program's data, so an audit blind to it would make adding a dependency the
way to hide one.

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
    case Ok(m):
        print(m)
    case Err(why):
        print(why)   # ValueError: math domain error
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
| Imports are paths, not package names | `use "./lib/tax.ko" as tax`; no package manager yet |
| No `export` keyword | every top-level name is public |
| Methods are functions | `append(xs, v)`, not `xs.append(v)` |
| Type annotations are checked | not hints |

**Not yet built:** classes, list comprehensions, per-name privacy on modules,
user-defined generics, `try`/`except`, keyword arguments on user functions
(only `analyze` takes them), `*args`/`**kwargs`, integer keys in dicts.
