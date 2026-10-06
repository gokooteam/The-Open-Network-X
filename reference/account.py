#!/usr/bin/env python3
"""AccountState record encoding.

Derived from docs/specification/state-model.md §4.1 (Account State
Record Layout).

Active codeless account: exactly 141 bytes:
  type(1)=0x01 || balance u128be(16) || last_trans_lt u64be(8) ||
  code_hash(32)=zeros || data_hash(32)=zeros ||
  cell_count u32be(4)=0 || byte_count u64be(8)=0 ||
  pubkey(32) || nonce u64be(8)

Contract accounts (with code/data cells) are out of scope for this
reference — see README.md non-goals. All vector accounts are codeless.
"""

from .primitives import u32be, u64be, u128be

STATE_ACTIVE = 0x01
STATE_UNINITIALIZED = 0x00
STATE_FROZEN = 0x02
STATE_DESTROYED = 0x03

ZERO_HASH = bytes(32)


class Account:
    """A codeless Active account."""

    def __init__(self, balance: int, lt: int, pubkey: bytes, nonce: int):
        if len(pubkey) != 32:
            raise ValueError("pubkey must be 32 bytes")
        if balance < 0:
            raise ValueError("negative balance")
        self.balance = balance
        self.lt = lt
        self.pubkey = bytes(pubkey)
        self.nonce = nonce

    def record_bytes(self) -> bytes:
        out = bytearray()
        out.append(STATE_ACTIVE)
        out += u128be(self.balance)
        out += u64be(self.lt)
        out += ZERO_HASH  # code_hash
        out += ZERO_HASH  # data_hash
        out += u32be(0)  # cell_count
        out += u64be(0)  # byte_count
        out += self.pubkey
        out += u64be(self.nonce)
        assert len(out) == 141, f"account record must be 141 bytes, got {len(out)}"
        return bytes(out)

    def copy(self):
        return Account(self.balance, self.lt, self.pubkey, self.nonce)

    def __repr__(self):
        return (
            f"Account(balance={self.balance}, lt={self.lt}, "
            f"nonce={self.nonce}, key={'yes' if self.pubkey != ZERO_HASH else 'no'})"
        )
