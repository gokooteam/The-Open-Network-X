# Council Operating Costs & Usage

What the council costs to run, per participant. Updated 2026-10-10.

**Approach:** We do not try to track exact usage quotas in advance — that
proved impractical. Instead, we record what we know and document outages
when they happen. If a council member shows clear signs of being unable
to operate (quota exhausted, trial expired, API errors), record it in
`incidents.md` and update their `status` in `registry.yaml`.

**Why this exists:** The council at full effectiveness costs real money.
This document makes the known costs visible so availability decisions
are explicit, not accidental.

---

## Known operating costs (2026-10-10)

| Participant | What we know |
|---|---|
| Gokoo (Muse tokens) | ~2.8B tokens remaining (~6% used). Never expires. Primary build + verification budget. |
| Claude (audit) | Audit-only by policy — usage burns fast. Exact billing not tracked; treat as constrained resource. |
| CodeRabbit | Active on PRs. Billing details not tracked. |
| Devin | Active on PRs. Billing details not tracked. |
| Greptile | Unlimited for ~11–12 days (as of 2026-10-10). Plan for what comes after the window expires. |
| Copilot (Adv. Security) | Quota exhausted 2026-10-10. First recorded outage (see `incidents.md`). Reset date TBD. |

---

## What to watch for

- **Greptile unlimited expiry** (~2026-10-21/22): When the window ends, either a paid plan kicks in or Greptile goes `UNAVAILABLE`. Record the outcome in `incidents.md`.
- **Claude usage:** If audits start failing or slowing due to billing limits, that's an `UNAVAILABLE` event — document it, don't just work around it silently.
- **Any new quota/trial:** When a new tool joins the council on a trial or limited quota, note the expiry here so the outage isn't a surprise.

---

## Policy

- A cost-driven `UNAVAILABLE` is recorded in `incidents.md` like any other outage.
  After recording, update the participant's `status` field in `registry.yaml`
  per `status-model.md` — the incident log alone does not change the status.
- The minimum viable council is Gokoo (lead) + Claude (audit). Below this,
  the council cannot function as a council — see `degraded-operation.md`
  for the per-role blocking policy.
- Update this file when known costs change (new billing, trial expiry, plan changes).
