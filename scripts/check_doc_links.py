#!/usr/bin/env python3
"""Docs guard: every relative link in the live docs points at a file in the repo, and no link names
a developer's disk. Run from anywhere: python3 scripts/check_doc_links.py

Live docs: README.md, LICENSE's neighbours under docs/ (except docs/history/, which is kept as it
was written), and the READMEs of crates/, nodes/ and go/. Anchors are not checked.
"""
import pathlib
import re
import subprocess
import sys
from urllib.parse import unquote

ROOT = pathlib.Path(__file__).resolve().parent.parent
LINK = re.compile(r"!?\[[^\]]*\]\(([^)\s]+)(?:\s+\"[^\"]*\")?\)")
ABSOLUTE = re.compile(r"(?<![\w.])/(?:Users|home)/[\w.-]+/")


def live_docs():
    files = subprocess.run(
        ["git", "ls-files", "*.md"], cwd=ROOT, capture_output=True, text=True, check=True
    ).stdout.split("\n")
    for name in files:
        if not name:
            continue
        if name == "README.md" or (name.startswith("docs/") and not name.startswith("docs/history/")):
            yield name
        elif name.startswith(("crates/", "nodes/", "go/")) and name.endswith("README.md"):
            yield name


def main():
    problems = []
    for name in live_docs():
        path = ROOT / name
        text = path.read_text(encoding="utf-8", errors="replace")
        for number, line in enumerate(text.split("\n"), 1):
            if ABSOLUTE.search(line):
                problems.append(f"{name}:{number}: a path on a developer's disk")
            for target in LINK.findall(line):
                if re.match(r"^[a-z][a-z0-9+.-]*:", target) or target.startswith("#"):
                    continue
                file_part = unquote(target.split("#", 1)[0])
                if not file_part:
                    continue
                resolved = (ROOT / file_part.lstrip("/")) if file_part.startswith("/") else (path.parent / file_part)
                if not resolved.exists():
                    problems.append(f"{name}:{number}: broken link {target}")
    for problem in problems:
        print(problem)
    print(f"docs links: {len(problems)} problem(s)")
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
