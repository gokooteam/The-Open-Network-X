#!/usr/bin/env python3
"""Workspace version management for ONX (policy: docs/adr/0034-versioning-standard.md).

The single source of truth is `[workspace.package] version` in the root
Cargo.toml. Every workspace member inherits it with `version.workspace = true`.

Usage:
    scripts/version.py current
    scripts/version.py check [--base-version X.Y.Z]
    scripts/version.py bump {patch|minor|major|X.Y.Z} [--date YYYY-MM-DD]
    scripts/version.py notes X.Y.Z

`bump` rewrites Cargo.toml and CHANGELOG.md only; run
`cargo update --workspace` afterwards so Cargo.lock follows.
"""

from __future__ import annotations

import argparse
import datetime
import re
import sys
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
CARGO_TOML = ROOT / "Cargo.toml"
CARGO_LOCK = ROOT / "Cargo.lock"
CHANGELOG = ROOT / "CHANGELOG.md"

# SemVer 2.0.0. Pre-release identifiers are allowed (e.g. 0.3.0-rc.1);
# build metadata is not used by this project.
SEMVER_RE = re.compile(
    r"^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)"
    r"(?:-((?:0|[1-9]\d*|\d*[a-zA-Z-][0-9a-zA-Z-]*)"
    r"(?:\.(?:0|[1-9]\d*|\d*[a-zA-Z-][0-9a-zA-Z-]*))*))?$"
)
WORKSPACE_VERSION_RE = re.compile(
    r'(\[workspace\.package\][^\[]*?^version\s*=\s*")([^"]+)(")', re.M | re.S
)
UNRELEASED_HEADING = "## [Unreleased]"


def fail(msg: str) -> None:
    print(f"error: {msg}", file=sys.stderr)


def parse(version: str) -> tuple[int, int, int, str | None]:
    m = SEMVER_RE.match(version)
    if not m:
        raise ValueError(f"{version!r} is not a valid SemVer 2.0.0 version")
    return int(m[1]), int(m[2]), int(m[3]), m[4]


def precedence_key(version: str) -> tuple:
    major, minor, patch, pre = parse(version)
    # A release sorts after any of its pre-releases.
    if pre is None:
        return (major, minor, patch, 1, ())
    ids = tuple((0, int(p), "") if p.isdigit() else (1, 0, p) for p in pre.split("."))
    return (major, minor, patch, 0, ids)


def current_version() -> str:
    data = tomllib.loads(CARGO_TOML.read_text())
    try:
        return data["workspace"]["package"]["version"]
    except KeyError:
        raise SystemExit("error: Cargo.toml has no [workspace.package] version")


def workspace_members() -> list[Path]:
    data = tomllib.loads(CARGO_TOML.read_text())
    return [ROOT / m / "Cargo.toml" for m in data["workspace"]["members"]]


def next_version(current: str, bump: str) -> str:
    if bump not in ("patch", "minor", "major"):
        parse(bump)
        if precedence_key(bump) <= precedence_key(current):
            raise ValueError(f"{bump} is not greater than current version {current}")
        return bump
    major, minor, patch, pre = parse(current)
    if pre is not None:
        # Finalising a pre-release: 0.3.0-rc.1 + any bump -> 0.3.0 first.
        return f"{major}.{minor}.{patch}"
    if bump == "major":
        return f"{major + 1}.0.0"
    if bump == "minor":
        return f"{major}.{minor + 1}.0"
    return f"{major}.{minor}.{patch + 1}"


def allowed_successors(base: str) -> set[str]:
    """Versions a single release step may move to from `base` (ignoring pre-release)."""
    major, minor, patch, _ = parse(base)
    return {
        f"{major}.{minor}.{patch}",
        f"{major}.{minor}.{patch + 1}",
        f"{major}.{minor + 1}.0",
        f"{major + 1}.0.0",
    }


def changelog_section(version: str) -> str | None:
    text = CHANGELOG.read_text()
    heading = re.compile(rf"^## \[{re.escape(version)}\][^\n]*\n", re.M)
    m = heading.search(text)
    if not m:
        return None
    nxt = re.compile(r"^## \[", re.M).search(text, m.end())
    end = nxt.start() if nxt else len(text)
    body = text[m.end():end]
    # Drop trailing link-reference definitions that belong to the whole file.
    body = re.sub(r"^\[[^\]]+\]:\s*\S+\s*$", "", body, flags=re.M)
    return body.strip()


def cmd_current(_: argparse.Namespace) -> int:
    print(current_version())
    return 0


