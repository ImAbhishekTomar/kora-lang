#!/usr/bin/env python3
"""Check that the documentation still describes the language that exists.

Documentation rot is not a style problem: a README that tells someone to run a
command that fails is worse than no README. Everything here was found by hand
at least once, which is the argument for automating it.

Checks:
  1. Every Kora code block in the docs parses and resolves.
  2. Every `kora <command>` and `--flag` mentioned exists in the binary.
  3. Every stdlib module and function documented is exported by the runtime.
  4. Every relative markdown link, and every site route, resolves.
  5. Every example file passes `kora check`.
  6. The published copy of DECISIONS.md matches the source.

"The docs" means the reference documents at the root *and* the pages the
public site serves out of `site/app/`. The site was unchecked for a while and
that is precisely where a broken snippet does the most damage: it is the copy
people actually read.

Usage: scripts/check_docs.py [--kora path/to/kora]
"""

from __future__ import annotations

import argparse
import os
import re
import subprocess
import sys
import tempfile

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

# Markdown marks Kora snippets as ```python, because that is what renders
# usefully on GitHub. The site has real syntax highlighting for the language,
# so it marks them ```kora -- and its ```python blocks are genuinely Python,
# on the comparison page. Checking the wrong fence per file would either miss
# every site snippet or send Python to the Kora parser.
MARKDOWN, SITE = "markdown", "site"
FENCE = {MARKDOWN: "python", SITE: "kora"}

DOCS = [
    ("README.md", MARKDOWN),
    ("DECISIONS.md", MARKDOWN),
    ("AGENTS.md", MARKDOWN),
    ("docs/language.md", MARKDOWN),
    ("docs/stdlib.md", MARKDOWN),
    ("docs/cli.md", MARKDOWN),
    ("examples/README.md", MARKDOWN),
    ("site/app/cli/page.mdx", SITE),
    ("site/app/comparison/page.mdx", SITE),
    ("site/app/ecosystem/page.mdx", SITE),
    ("site/app/internals/page.mdx", SITE),
    ("site/app/installation/page.mdx", SITE),
    ("site/app/language/page.mdx", SITE),
    ("site/app/model-calls/page.mdx", SITE),
    ("site/app/reference/page.mdx", SITE),
    ("site/app/roadmap/page.mdx", SITE),
    ("site/app/releases/page.mdx", SITE),
    ("site/app/releases/0.2.0/page.mdx", SITE),
    ("site/app/releases/0.0.1/page.mdx", SITE),
    ("site/app/releases/0.0.2/page.mdx", SITE),
    ("site/app/releases/0.1.0/page.mdx", SITE),
    ("site/app/start-here/page.mdx", SITE),
    ("site/app/versions/page.mdx", SITE),
]

# Generated from DECISIONS.md, which is in the list above already. Checking it
# twice would only report the same problem in two places.
GENERATED = {"site/app/decisions/page.mdx"}

SITE_APP = "site/app"
SITE_PUBLIC = "site/public"

# Blocks that are not Kora at all, or that deliberately show a failure.
SKIP_MARKERS = (
    "error:",       # showing what a failure looks like
    "# error",      # the same, as a comment
    "...",          # an elided body
)

failures: list[str] = []


def fail(message: str) -> None:
    failures.append(message)
    print(f"  FAIL {message}")


def read(path: str) -> str:
    with open(os.path.join(ROOT, path), encoding="utf-8") as f:
        return f.read()


def code_blocks(text: str, fence: str) -> list[tuple[int, str]]:
    """Fenced blocks in the given language, with the line each starts on."""
    out = []
    for chunk in re.finditer(r"```" + fence + r"\n(.*?)```", text, re.S):
        start = text[: chunk.start()].count("\n") + 1
        out.append((start, chunk.group(1)))
    return out


def check_code_blocks(kora: str) -> None:
    """Every Kora snippet in the docs should parse and resolve."""
    print("Kora code blocks")
    checked = skipped = 0
    for doc, kind in DOCS:
        if not os.path.exists(os.path.join(ROOT, doc)):
            # check_site_coverage names this properly; do not mask it here.
            continue
        for line_no, block in code_blocks(read(doc), FENCE[kind]):
            if any(marker in block for marker in SKIP_MARKERS):
                skipped += 1
                continue
            # A snippet is usually an excerpt, so wrap bare statements in a
            # function to give them somewhere to live.
            source = block
            if not re.match(r"^\s*(def|agent|tool|type|use|test|#)", block.strip()):
                indented = "\n".join("    " + l if l.strip() else l
                                     for l in block.splitlines())
                source = "def main():\n" + indented

            with tempfile.NamedTemporaryFile("w", suffix=".ko", delete=False) as f:
                f.write(source)
                path = f.name
            try:
                # Syntax only. An excerpt legitimately mentions names defined
                # elsewhere in the surrounding prose, but it must be valid
                # Kora -- which is exactly the rot worth catching, since a
                # snippet that cannot parse was never run by its author.
                result = subprocess.run([kora, "check", "--syntax", path],
                                        capture_output=True, text=True)
                if result.returncode != 0:
                    detail = (result.stderr or result.stdout).strip().splitlines()
                    fail(f"{doc}:{line_no} is not valid Kora: "
                         + (detail[0] if detail else "unknown"))
                else:
                    checked += 1
            finally:
                os.unlink(path)
    print(f"  {checked} checked, {skipped} skipped as fragments")


