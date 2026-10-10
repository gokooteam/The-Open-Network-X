# ADR-0049 — AI review council as a first-class component

**Status:** Proposed.
**Decider:** Amethyst.

## Context

The Open Network X development process has evolved beyond a single AI coding agent.

Gokoo is the lead development agent, but the repository now also relies on multiple independent AI systems for review, adversarial analysis, deliberation, and quality control. These currently include Claude, CodeRabbit, Devin, and Greptile.

The evolution, in order:

```
single AI developer (Gokoo builds)
        ↓
independent AI reviewers (Claude audits PRs)
        ↓
multiple reviewers (CodeRabbit, Devin, Greptile added as GitHub Apps)
        ↓
sequential review relay (docs/guides/review-relay.md — turns 1-3 coordinate
  the reviewers so later turns respond to earlier ones)
        ↓
deliberative council (turn 2 second-opinion, turn 3 moderator synthesis,
  turn 4 Gokoo live verification — disagreements surfaced and resolved)
        ↓
council recognized as a first-class system component (this ADR)
```

The council's perspectives, sequencing, disagreements, and synthesis have become part of how the repository is developed and validated. A PR does not merge on Gokoo's say-so; it merges when the council reaches consensus GO and Amethyst merges.

Therefore, council availability and participation are now system-level concerns.

Some providers are available through subscriptions or free trials and may become temporarily unavailable. A temporary service outage, expired trial, API failure, or authentication problem must not silently redefine the architecture.

## Decision

**The AI review council is a first-class component of the Open Network X development system.**

This does not mean the AI systems possess independent project authority. Human-defined project rules and repository policy remain the governing layer. Amethyst merges; the council advises.

It means the council is now an explicit architectural subsystem with:

- defined participants (`docs/architecture/ai-council/registry.yaml`);
- defined roles (lead, independent reviewer, adversarial reviewer, security reviewer, synthesis/moderator — separated from specific vendors);
- defined interfaces (GitHub PR reviews, review-relay turns, commit statuses);
- defined review stages (turns 1–4, documented in `docs/guides/review-relay.md`);
- defined state (the status model: AVAILABLE, DEGRADED, UNAVAILABLE, DISABLED, NOT_CONFIGURED);
- defined availability (health checks, `scripts/ai-council/health.sh`);
- defined failure modes (what happens when a participant is unavailable);
- defined participation records (review provenance per PR);
- defined degraded-operation behavior (when full council is required vs. permitted to degrade).

### Architecture vs. availability

The critical invariant:

> **UNAVAILABLE does not mean REMOVED FROM THE ARCHITECTURE.**

If a free trial expires:

```text
Greptile
Architectural membership: Council Reviewer
Operational status: UNAVAILABLE
```

The system must not silently reinterpret this as "Greptile never existed." Architecture and operational availability are separate concepts. The registry records both.

### Degraded operation

A missing reviewer must never silently reduce review coverage. The policy (`docs/architecture/ai-council/degraded-operation.md`):

- defines when full council participation is required (consensus-impact PRs);
- defines when degraded operation is permitted (docs-only, with explicit warning);
- defines which roles are irreplaceable for which change classes (the independent audit role cannot be waived for consensus changes);
- makes every absence visible in the council health report and the PR's review provenance.

### Governance boundary

```
Human project intent / constitutional rules (Amethyst, MILESTONES.md, ADRs)
                    ↓
               Lead agent (Gokoo)
                    ↓
              Implementation
                    ↓
           Independent council (this ADR)
                    ↓
               Deliberation
                    ↓
                Revision
                    ↓
             Tests / validation
                    ↓
              Repository state
```

The council does not become the ultimate authority merely because it is a first-class component. Its role is independent scrutiny and deliberation within the project's governing rules.

## Consequences

- The council registry becomes the canonical source of truth for membership. Adding or removing a participant is an architectural change requiring its own review.
- CI validates the registry (required fields, valid statuses, role definitions).
- The health report makes current operational state visible to anyone entering the repository.
- Review provenance records which council configuration reviewed each PR, enabling future audit of the development process itself.
- This ADR, and the PR that implements it, become the historical record of the decision.

## History

This ADR formalizes what the repository already practices. The review relay (`docs/guides/review-relay.md`), the turn structure, and the consensus gate predate this decision. What changes is that the council is now *explicit* — named, registered, health-checked, and governed — rather than incidental tooling that happens to be useful.
