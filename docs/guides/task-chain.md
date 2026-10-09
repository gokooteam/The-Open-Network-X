# Milestone task chain

How ONX works through `MILESTONES.md` one exit criterion at a time, with each
merged task starting the next one. The chain is driven by Gokoo's own
machinery (the plan driver cron + the PR event watch), not by GitHub Actions
workflows — the runner that does the work is already scheduled, so a second
scheduler would be pure overhead.

## The loop

1. **Pick.** `python3 scripts/site.py next-task` prints the first unchecked
   exit criterion across Part 2 then Part 3 as JSON
   (`{milestone, index, text, task_id}`). The task ID (`M6-` + 12 hex chars
   of the criterion text's sha256) is stable, so a retried task is recognized
   as the same task.
2. **Track.** Gokoo opens a GitHub issue titled `M6: <criterion, truncated>`,
   labeled `gokoo-task`. The body holds the full criterion text, the task ID,
   the milestone, and the rules below. One `gokoo-task` issue open at a time —
   if one is already open, nothing new starts.
3. **Branch.** `gokoo/<task-id>`. If the branch already exists (a previous run
   died partway), continue from it — never restart.
4. **Work.** Do the criterion, tick its box **with linked evidence** (PR, CI
   run, or named test — per "How to use this file" in `MILESTONES.md`; no
   evidence, no tick), and run `python3 scripts/site.py build` in the same PR.
5. **PR.** Title starts with the milestone ID (`M6: …`) so the Milestones
   workflow assigns it. Label it `gokoo-task-pr` and put `Closes #<issue>` in
   the body.
6. **Consensus, unchanged.** Claude audits the PR, the review relay runs
   turns 2–3, Gokoo answers findings (Turn 4 included). Nothing about the
   audit process changes for chain PRs.
7. **Merge → next.** Amethyst merges (only she merges). The event watch sees
   the merge, comments the merge SHA and time on the issue, closes it, and
   immediately opens the next issue. The issue timeline is the ledger: it
   records exactly when each merge happened and when the next task started.

## Retry and resume

If Gokoo's run dies (credit/usage exhaustion) mid-task, the next driver run —
every 30 minutes — resumes from the `gokoo/<task-id>` branch. The branch is
the resume token; there is no separate retry state to lose. Before stopping,
push whatever partial work exists and comment on the issue
(`credit exhaustion at <time>`); if death comes before that, the driver
treats an open `gokoo-task` issue with no linked PR and no recent activity
as a stalled attempt and resumes it anyway.

Credit exhaustion is not failure. A real failure (tests won't pass, the build
breaks, a design blocker) gets a comment explaining why and the
`needs-owner` label — never a silent retry.

## Pausing the chain

- **A task needs Amethyst** (an owner decision like D2/D3, a 7-day soak, a
  validator she runs, an independent review): comment why on the issue, label
  it `needs-owner`, stop. The chain waits until she acts.
- **Stop everything**: close the open `gokoo-task` issue. The driver starts
  nothing while no chain issue is open.

## The first task

The chain is merge-triggered, so the first task needs a manual kick: Gokoo
opens its issue and starts work directly. Everything after that follows the
loop.

## Labels

| Label | Meaning |
| --- | --- |
| `gokoo-task` | The one in-flight chain task (issue). |
| `gokoo-task-pr` | The PR doing that task. |
| `needs-owner` | Blocked on Amethyst; chain paused. |

## What the chain doesn't do

- Never merges, never approves, never pushes to main. Amethyst merges.
- Never reorders `MILESTONES.md` or the plan queue.
- Never touches `claude-review.yml`, `review-relay.yml`,
  `review-consensus-override.yml`, or their scripts.
