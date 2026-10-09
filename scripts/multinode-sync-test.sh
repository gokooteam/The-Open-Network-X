#!/usr/bin/env bash
# M5 multi-node sync test — "a multi-node test of the real binaries runs
# in CI" (MILESTONES.md).
#
# Two REAL onxd binaries on localhost: a producer and a follower. The
# producer makes blocks from faucet transfers; the follower syncs them
# over ADNL, verifying everything itself. Then the follower is kill -9'd
# and restarted, and must resume and converge.
#
# PASS criteria:
#   1. The follower syncs block 1; its state root equals the producer's
#      (compared via two INDEPENDENT `onx replay` runs on the two block
#      dirs — same genesis, same result).
#   2. After kill -9 and restart, the follower resumes from its head,
#      syncs block 2, and the roots converge again.
#
# All key material is throwaway test material. Ports are fixed
# (19001/19002) so the static peer lists need no discovery.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

echo "== building binaries =="
cargo build --bin onxd --bin onx --bin onx-cli
ONXD="$ROOT/target/debug/onxd"
ONX="$ROOT/target/debug/onx"
ONXCLI="$ROOT/target/debug/onx-cli"

WORK="$(mktemp -d "${TMPDIR:-/tmp}/onx-multinode-XXXXXX")"
PROD_PID=""; FOL_PID=""
cleanup() {
    [ -n "$PROD_PID" ] && kill "$PROD_PID" 2>/dev/null || true
    [ -n "$FOL_PID" ] && kill "$FOL_PID" 2>/dev/null || true
    wait 2>/dev/null || true
    rm -rf "$WORK"
}
trap cleanup EXIT

# --- keys ---
# Validator/faucet: 32 bytes of 0x11 (public test material, like the
# devnet faucet). The daemon wants the raw 32-byte seed file (0600);
# onx-cli wants the same seed as hex text.
python3 -c "import sys; sys.stdout.buffer.write(bytes([0x11]) * 32)" > "$WORK/validator.key"
python3 -c "print('11' * 32)" > "$WORK/validator.seedhex"
# Node keys: fresh random, in both encodings (0600 either way).
rand_seed() { # $1 = output stem
    python3 -c "
import os, sys
seed = os.urandom(32)
open('$1.seedhex', 'w').write(seed.hex())
open('$1.key', 'wb').write(seed)
"
    chmod 600 "$1.seedhex" "$1.key"
}
rand_seed "$WORK/prod-node"
rand_seed "$WORK/fol-node"
chmod 600 "$WORK/validator.key"

# ed25519 pubkeys via the repo's own reference implementation
# (reference/ed25519.py — the same code that generates the checked-in
# test vectors the Rust code verifies against).
pubkey_of() {
    python3 -c "
import sys; sys.path.insert(0, '$ROOT/reference')
import ed25519
seed = bytes.fromhex(open('$1').read().strip())
print(ed25519.pubkey_from_seed(seed).hex())
"
}
VALIDATOR_PK="$(pubkey_of "$WORK/validator.seedhex")"
PROD_NODE_PK="$(pubkey_of "$WORK/prod-node.seedhex")"
FOL_NODE_PK="$(pubkey_of "$WORK/fol-node.seedhex")"
echo "validator/faucet pubkey: $VALIDATOR_PK"

# --- genesis: the validator's account is funded (faucet) ---
cat > "$WORK/genesis.toml" <<EOF
[[balances]]
address = "$VALIDATOR_PK"
amount = 1000000000
public_key = "$VALIDATOR_PK"

[[validators]]
public_key = "$VALIDATOR_PK"
stake = 1000

[[workchains]]
id = -1
name = "masterchain"
enabled = true
EOF

# --- configs ---
PROD_PORT=19001; FOL_PORT=19002
mkdir -p "$WORK/prod/txpool" "$WORK/fol"
cat > "$WORK/prod/onxd.toml" <<EOF
role = "validator"
storage_path = "$WORK/prod/data"
network_enabled = true
network_bind = "127.0.0.1:$PROD_PORT"
peers = "$FOL_NODE_PK@127.0.0.1:$FOL_PORT"
bootstrap_genesis = "$WORK/genesis.toml"
tx_pool_dir = "$WORK/prod/txpool"
fee_collector = "$VALIDATOR_PK"
block_poll_interval_ms = 100
signing_key_path = "$WORK/validator.key"
node_key_path = "$WORK/prod-node.key"
EOF
cat > "$WORK/fol/onxd.toml" <<EOF
role = "full"
storage_path = "$WORK/fol/data"
network_enabled = true
network_bind = "127.0.0.1:$FOL_PORT"
peers = "$PROD_NODE_PK@127.0.0.1:$PROD_PORT"
bootstrap_genesis = "$WORK/genesis.toml"
follower = true
block_poll_interval_ms = 100
node_key_path = "$WORK/fol-node.key"
EOF

