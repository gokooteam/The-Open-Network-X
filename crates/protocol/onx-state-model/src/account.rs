use crate::cell::Cell;
use crate::error::StateModelError;
use onx_primitives::{Uint128, Uint32, Uint64, Uint8};

/// Maximum canonical bytes of one embedded code/data cell inside an
/// account record. Bounds account size: a cell is at most 2 + 128 + 4*32
/// bytes by construction, so this is defense in depth, not the real limit.
const MAX_EMBEDDED_CELL_BYTES: usize = 1024;

/// All-zero hash sentinel: marks "no cell appended" in the account codec.
/// A real cell's hash is SHA-256-based and never all zeros in practice;
/// even so, the decoder treats a zero hash as absent, never as a cell
/// whose hash must verify — so there is no ambiguity to exploit.
const NO_CELL_HASH: [u8; 32] = [0u8; 32];

/// Encode an optional cell's payload for appending after the fixed account
/// header. `len u32be(4) || cell_bytes`. Fail-closed on decode.
fn encode_embedded_cell(out: &mut Vec<u8>, cell: &Cell) {
    let bytes = cell.to_bytes();
    out.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
    out.extend_from_slice(&bytes);
}

/// Decode an embedded cell payload from the front of `slice`, verifying it
/// hashes to `expected`. Returns the cell and bytes consumed. Fail-closed
/// on truncation, over-long lengths, malformed cell bytes, or hash mismatch.
fn decode_embedded_cell(slice: &[u8], expected: &[u8; 32]) -> Result<(Cell, usize), String> {
    if slice.len() < 4 {
        return Err("truncated embedded cell length prefix".to_string());
    }
    let len = u32::from_be_bytes(slice[0..4].try_into().expect("len checked")) as usize;
    if len > MAX_EMBEDDED_CELL_BYTES {
        return Err(format!("embedded cell too large: {len} bytes"));
    }
    if slice.len() < 4 + len {
        return Err(format!(
            "truncated embedded cell: need {}, got {}",
            4 + len,
            slice.len()
        ));
    }
    let (cell, consumed) = Cell::from_bytes(&slice[4..4 + len]).map_err(|e| e.to_string())?;
    if consumed != len {
        return Err(format!(
            "embedded cell length mismatch: prefix {len}, parsed {consumed}"
        ));
    }
    if &cell.hash() != expected {
        return Err("embedded cell hash does not match account header".to_string());
    }
    Ok((cell, 4 + len))
}

/// Canonical account lifecycle states per docs/specification/state-model.md §3.1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum AccountType {
    Uninitialized = 0x00,
    Active = 0x01,
    Frozen = 0x02,
    Destroyed = 0x03,
}

impl AccountType {
    pub fn from_u8(value: u8) -> Result<Self, StateModelError> {
        match value {
            0x00 => Ok(Self::Uninitialized),
            0x01 => Ok(Self::Active),
            0x02 => Ok(Self::Frozen),
            0x03 => Ok(Self::Destroyed),
            other => Err(StateModelError::InvalidStateType(other)),
        }
    }

    pub fn to_u8(self) -> u8 {
        self as u8
    }
}

/// Storage resource consumption statistics for an account.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StorageStat {
    pub cell_count: u32,
    pub byte_count: u64,
}

/// Canonical Account State record per docs/specification/state-model.md §3.2 and §4.1.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccountState {
    Uninitialized,
    Active {
        balance_nanos: u128,
        last_trans_lt: u64,
        /// Contract code cell. `None` for plain accounts; `Some` makes this
        /// account a contract whose code the STF executes on message-carrying
        /// transactions. Single source of truth — there is no separate
        /// `code_hash` field to drift out of sync; hashes are computed from
        /// the cell on demand.
        code: Option<Cell>,
        /// Contract persistent data cell (TVM c4). Updated by contract
        /// execution; `None` for plain accounts.
        data: Option<Cell>,
        storage_stat: StorageStat,
        /// Ed25519 public key authorized to spend from this account.
        /// All zeros means *keyless*: the account can receive but never
        /// send (the STF rejects spends from keyless accounts explicitly —
        /// the all-zero encoding is an order-4 Ed25519 point, for which
        /// degenerate signatures verify under *any* message on cofactored
        /// verifiers, so it must never be treated as a real key;
        /// `verify_strict` rejects it outright).
        pubkey: [u8; 32],
        /// Next expected transaction nonce. Starts at 0; incremented by
        /// one on every successful spend. A transaction is valid only if
        /// its nonce equals the account's current nonce — this is what
        /// makes transaction replay impossible.
        nonce: u64,
    },
    Frozen {
        balance_nanos: u128,
        last_trans_lt: u64,
        storage_hash: [u8; 32],
    },
    Destroyed,
}

