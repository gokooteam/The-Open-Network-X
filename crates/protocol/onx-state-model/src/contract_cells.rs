//! Persisted cell DAGs for contract accounts.
//!
//! A contract's `code` and `data` cells are stored in the account record as
//! *root cells only* (hashes in the header, cell bytes appended). But a
//! root can reference child cells, and the TVM's `LDREF` needs the actual
//! child *content* — which the account record does not carry. This module
//! is the home for the full DAGs.
//!
//! [`ContractCellDags`] holds an account's complete code and data DAGs as
//! [`BagOfCells`], the natural carrier (content-addressed, canonical
//! encoding). The STF seeds the interpreter's cell store from these on
//! contract load and rebuilds them from the drained store after execution.
//! They are auxiliary state: they do NOT enter the state root (the root
//! commits to the account record, which commits to the cell *hashes*).

use crate::boc::BagOfCells;
use crate::error::StateModelError;

/// An account's full contract cell DAGs: code and data, each as a
/// [`BagOfCells`] rooted at the respective cell hash from the account
/// record. Carries the complete DAG *content*, not just root hashes, so a
/// later invocation's `LDREF` resolves to the actual stored children.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContractCellDags {
    /// The code DAG, rooted at the account's `code` cell hash.
    pub code: BagOfCells,
    /// The data DAG, rooted at the account's `data` cell hash.
    pub data: BagOfCells,
}

impl ContractCellDags {
    /// Canonical encoding: `code_len u32be || code_boc || data_len u32be || data_boc`.
    pub fn to_bytes(&self) -> Vec<u8> {
        let code_bytes = self.code.to_bytes();
        let data_bytes = self.data.to_bytes();
        // Capacity hint only: under-reserving is harmless (Vec grows).
        let mut out = Vec::with_capacity(
            8usize
                .saturating_add(code_bytes.len())
                .saturating_add(data_bytes.len()),
        );
        out.extend_from_slice(&(code_bytes.len() as u32).to_be_bytes());
        out.extend_from_slice(&code_bytes);
        out.extend_from_slice(&(data_bytes.len() as u32).to_be_bytes());
        out.extend_from_slice(&data_bytes);
        out
    }

    /// Strict decode: exact-length prefixes, no trailing bytes. Fail-closed.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, StateModelError> {
        if bytes.len() < 8 {
            return Err(StateModelError::DeserializationError(format!(
                "contract cell DAGs truncated: {} bytes, need at least 8",
                bytes.len()
            )));
        }
        let code_len = u32::from_be_bytes(bytes[0..4].try_into().expect("length checked")) as usize;
        // `code_len <= u32::MAX`, so these additions cannot overflow on 64-bit;
        // saturating form is used purely to satisfy the arithmetic lint.
        if bytes.len() < 4usize.saturating_add(code_len).saturating_add(4) {
            return Err(StateModelError::DeserializationError(format!(
                "contract cell DAGs truncated: code claims {code_len} bytes, {} remain",
                bytes.len().saturating_sub(4)
            )));
        }
        let (code, code_used) = BagOfCells::from_bytes(&bytes[4..4 + code_len])?;
        if code_used != code_len {
            return Err(StateModelError::DeserializationError(format!(
                "code BoC length mismatch: prefix {code_len}, parsed {code_used}"
            )));
        }
        let data_off = 4 + code_len;
        let data_len = u32::from_be_bytes(
            bytes[data_off..data_off.saturating_add(4)]
                .try_into()
                .expect("length checked"),
        ) as usize;
        if bytes.len() < data_off.saturating_add(4).saturating_add(data_len) {
            return Err(StateModelError::DeserializationError(format!(
                "contract cell DAGs truncated: data claims {data_len} bytes, {} remain",
                bytes.len().saturating_sub(data_off).saturating_sub(4)
            )));
        }
        let (data, data_used) = BagOfCells::from_bytes(
            &bytes[data_off.saturating_add(4)..data_off.saturating_add(4).saturating_add(data_len)],
        )?;
        if data_used != data_len {
            return Err(StateModelError::DeserializationError(format!(
                "data BoC length mismatch: prefix {data_len}, parsed {data_used}"
            )));
        }
        if bytes.len() != data_off.saturating_add(4).saturating_add(data_len) {
            return Err(StateModelError::DeserializationError(format!(
                "contract cell DAGs have {} trailing bytes",
                bytes
                    .len()
                    .saturating_sub(data_off)
                    .saturating_sub(4)
                    .saturating_sub(data_len)
            )));
        }
        Ok(Self { code, data })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cell::Cell;

    fn dags() -> ContractCellDags {
        let child = Cell::new(vec![0xAA], vec![]).unwrap();
        let code = Cell::new(vec![0xC0], vec![child.hash()]).unwrap();
        let data = Cell::new(vec![0xDA], vec![]).unwrap();

        let mut code_cells = std::collections::BTreeMap::new();
        code_cells.insert(child.hash(), child.clone());
        code_cells.insert(code.hash(), code.clone());
        let mut data_cells = std::collections::BTreeMap::new();
        data_cells.insert(data.hash(), data.clone());

        ContractCellDags {
            code: BagOfCells::new(code.hash(), code_cells).unwrap(),
            data: BagOfCells::new(data.hash(), data_cells).unwrap(),
        }
    }

    #[test]
    fn dags_round_trip() {
        let d = dags();
        let bytes = d.to_bytes();
        let back = ContractCellDags::from_bytes(&bytes).unwrap();
        assert_eq!(back, d);
    }

    #[test]
    fn dags_reject_truncation_and_trailing() {
        let bytes = dags().to_bytes();
        assert!(ContractCellDags::from_bytes(&bytes[..bytes.len() - 1]).is_err());
        let mut long = bytes.clone();
        long.push(0);
        assert!(ContractCellDags::from_bytes(&long).is_err());
        assert!(ContractCellDags::from_bytes(&bytes[..3]).is_err());
    }
}
