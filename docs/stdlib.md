# The standard library

Nine native modules, each backed by a Rust crate. Every one exists to fix a
specific, well-known defect in its equivalent elsewhere — rewriting a library
is only worth it if the rewrite fixes what everyone already knows is broken.

Three rules hold across all of them:

1. **Failure is a value.** Every fallible call returns `Ok(...)` or
   `Err(reason)`. No silent `None`, no forgotten exception.
2. **Data from outside is `unverified`** and cannot reach a dangerous sink
   until something narrows it.
3. **Classified data cannot leave** without an explicit `declassify`.

```python
use json
use csv as sheet
```

---

## `json`

**What everyone else gets wrong.** `json.loads` returns an untyped blob, so a
mistake surfaces three functions later as an attribute error. And a parse
failure reports a byte offset — "line 1 column 4318" — which says nothing
about one-line JSON.

| | |
|---|---|
| `json.parse(text)` | `Ok(value)` — untyped, for exploring |
| `json.parse(text, Type)` | `Ok(typed)` — checked against a declared type |
| `json.stringify(value)` | `Ok(text)` |
| `json.get(value, "users.0.email")` | `Ok(value)` |

```python
match json.parse(body, User):
    case Ok(u):
        print(u.address.city)
    case Err(why):
        print(why)      # $.tags.1: expected str, got int
```

Errors name the path through nested types and list elements:

```
$.age: expected int, got str
$.tags.1: expected str, got int
$.address: missing
```

`json.stringify` refuses classified data, including a `classified` field
inside an object you pass whole.

---

## `csv`

**What everyone else gets wrong.** Python's `csv` makes everything a string,
so you convert by hand and forget a column. pandas guesses instead, and a zip
code column of `01234` becomes the integer `1234` — unrecoverable by the time
you see it.

Kora does neither: you declare the shape, and nothing is inferred.

| | |
|---|---|
| `csv.parse(text, RowType)` | `Ok(list)` — typed rows |
| `csv.rows(text)` | `Ok(list of dict)` — every value a string |
| `csv.write(rows)` | `Ok(text)` |

```python
type Person:
    name: str
    zip: str        # stays "01234"
    amount: float

match csv.parse(text, Person):
    case Ok(people):
        for p in people:
            print(p.zip)
    case Err(why):
        print(why)  # row 2, column `amount`: expected a number, got `n/a`
```

Ragged rows and missing columns are errors naming the row and column. A byte
order mark is stripped, since a leading BOM silently corrupts the first column
name and the resulting error never mentions it.

---

## `http`

**What everyone else gets wrong.** `requests.get(url)` has no default timeout
and waits forever — the most common way a Python service hangs. It returns a
response object for a 500, so a failed call flows onward as data unless
someone remembers `raise_for_status()`. Retries need a second library. And
when a URL can come from a model, `http.get(url)` reaches internal services.

| | |
|---|---|
| `http.get(url)` | `Ok(response)` — retried with backoff |
| `http.post(url, body)` | `Ok(response)` — attempted once |

```python
match http.get("https://api.example.com/v1/status"):
    case Ok(r):
        status = r["status"]
        body = r["body"]
    case Err(why):
        print(why)  # ... returned HTTP 404: {"message":"Not Found"}
```

- A timeout always exists. `[http] timeout_secs` changes it; `0` is clamped,
  not honoured.
- A non-2xx status is `Err`.
- `GET` retries with exponential backoff; `POST` does not — a retried payment
  is worse than a failed one.
- Private and loopback ranges are refused, including `169.254.169.254`. Set
  `[http] allow_private = true` to permit them.
- URLs must be verified data; a URL read from a file or a model is refused.
- Response bodies are `unverified`.
- In a durable run, calls are journaled and not repeated on resume.

---

## `sql`

**What everyone else gets wrong.** SQL injection persists because the unsafe
path is shorter: an f-string is easier to type than a bound parameter, and it
works until the input contains a quote.

Here query text must be data the program itself produced. A value from
outside can only be **bound**, never spliced. Backed by SQLite, so a program
has a working database with no server.

| | |
|---|---|
| `sql.query(db, statement, [params])` | `Ok(rows)` |
| `sql.execute(db, statement, [params])` | `Ok(count)` |

```python
sql.execute(db, "create table t (id integer, name text)")
sql.execute(db, "insert into t values (?, ?)", [1, "ada"])

match sql.query(db, "select name from t where id = ?", [user_input]):
    case Ok(rows):
        print(len(rows))
    case Err(why):
        print(why)
```

Splicing outside data into a statement is refused, and the hint points at the
safe path. Rows come back `unverified`.

In a durable run, `sql.execute` is journaled: a resume replays its recorded
outcome instead of running the statement again, so a killed pipeline does not
insert the same rows twice. `sql.query` is not replayed — rows can be larger
than every other effect in a run put together — but a digest of what it
returned is, so a resume whose query answers differently stops rather than
continuing against data the run it is continuing never saw.

---

## `fs`

**What everyone else gets wrong.** A crash during `write()` leaves a truncated
file and the original is gone. `io` errors omit the path, which is the first
thing you want. A path built from untrusted data is how traversal happens.
And `os.listdir` and `glob.glob` hand back whatever order the filesystem
stored, which differs between machines — so a program that fans a listing out
across threads does its work in a different sequence on every host.

| | |
|---|---|
| `fs.read(path)` | `Ok(text)` — `unverified` |
| `fs.lines(path)` | `Ok(list)` — `unverified` |
| `fs.image(path)` | `Ok(image)` — `unverified` |
| `fs.list(dir)` | `Ok(list of paths)` — sorted |
| `fs.glob(pattern)` | `Ok(list of paths)` — sorted |
| `fs.write(path, text)` | `Ok(None)` — atomic |
| `fs.append(path, text)` | `Ok(None)` |
| `fs.exists(path)` | `bool` |

