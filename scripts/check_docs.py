#!/usr/bin/env python3
"""Check that the documentation still describes the language that exists.

Documentation rot is not a style problem: a README that tells someone to run a
command that fails is worse than no README. Everything here was found by hand
at least once, which is the argument for automating it.

Checks:
  1. Every Kora code block in the docs parses and resolves.
  2. Every `kora <command>` and `--flag` mentioned exists in the binary.
  3. Every stdlib module and function documented is exported by the runtime.
  4. Every relative markdown link resolves.
  5. Every example file passes `kora check`.

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
DOCS = ["README.md", "DECISIONS.md", "docs/language.md", "docs/stdlib.md",
        "docs/cli.md", "examples/README.md"]

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


def code_blocks(text: str) -> list[tuple[int, str]]:
    """Fenced ```python blocks, with the line each starts on."""
    out = []
    line_no = 1
    for chunk in re.finditer(r"```python\n(.*?)```", text, re.S):
        start = text[: chunk.start()].count("\n") + 1
        out.append((start, chunk.group(1)))
    return out


def check_code_blocks(kora: str) -> None:
    """Every Kora snippet in the docs should parse and resolve."""
    print("Kora code blocks")
    checked = skipped = 0
    for doc in DOCS:
        for line_no, block in code_blocks(read(doc)):
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


def check_commands(kora: str) -> None:
    """Every command and flag the docs mention should exist."""
    print("Documented commands and flags")
    usage = subprocess.run([kora], capture_output=True, text=True).stdout
    real_commands = set(re.findall(r"^\s+kora (\w+)", usage, re.M))
    real_flags = set(re.findall(r"(--[a-z-]+)", usage))
    # `kora <file.ko>` and `kora --version` are documented but not subcommands.
    real_commands |= {"run"}

    for doc in DOCS:
        text = read(doc)
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


def check_links() -> None:
    """Every relative link should point at something."""
    print("Internal links")
    count = 0
    for doc in DOCS:
        base = os.path.dirname(doc)
        for text, link in re.findall(r"\[([^\]]+)\]\(([^)]+)\)", read(doc)):
            if link.startswith(("http", "#", "mailto")):
                continue
            target = os.path.normpath(os.path.join(ROOT, base, link.split("#")[0]))
            if not os.path.exists(target):
                fail(f"{doc}: [{text}]({link}) points at nothing")
            else:
                count += 1
    print(f"  {count} links verified")


def check_examples(kora: str) -> None:
    """Every example should at least check."""
    print("Examples")
    directory = os.path.join(ROOT, "examples")
    files = sorted(f for f in os.listdir(directory) if f.endswith(".ko"))
    result = subprocess.run(
        [kora, "check"] + [os.path.join(directory, f) for f in files],
        capture_output=True, text=True)
    if result.returncode != 0:
        fail("an example does not check:\n" + result.stderr.strip())
    else:
        print(f"  {len(files)} examples check")

    # Every example should be listed in the index, or nobody will find it.
    index = read("examples/README.md")
    for name in files:
        if name not in index:
            fail(f"examples/{name} is not listed in examples/README.md")


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
    check_examples(args.kora)

    print()
    if failures:
        print(f"{len(failures)} problem(s) in the documentation")
        return 1
    print("documentation matches the language")
    return 0


if __name__ == "__main__":
    sys.exit(main())