impl AccountState {
    pub fn account_type(&self) -> AccountType {
        match self {
            Self::Uninitialized => AccountType::Uninitialized,
            Self::Active { .. } => AccountType::Active,
            Self::Frozen { .. } => AccountType::Frozen,
            Self::Destroyed => AccountType::Destroyed,
        }
    }

    /// Returns the account balance in nanocoins, or 0 if uninitialized or destroyed.
    pub fn balance_nanos(&self) -> u128 {
        match self {
            Self::Active { balance_nanos, .. } | Self::Frozen { balance_nanos, .. } => {
                *balance_nanos
            }
            Self::Uninitialized | Self::Destroyed => 0,
        }
    }

    /// Returns the next expected transaction nonce, or 0 if the account
    /// has no nonce (uninitialized, frozen, or destroyed).
    pub fn nonce(&self) -> u64 {
        match self {
            Self::Active { nonce, .. } => *nonce,
            Self::Uninitialized | Self::Frozen { .. } | Self::Destroyed => 0,
        }
    }

    /// Serializes an active account state record according to docs/specification/state-model.md §4.1.
    ///
    /// Active layout (fixed 141-byte header, then optional cell payloads):
    /// `type(1) || balance u128be(16) || lt u64be(8) || code_hash(32) ||`
    /// `data_hash(32) || cell_count u32be(4) || byte_count u64be(8) ||`
    /// `pubkey(32) || nonce u64be(8)`
    /// `[|| code_len u32be(4) || code_bytes]`
    /// `[|| data_len u32be(4) || data_bytes]`.
    ///
    /// `code_hash` is all zeros when the account has no code, otherwise the
    /// hash of the appended code cell (same for `data_hash`/`data`). A
    /// plain account (no code, no data) encodes to **exactly the V2 bytes**:
    /// the 64 hash bytes are zero and nothing is appended — so pre-contract
    /// state roots and golden vectors are unaffected by this upgrade.
    ///
    /// Appending the cells (rather than storing only their hashes) is what
    /// makes contract code and data part of the persisted state and
    /// therefore part of the state root: the trie commits to the full
    /// account, code included.
    pub fn to_bytes(&self) -> Vec<u8> {
        match self {
            Self::Uninitialized => vec![AccountType::Uninitialized.to_u8()],
            Self::Active {
                balance_nanos,
                last_trans_lt,
                code,
                data,
                storage_stat,
                pubkey,
                nonce,
            } => {
                let mut bytes = Vec::with_capacity(141);
                bytes.push(AccountType::Active.to_u8());
                bytes.extend_from_slice(&Uint128(*balance_nanos).encode());
                bytes.extend_from_slice(&Uint64(*last_trans_lt).encode());
                bytes.extend_from_slice(&code.as_ref().map(|c| c.hash()).unwrap_or(NO_CELL_HASH));
                bytes.extend_from_slice(&data.as_ref().map(|c| c.hash()).unwrap_or(NO_CELL_HASH));
                bytes.extend_from_slice(&Uint32(storage_stat.cell_count).encode());
                bytes.extend_from_slice(&Uint64(storage_stat.byte_count).encode());
                // Appended at the end (tx-auth upgrade): pubkey then nonce.
                bytes.extend_from_slice(pubkey);
                bytes.extend_from_slice(&Uint64(*nonce).encode());
                if let Some(c) = code {
                    encode_embedded_cell(&mut bytes, c);
                }
                if let Some(d) = data {
                    encode_embedded_cell(&mut bytes, d);
                }
                bytes
            }
            Self::Frozen {
                balance_nanos,
                last_trans_lt,
                storage_hash,
            } => {
                let mut bytes = Vec::with_capacity(57);
                bytes.push(AccountType::Frozen.to_u8());
                bytes.extend_from_slice(&Uint128(*balance_nanos).encode());
                bytes.extend_from_slice(&Uint64(*last_trans_lt).encode());
                bytes.extend_from_slice(storage_hash);
                bytes
            }
            Self::Destroyed => vec![AccountType::Destroyed.to_u8()],
        }
    }

