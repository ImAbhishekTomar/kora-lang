#!/usr/bin/env python3
"""Publish DECISIONS.md as a page on the documentation site.

`DECISIONS.md` stays the source of truth at the repository root, where every
contributor and every agent already looks for it. The site needs the same
text as MDX, and a hand-kept second copy is exactly the rot this repository
keeps trying to avoid -- so the page is generated, and `--check` fails CI when
the two drift apart.

Usage:
  scripts/sync_decisions.py            # write the page
  scripts/sync_decisions.py --check    # fail if the page is out of date
"""

from __future__ import annotations

import argparse
import os
import re
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SOURCE = "DECISIONS.md"
TARGET = "site/app/decisions/page.mdx"
BLOB = "https://github.com/ImAbhishekTomar/kora-lang/blob/main/"

DESCRIPTION = "Why Kora is the way it is, and the trade-offs taken on purpose."


def spans_outside_code(text: str) -> list[tuple[int, int]]:
    """Character ranges that are prose, not fenced or inline code.

    MDX reads `{` and `<` in prose as JSX. Inside code -- fenced or inline --
    it does not, which is why the check below has to know the difference.
    """
    code = []
    for match in re.finditer(r"```.*?```|`[^`\n]*`", text, re.S):
        code.append((match.start(), match.end()))
    spans, cursor = [], 0
    for start, end in code:
        if start > cursor:
            spans.append((cursor, start))
        cursor = end
    if cursor < len(text):
        spans.append((cursor, len(text)))
    return spans


def check_mdx_safe(text: str) -> list[str]:
    """Prose that MDX would try to read as JSX, and choke on."""
    problems = []
    for start, end in spans_outside_code(text):
        for match in re.finditer(r"[{}]|<[a-zA-Z/!]", text[start:end]):
            line = text[: start + match.start()].count("\n") + 1
            problems.append(
                f"{SOURCE}:{line}: {match.group(0)!r} outside code would be read "
                "as JSX by MDX -- wrap it in backticks"
            )
    return problems


def render(source: str) -> str:
    """DECISIONS.md as the MDX the site serves."""
    body = source

    # The site puts the title in the page chrome, so the file's own H1 would
    # show up twice.
    body = re.sub(r"\A#\s+(.*?)\n+", "", body, count=1)

    # Relative links are relative to the repository, which the site is not.
    def absolute(match: re.Match[str]) -> str:
        label, link = match.group(1), match.group(2)
        if link.startswith(("http", "#", "mailto")):
            return match.group(0)
        return f"[{label}]({BLOB}{link})"

    body = re.sub(r"\[([^\]]+)\]\(([^)]+)\)", absolute, body)

    return (
        "---\n"
        "title: Decisions\n"
        f"description: {DESCRIPTION}\n"
        "---\n"
        "\n"
        "{/* Generated from DECISIONS.md by scripts/sync_decisions.py.\n"
        "    Edit that file, not this one. */}\n"
        "\n"
        "# Decisions\n"
        "\n"
        + body.lstrip("\n")
    )


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--check", action="store_true",
                        help="fail instead of writing when the page is stale")
    args = parser.parse_args()

    with open(os.path.join(ROOT, SOURCE), encoding="utf-8") as f:
        source = f.read()

    problems = check_mdx_safe(source)
    if problems:
        for problem in problems:
            print(f"  FAIL {problem}")
        return 1

    page = render(source)
    path = os.path.join(ROOT, TARGET)
    current = None
    if os.path.exists(path):
        with open(path, encoding="utf-8") as f:
            current = f.read()

    if current == page:
        print(f"{TARGET} is current")
        return 0

    if args.check:
        print(f"  FAIL {TARGET} is out of date; run scripts/sync_decisions.py")
        return 1

    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w", encoding="utf-8") as f:
        f.write(page)
    print(f"wrote {TARGET}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
