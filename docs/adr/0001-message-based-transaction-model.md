# ADR-0001 — Replace synchronous transfers with the actor message model

**Status:** Accepted
**Date:** 2026-10-05
**Milestone:** message-based single-shard chain

## Context

The protocol's own spec (`docs/specification/transactions.md:26`) states that
accounts interact *exclusively* via asynchronous messages. The whitepaper
(§2.4) describes the full actor model: external messages ("from nowhere"),
internal messages between accounts, output queues, bounce semantics, and
double-delivery prevention.

The implementation, however, used synchronous transactions: `apply_tx`
debited the sender and credited the receiver atomically, with the receiver's
contract executing inline in the sender's transaction. An external review
flagged this as a spec contradiction, and every feature built on sync
transfers (fees, nonces, contract calls) compounds the eventual rewrite.

This milestone replaces the **dispatch layer**. The VM (`onx-execution`)
stays as the receiver-side execution engine, audited and untouched.

## Decision

Every value movement becomes a message delivery in two phases:

1. **Phase 1 — wallet (auth).** Each external message is authenticated in
   block order by a built-in wallet handler in the STF (not the VM):
   chain-ID binding, sender key/nonce/signature, balance. On success the
   sender is debited (`amount + fee`, fee split 50/50 burn/validator as
   before), the nonce bumps, and exactly one internal message is queued.
2. **Phase 2 — delivery.** The queue drains FIFO. Each delivery executes as
   the receiver's own transaction: value credit, plus TVM execution when the
   payload is non-empty and the receiver has contract code. A message that
   cannot be processed **bounces** (value minus fees returns to the sender
   as a new internal message).

Block bodies commit **only** to the ordered external-message set
(`msgs_root`). Internal messages are derived during execution and never
submitted — cross-block internal replay is structurally impossible.

## Alternatives considered

- **Keep sync transfers, add messages alongside.** Rejected: two dispatch
  models means two auth models, two replay schemes, and the spec
  contradiction remains. The review was explicit that the sync model had to go.
- **Full TON-style multi-block delivery** (messages live in persistent
  output queues across blocks). Rejected for this milestone: single shard,
  single node — same-shard delivery within the originating block is
  explicitly permitted by the whitepaper (§2.4.27) and needs no queue
  persistence. See ADR-0003.
- **Run contract code in the sender's phase** (keep `apply_tx` shape, just
  rename). Rejected: it preserves the spec violation (the receiver's code
  executing as part of the sender's transaction) while adding message
  ceremony.

## Consequences

- `Transaction`/`TxKind`/`apply_tx` are gone, replaced by
  `ExternalMessage`/`InternalMessage`, `wallet_receive`, and `deliver`.
- Delivery is observable: receipts now carry per-message delivery outcomes
  (delivered/bounced, gas used), not just balance deltas.
- VM exceptions no longer invalidate blocks — they bounce (ADR-0004).
  This is the largest semantic change and is deliberate: in the actor
  model, a failed delivery is a message-level event, not a chain-level one.
- The old `phase3_stf`, `tx_auth`, `tvm_integration`, `phase5_replay`,
  `tvm_replay`, and `producer_loop` tests were reworked to the message
  model; `tx_auth.rs` was migrated to `message_auth.rs`; hand-derived
  vectors were refrozen under the new domain tags.
- Deferred (unchanged): multi-cell code, gas refunds, contract message
  egress (a contract that emits messages bounces its delivery), sharding,
  networking, consensus.
