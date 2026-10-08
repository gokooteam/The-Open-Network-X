use crate::error::StateModelError;
use onx_primitives::{domain_hash, DomainTag, Uint16};

/// Domain separation tag for Cell representation hashing per docs/specification/state-model.md §4.3.
pub const ONX_CELL_HASH_V1_TAG: DomainTag = DomainTag::from_ascii("ONX_CELL_HASH_V1");

pub const MAX_CELL_DATA_BYTES: usize = 128;
pub const MAX_CELL_REFS: usize = 4;
/// Maximum meaningful data bits in a cell (128 bytes × 8). A bit-granular
/// cell (flag set) holds at most 1023 data bits — see ADR-0036.
pub const MAX_CELL_DATA_BITS: usize = MAX_CELL_DATA_BYTES * 8;

/// Descriptor `d1` bit 4: the cell is bit-granular. Its hash commits an exact
/// bit length (not just a byte length) via the completion tag in the last
/// data byte (ADR-0036, spec §4.2). Bits 5–7 of `d1` stay reserved.
pub const BIT_GRANULAR_FLAG: u8 = 0x10;

/// A canonical Cell structure containing up to 128 data bytes and up to 4 references to child cell hashes.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Cell {
    data_bytes: Vec<u8>,
    cell_refs: Vec<[u8; 32]>,
    is_special: bool,
    is_bit_granular: bool,
}

impl Cell {
    /// Creates a new standard cell with given data bytes and child cell hashes.
    /// The cell is byte-granular: its bit length is exactly `8 * data_bytes.len()`.
    pub fn new(data_bytes: Vec<u8>, cell_refs: Vec<[u8; 32]>) -> Result<Self, StateModelError> {
        Self::new_with_special(data_bytes, cell_refs, false)
    }

    /// Creates a new cell specifying if special flag is set.
    /// The cell is byte-granular (see [`Cell::new`]).
    pub fn new_with_special(
        data_bytes: Vec<u8>,
        cell_refs: Vec<[u8; 32]>,
        is_special: bool,
    ) -> Result<Self, StateModelError> {
        Self::new_full(data_bytes, cell_refs, is_special, false)
    }

    fn new_full(
        data_bytes: Vec<u8>,
        cell_refs: Vec<[u8; 32]>,
        is_special: bool,
        is_bit_granular: bool,
    ) -> Result<Self, StateModelError> {
        if data_bytes.len() > MAX_CELL_DATA_BYTES {
            return Err(StateModelError::DataTooLarge {
                length: data_bytes.len(),
                max: MAX_CELL_DATA_BYTES,
            });
        }
        if cell_refs.len() > MAX_CELL_REFS {
            return Err(StateModelError::TooManyReferences {
                count: cell_refs.len(),
                max: MAX_CELL_REFS,
            });
        }
        Ok(Self {
            data_bytes,
            cell_refs,
            is_special,
            is_bit_granular,
        })
    }

