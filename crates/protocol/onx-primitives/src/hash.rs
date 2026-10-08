//! SHA-256 hashing with mandatory domain separation.
//!
//! Spec: `docs/specification/protocol-primitives.md`, §3.3 and §4.5;
//! `docs/adr/0009-protocol-primitives-and-serialization.md`.
//! Every protocol hash context must prepend a 32-byte domain separation
//! tag before hashing, so identical bytes hashed in two different protocol
//! contexts never collide. There is deliberately no way to compute a
//! domain-separated hash without supplying a [`DomainTag`].

use crate::error::PrimitiveError;
use sha2::{Digest, Sha256};

/// A 32-byte, zero-padded ASCII domain separation tag.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DomainTag([u8; 32]);

impl DomainTag {
    /// Builds a tag from an ASCII string, zero-padded to 32 bytes.
    /// `tag` must be at most 32 bytes long.
    pub const fn from_ascii(tag: &'static str) -> Self {
        let bytes = tag.as_bytes();
        assert!(bytes.len() <= 32, "domain separation tag exceeds 32 bytes");
        let mut buf = [0u8; 32];
        let mut i = 0;
        while i < bytes.len() {
            buf[i] = bytes[i];
            i += 1;
        }
        DomainTag(buf)
    }

    /// Parses a tag from an already-padded 32-byte buffer, as it would
    /// appear on the wire.
    pub fn decode_exact(bytes: &[u8]) -> Result<Self, PrimitiveError> {
        if bytes.len() != 32 {
            return Err(PrimitiveError::InvalidDomainTag);
        }
        let mut buf = [0u8; 32];
        buf.copy_from_slice(bytes);
        Ok(DomainTag(buf))
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Derives the chain-bound tag for this base tag: the tag bytes are
    /// `SHA256(pad32(base) || chain_id)`.
    ///
    /// A signature verified under the derived tag commits to the chain
    /// whose genesis hash produced `chain_id`: the same key signing the
    /// same message on another chain yields a different tag, so the
    /// signature cannot verify there. The base tag itself is never used
    /// raw for verification — using it raw would unbind the chain.
    ///
    /// Collision note: the derived tag is a SHA-256 digest, not a
    /// zero-padded ASCII string, so it cannot collide with a registered
    /// ASCII tag except with negligible probability.
    pub fn bind_chain(&self, chain_id: &[u8; 32]) -> DomainTag {
        let mut hasher = Sha256::new();
        hasher.update(self.as_bytes());
        hasher.update(chain_id);
        DomainTag(hasher.finalize().into())
    }
}

/// `ONX:BLOCK:HEADER:V1` domain tag (§4.5).
pub const BLOCK_HEADER_V1: DomainTag = DomainTag::from_ascii("ONX_BLK_HDR_V1");
/// `ONX:CHKSIGNU:V1` base domain tag (§4.5, ADR-0038). Never used raw:
/// `CHKSIGNU` verification always uses `CHKSIGNU_V1.bind_chain(chain_id)`,
/// so a contract signature commits to the chain it was produced for.
pub const CHKSIGNU_V1: DomainTag = DomainTag::from_ascii("ONX_CHKSIGNU_V1");
/// `ONX:VALIDATOR:SIGN:V1` domain tag (§4.5).
pub const VALIDATOR_SIGN_V1: DomainTag = DomainTag::from_ascii("ONX_VALIDATOR_SIGN_V1");

/// Raw SHA-256 (FIPS PUB 180-4), with no domain separation.
///
/// This exists to verify the underlying primitive against standard NIST
/// test vectors. Consensus-critical code must use [`domain_hash`] instead,
/// never this function directly, per §3.5.
pub fn sha256(data: &[u8]) -> [u8; 32] {
    Sha256::digest(data).into()
}

/// SHA-256 over `tag || data`, per §4.5. This is the only hashing entry
/// point consensus-critical code should use.
pub fn domain_hash(tag: &DomainTag, data: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(tag.as_bytes());
    hasher.update(data);
    hasher.finalize().into()
}
