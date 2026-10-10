# AI Council — Degraded Operation Policy

## Principle

> **A missing reviewer must never silently reduce review coverage.**

When a council participant is UNAVAILABLE or DEGRADED, the system must make that fact visible and apply an explicit policy — not proceed as if the reviewer had never existed.

## PR classes and council requirements

PRs are classified by consensus impact (see `docs/guides/review-relay.md`):

| PR class | Definition | Full council required? |
|----------|------------|----------------------|
| `breaking` | Nodes with and without the PR compute different state for the same blocks | **Yes** — all `required: true` participants must be AVAILABLE or the PR is blocked |
| `adjacent` | Touches consensus code, results unchanged | **Yes** — same as breaking |
| `none` | No consensus impact (docs, site, tooling) | No — degraded operation permitted with warning |
| `unclear` | Impact not yet determined | Treated as `adjacent` until classified |

## Per-role policy

| Role | If UNAVAILABLE... | Rationale |
|------|-------------------|-----------|
| `lead_implementation` (Gokoo) | **All work stops.** Not a degraded state; nothing to review. | No implementation, no PR. |
| `independent_reviewer` (Claude/turn-1, required) | **Blocks** `breaking`/`adjacent` PRs. Docs-only PRs proceed with a warning banner. | The primary audit cannot be waived for consensus changes. No substitute exists in the current architecture. |
| `independent_reviewer` (CodeRabbit, Devin, Greptile — advisory) | **Warning only.** Health report shows the gap; review proceeds. | These are defense-in-depth, not the primary gate. Their absence reduces confidence but does not remove the audit layer. |
| `second_opinion` (turn-2) | **Blocks.** Turn-3 is gated on turn-2 success (`review-relay.yml`); if turn-2 fails, the relay must be re-run. | The workflow enforces sequencing; a failed turn-2 means no synthesis. |
| `synthesis_moderator` (turn-3) | **Blocks.** No consensus status is set; the PR cannot merge until the relay re-runs. | Without the moderator there is no GO/NO-GO verdict. This is a hard block, not a silent pass. |
| `live_verifier` (turn-4) | **Warning only.** PR notes that live verification did not run. | Advisory; the relay's verdict stands on its own. |

## What "blocked" means

A blocked PR:

1. Keeps the `review-blocking` label.
2. Shows the missing required participant in the relay summary comment.
3. Cannot merge until the participant recovers OR Amethyst applies the `consensus-override` label (explicit human override, recorded in provenance).

A blocked PR does **not**:

- silently proceed without the reviewer;
- reinterpret the missing reviewer as "not part of the council";
- lower the bar for the remaining reviewers.

## What "warning" means

A warning:

1. Appears in the council health report.
2. Appears in the PR's relay summary as a noted absence.
3. Is recorded in the review provenance for that PR.
4. Does not block merging.

## Amethyst's override

Amethyst may override any block with the `consensus-override` label (see `.github/workflows/consensus-override.yml`). The override:

- is a deliberate human decision, not a system default;
- is recorded in the PR's provenance;
- does not change the participant's architectural membership or status.

## Detecting stale or missing results

The relay's turn-3 moderator is responsible for detecting when an expected review did not appear (e.g., a GitHub App that normally reviews within 15 minutes is silent after 60). When detected:

1. The participant's status is flagged for health-check review.
2. The absence is noted in the relay summary.
3. The degraded-operation policy above determines whether the PR proceeds.