    /// Creates a cell from raw builder output: `data_bytes` holds the stored
    /// bytes and `bit_len` is the exact number of meaningful bits.
    ///
    /// - `bit_len == 0` requires empty `data_bytes`; the cell is byte-granular.
    /// - `bit_len` a multiple of 8 requires `data_bytes.len() == bit_len / 8`;
    ///   the cell is byte-granular.
    /// - Otherwise the cell is bit-granular: `data_bytes.len()` must equal
    ///   `bit_len.div_ceil(8)`, the completion tag (a single 1 bit at position
    ///   `bit_len`, zeros below) is written into the last byte, and the
    ///   compatible flag is set (ADR-0036). Any non-zero bits below the tag
    ///   position are a caller bug and fail closed — they are never silently
    ///   masked away.
    pub fn new_with_bit_len(
        mut data_bytes: Vec<u8>,
        bit_len: usize,
        cell_refs: Vec<[u8; 32]>,
    ) -> Result<Self, StateModelError> {
        if bit_len > MAX_CELL_DATA_BITS {
            return Err(StateModelError::DataTooLarge {
                length: bit_len,
                max: MAX_CELL_DATA_BITS,
            });
        }
        let want_bytes = bit_len.div_ceil(8);
        if data_bytes.len() != want_bytes {
            return Err(StateModelError::InvalidDescriptor);
        }
        let remainder = bit_len % 8;
        if remainder == 0 {
            return Self::new_full(data_bytes, cell_refs, false, false);
        }
        // Bit-granular: the last byte must exist and its tag region (every
        // bit below position `remainder`) must be zero before we set the tag.
        // `remainder != 0` implies `want_bytes >= 1`, so the saturating index
        // below is exact, not a clamp.
        let last_idx = want_bytes.saturating_sub(1);
        // Completion bit mask: bit position `remainder` counted from the MSB,
        // i.e. mask bit `7 - remainder` in LSB-indexed numbering.
        let tag_mask: u8 = 1u8 << (7usize.saturating_sub(remainder));
        let below_mask: u8 = tag_mask.saturating_sub(1);
        if data_bytes[last_idx] & below_mask != 0 {
            return Err(StateModelError::InvalidDescriptor);
        }
        data_bytes[last_idx] |= tag_mask;
        Self::new_full(data_bytes, cell_refs, false, true)
    }

    pub fn data_bytes(&self) -> &[u8] {
        &self.data_bytes
    }

    pub fn cell_refs(&self) -> &[[u8; 32]] {
        &self.cell_refs
    }

    pub fn is_special(&self) -> bool {
        self.is_special
    }

    /// Whether the cell is bit-granular: its hash commits an exact bit length
    /// via the completion tag (ADR-0036). When false, the cell is
    /// byte-granular and [`Cell::bit_len`] is `8 * data_bytes.len()`.
    pub fn is_bit_granular(&self) -> bool {
        self.is_bit_granular
    }

    /// The exact number of meaningful data bits.
    ///
    /// Byte-granular cells: `8 * data_bytes.len()`. Bit-granular cells: derived
    /// from the completion tag — `8 * (n - 1) + (7 - trailing_zeros(last))`
    /// where `n` is the data byte count. Total by construction, so the
    /// saturating forms below are exact on every constructible cell.
    pub fn bit_len(&self) -> usize {
        if !self.is_bit_granular {
            return self.data_bytes.len().saturating_mul(8);
        }
        let n = self.data_bytes.len();
        // Construction (`new_with_bit_len`, `from_bytes`) guarantees `n >= 1`
        // and a non-zero last byte carrying the completion tag.
        let last = self.data_bytes[n.saturating_sub(1)];
        let data_bits_in_last = 7usize.saturating_sub(last.trailing_zeros() as usize);
        n.saturating_sub(1)
            .saturating_mul(8)
            .saturating_add(data_bits_in_last)
    }

    /// Computes the descriptor bytes `(d1, d2)` per spec §4.2.
    /// `d1` = `ref_count | (if is_special { 8 } else { 0 }) | (if is_bit_granular { 16 } else { 0 })`
    /// `d2` = `data_byte_length`
    pub fn descriptor_bytes(&self) -> (u8, u8) {
        let mut d1 = self.cell_refs.len() as u8;
        if self.is_special {
            d1 |= 0x08;
        }
        if self.is_bit_granular {
            d1 |= BIT_GRANULAR_FLAG;
        }
        let d2 = self.data_bytes.len() as u8;
        (d1, d2)
    }

    /// Computes the 32-byte domain-separated SHA-256 cell representation hash per spec §4.3.
    pub fn hash(&self) -> [u8; 32] {
        let (d1, d2) = self.descriptor_bytes();
        // Capacity hint only; bounded by construction (<=128 data bytes, <=4 refs,
        // total <= 258), so the saturating ops never saturate in practice.
        let capacity = 2usize
            .saturating_add(self.data_bytes.len())
            .saturating_add(self.cell_refs.len().saturating_mul(32));
        let mut payload = Vec::with_capacity(capacity);
        payload.push(d1);
        payload.push(d2);
        payload.extend_from_slice(&self.data_bytes);
        for ref_hash in &self.cell_refs {
            payload.extend_from_slice(ref_hash);
        }
        domain_hash(&ONX_CELL_HASH_V1_TAG, &payload)
    }

