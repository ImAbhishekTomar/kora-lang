# Working on Kora

Notes for anyone — human or agent — changing this repository.

## Feature Development Principle

When building a new feature, always identify the problems that existing languages face—especially those they cannot solve because of legacy dependencies, backward-compatibility requirements, or ecosystem constraints.

Since Kora is a new language, prioritize solving these problems at the language, compiler, or runtime level rather than adding workarounds.

**Todo** Maintain a todo list during development like below, with all required details, file `TODO.md`:
- [ ] Queue
- [x] Completed
- [ ] Development  (where we are, details of dev things etc)

**Roadmap** Make sure to keep roadmap up-to-date  `site/app/roadmap/page.mdx` and the same has to be updated under github project also in view `Kora Lang - Roadmap` using `gh` cli.

Read [DECISIONS.md](DECISIONS.md) first. It records *why* the language is the
way it is; the docs record *what*. A change that contradicts a decision needs
that file updated in the same commit, not a comment explaining the exception.

`DECISIONS.md` stays at the repository root -- that is where everyone, human
and agent, already looks for it. It is also published, as the site's
`/decisions` page, so the reasoning is public rather than folklore. That page is **generated**: edit `DECISIONS.md` and run
`python3 scripts/sync_decisions.py`. Never edit
`site/app/decisions/page.mdx` by hand; `check_docs.py` fails when the two
drift.

## Where things live

```
crates/          the compiler, runtime, language server, and debugger
editors/vscode   the VS Code extension
examples/        runnable .ko programs
benches/         performance benchmarks
docs/            language, stdlib, and CLI references (markdown)
site/            the public documentation site (Next.js + Nextra)
scripts/         check_docs.py, sync_decisions.py, bench.py, packaging
DECISIONS.md     why the language is the way it is
```

The site is a self-contained project: its `package.json`, `pnpm-lock.yaml`,
`tsconfig.json`, `next.config.mjs`, `styles.css`, and `public/` all live under
`site/`, and pnpm commands run from there. Its one reach outside is
`site/next.config.mjs`, which imports the Kora TextMate grammar from
`editors/vscode/syntaxes/` so the site highlights Kora with the same grammar
the editor uses.

## The rule that matters most

**A language change is not done when the runtime works.** Kora ships a
compiler, a language server, an editor extension, three reference documents, a
public docs site, and a runnable example set. All of them describe the same
language. Any one of them left behind is a bug that reaches a user before it
reaches CI.


So: when you add or change a construct, walk the whole list below and either
update the item or say out loud why it needs nothing.

## Kora Lang Project Management

A GitHub Project named **`Kora Lang`** has been created for this repository.

Always keep the project up to date using the **GitHub CLI (`gh`)**. Before making any changes, ensure that the GitHub CLI is authenticated and authorized to access the project.

The project contains two views:

1. **`Kora Lang - Tracker`**
   Manage tasks across the **Todo**, **In Progress**, and **Done** stages.

2. **`Kora Lang - Roadmap`**
   Manage and track the roadmap for Kora Lang.

## Package vs core compiler — decide out loud, every time

Kora has two extension points: the compiler (this repo) and packages (see
DECISIONS.md, ecosystem strategy). Every new feature request lands in one of
them, never both, and the split is not a judgment call to make silently.

**The test:** does it need a new *effect*? Effects are the things the
checker and runtime already know about — `analyze`, the tool loop,
`ask_human`, `declassify`, the clock, network, filesystem, subprocess,
random.

- Needs a new effect, or changes how an existing one is checked (schema
  shape, label rules, budget accounting) → **core compiler change**. Walk
  the checklist below.
- Composes effects that already exist → **package**, full stop, even if it
  would be more convenient to bolt onto the runtime.

**Before writing code for any requested feature, state the classification
and the one-line reason, unprompted.** This applies at every permission
level, including full-approval/auto-accept modes — a fast "yes" from the
harness is not a fast "yes" from the user on *what* is being built. One
line is enough:

> This is a package (composes `analyze` + `parallel for`, no new effect).
> This needs a compiler change (new schema shape for `analyze` results).

If genuinely ambiguous, say so and pick the smaller-blast-radius reading
(package over compiler) rather than asking, unless the ambiguity changes
what gets built enough to need a decision from the user.

## Two answers that are already decided

Both come up whenever packages are discussed. Neither is open.

