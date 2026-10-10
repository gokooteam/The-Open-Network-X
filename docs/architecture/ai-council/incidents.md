# Council Incident Log

Real-world availability events affecting council participants or review tooling.
Each entry records what went down, how the council responded, and what was learned.

The degraded operation policy (`degraded-operation.md`) defines how the council
*should* behave when participants are unavailable. This log records how it
*actually* behaved.

---

## 2026-10-10: Copilot agent quota exhausted (first recorded incident)

**What happened:**
The `github-advanced-security` check on PR #60 failed because the Copilot agent
monthly quota was exhausted. The "Code scanning AI findings" workflow failed in
its "Processing Request" step with `SessionModelError: You have exceeded your
monthly quota` (HTTP 402).

**Scope:**
- Affected: GitHub Advanced Security AI code scanning on all PRs.
- Not affected: Claude (turn-1 audit), CodeRabbit, Devin, Greptile — all continued
  normal review activity. The review relay, consensus computation, and all other
  council functions were unimpaired.

**Council response:**
- The failure was visible (check marked failed on the PR) and correctly diagnosed
  as an infrastructure/quota issue, not a code defect.
- No council participant was blocked. Claude's turn-1 audit completed normally
  ("Consensus impact: none"). CodeRabbit, Devin, and Greptile reviews posted as usual.
- The PR was not mechanically blocked (main has no required-checks protection
  that would halt on this failure).
- Per the degraded operation policy, this is a **warning, not a block**: the
  unavailable tool is supplementary scanning, not a required council role.

**Classification:**
- Status: `UNAVAILABLE` (quota exhaustion — temporary, resolves on reset or billing change).
- This is the **first recorded instance** of a review agent/tool becoming
  unavailable in production. It validates the core architectural distinction:
  the tool went down, the council kept working, and the absence was visible
  rather than silent.

**What was learned:**
- The status model held: one component `UNAVAILABLE` did not cascade.
- The failure was correctly attributed (quota, not code) because the error was
  explicit. Silent failures would be harder — this reinforces the requirement
  that absence must be observable.
- Billing/quota limits are a real availability vector for AI review tooling,
  distinct from API outages or model errors. Future registry entries for
  quota-limited tools should note their quota scope.
