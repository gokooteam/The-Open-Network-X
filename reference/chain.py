#!/usr/bin/env python3
"""Message-model block execution.

Derived from:
  - ADR-0001 (two-phase dispatch: wallet handler then delivery)
  - ADR-0003 (same-block delivery, transient FIFO queue, no intra-block
    chaining, 4x delivery bound)
  - ADR-0004 (bounce semantics; fees not returned; bounce never bounced)
  - ADR-0005 (chain-ID binding)
  - ADR-0006 (key-derived addresses; first spend reveals the key)
  - ADR-0007 (exact-match nonces; per-block processed set)
  - docs/specification/economics.md (50/50 fee burn/validator split)

Field-level details (initial lt/nonce, lt updates on debit/credit, fee
split rounding) follow the wallet/deliver logic; the design — two phases,
bounce table, key-reveal rule — comes from the ADRs.

Out of scope: TVM execution (contract calls bounce in this reference;
see README.md), persistent storage, networking, consensus.
"""

from collections import deque

from .account import Account, ZERO_HASH
from .messages import (
    ExternalMessage, InternalMessage, KIND_TRANSFER, KIND_CONTRACT_CALL,
    ZERO_PUBKEY, derive_address, msgs_root, block_header_bytes, block_hash,
)
from .trie import state_root

FEE_BURN_PERCENT = 50
MAX_DELIVERIES_PER_EXTERNAL = 4


class ExecError(Exception):
    """Fail-closed execution error (mirrors StfError)."""


def split_fee(fee: int):
    burned = (fee * FEE_BURN_PERCENT) // 100
    return burned, fee - burned


class State:
    """In-memory chain state: accounts + chain metadata."""

    def __init__(self, chain_id: bytes, workchain: int):
        self.chain_id = bytes(chain_id)
        self.workchain = workchain
        self.accounts: dict = {}  # id -> Account; missing = Uninitialized
        self.seqno = 0
        self.last_lt = 0
        self.last_hash = bytes(chain_id)  # genesis: last_hash = chain_id

    def get(self, account_id: bytes):
        return self.accounts.get(bytes(account_id))

    def state_root(self) -> bytes:
        records = {aid: acc.record_bytes() for aid, acc in self.accounts.items()}
        return state_root(records)


def _check_lt(account, lt: int, aid: bytes):
    cur = account.lt if account is not None else 0
    if lt < cur:
        raise ExecError(f"lt regression for {aid.hex()[:8]}")


def wallet_receive(state: State, ext: ExternalMessage, lt: int,
                   fee_collector: bytes):
    """Phase 1: authenticate one external message.

    Returns (internal_message, receipt_dict). Raises ExecError on any
    failure — the block is invalid; nothing is written before all checks
    pass.
    """
    # 1. Chain binding (explicit check; signature also binds it).
    if ext.chain_id != state.chain_id:
        raise ExecError("WrongChainId")

    # 2. Kind-specific sanity.
    if ext.kind == KIND_TRANSFER:
        if ext.amount == 0:
            raise ExecError("ZeroAmount")
    elif ext.kind == KIND_CONTRACT_CALL:
        if ext.fee == 0:
            raise ExecError("ZeroFeeContractCall")
    else:
        raise ExecError("bad kind")
    total_debit = ext.amount + ext.fee

    # 3. Sender must be Active.
    sender = state.get(ext.from_id)
    if sender is None:
        raise ExecError("SenderNotSpendable")

    # 4. Key resolution (ADR-0006).
    if sender.pubkey == ZERO_HASH:
        if ext.pubkey == ZERO_PUBKEY:
            raise ExecError("SenderHasNoKey")
        if derive_address(ext.pubkey) != ext.from_id:
            raise ExecError("AddressKeyMismatch")
        if ext.pubkey == ZERO_PUBKEY:
            raise ExecError("degenerate key")
        effective_pubkey = ext.pubkey
    else:
        if ext.pubkey != ZERO_PUBKEY:
            raise ExecError("UnexpectedPubkeyReveal")
        effective_pubkey = sender.pubkey

    # 5. Nonce, signature, balance, lt (read-only).
    if ext.nonce != sender.nonce:
        raise ExecError(f"NonceMismatch: expected {sender.nonce}, got {ext.nonce}")
    if not ext.verify_signature(effective_pubkey):
        raise ExecError("InvalidSignature")
    if sender.balance < total_debit:
        raise ExecError("InsufficientFunds")
    _check_lt(sender, lt, ext.from_id)

    # 6. Fee split; collector must be receivable if credited.
    burned, validator_fee = split_fee(ext.fee)
    if validator_fee > 0:
        collector = state.get(fee_collector)
        if collector is not None and not isinstance(collector, Account):
            raise ExecError("FeeCollectorNotReceivable")
        # (Frozen/Destroyed have no Python representation here; the
        # reference never creates them. Uninitialized/missing is fine.)
        if collector is not None:
            _check_lt(collector, lt, fee_collector)

    # 7. Write: debit, nonce++, store revealed key, lt.
    sender.balance -= total_debit
    sender.nonce += 1
    sender.pubkey = effective_pubkey
    sender.lt = lt

    # 8. Collector credit (re-read post-debit: from may be the collector).
    if validator_fee > 0:
        collector = state.get(fee_collector)
        if collector is None:
            collector = Account(0, lt, ZERO_HASH, 0)
            state.accounts[bytes(fee_collector)] = collector
        else:
            _check_lt(collector, lt, fee_collector)
        collector.balance += validator_fee
        collector.lt = lt

    # 9. Emit the internal message.
    internal = InternalMessage(
        src=ext.from_id, dest=ext.to_id,
        value=ext.amount, fee=ext.fee, payload=ext.message,
        is_bounce=False, origin=ext.hash(),
    )
    receipt = {
        "msg_hash": ext.hash().hex(),
        "fee_burned": burned,
        "fee_validator": validator_fee,
        "deliveries": [],
    }
    return internal, receipt


