# AI Council — Status Model

## Operational states

| State | Meaning |
|-------|---------|
| `AVAILABLE` | The participant is operational and participating normally. |
| `DEGRADED` | The participant is partially operational (e.g., rate-limited, slow, returning partial results). Findings should be treated with reduced confidence. |
| `UNAVAILABLE` | The participant cannot currently operate (e.g., expired trial, API outage, auth failure). **The participant REMAINS in the architecture.** This state is temporary and must not be confused with removal. |
| `DISABLED` | A human (Amethyst) has deliberately disabled the participant. The architecture still lists them, but they are intentionally not participating. |
| `NOT_CONFIGURED` | The participant's role exists in the architecture but no provider is currently assigned (e.g., a future role awaiting its first implementation). |

## The central invariant

> **UNAVAILABLE does not mean REMOVED FROM THE ARCHITECTURE.**

Example — a trial expires:

```text
CORRECT:
  Greptile
  Architectural membership: Council Reviewer (independent_reviewer)
  Operational status: UNAVAILABLE
  Reason: trial expired 2026-10-08

WRONG:
  (Greptile silently disappears from all docs and checks)
```

When a participant becomes UNAVAILABLE:

1. Their `status` field in `registry.yaml` is updated (by the health check or manually).
2. The `last_successful_check` field is left at its last good value (it records history, not current state).
3. A note is added explaining the reason.
4. The council health report reflects the change immediately.
5. The degraded-operation policy determines whether work can proceed.

Removing a participant from the registry entirely is an **architectural change**. It requires its own PR, its own review, and Amethyst's decision. It is never done silently as a side effect of an outage.

## State transitions

```
NOT_CONFIGURED ──→ AVAILABLE     (provider assigned and working)
AVAILABLE      ──→ DEGRADED      (partial failure detected)
AVAILABLE      ──→ UNAVAILABLE   (full failure detected)
DEGRADED       ──→ AVAILABLE     (recovered)
DEGRADED       ──→ UNAVAILABLE   (worsened)
UNAVAILABLE    ──→ AVAILABLE     (recovered)
any            ──→ DISABLED      (human decision only)
DISABLED       ──→ AVAILABLE     (human decision only)
```

Only a human (Amethyst) can set DISABLED. All other transitions are determined by health checks or observed behavior.
