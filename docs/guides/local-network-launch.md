# Launch Guide — single node

This guide runs one `onxd` block producer on your machine. There is no
multi-node network yet: networking is unfrozen per ADR-0042 but not yet
implemented (M5 in progress), and `onxd`
refuses to start with `network_enabled=true`. The four `node-*.toml`
stubs that `onx-genesis` emits are for a future validator network; they
are not usable today.

## 1. Generate genesis

```sh
cargo run -p onx-genesis -- --config config/genesis.toml --out target/onx-genesis
```

This writes `target/onx-genesis/genesis.boc` (a deterministic genesis
payload) plus the not-yet-usable `node-0.toml` … `node-3.toml` stubs.

## 2. Write a single-node config

`onxd` needs a config file with networking disabled (the default is
`network_enabled = true`, which `onxd` currently refuses to start with).
Minimum viable `onxd.toml`:

```toml
role = "full"
storage_path = "./onx-data"
network_enabled = false
bootstrap_genesis = "config/genesis.toml"
tx_pool_dir = "./onx-txpool"
fee_collector = "<64-hex-chars of the fee-collector account>"
```

`bootstrap_genesis` points at the genesis TOML (the same format
`onx-genesis --config` reads). `fee_collector` is required: block
production is explicit about who collects fees.

## 3. Run the producer

```sh
cargo run -p onxd -- --config onxd.toml
```

The daemon watches `tx_pool_dir` for signed external-message files
(`*.msg`). Drop one in and a block is produced on demand — no empty
blocks, no busy-spin, no wall-clock time in block content. Committed
blocks land as `block-*.blk` files under `./onx-data/blocks/`, and the
chain database lives at `./onx-data/chain.redb`.

Shut down with Ctrl-C: an in-flight commit finishes atomically before
exit, and the mempool rehydrates from `pending/` on restart.

## 4. Replay and verify

The producer's output is independently verifiable with the replay tool:

```sh
cargo run -p onx -- replay --genesis config/genesis.toml --blocks ./onx-data/blocks/ --data-dir ./verify-data
```

The roots must match the producer's. Kill -9 the daemon mid-block and
resume: replay converges on the same state root.

## When multi-node arrives

Validator networking, peer discovery, and consensus are specified but
frozen (see the README status table). This guide will grow a real
"launch N nodes" section when the network scaffold is replaced —
tracked in `ROADMAP.md`, not here.
