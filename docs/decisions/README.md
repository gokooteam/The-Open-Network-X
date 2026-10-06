# Decision records have moved

All Architecture Decision Records now live in a single series under
[`docs/adr/`](../adr/). The `docs/decisions/` location is retired; this
README remains for one release cycle so existing links can be followed.

## Why

Two ADR series grew up side by side with colliding numbers (`docs/adr/`
0001–0007 for the October message-model work, `docs/decisions/`
ADR-0001–ADR-0020 for the September specification work), so a bare
reference like "ADR-0003" was ambiguous. They are now one continuous
series, `docs/adr/0001`–`docs/adr/0027`. File contents are unchanged;
only the numbers 0008–0027 below are new.

## Mapping

| Old path | New path |
|---|---|
| `docs/decisions/ADR-0001-preserve-multichain-architecture.md` | `docs/adr/0008-preserve-multichain-architecture.md` |
| `docs/decisions/ADR-0002-protocol-primitives-and-serialization.md` | `docs/adr/0009-protocol-primitives-and-serialization.md` |
| `docs/decisions/ADR-0003-state-model-and-account-lifecycle.md` | `docs/adr/0010-state-model-and-account-lifecycle.md` |
| `docs/decisions/ADR-0004-implementation-language.md` | `docs/adr/0011-implementation-language.md` |
| `docs/decisions/ADR-0005-transactions-and-messaging.md` | `docs/adr/0012-transactions-and-messaging.md` |
| `docs/decisions/ADR-0006-blocks-and-masterchain-coupling.md` | `docs/adr/0013-blocks-and-masterchain-coupling.md` |
| `docs/decisions/ADR-0007-execution-model-and-merkle-proof-reservation.md` | `docs/adr/0014-execution-model-and-merkle-proof-reservation.md` |
| `docs/decisions/ADR-0008-consensus-and-validator-operation.md` | `docs/adr/0015-consensus-and-validator-operation.md` |
| `docs/decisions/ADR-0009-networking-adnl-and-rldp.md` | `docs/adr/0016-networking-adnl-and-rldp.md` |
| `docs/decisions/ADR-0010-networking-dht.md` | `docs/adr/0017-networking-dht.md` |
| `docs/decisions/ADR-0011-networking-overlay.md` | `docs/adr/0018-networking-overlay.md` |
| `docs/decisions/ADR-0012-dynamic-sharding.md` | `docs/adr/0019-dynamic-sharding.md` |
| `docs/decisions/ADR-0013-economics.md` | `docs/adr/0020-economics.md` |
| `docs/decisions/ADR-0014-payment-channels.md` | `docs/adr/0021-payment-channels.md` |
| `docs/decisions/ADR-0015-project-license.md` | `docs/adr/0022-project-license.md` |
| `docs/decisions/ADR-0016-merge-block-second-parent-reference.md` | `docs/adr/0023-merge-block-second-parent-reference.md` |
| `docs/decisions/ADR-0017-tvm-instruction-set.md` | `docs/adr/0024-tvm-instruction-set.md` |
| `docs/decisions/ADR-0018-hypercube-fast-path-and-cross-workchain-rules.md` | `docs/adr/0025-hypercube-fast-path-and-cross-workchain-rules.md` |
| `docs/decisions/ADR-0019-economics-parameters.md` | `docs/adr/0026-economics-parameters.md` |
| `docs/decisions/ADR-0020-validator-slashing-and-reward-distribution.md` | `docs/adr/0027-validator-slashing-and-reward-distribution.md` |

New records go in `docs/adr/` as the next number after the highest
existing one. See `CONTRIBUTING.md` for the process.