    /// Deserializes an account state record from binary bytes.
    pub fn from_bytes(slice: &[u8]) -> Result<(Self, usize), StateModelError> {
        if slice.is_empty() {
            return Err(StateModelError::DeserializationError(
                "Empty byte slice for AccountState".to_string(),
            ));
        }

        let mut cursor = slice;
        let state_type_val = Uint8::read(&mut cursor)
            .map_err(|e| StateModelError::DeserializationError(e.to_string()))?;
        let state_type = AccountType::from_u8(state_type_val.0)?;
        // `offset` counts consumed bytes, so it is always <= slice.len() < usize::MAX:
        // saturation is unreachable here. `saturating_add` is used purely to make
        // overflow behavior explicit per the crate's `arithmetic_side_effects` policy.
        let mut offset = Uint8::BYTE_LEN;

        match state_type {
            AccountType::Uninitialized => Ok((Self::Uninitialized, offset)),
            AccountType::Destroyed => Ok((Self::Destroyed, offset)),
            AccountType::Frozen => {
                let balance_val = Uint128::read(&mut cursor)
                    .map_err(|e| StateModelError::DeserializationError(e.to_string()))?;
                offset = offset.saturating_add(Uint128::BYTE_LEN);

                let lt_val = Uint64::read(&mut cursor)
                    .map_err(|e| StateModelError::DeserializationError(e.to_string()))?;
                offset = offset.saturating_add(Uint64::BYTE_LEN);

                if cursor.len() < 32 {
                    return Err(StateModelError::DeserializationError(
                        "Truncated Frozen AccountState storage hash".to_string(),
                    ));
                }

                let mut storage_hash = [0u8; 32];
                storage_hash.copy_from_slice(&cursor[..32]);
                offset = offset.saturating_add(32);

                Ok((
                    Self::Frozen {
                        balance_nanos: balance_val.0,
                        last_trans_lt: lt_val.0,
                        storage_hash,
                    },
                    offset,
                ))
            }
            AccountType::Active => {
                let balance_val = Uint128::read(&mut cursor)
                    .map_err(|e| StateModelError::DeserializationError(e.to_string()))?;
                offset = offset.saturating_add(Uint128::BYTE_LEN);

                let lt_val = Uint64::read(&mut cursor)
                    .map_err(|e| StateModelError::DeserializationError(e.to_string()))?;
                offset = offset.saturating_add(Uint64::BYTE_LEN);

                if cursor.len() < 32 {
                    return Err(StateModelError::DeserializationError(
                        "Truncated Active AccountState code hash".to_string(),
                    ));
                }
                let mut code_hash = [0u8; 32];
                code_hash.copy_from_slice(&cursor[..32]);
                cursor = &cursor[32..];
                offset = offset.saturating_add(32);

                if cursor.len() < 32 {
                    return Err(StateModelError::DeserializationError(
                        "Truncated Active AccountState data hash".to_string(),
                    ));
                }
                let mut data_hash = [0u8; 32];
                data_hash.copy_from_slice(&cursor[..32]);
                cursor = &cursor[32..];
                offset = offset.saturating_add(32);

                let cell_count_val = Uint32::read(&mut cursor)
                    .map_err(|e| StateModelError::DeserializationError(e.to_string()))?;
                offset = offset.saturating_add(Uint32::BYTE_LEN);

                let byte_count_val = Uint64::read(&mut cursor)
                    .map_err(|e| StateModelError::DeserializationError(e.to_string()))?;
                offset = offset.saturating_add(Uint64::BYTE_LEN);

                if cursor.len() < 40 {
                    return Err(StateModelError::DeserializationError(
                        "Truncated Active AccountState pubkey/nonce".to_string(),
                    ));
                }
                let mut pubkey = [0u8; 32];
                pubkey.copy_from_slice(&cursor[..32]);
                cursor = &cursor[32..];
                offset = offset.saturating_add(32);

                let nonce_val = Uint64::read(&mut cursor)
                    .map_err(|e| StateModelError::DeserializationError(e.to_string()))?;
                offset = offset.saturating_add(Uint64::BYTE_LEN);

                // Optional appended cell payloads, present iff the header
                // hash is non-zero. Each payload's hash is verified against
                // the header (fail-closed).
                let code = if code_hash != NO_CELL_HASH {
                    let (cell, used) = decode_embedded_cell(cursor, &code_hash).map_err(|e| {
                        StateModelError::DeserializationError(format!(
                            "Truncated Active AccountState code cell: {e}"
                        ))
                    })?;
                    cursor = &cursor[used..];
                    offset = offset.saturating_add(used);
                    Some(cell)
                } else {
                    None
                };
                let data = if data_hash != NO_CELL_HASH {
                    let (cell, used) = decode_embedded_cell(cursor, &data_hash).map_err(|e| {
                        StateModelError::DeserializationError(format!(
                            "Truncated Active AccountState data cell: {e}"
                        ))
                    })?;
                    offset = offset.saturating_add(used);
                    Some(cell)
                } else {
                    None
                };

                Ok((
                    Self::Active {
                        balance_nanos: balance_val.0,
                        last_trans_lt: lt_val.0,
                        code,
                        data,
                        storage_stat: StorageStat {
                            cell_count: cell_count_val.0,
                            byte_count: byte_count_val.0,
                        },
                        pubkey,
                        nonce: nonce_val.0,
                    },
                    offset,
                ))
            }
        }
    }