    /// Serializes the cell into binary payload per spec §4.2.
    pub fn to_bytes(&self) -> Vec<u8> {
        let (d1, d2) = self.descriptor_bytes();
        let descriptor_u16 = ((d1 as u16) << 8) | (d2 as u16);
        // Same bounded-capacity reasoning as in `hash` above.
        let capacity = 2usize
            .saturating_add(self.data_bytes.len())
            .saturating_add(self.cell_refs.len().saturating_mul(32));
        let mut bytes = Vec::with_capacity(capacity);
        bytes.extend_from_slice(&Uint16(descriptor_u16).encode());
        bytes.extend_from_slice(&self.data_bytes);
        for ref_hash in &self.cell_refs {
            bytes.extend_from_slice(ref_hash);
        }
        bytes
    }

    /// Deserializes a Cell from binary slice.
    pub fn from_bytes(slice: &[u8]) -> Result<(Self, usize), StateModelError> {
        if slice.len() < 2 {
            return Err(StateModelError::DeserializationError(
                "Slice too short for cell descriptor".to_string(),
            ));
        }

        let mut cursor = slice;
        let descriptor_val = Uint16::read(&mut cursor)
            .map_err(|e| StateModelError::DeserializationError(e.to_string()))?;
        let descriptor_u16 = descriptor_val.0;
        let mut offset = Uint16::BYTE_LEN;

        let d1 = (descriptor_u16 >> 8) as u8;
        let d2 = (descriptor_u16 & 0xFF) as u8;

        let ref_count = (d1 & 0x07) as usize;
        let is_special = (d1 & 0x08) != 0;
        let is_bit_granular = (d1 & BIT_GRANULAR_FLAG) != 0;
        let data_len = d2 as usize;

        // Strict canonical form: d1 carries the reference count in bits 0-2,
        // the special flag in bit 3, and the bit-granular ("compatible") flag
        // in bit 4 (ADR-0036). Bits 5-7 are reserved and must be zero. The
        // encoder never sets them, so any input with them set is
        // non-canonical and must be rejected: two implementations must not
        // disagree on whether a cell encoding is valid.
        if d1 & 0xE0 != 0 {
            return Err(StateModelError::InvalidDescriptor);
        }

        if ref_count > MAX_CELL_REFS {
            return Err(StateModelError::TooManyReferences {
                count: ref_count,
                max: MAX_CELL_REFS,
            });
        }
        if data_len > MAX_CELL_DATA_BYTES {
            return Err(StateModelError::DataTooLarge {
                length: data_len,
                max: MAX_CELL_DATA_BYTES,
            });
        }

        // `data_len <= MAX_CELL_DATA_BYTES` and `ref_count <= MAX_CELL_REFS` were
        // checked above, so `required_len <= 258`: saturation is unreachable.
        let required_len = offset
            .saturating_add(data_len)
            .saturating_add(ref_count.saturating_mul(32));
        if slice.len() < required_len {
            return Err(StateModelError::DeserializationError(format!(
                "Truncated Cell payload: expected {} bytes, got {}",
                required_len,
                slice.len()
            )));
        }

        let data_bytes = slice[offset..offset.saturating_add(data_len)].to_vec();
        offset = offset.saturating_add(data_len);

        // Bit-granular canonicality (ADR-0036, spec §4.2 rules 4–5): the last
        // data byte carries the completion tag — a single 1 bit marking the
        // end of the meaningful bits, zeros below it. `0x00` has no completion
        // 1 at all; `0x80` is a lone tag with zero data bits in the final
        // byte, which would make the bit length a multiple of 8 and contradict
        // the flag. Both are non-canonical and rejected fail-closed. An empty
        // flagged cell is likewise impossible (a zero bit length is byte-granular).
        if is_bit_granular {
            if data_len == 0 {
                return Err(StateModelError::InvalidDescriptor);
            }
            // `data_len >= 1` checked above, so the saturating index is exact.
            let last = data_bytes[data_len.saturating_sub(1)];
            if last == 0x00 || last == 0x80 {
                return Err(StateModelError::InvalidDescriptor);
            }
        }

        let mut cell_refs = Vec::with_capacity(ref_count);
        for _ in 0..ref_count {
            let mut ref_hash = [0u8; 32];
            ref_hash.copy_from_slice(&slice[offset..offset.saturating_add(32)]);
            cell_refs.push(ref_hash);
            offset = offset.saturating_add(32);
        }

        let cell = Self::new_full(data_bytes, cell_refs, is_special, is_bit_granular)?;
        Ok((cell, offset))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collision_pair_hashes_differ() {
        // ADR-0036's motivating collision: 3-bit `101` vs 8-bit `10100000`.
        // The flagged cell carries the completion tag (`10110000`); the
        // byte-granular cell keeps its 8 data bits. Different descriptors AND
        // different data, so the domain-separated hashes must differ.
        let three_bit = Cell::new_with_bit_len(vec![0xA0], 3, vec![]).unwrap();
        let eight_bit = Cell::new(vec![0xA0], vec![]).unwrap();
        assert!(three_bit.is_bit_granular());
        assert!(!eight_bit.is_bit_granular());
        assert_eq!(three_bit.descriptor_bytes(), (BIT_GRANULAR_FLAG, 1));
        assert_eq!(eight_bit.descriptor_bytes(), (0, 1));
        assert_eq!(three_bit.data_bytes(), &[0xB0]);
        assert_eq!(three_bit.bit_len(), 3);
        assert_eq!(eight_bit.bit_len(), 8);
        assert_ne!(three_bit.hash(), eight_bit.hash());
    }

    #[test]
    fn new_with_bit_len_byte_granular_cases() {
        let empty = Cell::new_with_bit_len(vec![], 0, vec![]).unwrap();
        assert!(!empty.is_bit_granular());
        assert_eq!(empty.bit_len(), 0);

        let full = Cell::new_with_bit_len(vec![0xA0], 8, vec![]).unwrap();
        assert!(!full.is_bit_granular());
        assert_eq!(full.data_bytes(), &[0xA0]);
        assert_eq!(full.bit_len(), 8);

        // 1024 bits is byte-granular (flag only marks partial-bit cells).
        let max = Cell::new_with_bit_len(vec![0xFF; 128], 1024, vec![]).unwrap();
        assert!(!max.is_bit_granular());
        assert_eq!(max.bit_len(), 1024);
    }

    #[test]
    fn new_with_bit_len_flagged_writes_completion_tag() {
        // 1 meaningful bit (value 0): the tag goes at bit position 1, the
        // first unwritten bit — `0` + tag `1` + pad = `01000000`.
        let one = Cell::new_with_bit_len(vec![0x00], 1, vec![]).unwrap();
        assert!(one.is_bit_granular());
        assert_eq!(one.data_bytes(), &[0x40]);
        assert_eq!(one.bit_len(), 1);

        // 7 meaningful bits: tag at the LSB.
        let seven = Cell::new_with_bit_len(vec![0xFE], 7, vec![]).unwrap();
        assert_eq!(seven.data_bytes(), &[0xFF]);
        assert_eq!(seven.bit_len(), 7);

        // 9 bits across two bytes: tag at bit position 1 of the second byte.
        let nine = Cell::new_with_bit_len(vec![0xFF, 0x00], 9, vec![]).unwrap();
        assert!(nine.is_bit_granular());
        assert_eq!(nine.data_bytes(), &[0xFF, 0x40]);
        assert_eq!(nine.bit_len(), 9);

        // 1023 bits: the largest bit-granular cell (128 bytes, partial last).
        let max = Cell::new_with_bit_len(vec![0xFF; 128], 1023, vec![]).unwrap();
        assert!(max.is_bit_granular());
        assert_eq!(max.data_bytes()[127], 0xFF);
        assert_eq!(max.bit_len(), 1023);
    }

    #[test]
    fn new_with_bit_len_rejects_garbage_below_tag() {
        // Bit 0 set below where the tag for bit_len=3 goes: fail closed,
        // never silently mask caller data.
        let err = Cell::new_with_bit_len(vec![0xA1], 3, vec![]).unwrap_err();
        assert_eq!(err, StateModelError::InvalidDescriptor);
    }

    #[test]
    fn new_with_bit_len_rejects_length_mismatch() {
        // bit_len=3 needs exactly 1 byte, not 2.
        let err = Cell::new_with_bit_len(vec![0xA0, 0x00], 3, vec![]).unwrap_err();
        assert_eq!(err, StateModelError::InvalidDescriptor);
        // bit_len=9 needs exactly 2 bytes, not 1.
        let err = Cell::new_with_bit_len(vec![0xFF], 9, vec![]).unwrap_err();
        assert_eq!(err, StateModelError::InvalidDescriptor);
        // Over capacity.
        let err = Cell::new_with_bit_len(vec![0xFF; 128], 1025, vec![]).unwrap_err();
        assert!(matches!(err, StateModelError::DataTooLarge { .. }));
    }

    #[test]
    fn from_bytes_rejects_noncanonical_flagged_cells() {
        // Flagged, last byte 0x00: no completion 1 present.
        let bad = [BIT_GRANULAR_FLAG, 0x01, 0x00];
        assert_eq!(
            Cell::from_bytes(&bad).unwrap_err(),
            StateModelError::InvalidDescriptor
        );
        // Flagged, last byte 0x80: lone tag, zero data bits in the final byte.
        let bad = [BIT_GRANULAR_FLAG, 0x01, 0x80];
        assert_eq!(
            Cell::from_bytes(&bad).unwrap_err(),
            StateModelError::InvalidDescriptor
        );
        // Flagged but empty: a zero bit length is byte-granular.
        let bad = [BIT_GRANULAR_FLAG, 0x00];
        assert_eq!(
            Cell::from_bytes(&bad).unwrap_err(),
            StateModelError::InvalidDescriptor
        );
        // Reserved d1 bit 5 still rejected.
        let bad = [0x20, 0x01, 0xA0];
        assert_eq!(
            Cell::from_bytes(&bad).unwrap_err(),
            StateModelError::InvalidDescriptor
        );
    }

    #[test]
    fn flagged_cell_round_trips_through_bytes() {
        let cell = Cell::new_with_bit_len(vec![0xA0], 3, vec![[0x11; 32]]).unwrap();
        let bytes = cell.to_bytes();
        let (back, consumed) = Cell::from_bytes(&bytes).unwrap();
        assert_eq!(consumed, bytes.len());
        assert_eq!(back, cell);
        assert!(back.is_bit_granular());
        assert_eq!(back.bit_len(), 3);
        assert_eq!(back.hash(), cell.hash());
    }

    #[test]
    fn flagged_special_cell_round_trips() {
        // The flag and the special bit are independent d1 bits.
        let cell = Cell::new_full(vec![0xB0], vec![], true, true).unwrap();
        assert_eq!(cell.descriptor_bytes(), (0x18, 1));
        let bytes = cell.to_bytes();
        let (back, _) = Cell::from_bytes(&bytes).unwrap();
        assert_eq!(back, cell);
        assert!(back.is_special());
        assert!(back.is_bit_granular());
        assert_eq!(back.bit_len(), 3);
    }

    #[test]
    fn byte_granular_cells_unchanged_by_the_flag() {
        // Pre-ADR-0036 cells (flag clear) keep their exact descriptor and hash
        // inputs: d1 has no new bits set.
        let cell = Cell::new(vec![0xA0], vec![]).unwrap();
        assert_eq!(cell.descriptor_bytes(), (0, 1));
        assert!(!cell.is_bit_granular());
        let bytes = cell.to_bytes();
        assert_eq!(&bytes[0..2], &[0x00, 0x01]);
        let (back, _) = Cell::from_bytes(&bytes).unwrap();
        assert_eq!(back, cell);
    }
}
