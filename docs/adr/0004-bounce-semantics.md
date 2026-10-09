# ADR-0004 — Bounce semantics

**Status:** Accepted; **amended by ADR-0037** (2026-10-07): the "VM
exceptions bounce (including out-of-gas)" rule below no longer holds for
`ExceptionKind::OutOfGas`, which is now **fatal** (value credited to the
destination, no bounce). All other exception kinds still bounce as
described here. See ADR-0037 for the taxonomy and rationale. The fatal
`OutOfGas` part of that amendment was **rejected** on 2026-10-08 (see
ADR-0037's status) and reverted in code, so out-of-gas deliveries bounce
again, as described here. Bounce receipts report the gas burned.
**Date:** 2026-10-05
**Milestone:** message-based single-shard chain

## Context

The whitepaper (§2.4.7–§2.4.8) defines bouncing: a message that cannot be
processed returns its value to the sender, minus fees. In the old sync
model, an unprocessable transfer (frozen receiver, failing contract) made
the *block* invalid. The actor model needs a message-level failure that
doesn't poison the block.

## Decision

A delivery bounces when the destination cannot process it:

| Destination state | Payload | Outcome |
|---|---|---|
| Active | empty | deliver: credit value |
| Active with code | non-empty | run TVM; success → credit + update data; exception/out-messages → bounce |
| Active, no code | non-empty | bounce (calls never create code) |
| Uninitialized | empty | deliver: create keyless Active account |
| Uninitialized | non-empty | bounce (calls never create accounts) |
| Frozen / Destroyed | any | bounce |

Bounce mechanics:
- The bounce is a **new internal message**: `{src: dest, dest: src,
  value: original value, fee: 0, payload: empty, is_bounce: true,
  origin: bounced message ID}`, appended to the delivery queue —
  subject to the same FIFO ordering and replay rules (ADR-0007).
- **Fees are not returned.** The sender was debited `amount + fee` at the
  wallet handler; the bounce returns `amount`. The fee paid for the
  attempted delivery (including any VM gas burned before the exception).
- **A bounced delivery writes nothing.** The VM runs pure before any state
  write; on failure neither the value credit nor the contract data update
  is applied — revert-by-construction extended to delivery.
- **A bounce is never itself bounced.** If the bounce's destination cannot
  receive, the block fails closed (`BounceUndeliverable`) rather than
  silently burning the value. This is unreachable in honest operation —
  the bounce target was an `Active` sender at wallet time and nothing
  freezes accounts mid-block — so reaching it means corruption or a
  dispatch bug, and halting is safer than silent loss.
- **VM exceptions bounce; they don't invalidate blocks.** This is the
  deliberate break from the old model: a contract that throws (including
  out-of-gas, including attempting message egress, which is still unwired)
  causes its delivery to bounce, not the block to fail. In the actor model,
  failed delivery is a message-level event.

## Alternatives considered

- **Keep block-invalid on VM failure.** Rejected: it makes one contract's
  bug everyone's liveness problem and contradicts the actor model, where
  the sender and receiver are decoupled. It also creates a trivial DoS:
  anyone can wedge a block by calling a throwing contract.
- **Bounce with fees returned.** Rejected: fees pay for work actually done
  (signature verification, VM gas). Returning them prices failed
  deliveries at zero and invites spam.
- **Burn on undeliverable bounce.** Rejected: silent fund loss on an
  unreachable path. Fail-closed halts visibly instead.
- **Bounce the contract's state changes too (keep them).** Rejected:
  keeping partial effects of a failed execution is the sync-model
  confusion this milestone removes. Bounce = the delivery didn't happen.

## Consequences

- Gas accounting (documented): `gas_limit = fee_nanos × 1000`, unchanged.
  The fee is debited and split at the wallet handler; delivery consumes
  gas against that budget. A bounced contract call still cost its fee —
  the sender pays for the attempt.
- Receipts record `bounced: bool` per delivery, so replays and explorers
  can distinguish "transferred" from "bounced".
- Contract developers must handle bounces (no return values in the actor
  model) — a programming-model change documented for the future SDK.