Writes go to a temporary file and are renamed, so a reader sees the old
contents or the new ones, never a half-written mix. Paths containing `..` are
refused rather than normalised, and a path from outside the program is refused
outright. Errors name the path: `no such file: config.txt`.

In a durable run, `fs.write` and `fs.append` happen **exactly once** across a
crash: the outcome is journaled, and a resume hands it back rather than
writing again. The narrow case in between — the process died after the write
ran and before the journal learned what it returned — is recorded as an
attempt, and a resume that reaches it stops and names the call rather than
guessing. Repeating it could double a row; assuming it worked could drop one.

`fs.read`, `fs.lines`, `fs.list` and `fs.glob` are read live on every attempt,
with a digest journaled: a resume that would read different data stops instead
of mixing two inputs into one run.

```python
match fs.glob("dataset/*.png"):
    case Ok(paths):
        rows = parallel for p in paths:
            return classify(p)
    case Err(why):
        print(why)
```

`*` and `?` match within one path component, `**` matches any run of
directories. Dotfiles are only matched when the pattern asks for one, so `*`
does not sweep up `.git`. Symlinked directories are not followed under `**`,
since a link to an ancestor makes the walk infinite. No matches is an empty
list, not an error.

Both listings are **sorted**, and both return full paths rather than bare
names — a name alone has to be re-joined by hand, and forgetting to is how a
loop ends up reading the wrong directory. Unlike file *contents*, listed
paths are verified: the program named the directory and the shape of the
names, and every result was matched against it, which is the same narrowing
that lifts `unverified` elsewhere.

`fs.image` reads PNG, JPEG, GIF, and WebP. The type comes from the file's
magic bytes, not its extension — `mimetypes.guess_type` trusts the filename,
so a JPEG named `.png` reaches the provider mislabelled and returns an opaque
`400`. Files above 20 MB are refused at the call site, where the error can
name the file, rather than at the provider. See
[Images](language.md#images) for what an image does next.

---

## `env`

**What everyone else gets wrong.** `os.environ["API_KEY"]` returns an ordinary
string, so a credential reaches a log line, an error message, or a crash
report without anyone noticing.

| | |
|---|---|
| `env.get(name)` | `Ok(value)` — `classified` if the name looks like a secret |
| `env.has(name)` | `bool` |

A name containing `key`, `token`, `secret`, `password`, `credential`, `auth`,
`session`, `cookie`, or `private` comes back classified. It works normally for
its real purpose, but reaching a model, a file, or a serializer needs an
explicit release.

---

## `time`

**What everyone else gets wrong.** `datetime.now()` returns a *naive* value
with no zone, and everything downstream guesses.

| | |
|---|---|
| `time.now()` | seconds since the Unix epoch, UTC |
| `time.format(seconds, "iso" \| "date" \| "unix")` | `Ok(text)` |
| `time.elapsed(since)` | whole seconds |

Instants are always absolute; there is no naive type to misuse. `now()` is
journaled, so a durable replay sees the instant the original run saw — a live
clock during replay would send a resumed program down a different branch.

---

## `re`

**What everyone else gets wrong.** Backtracking engines take exponential time
on inputs an attacker chooses: `(a+)+$` against a long run of `a`s hangs a
server. Ecosystems cannot fix it because they depend on backtracking features.

| | |
|---|---|
| `re.matches(pattern, text)` | `Ok(bool)` |
| `re.find(pattern, text)` | `Ok(text)` or `Err("no match")` |
| `re.find_all(pattern, text)` | `Ok(list)` |
| `re.replace(pattern, text, replacement)` | `Ok(text)` |
| `re.split(pattern, text)` | `Ok(list)` |

A finite-automaton engine with a linear-time guarantee. The cost is no
backreferences and no lookaround — the right trade where patterns may come
from a model and text from a web page.

A bad pattern is `Err`, not a crash, since patterns come from configuration.

---

## `notes`

**What everyone else gets wrong.** A tool loop's plan, partial results, and
things learned two turns ago live in local variables — gone the moment the
process dies, invisible to anything outside it. Reaching for a plain file
gives durability but drops label propagation and durable-replay correctness.

| | |
|---|---|
| `notes.write(key, value)` | `Ok(None)` or `Err(reason)` |
| `notes.read(key, default)` | the stored value, or `default` if the key is absent |

A single key-value store scoped to exactly one identity — the current durable
run — at `.kora/notes/<run-id>.json`. `notes.write` goes straight to the
file, live; `notes.read` is journaled, the same way `time.now()` is, so a
resumed run sees what the live run actually read rather than whatever the
store holds by the time the replay happens.

A classified value keeps its label crossing `notes.write`/`notes.read`, and a
value read back is additionally `unverified` — the store is external to this
evaluation, the same rule `fs.read` follows. `notes.read`'s `default` is
positional, not `Ok`/`Err`: a missing key is not a failure, it is simply the
default, the same way a dict lookup with a fallback is not.

Requires a durable run (`kora run --durable`), for the same reason
`ask_human` does — there is no run id, and so no store to address, without
one. See [`examples/18_notes.ko`](../examples/18_notes.ko).

---

## Not built yet

`polars`-backed dataframes, S3, PDF, full-text search, and Postgres. Images
are in (`fs.image`); documents are not.

This is about stdlib bindings specifically — MCP client support itself is
already implemented (`use mcp` in [language.md](language.md#mcp-servers)) and
is how a program reaches most of these systems today (a Postgres MCP server,
an S3 MCP server, and so on) without a dedicated binding. See the ecosystem
strategy in [DECISIONS.md](../DECISIONS.md) for what comes next.
