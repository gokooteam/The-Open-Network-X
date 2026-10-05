# ADR-0005 — Chain-ID-bound signatures

**Status:** Accepted
**Date:** 2026-10-05
**Milestone:** message-based single-shard chain

## Context

Signed external messages are bearer authorizations: whoever holds one can
submit it. Without a chain binding, a message signed for one chain (testnet,
a fork, a replayed history) verifies on any other chain with the same
account keys — a classic cross-chain replay.

The old model had no chain binding at all (signatures covered only the
transaction body). The chain ID must live somewhere the STF can check it:
it cannot be `last_hash`, which stops being the genesis hash after block 1.

## Decision

- **Chain ID = genesis hash**, stored in `State.chain_id`, set from
  `GenesisDocument::genesis_hash()` at genesis, carried forward unchanged
  by `apply_block`, and populated by storage `load_state` from the meta
  table's existing `chain_id` key (already written at genesis init).
- **The signature covers the chain ID.** The signed body bytes include
  `chain_id` as their first field, under domain tag `ONX_MSG_EXT_SIGN_V1`.
  A message signed for chain A carries A's genesis hash in its signed
  bytes; on chain B the wallet handler rejects it twice: first the
  explicit `WrongChainId` check (clear error), and even without that check
  the signature would not verify against B's key resolution — the signed
  bytes commit to A's ID.
- **Defense in depth, not either/or.** The explicit check fails fast with
  a descriptive error; the signature binding is the cryptographic guarantee
  that survives even if a future code path forgets the check.

## Alternatives considered

- **Chain ID as a config file value.** Rejected: config can drift between
  the mempool, the producer, and the store. The genesis hash is already
  the chain's canonical identity and already persisted; deriving the ID
  from it keeps one source of truth.
- **`last_hash` as the chain ID.** Rejected: it changes every block. The
  task called this out explicitly.
- **Signature covers chain ID but no explicit check.** Rejected: a bare
  `InvalidSignature` on a cross-chain replay is a miserable debugging
  experience. The explicit `WrongChainId { expected, got }` names the
  problem.
- **Chain ID in the header only.** Rejected: headers are producer-built;
  the binding must be in the *signed* bytes, verified against state, or a
  malicious producer could retarget messages.

## Consequences

- The mempool validates chain ID at the door (`Mempool::new` takes the
  chain ID from the store); wrong-chain messages land in `rejected/`,
  never in a block.
- Key-reveal messages (ADR-0006) are chain-bound too — a reveal for chain
  A cannot be replayed to claim the same address on chain B.
- Test scaffolding (`test_block_txs`, `TestTxGen`, the crash probe) takes
  an explicit `chain_id`; there is no ambient default that could
  silently cross chains.
