# ADR-0040: Versioning standard — SemVer 2.0.0, one workspace version

**Status:** Accepted (2026-10-08). In use since #23: `v0.2.0` is tagged and
`versioning.yml` enforces the rules. This record was numbered ADR-0034 until
#32 added a second ADR-0034 (gas caps). Gas caps kept 0034 because it sits
inside the Wave 4 run 0034–0039, and code and golden vectors cite it. This
record became ADR-0040.
**Decider:** Amethyst

## Context

Every crate carried a placeholder `version = "0.1.0"` from scaffolding. No
release had ever been tagged, there was no changelog, and `release.yml`
fired only on manually pushed `v*` tags. Meanwhile the code moved through
several incompatible wire formats (`ONXBLK04` → `ONXBLK05`, the message
model, storage schema 4) and a devnet (devnet-1) is running. Nothing told a
node operator, explorer, or contributor which build they had.

## Decision

1. **Standard: [Semantic Versioning 2.0.0](https://semver.org/spec/v2.0.0.html).**
   Tags are `vMAJOR.MINOR.PATCH`, optionally with a pre-release suffix
   (`v0.3.0-rc.1`). No build metadata.
2. **One version for the whole workspace.** `[workspace.package] version` in
   the root `Cargo.toml` is the single source of truth; every member uses
   `version.workspace = true`. The crates ship together, depend on each other
   by path, and are not published to crates.io, so per-crate versions would
   only add drift. (`fuzz/` is excluded from the workspace and stays `0.0.0`.)
3. **Bump rules while pre-1.0 (`0.y.z`)**, per SemVer §4:
   - **MINOR** (`0.y.0`): any breaking change — block/transaction/message
     encoding, domain tags, genesis format, storage schema, state root
     derivation, consensus rules, RPC or CLI interface, public Rust API.
   - **PATCH** (`0.y.z`): compatible fixes, docs, CI, performance, and
     additive changes that change no bytes on the wire or on disk.
4. **After 1.0.0** (mainnet-ready, a separate future decision): MAJOR for
   breaking, MINOR for compatible features, PATCH for compatible fixes.
5. **Software version ≠ protocol version.** `PROTOCOL_VERSION`, block magic
   (`ONXBLKnn`), storage `SCHEMA_VERSION`, and versioned domain tags
   (`ONX_*_Vn`) are wire/disk-format identifiers governed by the
   specification and their own ADRs. They are integers that change only
   when the format changes. Changing any of them requires at least a MINOR
   bump pre-1.0 (MAJOR after), and the release notes list their values.
6. **Changelog:** `CHANGELOG.md` in [Keep a Changelog 1.1.0](https://keepachangelog.com/en/1.1.0/)
   format. Changes go under `## [Unreleased]` in the PR that makes them.
7. **Current version: 0.2.0.** 0.1.0 stands for the untagged protocol-library
   baseline of 2026-09-10. Everything since (PRs #1–#21) is breaking under
   rule 3, so the first tagged release is the next minor, 0.2.0, rather than
   retroactively inventing intermediate releases that never existed.

## Automation

- `scripts/version.py` — `current`, `check`, `bump`, `notes`.
- `.github/workflows/versioning.yml` — on every PR and push to `main`,
  checks that all crates inherit the workspace version, `Cargo.lock` agrees,
  `CHANGELOG.md` has `Unreleased` and a section for the current version, and
  that a version change on a PR is exactly one SemVer step forward. On
  `main`, when the version has no tag yet, it creates `vX.Y.Z` and runs
  `release.yml`, which builds, tests, and drafts a GitHub Release whose body
  is that version's changelog section.
- `.github/workflows/version-bump.yml` — manual (`workflow_dispatch`): pick
  patch/minor/major or an explicit version; it opens a `release/vX.Y.Z` PR
  with the bumped `Cargo.toml`, `Cargo.lock`, and rolled changelog. Merging
  that PR triggers the tag and draft release.

## Consequences

- One number identifies a build across all binaries (`onxd`, `onx`,
  `onx-cli`, `onx-genesis`) via `CARGO_PKG_VERSION`.
- Releases are drafts; a maintainer reviews and publishes them.
- PRs opened by the bump workflow with the default `GITHUB_TOKEN` do not
  trigger other workflows (a GitHub rule). Add a `RELEASE_PR_TOKEN` secret
  (fine-grained PAT: contents + pull requests write) so CI runs on them, or
  push any commit to the release branch to start CI. The repository must also
  allow GitHub Actions to create pull requests (Settings → Actions → General).
