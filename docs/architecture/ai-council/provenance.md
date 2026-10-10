# AI Council — Review Provenance

## Purpose

For each PR, preserve enough information to reconstruct the review — not merely "PR #X was reviewed" but:

> "PR #X was reviewed by this council configuration, with these participants available, these participants absent, these findings, these disagreements, and this resulting decision."

This is part of the historical record of Open Network X as an experiment in AI-mediated software development.

## What to record

For every PR that goes through the review relay, the following should be determinable from the PR itself (comments, reviews, commit statuses) or from the repository state at merge time:

### 1. Expected participants

Which council members were expected to review, per `registry.yaml` at the time. This is derived from the registry — not from who actually showed up.

### 2. Actual participants

Which members actually posted reviews, inline findings, or verdicts. Determined from the PR's review list.

### 3. Absent participants

Expected members who did not participate, with reason if known (e.g., "Greptile: UNAVAILABLE — trial expired"). Absence is data, not an oversight.

### 4. Findings

The distinct problems raised, per the relay's turn-3 synthesis. Each finding records which reviewers raised it (multiple reviewers flagging the same issue is the strongest signal).

### 5. Disagreements

Where reviewers disagreed — e.g., turn-2 disputing a turn-1 finding, or two app reviewers giving conflicting advice. The relay's turn-2 and turn-3 comments capture these.

### 6. Synthesis

The turn-3 moderator's resolution: which findings stand, which were dismissed and why, and the final list of what the PR needs.

### 7. Final decision

The consensus verdict (GO or NO-GO), the `Review consensus` commit status, and — if merged — who merged and whether an override label was used.

## Where provenance lives

| Element | Location |
|---------|----------|
| Expected participants | `registry.yaml` at the reviewed commit (not the merge commit — the registry may change between review and merge) |
| Actual reviews | PR review list (GitHub API) |
| Turn 1 findings | Claude's summary comment |
| Turn 2 second opinion | Relay turn-2 review |
| Turn 3 synthesis | "Review relay: state of the review" comment |
| Turn 4 verification | "Gokoo verification" comment |
| Consensus verdict | `Review consensus` commit status |
| Override (if any) | `consensus-override` label + who applied it |

## Provenance template

When documenting a PR's review after the fact (e.g., in a retrospective or audit), use this format:

```markdown
## PR #___ — Review Provenance

**Council configuration:** registry.yaml @ <commit>
**Review period:** <first review> to <consensus status set>

### Expected
- Claude (turn-1, required)
- CodeRabbit (app, advisory)
- Devin (app, advisory)
- Greptile (app, advisory)
- Relay turn-2 (required)
- Relay turn-3 (required)
- Gokoo turn-4 (advisory)

### Participated
- Claude: 4 findings ([critical] x1, [high] x2, [medium] x1)
- CodeRabbit: 2 nits
- Relay turn-2: confirmed 3/4 Claude findings, disputed 1
- Relay turn-3: synthesis, NO-GO
- Gokoo turn-4: verified, NO-GO

### Absent
- Devin: no review posted (reason unknown)
- Greptile: UNAVAILABLE (trial expired 2026-10-08)

### Disagreements
- Turn-2 disputed Claude's [medium] on <file>:<line> — argued it was a false positive because <reason>. Turn-3 agreed with turn-2; finding dismissed.

### Decision
- Consensus: NO-GO
- Blocking: [critical] <summary>, [high] <summary>
- Merged: no (or: yes via consensus-override by <who> on <date>)
```

## Why this matters

The council is a first-class component (ADR-0049). Its operation — who reviewed what, who was missing, what was decided — is system behavior. Without provenance, a future reader cannot distinguish:

- "This PR was thoroughly reviewed and passed" from
- "This PR was reviewed by a degraded council and passed with warnings" from
- "This PR was merged over a NO-GO by human override."

All three are legitimate outcomes. Only the first is the normal case. Provenance makes the difference visible.
