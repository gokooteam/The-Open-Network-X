# ADR-0006 — Key-derived addresses

**Status:** Accepted
**Date:** 2026-10-05
**Milestone:** message-based single-shard chain

## Context

An account that receives funds with no key on file must be able to spend
later — otherwise every plain transfer to a new address burns the funds
into an unspendable account. The old model punted: keyless accounts "can
receive but never spend," with key assignment deferred to "a future
transaction type or VM hook."

Two designs were on the table:
1. **Key-derived addresses**: the address commits to the pubkey
   (`address = hash(pubkey)`); the first spend reveals the pubkey, which
   must hash to the address.
2. **Key-assignment message**: a special message type that sets the key
   on a keyless account.

## Decision

**Key-derived addresses** (option 1):

- `derive_address(pubkey) = domain_hash(ONX_ADDR_V1, pubkey)`.
- Accounts created by receiving (delivery to `Uninitialized`) are born
  keyless as before — no protocol change at creation.
- The first spend from a keyless account MUST carry a non-zero `pubkey`
  in the external message. The wallet handler checks
  `derive_address(pubkey) == from`, verifies the signature against the
  revealed key, and **stores the key** — the account is keyed from then on.
- A keyed account's message must NOT carry a pubkey
  (`UnexpectedPubkeyReveal`); key rotation is out of scope.
- An address that was **not** derived from any key can never satisfy the
  reveal check and stays unspendable — exactly the old behavior, with no
  special-casing.

## Alternatives considered

- **Key-assignment message (option 2).** Rejected on auth-anchor grounds:
  a keyless account has no key to authorize the assignment *with*. Anyone
  could race to claim any keyless account's funds by assigning their own
  key first — the assignment message would need its own authorization
  story, which is exactly the problem it was supposed to solve. Key
  derivation sidesteps it: the address *is* the commitment, so there is
  nothing to race over.
- **Require keys at account creation** (sender supplies the receiver's
  pubkey). Rejected: the sender doesn't know the receiver's key in the
  general case (someone paying an address from an invoice). Creation must
  stay keyless.
- **Social/recovery keys, multisig.** Out of scope — future auth work,
  not this milestone.

## Consequences

- Wallets must derive addresses as `domain_hash(ONX_ADDR_V1, pubkey)` for
  the "receive first, spend later" flow; random addresses remain valid but
  permanently unspendable (documented, not an error).
- The reveal is inside the signed body (ADR-0002), so it cannot be
  stripped or swapped by a relayer.
- The mempool mirrors the wallet's key-resolution at intake, so invalid
  reveals are rejected at the door.
- Genesis accounts with explicit pubkeys are unaffected.