def cmd_check(args: argparse.Namespace) -> int:
    errors = 0
    version = current_version()
    try:
        parse(version)
    except ValueError as e:
        fail(str(e))
        return 1

    for manifest in workspace_members():
        pkg = tomllib.loads(manifest.read_text()).get("package", {})
        if pkg.get("version") != {"workspace": True}:
            fail(f"{manifest.relative_to(ROOT)}: use `version.workspace = true`")
            errors += 1

    lock = tomllib.loads(CARGO_LOCK.read_text())
    names = {
        tomllib.loads(m.read_text())["package"]["name"] for m in workspace_members()
    }
    for pkg in lock.get("package", []):
        if pkg["name"] in names and "source" not in pkg and pkg["version"] != version:
            fail(
                f"Cargo.lock has {pkg['name']} {pkg['version']}, expected {version}; "
                "run `cargo update --workspace`"
            )
            errors += 1

    if not CHANGELOG.exists():
        fail("CHANGELOG.md is missing")
        return errors + 1
    text = CHANGELOG.read_text()
    if UNRELEASED_HEADING not in text:
        fail(f"CHANGELOG.md must keep a '{UNRELEASED_HEADING}' section")
        errors += 1
    if changelog_section(version) is None:
        fail(f"CHANGELOG.md has no '## [{version}]' section for the current version")
        errors += 1

    if args.base_version:
        base = args.base_version
        if version != base:
            if precedence_key(version) <= precedence_key(base):
                fail(f"version went backwards or sideways: {base} -> {version}")
                errors += 1
            else:
                core = version.split("-", 1)[0]
                if core not in allowed_successors(base):
                    fail(
                        f"{base} -> {version} skips versions; allowed next releases: "
                        + ", ".join(sorted(allowed_successors(base) - {base.split('-', 1)[0]}))
                    )
                    errors += 1
            print(f"version change: {base} -> {version}")
        else:
            print(f"version unchanged: {version}")

    if errors:
        return 1
    print(f"ok: workspace version {version} is consistent")
    return 0


def cmd_bump(args: argparse.Namespace) -> int:
    current = current_version()
    new = next_version(current, args.bump)
    date = args.date or datetime.date.today().isoformat()

    cargo = CARGO_TOML.read_text()
    cargo, n = WORKSPACE_VERSION_RE.subn(rf"\g<1>{new}\g<3>", cargo, count=1)
    if n != 1:
        raise SystemExit("error: could not locate [workspace.package] version")
    CARGO_TOML.write_text(cargo)

    text = CHANGELOG.read_text()
    if UNRELEASED_HEADING not in text:
        raise SystemExit(f"error: CHANGELOG.md has no '{UNRELEASED_HEADING}' section")
    text = text.replace(
        UNRELEASED_HEADING, f"{UNRELEASED_HEADING}\n\n## [{new}] - {date}", 1
    )
    # Maintain the compare links at the bottom of the file.
    link_re = re.compile(r"^\[Unreleased\]:\s*(\S+)/compare/v(\S+)\.\.\.HEAD\s*$", re.M)
    m = link_re.search(text)
    if m:
        repo_url = m[1]
        text = link_re.sub(
            f"[Unreleased]: {repo_url}/compare/v{new}...HEAD\n"
            f"[{new}]: {repo_url}/compare/v{m[2]}...v{new}",
            text,
            count=1,
        )
    CHANGELOG.write_text(text)
    print(new)
    return 0


def cmd_notes(args: argparse.Namespace) -> int:
    section = changelog_section(args.version)
    if section is None:
        fail(f"CHANGELOG.md has no section for {args.version}")
        return 1
    print(section or "No notable changes recorded.")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    sub = parser.add_subparsers(dest="command", required=True)
    sub.add_parser("current", help="print the workspace version").set_defaults(fn=cmd_current)
    p = sub.add_parser("check", help="verify version consistency")
    p.add_argument("--base-version", help="version on the target branch, to validate the bump")
    p.set_defaults(fn=cmd_check)
    p = sub.add_parser("bump", help="bump the version and roll the changelog")
    p.add_argument("bump", help="patch, minor, major, or an explicit X.Y.Z[-pre]")
    p.add_argument("--date", help="release date for the changelog (default: today)")
    p.set_defaults(fn=cmd_bump)
    p = sub.add_parser("notes", help="print the changelog section for a version")
    p.add_argument("version")
    p.set_defaults(fn=cmd_notes)
    args = parser.parse_args()
    try:
        return args.fn(args)
    except ValueError as e:
        fail(str(e))
        return 1


if __name__ == "__main__":
    sys.exit(main())
