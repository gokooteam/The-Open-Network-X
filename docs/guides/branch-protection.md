# Branch protection for `main`

`main` only takes changes whose CI has passed. This is enforced by a
repository ruleset, defined in
[`.github/rulesets/main.json`](../../.github/rulesets/main.json).

## Why

CI runs only on pull requests and pushes to `main`, and a full run takes
about 7–8 minutes. Without a ruleset, a PR can be merged before its checks
finish, or after one has failed. That happened: #2, #5, #7, #9, #17, #18,
#20 and #23 were merged within about a minute of being opened, and #28 and
#29 were merged after their own Fuzz run had already failed. #33 was merged
51 seconds after it was opened; its `Rust checks` finished 8 minutes later.

## What the ruleset enforces

On the default branch (`main`):

- Changes land only through a pull request (no direct pushes). No approval
  is required.
- These checks must pass on the PR, and the PR branch must be up to date
  with `main` (so the checks ran against what will actually land):

  | Workflow | Required check(s) |
  |---|---|
  | CI | `Rust checks`, `Deterministic replay acceptance`, `Security audit`, `Python reference vectors` |
  | Fuzz | `Fuzz boc_parser`, `Fuzz tvm_execution`, `Fuzz block_header` |
  | Docs and site | `site-and-docs` |
  | Cargo Deny | `Cargo Deny` |
  | Rust Hygiene | `Rust hygiene` |
  | TruffleHog Secret Scan | `TruffleHog` |
  | Versioning | `Version consistency` |
  | Dependency Review | `Dependency Review` |

  Each check is pinned to the GitHub Actions app (`integration_id` 15368),
  so another app cannot satisfy it by posting a status with the same name.
- No force-pushes to `main`, and `main` cannot be deleted.
- Nobody bypasses it, admins included.

Deliberately not required:

- **Cargo Audit** and **Workflow Security**: they run only when certain
  paths change. A required check that never starts blocks the PR forever.
- **Claude audit** and **Codex review**: they need secrets and are skipped
  for forks; they are review aids, not gates.
- **Code Coverage**, **CodeQL**, **rust-clippy analyze**, **Scorecard**:
  reporting jobs. The tests and Clippy already gate through `Rust checks`.

## Keeping it in sync

A required check name is the job's `name:` (with matrix values expanded,
e.g. `Fuzz ${{ matrix.target }}` → `Fuzz boc_parser`). If you rename a
job, add a fuzz target, or add a gate, update `main.json` in the same PR
and re-apply it. Otherwise PRs wait on a check that no longer exists.

## Applying it

Changing this file does nothing by itself; a repository admin has to apply
it. Either:

- run `scripts/apply-branch-protection.sh` while logged in to `gh` as a
  repo admin (it creates the ruleset, or updates it if one with the same
  name exists), or
- in GitHub: Settings → Rules → Rulesets → New ruleset → Import a ruleset,
  and pick `.github/rulesets/main.json`.

Check it took effect: Settings → Rules → Rulesets should list
"main: require CI" as Active, and a new PR's merge box should list the
required checks.