    /// Validates a proposed state transition from `self` to `next` with transaction logical time and balance checks.
    pub fn validate_transition(
        &self,
        next: &AccountState,
        new_lt: u64,
    ) -> Result<(), StateModelError> {
        match (self, next) {
            (Self::Destroyed, _) => Err(StateModelError::InvalidStateTransition(
                "Cannot perform transition on a Destroyed account".to_string(),
            )),
            (Self::Uninitialized, Self::Active { last_trans_lt, .. }) => {
                if *last_trans_lt != new_lt {
                    return Err(StateModelError::LogicalTimeRegression {
                        current: *last_trans_lt,
                        next: new_lt,
                    });
                }
                Ok(())
            }
            (Self::Uninitialized, Self::Uninitialized) => Ok(()),
            (Self::Uninitialized, _) => Err(StateModelError::InvalidStateTransition(
                "Uninitialized account can only transition to Active or remain Uninitialized"
                    .to_string(),
            )),
            (
                Self::Active {
                    last_trans_lt: cur_lt,
                    ..
                },
                Self::Active {
                    last_trans_lt: next_lt,
                    ..
                },
            )
            | (
                Self::Active {
                    last_trans_lt: cur_lt,
                    ..
                },
                Self::Frozen {
                    last_trans_lt: next_lt,
                    ..
                },
            ) => {
                if new_lt <= *cur_lt || *next_lt != new_lt {
                    return Err(StateModelError::LogicalTimeRegression {
                        current: *cur_lt,
                        next: new_lt,
                    });
                }
                Ok(())
            }
            (Self::Active { .. }, Self::Destroyed) => Ok(()),
            (
                Self::Frozen {
                    last_trans_lt: cur_lt,
                    ..
                },
                Self::Active {
                    last_trans_lt: next_lt,
                    ..
                },
            ) => {
                if new_lt <= *cur_lt || *next_lt != new_lt {
                    return Err(StateModelError::LogicalTimeRegression {
                        current: *cur_lt,
                        next: new_lt,
                    });
                }
                Ok(())
            }
            (
                Self::Frozen {
                    last_trans_lt: cur_lt,
                    ..
                },
                Self::Frozen {
                    last_trans_lt: next_lt,
                    ..
                },
            ) => {
                if new_lt <= *cur_lt || *next_lt != new_lt {
                    return Err(StateModelError::LogicalTimeRegression {
                        current: *cur_lt,
                        next: new_lt,
                    });
                }
                Ok(())
            }
            (Self::Frozen { .. }, Self::Destroyed) => Ok(()),
            _ => Err(StateModelError::InvalidStateTransition(format!(
                "Invalid state transition from {:?} to {:?}",
                self.account_type(),
                next.account_type()
            ))),
        }
    }

    /// Validates a proposed state transition from `self` to `next` with logical time,
    /// lifecycle rules, and balance delta checks. Returns `StateModelError::BalanceUnderflow`
    /// if `balance_delta` causes the resulting account balance to fall below zero.
    pub fn validate_transition_with_delta(
        &self,
        next: &AccountState,
        new_lt: u64,
        balance_delta: i128,
    ) -> Result<(), StateModelError> {
        self.validate_transition(next, new_lt)?;

        let cur_balance = self.balance_nanos();
        if balance_delta < 0 && balance_delta.unsigned_abs() > cur_balance {
            return Err(StateModelError::BalanceUnderflow);
        }

        let expected_next_balance = if balance_delta >= 0 {
            // Fail closed on overflow: saturating here would mint u128::MAX.
            // Unreachable in practice (needs a balance > 2^127 nanos, versus
            // a 5B-Onyxi total supply), but the error is the correct behavior.
            cur_balance
                .checked_add(balance_delta as u128)
                .ok_or_else(|| {
                    StateModelError::InvalidStateTransition(format!(
                        "balance addition overflow: {cur_balance} + {balance_delta}"
                    ))
                })?
        } else {
            // Guarded by the `BalanceUnderflow` early-return above:
            // `unsigned_abs() <= cur_balance` here, so this never saturates.
            cur_balance.saturating_sub(balance_delta.unsigned_abs())
        };

        if let Self::Active { balance_nanos, .. } | Self::Frozen { balance_nanos, .. } = next {
            if *balance_nanos != expected_next_balance {
                return Err(StateModelError::InvalidStateTransition(format!(
                    "Expected next state balance {}, found {}",
                    expected_next_balance, balance_nanos
                )));
            }
        }

        Ok(())
    }
}
