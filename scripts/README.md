# Scripts

| Script | What it does | Run by |
| --- | --- | --- |
| `check-doc-links.py` | Every relative link in tracked Markdown (and every `github.com/gokooteam/The-Open-Network-X/blob/main/…` link) points at a file that exists, and every `#anchor` matches a heading. | `docs.yml` |
| `check-spec-citations.py` | `WHITEPAPER.md §N.N` citations (and ranges) in `docs/specification/` and Rust source name headings that exist in `WHITEPAPER.md`. | `docs.yml` |
| `site.py` | Generates the repository-derived facts on on-x.live (`site/index.html`) and checks the page against the repository. See [`site/README.md`](../site/README.md). | `docs.yml` (`check`), `site-monitor.yml` (`live`) |
| `site-pull-deploy.sh` | Runs on a web host (cron): fetches a site file from `main`, checks it, and replaces the served copy only if it changed. | the web host |
| `apply-branch-protection.sh` | Creates or updates the `main` ruleset (required CI checks) from `.github/rulesets/main.json`. Needs `gh` logged in as a repo admin. See [`docs/guides/branch-protection.md`](../docs/guides/branch-protection.md). | a repo admin, by hand |
| `version.py` | Workspace version consistency check and bump (ADR-0040). | `versioning.yml`, `version-bump.yml`, `release.yml` |
| `check-research-logbook.py` | Checks `docs/planning/research-logbook.md` entry numbering and `[ANSWER]`/`[QUESTION]` labels. | nothing (the logbook is a convention, not a gate; see `CONTRIBUTING.md`) |
| `fix-whitepaper-page-headers.py` | One-off: finds PDF page headers/footers interleaved into `WHITEPAPER.md` during PDF-to-Markdown conversion, removes the ones on paragraph boundaries, and reports the rest to `docs/reference/whitepaper-page-header-review.md`. `--apply` writes; omit it for a dry run. | by hand |

Run any of the checks locally from the repository root, e.g.
`python3 scripts/check-doc-links.py`.