def strip_frontmatter(text: str) -> str:
    """Drop a leading `---` fenced block, as MDX pages carry."""
    if not text.startswith("---\n"):
        return text
    end = text.find("\n---", 4)
    return text if end == -1 else text[end + 4 :]


def check_commands(kora: str) -> None:
    """Every command and flag the docs mention should exist."""
    print("Documented commands and flags")
    usage = subprocess.run([kora], capture_output=True, text=True).stdout
    real_commands = set(re.findall(r"^\s+kora (\w+)", usage, re.M))
    real_flags = set(re.findall(r"(--[a-z-]+)", usage))
    # `kora <file.ko>` and `kora --version` are documented but not subcommands.
    real_commands |= {"run"}

    for doc, _kind in DOCS:
        # Frontmatter is metadata, not prose about the CLI. A page whose
        # `description:` says "every kora command" is describing itself, and
        # reading that as a claim about a subcommand named `command` fails a
        # page for its own summary line.
        text = strip_frontmatter(read(doc))
        for command in set(re.findall(r"`?kora ([a-z]+)", text)):
            if command in real_commands:
                continue
            # DECISIONS describes planned work; it is allowed to name a
            # command that does not exist yet.
            if doc == "DECISIONS.md":
                continue
            # A path or a flag, not a command.
            if command.startswith("-") or "." in command:
                continue
            fail(f"{doc} mentions `kora {command}`, which does not exist")
        for line in text.splitlines():
            if "kora " not in line or "cargo" in line:
                continue
            for flag in re.findall(r"(--[a-z][a-z-]+)", line):
                if flag not in real_flags:
                    fail(f"{doc} mentions `kora {flag}`, which the binary does not accept")
    print(f"  {len(real_commands)} commands, {len(real_flags)} flags verified")


def check_stdlib() -> None:
    """docs/stdlib.md should describe the modules that actually exist."""
    print("Standard library")
    registry = read("crates/kora-runtime/src/stdlib/mod.rs")
    real_modules = set(re.findall(r'"(\w+)" => Some\(Module::new', registry))

    doc = read("docs/stdlib.md")
    documented = set(re.findall(r"^## `(\w+)`", doc, re.M))

    for missing in sorted(real_modules - documented):
        fail(f"module `{missing}` exists but is not in docs/stdlib.md")
    for extra in sorted(documented - real_modules):
        fail(f"docs/stdlib.md documents `{extra}`, which does not exist")

    functions = 0
    for module, function in set(re.findall(r"`?(\w+)\.(\w+)\(", doc)):
        path = f"crates/kora-runtime/src/stdlib/{module}.rs"
        if not os.path.exists(os.path.join(ROOT, path)):
            continue
        exports = re.search(r"EXPORTS[^=]*=\s*&\[(.*?)\];", read(path), re.S)
        names = set(re.findall(r'\("(\w+)"', exports.group(1))) if exports else set()
        if function not in names:
            fail(f"docs/stdlib.md documents `{module}.{function}`, which is not exported")
        else:
            functions += 1
    print(f"  {len(real_modules)} modules, {functions} functions verified")


def route_exists(route: str) -> bool:
    """Does `/foo` correspond to a page the site actually serves?

    A route is a directory holding a `page.*`, and Next.js does not care which
    extension that is: the landing page is React, the reference pages are MDX,
    and both are routes. Checking only for `.mdx` reported the landing page as
    missing the moment it stopped being Markdown.
    """
    slug = route.strip("/")
    directory = os.path.join(ROOT, SITE_APP, slug) if slug else \
        os.path.join(ROOT, SITE_APP)
    return any(
        os.path.exists(os.path.join(directory, f"page{ext}"))
        for ext in (".mdx", ".md", ".tsx", ".jsx", ".ts", ".js")
    )


def check_links() -> None:
    """Every relative link, and every site route, should point at something."""
    print("Internal links")
    count = 0
    for doc, kind in DOCS:
        if not os.path.exists(os.path.join(ROOT, doc)):
            continue
        text = read(doc)
        links = re.findall(r"\[([^\]]+)\]\(([^)]+)\)", text)
        # The site pages are MDX, so some links are JSX attributes rather
        # than markdown. A dead `href` is just as dead.
        if kind == SITE:
            links += [("href", href) for href in re.findall(r'href="([^"]+)"', text)]

        base = os.path.dirname(doc)
        for label, link in links:
            if link.startswith(("http", "#", "mailto")):
                continue
            bare = link.split("#")[0]
            if not bare:
                continue
            if kind == SITE:
                # A site link is a route, not a path on disk: `/reference`
                # is served by `site/app/reference/page.mdx`.
                if not link.startswith("/"):
                    fail(f"{doc}: [{label}]({link}) is not an absolute site "
                         "route; the site serves routes, not file paths")
                elif not route_exists(bare):
                    fail(f"{doc}: [{label}]({link}) is not a page the site serves")
                else:
                    count += 1
                continue
            target = os.path.normpath(os.path.join(ROOT, base, bare))
            if not os.path.exists(target):
                fail(f"{doc}: [{label}]({link}) points at nothing")
            else:
                count += 1
    print(f"  {count} links verified")


