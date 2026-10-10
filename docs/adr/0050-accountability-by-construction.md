# ADR-0050: Accountability by Construction

**Status:** Proposed
**Date:** 2026-10-10

## Context

Byzantine Fault Tolerant protocols assume some participants will act maliciously.
The standard response is to make the protocol *resistant* to such behavior —
to ensure safety and liveness despite it.

This ADR establishes a stronger principle: the protocol should make
misbehavior *self-incriminating*. Not just resistant to attack, but
structured so that attacking generates undeniable proof of the attack.

## Decision

**Design every protocol message so that equivocation is cryptographically
provable by any observer, without trusted intermediaries.**

Specifically:

1. **Every signed consensus message includes sufficient context** (height,
   round, block hash, message type) that two conflicting signatures from
   the same validator constitute independently verifiable proof of
   misbehavior. No trusted observer, no special access — anyone with both
   signatures can prove the equivocation.

2. **Define provable misbehavior in the protocol spec.** The exact conditions
   that constitute slashable evidence (double-signing, equivocating proposals,
   voting for conflicting blocks) are specified alongside the protocol rules,
   not as an afterthought.

3. **Standardize the evidence format.** Fraud proofs have a defined structure
   so that any participant — validator, full node, or external observer —
   can construct, verify, and submit them.

4. **Make the evidence permanent.** Once equivocation proof exists, it is
   recorded in a way that cannot be erased by the misbehaving party.

## Rationale

The goal is to make dishonesty **irrational**, not just difficult.

Every protocol message carries enough context that misbehavior is provable by anyone, from the messages alone — no trusted observer needed. Because detection is certain and the cost always exceeds the gain, a rational actor does not cheat. This is stronger than making cheating *hard* — hard just raises the bar for sophisticated attackers. Certain detection changes the incentive calculation entirely.

This principle also simplifies the security model. Instead of reasoning
about "what if an attacker does X and we don't catch them," we reason about
"what happens when we have proof they did X." The second question has
clearer answers.

## What This Is Not

- **Not a replacement for BFT safety.** The protocol must still be safe
  under Byzantine faults. Accountability is a complement, not a substitute.
- **Not automatic punishment.** Detection and punishment are separate.
  This ADR covers detection (making proof inevitable). Slashing mechanics,
  governance responses, and penalty calibration are separate decisions.
- **Not surveillance.** Validators know exactly what constitutes provable
  misbehavior because it's in the spec. There are no hidden tripwires.

## Consequences

- All new consensus message types must include equivocation-proof context.
- The slashing/enforcement design (deferred per earlier decisions) must
  consume this evidence format when implemented.
- Testing must include equivocation scenarios that verify proof generation,
  not just attack resistance.

## Related

- M6 equivocation handling (driver logs explicit EQUIVOCATION evidence)
- Deferred: slashing enforcement mechanism
- Deferred: correlation-scaled penalties for mass equivocation
