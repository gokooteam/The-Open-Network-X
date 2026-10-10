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
- defined availability (health checks, `scripts/ai-council/health.py`);
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

### The gate binds its creator

The consensus gate is a commitment device. It is designed so that no single person — including the project's founder — can merge a consensus-impact change on the strength of a feeling, a late night, or impatience with the process. The override mechanism exists for genuine emergencies, but the norm is that the gate holds until the council reaches GO.

This is intentional. A review system that its own creator can casually bypass is not a review system; it is a suggestion box. The architecture treats the founder's restraint as a feature to be preserved, not an obstacle to be routed around. Future contributors — human or AI — will see that the bar applied to them is the same bar that applied to everyone before them, including the person who built it.

## Historical basis

### Why look at history

The decision to formalize the council did not arise simply because the repository happens to use several review bots. It was informed by research into how historically significant blockchain projects organize distributed protocol development — specifically, the mechanisms they use for proposal review, technical criticism, process governance, and consensus-building.

The goal is not to claim that Open Network X reproduces Bitcoin or Ethereum governance. It does not. The goal is to show the reasoning: we observed structural similarities between established blockchain development practices and the system Open Network X had independently evolved, and we used those precedents to sharpen the architectural decision.

### What the primary sources say

**Bitcoin / BIP process** ([BIP-1](https://github.com/bitcoin/bips/blob/master/bip-0001.mediawiki)):

- A BIP is "a design document providing information to the Bitcoin community, or describing a new feature for Bitcoin or its processes or environment."
- "The BIP author is responsible for building consensus within the community and documenting dissenting opinions."
- "Because the BIPs are maintained as text files in a versioned repository, their revision history is the historical record of the feature proposal."
- The process defines explicit roles: the **author/champion** (shepherds discussion, builds consensus), the **BIP editors** (assign numbers, enforce formatting, maintain process integrity), and the **community** (reviews, discusses, objects).
- The process defines explicit states: Draft, Accepted, Deferred, Rejected, Withdrawn, Final, Superseded.
- There is a structural separation between *proposing* a change (anyone can author a BIP) and *accepting* it (editors gate process compliance; the community converges on consensus).

**Ethereum / EIP process** ([EIP-1](https://github.com/ethereum/EIPs/blob/master/EIPS/eip-1.md)):

- An EIP is "a design document providing information to the Ethereum community, or describing a new feature for Ethereum or its processes or environment."
- "The EIP author is responsible for building consensus within the community and documenting dissenting opinions."
- "Because the EIPs are maintained as text files in a versioned repository, their revision history is the historical record of the feature proposal."
- The process defines the **champion/author**, the **EIP editors**, and the **Ethereum Core Developers**, with the AllCoreDevs call serving as the venue where "client implementers discuss the technical merits of EIPs" and reach "rough consensus."
- The process defines explicit states: Idea, Draft, Review, Last Call, Final, Stagnant, Withdrawn, Living.
- EIP-1 explicitly notes that the EIP process was modeled on Bitcoin's BIP process — the lineage is documented, not inferred.

### The structural analogy

| Historical blockchain development | Open Network X |
|---|---|
| Proposal / author (shepherds discussion, builds consensus) | Lead agent (Gokoo) / implementation proposal |
| Maintainers / editors (process integrity, numbering, gating) | Council orchestration (relay workflow, turn sequencing, consensus status) |
| Independent contributors (review, object, discuss) | Independent AI reviewers (Claude, CodeRabbit, Devin, Greptile) |
| Peer review | Sequential AI review (turns 1–3) |
| Technical objections | Reviewer findings ([critical]/[high]/[medium]) |
| Discussion / dissent | Model disagreement (turn-2 disputes, recorded in relay) |
| Consensus-building | Council deliberation (turn-3 synthesis) |
| Versioned proposal history | Git + PR + review provenance |
| Process evolution | Changes to the orchestration system itself (this ADR) |

This is a **functional analogy**, not an identity claim. The table shows that the *organizational functions* — proposing, independently scrutinizing, disputing, synthesizing, recording — appear in both systems, even though the participants differ.

### What is novel

Traditional blockchain projects use human participants and human institutions. Open Network X is experimenting with a development institution in which several of those roles are performed by language models, coordinated as part of the software-development system itself.

The claim is not "AI has replaced blockchain governance." The claim is:

> **The project has begun applying a distributed, role-differentiated review and deliberation structure — of the kind historically used for blockchain protocol development — to AI-mediated software development.**

The interesting question is not whether the AI systems are equivalent to human maintainers. They are not. The interesting question is whether the *organizational structure* — differentiated roles, independent scrutiny, documented disagreement, deliberation, auditable history — can be meaningfully adapted when the participants are AI systems. This ADR asserts that it can, and that Open Network X is now doing so explicitly.

### Why the council became architectural: the actual evolution

This was not designed in its final form at the beginning. The honest history:

1. Gokoo built; Claude audited PRs (single reviewer).
2. More reviewers were added (CodeRabbit, Devin, Greptile) because more scrutiny seemed better.
3. Parallel reviewers posted overlapping findings within minutes of each other and never read each other. Signal was duplicated; disagreements were invisible.
4. The review relay was built to sequence the reviewers: turn 1 (Claude audit), then turn 2 (second opinion responding to turn 1), then turn 3 (moderator synthesis).
5. With sequencing, deliberation emerged: turn 2 began disputing turn-1 findings, turn 3 began settling disagreements, and the "state of the review" comment became a single synthesis rather than a pile of verdicts.
6. Turn 4 (Gokoo live verification) was added because the relay's claims needed independent checking against the code.
7. At that point the system had persistent participants, differentiated roles, sequencing, shared review state, disagreement handling, consensus computation, failure modes (what if Claude is down?), availability concerns (trial expiries), and historical provenance. Treating it as "a few useful bots" had become an inaccurate description.
8. This ADR formalizes what already exists operationally.

The emergence of the architecture is itself part of the story. A future reader should understand that the council was grown, not drawn.

### Why now

The architectural decision is being made now because the review system has acquired all of the properties that make something architectural rather than incidental:

- **persistent participants** (the same reviewers on every PR);
- **differentiated roles** (auditor, second opinion, moderator, verifier — not interchangeable);
- **sequencing** (turns 1–4, with defined dependencies);
- **shared review state** (the relay comment, the consensus status);
- **disagreement handling** (turn-2 disputes, turn-3 resolutions);
- **consensus/synthesis** (the GO/NO-GO computation);
- **failure modes** (what happens when a reviewer is absent);
- **availability concerns** (trials expire; APIs fail);
- **historical provenance** (per-PR review records);
- **automated orchestration** (workflows, not manual coordination).

When a subsystem has all of these, calling it "tooling" is a category error. It is part of the development system, and the architecture should say so.

### What we are claiming

Open Network X is not claiming to reproduce the governance structures of Bitcoin or Ethereum. The project is adopting a related architectural principle: important changes should pass through differentiated roles, independent technical scrutiny, documented disagreement, deliberation, and an auditable historical process. What is novel here is that several of those roles are performed by language models and coordinated as part of the software-development system itself — and that the system now names, registers, health-checks, and governs those roles explicitly.

### Reviewability

Because this PR establishes the council as part of the system, the council itself should be able to review this historical argument. If a reviewer finds that a historical claim is inaccurate, an analogy too strong, or a cited process misrepresented, that objection should be documented and addressed — not silently removed. Dissent preserved is consistent with the very processes studied here: BIP-1 and EIP-1 both require authors to document dissenting opinions, and both maintain versioned histories precisely so that the reasoning remains inspectable.

## Consequences

- The council registry becomes the canonical source of truth for membership. Adding or removing a participant is an architectural change requiring its own review.
- CI validates the registry (required fields, valid statuses, role definitions).
- The health report makes current operational state visible to anyone entering the repository.
- Review provenance records which council configuration reviewed each PR, enabling future audit of the development process itself.
- This ADR, and the PR that implements it, become the historical record of the decision.

## History

This ADR formalizes what the repository already practices. The review relay (`docs/guides/review-relay.md`), the turn structure, and the consensus gate predate this decision. What changes is that the council is now *explicit* — named, registered, health-checked, and governed — rather than incidental tooling that happens to be useful.