def deliver(state: State, msg: InternalMessage, lt: int, queue: deque,
            processed: set):
    """Phase 2: deliver one internal message as the receiver's own tx.

    Returns a delivery receipt dict. May append a bounce to `queue`.
    """
    msg_id = msg.id()
    if msg_id in processed:
        raise ExecError("DoubleDelivery")
    processed.add(msg_id)

    receipt = {
        "msg_id": msg_id.hex(),
        "value": msg.value,
        "bounced": False,
        "gas_used": 0,
    }
    dest = state.get(msg.dest)

    # Decide: process or bounce (ADR-0004 table).
    must_bounce = False
    if dest is None:
        # Uninitialized: empty payload creates, non-empty bounces.
        must_bounce = len(msg.payload) > 0
    elif msg.payload:
        # Active with payload but this reference has no TVM: bounce.
        # (Contract execution is out of scope; see README.md.)
        must_bounce = True

    if must_bounce:
        if msg.is_bounce:
            raise ExecError("BounceUndeliverable")
        bounced = InternalMessage(
            src=msg.dest, dest=msg.src, value=msg.value, fee=0,
            payload=b"", is_bounce=True, origin=msg_id,
        )
        queue.append(bounced)
        receipt["bounced"] = True
        return receipt

    # Process: check lt, credit (creating keyless account if needed).
    if dest is None:
        _check_lt(None, lt, msg.dest)
        dest = Account(0, lt, ZERO_HASH, 0)
        state.accounts[bytes(msg.dest)] = dest
    else:
        _check_lt(dest, lt, msg.dest)
    dest.balance += msg.value
    dest.lt = lt
    return receipt


def apply_block(state: State, externals: list, lt: int, fee_collector: bytes):
    """Apply one block: phase 1 (wallets in order), phase 2 (FIFO drain).

    Returns (receipts, state_root). Raises ExecError on invalid block.
    """
    if lt <= state.last_lt:
        raise ExecError("LogicalTimeRegression")
    queue = deque()
    receipts = []
    # Phase 1.
    for ext in externals:
        internal, receipt = wallet_receive(state, ext, lt, fee_collector)
        receipts.append(receipt)
        queue.append(internal)
    # Phase 2.
    processed = set()
    max_deliveries = len(externals) * MAX_DELIVERIES_PER_EXTERNAL
    done = 0
    # Map internal origin -> receipt index for delivery attribution.
    origin_to_idx = {bytes.fromhex(r["msg_hash"]): i for i, r in enumerate(receipts)}
    # (bounce origins are internal IDs; attribute to the same receipt)
    while queue:
        done += 1
        if done > max_deliveries:
            raise ExecError("TooManyDeliveries")
        msg = queue.popleft()
        receipt = deliver(state, msg, lt, queue, processed)
        # Attribute: wallet-emitted -> its external; bounce -> parent's.
        idx = origin_to_idx.get(msg.origin)
        if idx is None:
            # Bounce: origin is an internal ID; find the receipt whose
            # delivery produced it.
            for i, r in enumerate(receipts):
                if any(d["msg_id"] == msg.origin.hex() for d in r["deliveries"]):
                    idx = i
                    break
        receipts[idx]["deliveries"].append(receipt)
        # Register bounce IDs so chained bounces attribute correctly.
        if receipt["bounced"]:
            # The bounce's origin is this delivery's msg_id.
            pass
    root = state.state_root()
    state.seqno += 1
    state.last_lt = lt
    return receipts, root


def propose_block(state: State, externals: list, lt: int, fee_collector: bytes):
    """Build a block and its post-state.

    Executes on a copy of `state`; returns (header_bytes, wires,
    state_root, receipts, new_state). The caller adopts `new_state` to
    advance, or discards it for a dry run.
    """
    import copy
    new_state = copy.deepcopy(state)
    receipts, root = apply_block(new_state, externals, lt, fee_collector)
    ext_hashes = [e.hash() for e in externals]
    mr = msgs_root(ext_hashes)
    header = block_header_bytes(
        seqno=state.seqno + 1,
        prev_hash=state.last_hash,
        msgs_root_h=mr,
        state_root=root,
        lt=lt,
        workchain=state.workchain,
        fee_collector=fee_collector,
        msg_count=len(externals),
    )
    wires = [e.wire_bytes() for e in externals]
    new_state.last_hash = block_hash(header)
    return header, wires, root, receipts, new_state
