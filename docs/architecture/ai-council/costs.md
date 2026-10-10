# Council Operating Costs & Usage

What the council costs to run, per participant. Updated 2026-10-10.
This is a living document — update it when usage changes, quotas reset,
or billing changes.

**Why this exists:** The council at full effectiveness costs real money.
This document makes those costs visible so availability decisions
("can we afford to run the full council on this PR?") are explicit,
not accidental. A participant marked `UNAVAILABLE` for cost reasons
gets the same visibility and governance as one down for technical reasons.

---

## Current usage snapshot (2026-10-10)

| Participant | Plan / Quota | Used | Remaining | Resets / Expires | Notes |
|---|---|---|---|---|---|
| Gokoo (Muse tokens) | 3B referral tokens | ~6% (~180M) | ~2.8B | Never expires | Primary build + verification budget. Barely dented. |
| Claude (audit) | _Unknown_ | _Unknown_ | _Unknown_ | _Unknown_ | Audit-only by policy (usage burns fast). Need actual numbers. |
| CodeRabbit | _Unknown_ | _Unknown_ | _Unknown_ | _Unknown_ | Need plan/quota info. |
| Devin | _Unknown_ | _Unknown_ | _Unknown_ | _Unknown_ | Need plan/quota info. |
| Greptile | Unlimited | N/A (unlimited) | N/A | ~11–12 days remaining (as of 2026-10-10) | Unlimited window expiring soon — plan for what comes after. |
| Copilot (Adv. Security) | Monthly quota | 100% (exhausted) | 0 | _Reset date TBD_ | First recorded outage (see `incidents.md`). |

---

## Gaps to fill

- [ ] **Claude:** Actual usage/billing. What plan, what's been spent, what remains?
- [ ] **CodeRabbit:** Plan type, quota, current usage.
- [ ] **Devin:** Plan type, quota, current usage.
- [ ] **Copilot:** Exact quota reset date.
- [ ] **Greptile:** What happens after the unlimited window? Paid plan cost?

---

## Cost per PR (to be measured)

Once we have baseline usage data, track approximate cost per PR:
which reviewers ran, what they consumed. This informs the
"budget council vs. full council" decision for future work.

_No data yet — start logging after baseline is established._

---

## Policy

- Update this file when any participant's quota, plan, or billing changes.
- A cost-driven `UNAVAILABLE` is recorded in `incidents.md` like any other outage.
- The "two-member floor" (Gokoo + Claude) is the minimum viable council;
  see `degraded-operation.md`.
- Do not merge PRs that expand council usage without updating this document.