def check_site_coverage() -> None:
    """A new site page must be checked, so it has to be listed above."""
    print("Site page coverage")
    listed = {doc for doc, kind in DOCS if kind == SITE} | GENERATED
    found = set()
    for directory, _subdirs, files in os.walk(os.path.join(ROOT, SITE_APP)):
        for name in files:
            if name.endswith(".mdx"):
                found.add(os.path.relpath(
                    os.path.join(directory, name), ROOT).replace(os.sep, "/"))
    for missing in sorted(found - listed):
        fail(f"{missing} is served by the site but is not checked; add it to "
             "DOCS in scripts/check_docs.py")
    for stale in sorted(listed - found - GENERATED):
        fail(f"DOCS lists {stale}, which does not exist")
    print(f"  {len(found)} site pages, all checked")


def check_site_assets() -> None:
    """Every image the site serves should be a file that exists.

    A dead `<img>` renders as a broken icon rather than an error, so nothing
    else in this repository would notice. lychee cannot check these: it
    resolves root-relative links against `site/app`, and assets live in
    `site/public`.
    """
    print("Site assets")
    count = 0
    for doc, kind in DOCS:
        if kind != SITE:
            continue
        for src in re.findall(r'src="(/[^"]+)"', read(doc)):
            asset = os.path.join(ROOT, SITE_PUBLIC, src.lstrip("/"))
            if not os.path.exists(asset):
                fail(f"{doc}: src=\"{src}\" has no file at "
                     f"{SITE_PUBLIC}{src}")
            else:
                count += 1
    print(f"  {count} assets verified")


def check_decisions_page() -> None:
    """The published copy of DECISIONS.md should match the source."""
    print("Published decisions")
    script = os.path.join(ROOT, "scripts", "sync_decisions.py")
    result = subprocess.run([sys.executable, script, "--check"],
                            capture_output=True, text=True)
    if result.returncode != 0:
        detail = (result.stdout or result.stderr).strip().splitlines()
        fail(detail[-1].strip() if detail else "sync_decisions.py --check failed")
    else:
        print("  site/app/decisions/page.mdx matches DECISIONS.md")


def check_examples(kora: str) -> None:
    """Every example should at least check."""
    print("Examples")
    directory = os.path.join(ROOT, "examples")
    files = sorted(f for f in os.listdir(directory) if f.endswith(".ko"))
    # Library files an example imports are part of the examples too.
    library = os.path.join(directory, "lib")
    library_files = sorted(
        os.path.join("lib", f) for f in os.listdir(library) if f.endswith(".ko")
    ) if os.path.isdir(library) else []
    # The pattern set is a second tour with its own index, so it is checked
    # and indexed by the same rules rather than being trusted to stay right.
    patterns = os.path.join(directory, "patterns")
    pattern_files = sorted(
        os.path.join("patterns", f)
        for f in os.listdir(patterns)
        if f.endswith(".ko")
    ) if os.path.isdir(patterns) else []
    every = files + library_files + pattern_files
    result = subprocess.run(
        [kora, "check"] + [os.path.join(directory, f) for f in every],
        capture_output=True, text=True)
    if result.returncode != 0:
        fail("an example does not check:\n" + result.stderr.strip())
    else:
        print(f"  {len(every)} example files check")

    # Every example should be listed in the index, or nobody will find it.
    index = read("examples/README.md")
    for name in files:
        if name not in index:
            fail(f"examples/{name} is not listed in examples/README.md")

    if pattern_files:
        pattern_index = read("examples/patterns/README.md")
        for name in pattern_files:
            base = os.path.basename(name)
            if base not in pattern_index:
                fail(f"examples/{name} is not listed in examples/patterns/README.md")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--kora", default=os.path.join(ROOT, "target/debug/kora"))
    args = parser.parse_args()

    if not os.path.exists(args.kora):
        print(f"error: no kora binary at {args.kora} (cargo build first)")
        return 2

    check_code_blocks(args.kora)
    check_commands(args.kora)
    check_stdlib()
    check_links()
    check_site_coverage()
    check_site_assets()
    check_decisions_page()
    check_examples(args.kora)

    print()
    if failures:
        print(f"{len(failures)} problem(s) in the documentation")
        return 1
    print("documentation matches the language")
    return 0


if __name__ == "__main__":
    sys.exit(main())
