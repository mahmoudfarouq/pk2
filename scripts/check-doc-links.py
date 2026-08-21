#!/usr/bin/env python3
"""Verify relative markdown links and heading anchors in the given paths.

Catches the two ways cross-references in docs/ break silently: a link to a file
that has been renamed, and an anchor to a heading whose text has changed.

Usage: check-doc-links.py <file-or-directory> [...]
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

LINK = re.compile(r"\]\(\s*([^)\s#]*)(?:#([^)\s]+))?\s*\)")
HEADING = re.compile(r"^#{1,6}\s+(.*)$", re.MULTILINE)
# Skip anything with a scheme or a protocol-relative prefix.
EXTERNAL = re.compile(r"^(?:[a-zA-Z][a-zA-Z0-9+.-]*:|//)")


def anchor(heading: str) -> str:
    """Slugify a heading the way GitHub does."""
    slug = re.sub(r"[^\w\s-]", "", heading.strip().lower())
    return re.sub(r"\s+", "-", slug).strip("-")


def markdown_files(paths: list[str]) -> list[Path]:
    found: list[Path] = []
    for raw in paths:
        path = Path(raw)
        if path.is_dir():
            found.extend(sorted(path.rglob("*.md")))
        elif path.suffix == ".md":
            found.append(path)
        else:
            sys.exit(f"not a markdown file or directory: {path}")
    return found


def main() -> int:
    if len(sys.argv) < 2:
        sys.exit(__doc__)

    files = markdown_files(sys.argv[1:])
    if not files:
        sys.exit("no markdown files found")

    # Keyed by resolved path so cross-file lookups below actually match.
    anchors = {
        path.resolve(): {anchor(h) for h in HEADING.findall(path.read_text(encoding="utf-8"))}
        for path in files
    }

    problems = 0
    for path in files:
        for target, frag in LINK.findall(path.read_text(encoding="utf-8")):
            if EXTERNAL.match(target):
                continue

            resolved = path.resolve() if not target else (path.parent / target).resolve()

            if target:
                if not resolved.exists():
                    print(f"{path}: link target does not exist: {target}")
                    problems += 1
                    continue
                if resolved.suffix != ".md":
                    continue  # a real file, but not one whose anchors we know

            if frag:
                known = anchors.get(resolved)
                if known is None:
                    continue  # outside the checked set
                if frag not in known:
                    print(f"{path}: no such anchor '#{frag}' in {resolved.name}")
                    problems += 1

    checked = ", ".join(str(p) for p in files)
    if problems:
        print(f"\n{problems} broken link(s) across {len(files)} file(s)")
        return 1

    print(f"ok: {len(files)} file(s) checked ({checked})")
    return 0


if __name__ == "__main__":
    sys.exit(main())
