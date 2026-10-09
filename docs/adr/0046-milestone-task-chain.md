# ADR-0046 — Milestone task chain

**Status:** Accepted (2026-10-09).
**Decider:** Amethyst.

## Context

With M4 done and M5's exit criteria all checked, the remaining work (M6
consensus, M7 public testnet, M8 maintenance gate) is a long list of exit
criteria in `MILESTONES.md`. Amethyst commissioned a machine to work them
one at a time: *"make GitHub actions that activate you to do each task that
is left — literally all of them — activated by the merge of the task before
it, other than the first one of course, with the regular consensus we've
been doing."*

Then the redirect, which overrules the first message's letter: *"Don't
listen to that verbatim… do it the most efficient way that you feel that
you can, based off of what you've already proved you can do — that was
coming from a model who has no context about your abilities."* And the
non-negotiable that survived the redirect: *"we need some way of you noting
when a merge happened and when you would start your next task."*

A context-free model had proposed new GitHub Actions schedulers, retry
labels, and a separate orchestration layer. Amethyst explicitly rejected
following that proposal verbatim.

## Decision

**No new GitHub Actions.** The machinery that already exists does the job:

- The 30-minute ONX plan driver **is** the scheduler — each run checks for
  the in-flight task and advances it, or starts the next criterion.
- The 90-second PR event watch **is** the merge trigger — it already sees
  merges, and now also sees issues, labels, and comments.
- GitHub issues labeled `gokoo-task` are the visible ledger. The issue
  timeline records exactly when each merge happened and when the next task
  started — the record Amethyst required.
- `scripts/site.py next-task` exposes the first unchecked exit criterion as
  JSON (one parser shared with the criterion counter, so no drift; stable
  task IDs from milestone + SHA-256 of the criterion text).
- The `gokoo/<task-id>` branch is the resume token if a run dies mid-task
  (credit exhaustion); the next run resumes from the branch, never restarts.
- Task PRs are titled `M#: …`, labeled `gokoo-task-pr`, carry
  `Closes #<issue>`, and go through the unchanged audit → relay → consensus
  pipeline. Amethyst merges; nothing else changes about that.
- If a criterion needs Amethyst herself (an owner decision like D2/D3, a
  soak test, validators she runs, independent review): explain on the
  issue, label it `needs-owner`, pause. Credit exhaustion is resumable;
  genuine failures are not mislabeled as exhaustion.

One task in flight at a time. A box is ticked only with linked evidence
(PR, CI run, or named test). `python3 scripts/site.py build` runs in the PR
that ticks a box.

Amethyst's follow-up the same day made two things standing directives
(SD1–SD3 in `MILESTONES.md`): the chain itself, and the rule that her
direction is part of the trusted record — every task issue carries the
direction that commissioned it, and her prompts stay verbatim in
`docs/conductors-score.md`. Work is only trustworthy if the direction
behind it is documented.
