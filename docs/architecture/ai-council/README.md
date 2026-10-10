# AI Review Council — Architecture

The AI review council is a **first-class component** of the Open Network X development system (ADR-0049).

## What this is

Gokoo builds. Independent AI systems review. Their deliberation — findings, disagreements, synthesis — is part of how this repository is developed and validated. This directory makes that machinery explicit, auditable, and resilient to provider outages.

## Contents

| File | Purpose |
|------|---------|
| `registry.yaml` | Canonical council membership: who belongs, what role they perform, how to check if they're operational. **The source of truth.** |
| `status-model.md` | Operational states (AVAILABLE, DEGRADED, UNAVAILABLE, DISABLED, NOT_CONFIGURED) and the invariant that UNAVAILABLE ≠ REMOVED. |
| `degraded-operation.md` | What happens when members are unavailable: when it blocks, when it warns, and Amethyst's override. |
| `provenance.md` | How to reconstruct what the council did on any given PR. |

## Quick answers

**Who is supposed to be here?** See `registry.yaml` — the `participants` list.

**Who is actually available right now?** Run `python3 scripts/ai-council/health.py`.

**Why does this exist?** See `docs/adr/0049-ai-council-first-class.md`.

**How do reviews work?** See `docs/guides/review-relay.md` (the turn structure).

## Key invariants

1. **UNAVAILABLE does not mean REMOVED.** A trial expiring changes a status field; it never deletes a registry entry.
2. **A missing reviewer never silently reduces coverage.** Absence is visible in health reports and PR provenance, and the degraded-operation policy determines whether work proceeds.
3. **Architecture ≠ subscriptions.** Commercial availability does not determine architectural membership.
4. **Humans govern.** Amethyst merges. The council advises. The override label exists for explicit human decisions, recorded in provenance.
