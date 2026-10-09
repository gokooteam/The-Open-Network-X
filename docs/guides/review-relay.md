# PR review relay

Several AI reviewers look at every pull request. Without coordination they
all start on the same push, post within the same few minutes, and never read
each other. The relay runs the reviews we control as ordered turns, so later
turns respond to earlier ones and the PR ends with one summary instead of a
pile of separate verdicts.

## The turns

| When | Who | What it posts |
| --- | --- | --- |
| On each push | CI, labelers | checks and labels |
| On each push | CodeRabbit, Devin, Greptile (GitHub Apps) | their own reviews; they can't be delayed from a workflow |
| On each push | **Turn 1**: Claude PR audit (`claude-review.yml`) | inline findings and one summary comment |
| After turn 1 succeeds and the apps finish | **Turn 2**: second opinion (`review-relay.yml`) | replies where it disagrees with a finding, a 👍 on findings it confirms, and one review with anything everyone missed |
| After turn 2 | **Turn 3**: moderator (`review-relay.yml`) | replies settling disagreements, and the **Review relay: state of the review** comment |

The summary comment is edited in place on each push, so a PR has one. It
gives:

- **Blocking**: whether a critical, high or medium finding is open and not
  refuted. While one is, the PR carries the `review-blocking` label, so it
  shows in the PR list before anyone merges. The label is removed when the
  findings are fixed or refuted.
- **Consensus impact**: `breaking` (a node with and a node without the PR
  compute different state for the same blocks), `adjacent` (touches
  consensus code, results unchanged), `none`, or `unclear`. Turn 1 gives the
  same verdict and checks it against the one ticked in the PR template.
- **Determinism**: whether the change could behave differently on two
  honest nodes given the same inputs.
- **Open findings**, one row per distinct problem with every reviewer that
  raised it. Several reviewers flagging the same line is the strongest
  signal in the review, so the relay counts it instead of leaving three
  threads. Low findings are marked `nit:`.

The relay reports; it never approves. Whether to merge stays with the
maintainer.

Turn 2 waits until the `CodeRabbit` and `Devin Review` commit statuses on
the head commit are no longer pending and Greptile has reviewed the PR once
(it reviews only a PR's first commit). It waits at most 10 minutes, then
goes ahead with what is there. These are the `RELAY_WAIT_*` variables at the
top of `review-relay.yml`; edit them if a review app is added or removed.

## When the relay doesn't run

- **Docs-only PRs** (every changed file is under `docs/` or ends in `.md`):
  turn 1 is enough.
- **Drafts and fork PRs**: the same rules as turn 1.
- **A newer push**: the relay for the older commit is cancelled or skips.
  Each posting step checks the PR head again first, so an older run never
  overwrites the summary or label for a newer commit.
- **Turn 1 failed or was cancelled.**
- **Already ran for this commit** (for example when turn 1 is re-run).

To run it by hand, for example on a PR opened before the relay existed:
Actions → **Review Relay** → Run workflow, with the PR number. Tick *force*
to run again on a commit the relay already covered.

## Limits that keep it from turning into noise

- Turn 2 posts at most 5 replies and 5 new findings; turn 3 at most 3
  replies. Agreement is a 👍 reaction, not a comment.
- The model turns run with read-only tools (`Read`, `Grep`, `Glob`) and
  return JSON. `.github/scripts/review-relay.cjs` posts it, taken from the
  default branch rather than the PR. It replies only to comments in open
  threads, comments inline only on lines inside the diff (findings on a
  changed file the API sends no diff for go in one comment), caps lengths,
  and breaks `@mentions` so nobody is pinged. A model turn that returns
  nothing fails its job instead of leaving an old summary looking current.
- The models see comments only from people with write access (looked up,
  not inferred from their association with the repo) and from the review
  apps listed in `RELAY_TRUSTED_BOTS`. Anyone else's comment on a public PR
  is left out. Add an app to that list when you install one.
- The `review-blocking` label is created the first time it's needed. If it
  can't be set or removed, the job fails rather than leaving a wrong label.
- Nothing the relay posts can start another relay run: it posts with
  `GITHUB_TOKEN` and is only triggered by turn 1 finishing.

## Why it works this way

Two research notes from 2026-10-08 shaped it: a read of every review on
PRs #1–#42 here, and a survey of review practice in Bitcoin Core,
go-ethereum, Agave, the TON monorepo and the Cosmos SDK.

- Here, the same real bug was often found independently by two or three
  bots (the stale `c0` on #32, the `pipefail` bug on #31). That agreement
  is the signal; the three separate threads were the cost.
- #32 merged 17 minutes after its audit listed two HIGH findings; the
  fixes came in follow-up PRs because someone remembered. The label makes
  an open HIGH visible instead.
- Every project surveyed keeps consensus risk and design judgment with
  people and leaves bots the mechanical work. Cosmos and Agave make
  consensus-breaking changes a declared category; Cosmos makes determinism
  the first question asked. The relay summarises and declares; it doesn't
  approve.

## Changing it

`review-relay.yml` uses `workflow_run`, which GitHub runs only from the
default branch. Changes to it take effect after they merge to `main`, and
can't be tested from the PR that makes them. Test on a throwaway PR
afterwards, or with a manual run.

The Claude action also refuses to run a workflow file that differs from
the one on `main`. On a PR that edits `claude-review.yml` or
`review-relay.yml`, turn 1 reports success without reviewing ("Skipping
action due to workflow validation" in its log). The relay may still run
turns 2 and 3, from the copy on `main`.

The prompts for turns 2 and 3 are in `review-relay.yml`. The rules for what
may be posted are in `validate()` in `review-relay.cjs`.
