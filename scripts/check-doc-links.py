#!/usr/bin/env python3
"""Check that relative links in the repository's Markdown resolve.

Every `[text](path)` and `[text](path#anchor)` link in a tracked `*.md` file
whose target is a repository path (not `http(s):`, `mailto:` or a bare
`#anchor`) must name a file or directory that exists. For links into another
Markdown file, or `#anchor` links within the same file, the anchor must match
a heading in that file (GitHub's slug rules).

Absolute links to this repository on GitHub
(`https://github.com/gokooteam/The-Open-Network-X/blob|tree/main/<path>`)
are checked the same way, since they break exactly like relative ones when
a file moves.

WHITEPAPER.md is the unmodified reference (INSTRUCTIONS.md §4) and is skipped.

Usage: python3 scripts/check-doc-links.py
Exit status 1 lists every broken link as `file:line: target (reason)`.
"""

from __future__ import annotations

import re
import subprocess
import sys
import unicodedata
from functools import lru_cache
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
REPO_URL = re.compile(
    r"^https://github\.com/gokooteam/The-Open-Network-X/(?:blob|tree)/main/([^?#)]*)(#[^)]*)?$",
    re.IGNORECASE,
)
SKIP = {"WHITEPAPER.md"}
# Inline links. Images use the same syntax with a leading "!", also checked.
LINK = re.compile(r"\]\(\s*<?([^)\s>]+)>?(?:\s+\"[^\"]*\")?\s*\)")
FENCE = re.compile(r"^\s*(```|~~~)")


def tracked_markdown() -> list[Path]:
    out = subprocess.run(
        ["git", "ls-files", "*.md"], cwd=ROOT, capture_output=True, text=True, check=True
    ).stdout.split()
    return [ROOT / p for p in out if p not in SKIP]


def slug(heading: str) -> str:
    """GitHub's heading anchor: lowercase, drop punctuation, spaces to '-'."""
    text = re.sub(r"`([^`]*)`", r"\1", heading)
    text = re.sub(r"\[([^\]]*)\]\([^)]*\)", r"\1", text)
    text = re.sub(r"<[^>]+>", "", text)
    text = text.strip().lower()
    kept = []
    for ch in text:
        cat = unicodedata.category(ch)
        if ch in " -_" or cat[0] in "LN":
            kept.append(ch)
    return "".join(kept).replace(" ", "-")


@lru_cache(maxsize=None)
def anchors(path: Path) -> frozenset[str]:
    found: dict[str, int] = {}
    result = set()
    in_fence = False
    for line in path.read_text(encoding="utf-8").splitlines():
        if FENCE.match(line):
            in_fence = not in_fence
            continue
        if in_fence:
            continue
        m = re.match(r"^#{1,6}\s+(.*?)\s*#*\s*$", line)
        if not m:
            continue
        base = slug(m.group(1))
        n = found.get(base, 0)
        found[base] = n + 1
        result.add(base if n == 0 else f"{base}-{n}")
    # Explicit HTML anchors: <a id="x"> / <a name="x">
    for m in re.finditer(r"<a\s+(?:id|name)=\"([^\"]+)\"", path.read_text(encoding="utf-8")):
        result.add(m.group(1))
    return frozenset(result)


def check_target(src: Path, target: str) -> str | None:
    """Return a reason string if the link is broken, else None."""
    m = REPO_URL.match(target)
    if m:
        path_part, anchor = m.group(1), (m.group(2) or "")[1:]
        dest = ROOT / path_part
    else:
        if re.match(r"^[a-z][a-z0-9+.-]*:", target, re.IGNORECASE):
            return None  # external URL, mailto:, etc.
        path_part, _, anchor = target.partition("#")
        dest = src if not path_part else (src.parent / path_part)
    dest = Path(re.sub(r"%20", " ", str(dest)))
    try:
        dest = dest.resolve()
        dest.relative_to(ROOT)
    except ValueError:
        return "points outside the repository"
    if not dest.exists():
        return "no such file"
    if anchor and dest.is_file() and dest.suffix == ".md":
        if anchor.lower() not in anchors(dest):
            return f"no heading for #{anchor}"
    return None


def main() -> int:
    broken = []
    for md in tracked_markdown():
        in_fence = False
        for lineno, line in enumerate(md.read_text(encoding="utf-8").splitlines(), 1):
            if FENCE.match(line):
                in_fence = not in_fence
                continue
            if in_fence:
                continue
            for target in LINK.findall(line):
                reason = check_target(md, target)
                if reason:
                    broken.append(f"{md.relative_to(ROOT)}:{lineno}: {target} ({reason})")
    if broken:
        print("Broken documentation links:")
        print("\n".join(broken))
        return 1
    print("All relative documentation links resolve.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