wait_for_file() { # $1 = path, $2 = timeout secs, $3 = description
    for _ in $(seq 1 "$(( $2 * 2 ))"); do
        [ -f "$1" ] && return 0
        sleep 0.5
    done
    echo "FAIL: timed out waiting for $3 ($1)"
    echo "--- producer log ---"; tail -30 "$WORK/prod.log" 2>/dev/null || true
    echo "--- follower log ---"; tail -30 "$WORK/fol.log" 2>/dev/null || true
    exit 1
}

replay_root() { # $1 = blocks dir, $2 = label
    local out
    out="$("$ONX" replay --genesis "$WORK/genesis.toml" --blocks "$1" --data-dir "$WORK/replay-$2")"
    echo "$out" | grep '^final_state_root=' | cut -d= -f2
}

echo "== starting producer =="
"$ONXD" --config "$WORK/prod/onxd.toml" > "$WORK/prod.log" 2>&1 &
PROD_PID=$!
sleep 2
grep -q "ADNL node bound" "$WORK/prod.log" || { echo "FAIL: producer did not bind"; tail -20 "$WORK/prod.log"; exit 1; }

echo "== faucet transfer -> block 1 =="
RECIPIENT="$(python3 -c "print('99' * 32)")"
"$ONXCLI" transfer --genesis "$WORK/genesis.toml" --seed-file "$WORK/validator.seedhex" \
    --from "$VALIDATOR_PK" --to "$RECIPIENT" --amount 1000 --fee 10 --nonce 0 \
    --out "$WORK/prod/txpool"
wait_for_file "$WORK/prod/data/blocks/block-00000001.blk" 60 "producer block 1"
echo "producer made block 1"

echo "== starting follower =="
"$ONXD" --config "$WORK/fol/onxd.toml" > "$WORK/fol.log" 2>&1 &
FOL_PID=$!
wait_for_file "$WORK/fol/data/blocks/block-00000001.blk" 90 "follower block 1"
echo "follower synced block 1"

echo "== comparing roots via independent replays =="
PROD_ROOT="$(replay_root "$WORK/prod/data/blocks" prod)"
FOL_ROOT="$(replay_root "$WORK/fol/data/blocks" fol)"
echo "producer root: $PROD_ROOT"
echo "follower root: $FOL_ROOT"
[ "$PROD_ROOT" = "$FOL_ROOT" ] || { echo "FAIL: roots differ after block 1"; exit 1; }
echo "PASS(1/2): follower synced block 1 and roots match"

echo "== kill -9 the follower, make block 2, restart =="
kill -9 "$FOL_PID"
wait "$FOL_PID" 2>/dev/null || true
FOL_PID=""
sleep 1
"$ONXCLI" transfer --genesis "$WORK/genesis.toml" --seed-file "$WORK/validator.seedhex" \
    --from "$VALIDATOR_PK" --to "$RECIPIENT" --amount 500 --fee 10 --nonce 1 \
    --out "$WORK/prod/txpool"
wait_for_file "$WORK/prod/data/blocks/block-00000002.blk" 60 "producer block 2"
echo "producer made block 2 while the follower was dead"

"$ONXD" --config "$WORK/fol/onxd.toml" >> "$WORK/fol.log" 2>&1 &
FOL_PID=$!
wait_for_file "$WORK/fol/data/blocks/block-00000002.blk" 90 "follower block 2 after restart"
echo "follower resumed and synced block 2"

PROD_ROOT="$(replay_root "$WORK/prod/data/blocks" prod2)"
FOL_ROOT="$(replay_root "$WORK/fol/data/blocks" fol2)"
echo "producer root: $PROD_ROOT"
echo "follower root: $FOL_ROOT"
[ "$PROD_ROOT" = "$FOL_ROOT" ] || { echo "FAIL: roots differ after resume"; exit 1; }
echo "PASS(2/2): follower resumed after kill -9 and converged"
echo "ALL MULTINODE CHECKS PASSED"