**Never load a native shared library.** A `.so` or `.dll` runs inside the
process with full operating-system rights and does not pass through
`call_module_fn`, so no capability grant ever sees it. One native package and
the confinement the whole package system rests on is gone. Compiled
third-party code arrives as WASM components or not at all — see
[DECISIONS.md](DECISIONS.md#wasm-components-for-native-packages).

**A manifest has no field for install scripts.** Not off by default, not
gated: the format has nowhere to put one. That is the whole `postinstall`
attack class, refused by the file format rather than by a setting somebody
can turn on.

## Checklist for a language change

Adding, changing, or removing syntax or semantics touches these, roughly in
order:

**Compiler**

- `crates/kora-syntax/src/token.rs` — new keyword? add the token
- `crates/kora-syntax/src/lexer.rs` — and the word that produces it
- `crates/kora-syntax/src/ast.rs` — the node
- `crates/kora-syntax/src/parser.rs` — the parse, plus a test in the same file
- `crates/kora-syntax/src/lines.rs` — a new statement kind with a body must
  list its lines, or breakpoints inside it silently never fire
- `crates/kora-runtime/src/interp.rs` — execution
- `crates/kora-runtime/src/value.rs` — new runtime value shape?
- `crates/kora-runtime/src/portable.rs` — if a value must cross a
  `parallel for` boundary, it needs a portable form or workers break
- `crates/kora-runtime/src/audit.rs` — every `StmtKind` match lives here too;
  a new statement must at minimum be listed as inert
- `crates/kora-types/src/lib.rs` — name resolution and diagnostics, or the
  editor reports errors on code that plainly runs
- `crates/kora-cli/src/main.rs` — new command, flag, or usage text

**Editor**

- `crates/kora-lsp/src/lib.rs` — hover, completion, go-to-definition
- `crates/kora-dap/src/variables.rs` — a new runtime value shape needs a
  summary line and, if it has parts, children in the variables pane
- `crates/kora-dap/src/session.rs` — new launch options, capabilities, or
  requests
- `editors/vscode/syntaxes/kora.tmLanguage.json` — highlighting
- `editors/vscode/package.json` — add keywords if the feature is something
  people would search for. Do **not** hand-edit `version`: release-please owns
  every version in the repository (see [Releasing](#releasing)). A change here
  is only picked up when VS Code restarts, and a dev install's symlink should
  be renamed to match the manifest version or the extension scanner may serve
  the manifest it cached under the old one. Test a manifest change with
  `npx @vscode/vsce package --no-dependencies`, which runs the same validation
  the release does
- `editors/vscode/README.md` — the feature list is the marketplace page

**Documentation** (all of it, every time)

- `README.md` — the pitch and the status list ("not built yet")
- `docs/language.md` — the reference, including the contents list and the
  "Differences from Python" table
- `docs/stdlib.md` — if a module or function changed
- `docs/cli.md` — if a command's behavior changed, including `kora lsp` and
  `kora dap`
- `DECISIONS.md` — the *why*, and the trade-offs deliberately accepted. Run
  `python3 scripts/sync_decisions.py` after changing it, so the published
  page matches
- `site/app/language/page.mdx` and `site/app/reference/page.mdx` — the public
  docs site is a separate copy, and it is the copy people actually read.
  `site/app/ecosystem/page.mdx` and `site/app/installation/page.mdx` describe
  the editor experience. Site pages mark Kora snippets ```` ```kora ````, not
  ```` ```python ````; `check_docs.py` parses both, so a fence typo means the
  snippet is silently unchecked
- a new page under `site/app/` must be added to `DOCS` in
  `scripts/check_docs.py`, which fails if it is not

**Examples and tests**

- `examples/` — a runnable program per feature, listed in `examples/README.md`
- `crates/*/tests/` — end-to-end tests that exercise the feature the way a
  user hits it, not just the unit underneath
- `.github/workflows/ci.yml` — a new example that runs deterministically
  belongs in the examples job; a new `test` block belongs in the Kora suite
- `benches/` — a construct on a hot path (evaluation, calls, values, the
  journal) needs a benchmark, or its cost is invisible until someone
  complains. Run `python3 scripts/bench.py --against main` before claiming a
  change is free; see `benches/README.md`

A change to the *debugger* rather than the language touches
`crates/kora-runtime/src/debug.rs`, `crates/kora-dap/`, the `debuggers`
contribution in `editors/vscode/package.json`, `editors/vscode/src/extension.js`,
and the debugging sections of `docs/cli.md`, `README.md`, and
`editors/vscode/README.md`.

## Before you say it works

```bash
cargo fmt --all
cargo clippy --all-targets --all-features
cargo test
python3 scripts/sync_decisions.py    # only if DECISIONS.md changed
cargo build && python3 scripts/check_docs.py --kora ./target/debug/kora
./target/debug/kora check examples/*.ko examples/lib/*.ko
```

And, if anything under `site/` changed:

```bash
cd site && pnpm install --frozen-lockfile && pnpm build
```

`scripts/check_docs.py` parses every Kora code block in the reference
documents **and in every site page**, verifies every documented command and
flag exists, checks every internal link and every site route, and checks that
the published Decisions page still matches `DECISIONS.md`. It is the backstop
for the rule above, not a replacement for it: it cannot tell that a feature is
missing from a page, only that what is written is wrong.

## What CI checks, and what to update when you add something

Seven workflows. Two of them care about where files live, so a move that does
not update them fails quietly -- the job simply stops running, which looks the
same as passing.

| workflow | covers |
|---|---|
| `ci.yml` | format, clippy, tests on three platforms, release-mode tests, MSRV, `cargo deny`, the Kora test suite, the examples, benchmarks, the VS Code package, and the documentation |
| `docs-site.yml` | builds and deploys `site/` to Vercel |
| `release.yml` | tag builds, archives, crates.io, npm, Homebrew, both extension marketplaces |
| `release-please.yml` | the version and the changelog |
| `publish-npm.yml`, `publish-homebrew.yml` | manual re-publishes |
| `dependabot.yml` | auto-merges non-major bumps |

So:

- a new **example** that runs deterministically goes in the `examples` job of
  `ci.yml`; a new `test` block goes in `kora-tests`
- a new **page under `site/app/`** goes in `DOCS` in `scripts/check_docs.py`;
  the `check_site_coverage` check fails if it does not. The `lychee` step in
  the `docs` job already globs `site/app/**/*.mdx`, so external links on it
  are covered without another change
- a new **image or asset the site serves** goes in `site/public/`, and is
  referenced by its route (`/marketing/x.png`, not a file path).
  `check_site_assets` fails on a reference with no file behind it, because a
  dead `<img>` renders as a broken icon and nothing else notices
- a new **file the site needs at build time** that lives outside `site/` must
  be added to the trigger `paths` of `docs-site.yml`, or a change to it
  deploys nothing. Today that is only
  `editors/vscode/syntaxes/kora.tmLanguage.json`
- **pnpm commands run from `site/`.** `docs-site.yml` sets
  `working-directory: site` once, and points `pnpm/action-setup` at
  `site/package.json` and the Node cache at `site/pnpm-lock.yaml`. All three
  have to agree
- the Vercel project's **Root Directory must stay unset**: the workflow
  already runs the CLI from inside `site/`, so setting it would resolve to
  `site/site`. `docs-site.yml` reads the setting out of the `vercel pull`
  output and fails the job if someone sets it, rather than deploying a 404

## Releasing

Nobody picks a version number by hand. Every push to `main` is read as
[Conventional Commits](https://www.conventionalcommits.org), and
release-please keeps one open `chore: release` pull request holding the next
version and the generated `CHANGELOG.md`:

| commit prefix | effect on the version |
|---|---|
| `fix:` | patch — `0.1.0` to `0.1.1` |
| `feat:` | minor — `0.1.0` to `0.2.0` |
| `feat!:` or a `BREAKING CHANGE:` footer | major, and minor while below `1.0.0` |
| `docs:`, `test:`, `chore:`, `ci:`, `refactor:` | none |

So the commit message *is* the release decision, which is why the prefix is
not decoration. Merging the release PR tags `vX.Y.Z`, and that tag is what
`release.yml` already listens for — the build, the archives, Homebrew,
crates.io, npm, and both extension marketplaces are unchanged.

Five files carry a version, and all five move together in that PR:
`Cargo.toml` (the workspace and every internal path dependency, marked with
`# x-release-please-version`), `Cargo.lock`, `npm/package.json`,
`editors/vscode/package.json`, and `version.txt`. `release.yml` verifies the
tag matches `Cargo.toml` and refuses to publish a mismatch, so a missed bump
fails the release instead of shipping a wrong number.

Two details worth knowing before changing any of this. The `rust` release
type cannot be used: it walks every member `Cargo.toml` expecting a literal
`[package] version`, and these crates use `version.workspace = true`. And
release-please does not know about `Cargo.lock`, which pins the version of
every workspace member — so the workflow refreshes it on the release branch,
without which the `--locked` build fails *after* the tag exists.

A release PR is a pull request like any other: if the version or the notes
look wrong, say so there rather than tagging by hand.

### Release documentation

Every release has a permanent developer-facing page on the public site. The
release PR must add `site/app/releases/<version>/page.mdx` before it is merged,
then update `site/app/releases/page.mdx`, `site/app/releases/_meta.js`, and
the `DOCS` list in `scripts/check_docs.py`. Do this for the first release too;
an initial release says that no migration is needed.

Each page must state the publication date, link to the GitHub release, group
the user-visible changes, name every breaking change, and give an explicit
migration path from the previous version. "None" is a valid and required
answer for breaking changes, and "no migration needed" is a valid and
required answer for an initial release. Do not make developers infer either
from a changelog or a commit list. Update `/versions` when its historical
version table changes.

For a release already published before this convention, create the page from
the GitHub release notes and tagged source. Keep release pages immutable after
publication except to correct factual documentation errors.

## House style

- Errors carry a span, a plain-language message, and usually a hint naming the
  fix. Error messages are half the product.
- Comments explain *why*, not *what*. If a line needs a comment to say what it
  does, rename something instead.
- Prefer the design that stays correct as the language grows over the one
  that is quickest to write.


**worktree**  github worktree you can only use when looks really need, and if you start that make sure to commit and clean that.