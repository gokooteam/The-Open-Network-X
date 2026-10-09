//! Wire encoding for consensus messages (M6).
//!
//! `ConsensusProposal` and `ConsensusVote` travel between validators as
//! RLDP payloads. The format is tag-prefixed and fixed-layout, following
//! the `block_sync` codec pattern: a 4-byte tag, then big-endian fields.
//! Signatures are NOT recomputed here — they are part of the message, and
//! the engine verifies them on receipt (`receive_proposal` /
//! `receive_vote`). The signing bytes themselves are defined by
//! `proposal_signing_bytes` / `vote_signing_bytes` in `engine.rs`.

use crate::engine::{ConsensusProposal, ConsensusVote, VotePhase};
use crate::{ConsensusError, Signature, Uint256};

/// Wire tag for a consensus proposal payload.
pub const PROPOSAL_TAG: u32 = 0x4f58_4350; // "ONXCP" truncated
/// Wire tag for a consensus vote payload.
pub const VOTE_TAG: u32 = 0x4f58_4356; // "ONXCV" truncated

fn read_u32_be(bytes: &[u8]) -> Result<(u32, &[u8]), ConsensusError> {
    if bytes.len() < 4 {
        return Err(ConsensusError::InvalidMessage("truncated u32"));
    }
    Ok((
        u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
        &bytes[4..],
    ))
}

fn read_u64_be(bytes: &[u8]) -> Result<(u64, &[u8]), ConsensusError> {
    if bytes.len() < 8 {
        return Err(ConsensusError::InvalidMessage("truncated u64"));
    }
    let mut b = [0u8; 8];
    b.copy_from_slice(&bytes[..8]);
    Ok((u64::from_be_bytes(b), &bytes[8..]))
}

fn read_hash(bytes: &[u8]) -> Result<(Uint256, &[u8]), ConsensusError> {
    if bytes.len() < 32 {
        return Err(ConsensusError::InvalidMessage("truncated hash"));
    }
    let mut h = [0u8; 32];
    h.copy_from_slice(&bytes[..32]);
    Ok((Uint256(h), &bytes[32..]))
}

fn read_sig(bytes: &[u8]) -> Result<(Signature, &[u8]), ConsensusError> {
    if bytes.len() < 64 {
        return Err(ConsensusError::InvalidMessage("truncated signature"));
    }
    let mut s = [0u8; 64];
    s.copy_from_slice(&bytes[..64]);
    let sig = Signature::decode_exact(&s)
        .map_err(|_| ConsensusError::InvalidMessage("bad signature encoding"))?;
    Ok((sig, &bytes[64..]))
}

/// Encode a proposal: tag | height u64 | round u32 | block_hash 32B |
/// proposer_id u32 | signature 64B.
pub fn encode_proposal(p: &ConsensusProposal) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 + 8 + 4 + 32 + 4 + 64);
    out.extend_from_slice(&PROPOSAL_TAG.to_be_bytes());
    out.extend_from_slice(&p.height.to_be_bytes());
    out.extend_from_slice(&p.round.to_be_bytes());
    out.extend_from_slice(&p.block_hash.0);
    out.extend_from_slice(&p.proposer_id.to_be_bytes());
    out.extend_from_slice(&p.signature.encode());
    out
}

/// Decode a proposal. Rejects wrong tags and trailing bytes.
pub fn decode_proposal(bytes: &[u8]) -> Result<ConsensusProposal, ConsensusError> {
    let (tag, rest) = read_u32_be(bytes)?;
    if tag != PROPOSAL_TAG {
        return Err(ConsensusError::InvalidMessage("bad proposal tag"));
    }
    let (height, rest) = read_u64_be(rest)?;
    let (round, rest) = read_u32_be(rest)?;
    let (block_hash, rest) = read_hash(rest)?;
    let (proposer_id, rest) = read_u32_be(rest)?;
    let (signature, rest) = read_sig(rest)?;
    if !rest.is_empty() {
        return Err(ConsensusError::InvalidMessage(
            "trailing bytes in proposal",
        ));
    }
    Ok(ConsensusProposal {
        height,
        round,
        block_hash,
        proposer_id,
        signature,
    })
}

/// Encode a vote: tag | height u64 | round u32 | phase u8 | block_hash 32B |
/// validator_id u32 | signature 64B | header_sig flag u8 | header_sig 64B?
///
/// The trailing header signature is present only on Commit votes (M6): it
/// is the validator's signature over the block header it validated, so a
/// finalized block's commit quorum doubles as its multi-signature section.
/// The engine never sees it — the driver attaches it on send and strips it
/// on receipt, verifying it against the locally validated block header.
pub fn encode_vote(v: &ConsensusVote, header_sig: Option<&Signature>) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 + 8 + 4 + 1 + 32 + 4 + 64 + 1 + 64);
    out.extend_from_slice(&VOTE_TAG.to_be_bytes());
    out.extend_from_slice(&v.height.to_be_bytes());
    out.extend_from_slice(&v.round.to_be_bytes());
    out.push(match v.phase {
        VotePhase::PreVote => 1,
        VotePhase::PreCommit => 2,
        VotePhase::Commit => 3,
    });
    out.extend_from_slice(&v.block_hash.0);
    out.extend_from_slice(&v.validator_id.to_be_bytes());
    out.extend_from_slice(&v.signature.encode());
    match header_sig {
        Some(sig) => {
            out.push(1);
            out.extend_from_slice(&sig.encode());
        }
        None => out.push(0),
    }
    out
}

/// Decode a vote, returning the engine vote and the optional header
/// signature. Rejects wrong tags, unknown phases, trailing bytes, and a
/// header signature on a non-Commit vote.
pub fn decode_vote(bytes: &[u8]) -> Result<(ConsensusVote, Option<Signature>), ConsensusError> {
    let (tag, rest) = read_u32_be(bytes)?;
    if tag != VOTE_TAG {
        return Err(ConsensusError::InvalidMessage("bad vote tag"));
    }
    let (height, rest) = read_u64_be(rest)?;
    let (round, rest) = read_u32_be(rest)?;
    if rest.is_empty() {
        return Err(ConsensusError::InvalidMessage("truncated phase"));
    }
    let phase = match rest[0] {
        1 => VotePhase::PreVote,
        2 => VotePhase::PreCommit,
        3 => VotePhase::Commit,
        _ => return Err(ConsensusError::InvalidMessage("unknown vote phase")),
    };
    let rest = &rest[1..];
    let (block_hash, rest) = read_hash(rest)?;
    let (validator_id, rest) = read_u32_be(rest)?;
    let (signature, rest) = read_sig(rest)?;
    if rest.is_empty() {
        return Err(ConsensusError::InvalidMessage(
            "truncated header_sig flag",
        ));
    }
    let (header_sig, rest) = match rest[0] {
        0 => (None, &rest[1..]),
        1 => {
            if rest.len() < 65 {
                return Err(ConsensusError::InvalidMessage(
                    "truncated header signature",
                ));
            }
            let sig = Signature::decode_exact(&rest[1..65])
                .map_err(|_| ConsensusError::InvalidMessage("bad header sig encoding"))?;
            // A header signature is only meaningful on a Commit vote: it
            // attests to the finalized block. Refuse it elsewhere so a
            // malformed sender cannot smuggle bytes through.
            if phase != VotePhase::Commit {
                return Err(ConsensusError::InvalidMessage(
                    "header sig on non-commit vote",
                ));
            }
            (Some(sig), &rest[65..])
        }
        _ => return Err(ConsensusError::InvalidMessage("bad header_sig flag")),
    };
    if !rest.is_empty() {
        return Err(ConsensusError::InvalidMessage(
            "trailing bytes in vote",
        ));
    }
    Ok((
        ConsensusVote {
            height,
            round,
            phase,
            block_hash,
            validator_id,
            signature,
        },
        header_sig,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_proposal() -> ConsensusProposal {
        ConsensusProposal {
            height: 42,
            round: 3,
            block_hash: Uint256([0xab; 32]),
            proposer_id: 1,
            signature: Signature::decode_exact(&[0xcd; 64]).unwrap(),
        }
    }

    fn sample_vote() -> ConsensusVote {
        ConsensusVote {
            height: 42,
            round: 3,
            phase: VotePhase::PreCommit,
            block_hash: Uint256([0xab; 32]),
            validator_id: 2,
            signature: Signature::decode_exact(&[0xef; 64]).unwrap(),
        }
    }

    #[test]
    fn proposal_roundtrip() {
        let p = sample_proposal();
        let rt = decode_proposal(&encode_proposal(&p)).expect("decodes");
        assert_eq!(rt, p);
    }

    #[test]
    fn vote_roundtrip() {
        let v = sample_vote();
        let (rt, hs) = decode_vote(&encode_vote(&v, None)).expect("decodes");
        assert_eq!(rt, v);
        assert!(hs.is_none());
    }

    #[test]
    fn vote_roundtrip_with_header_sig() {
        let mut v = sample_vote();
        v.phase = VotePhase::Commit;
        let hs = Signature::decode_exact(&[0xaa; 64]).unwrap();
        let (rt, rt_hs) = decode_vote(&encode_vote(&v, Some(&hs))).expect("decodes");
        assert_eq!(rt, v);
        assert_eq!(rt_hs.unwrap().encode(), hs.encode());
    }

    #[test]
    fn header_sig_on_prevote_rejected() {
        let mut v = sample_vote();
        v.phase = VotePhase::PreVote;
        let hs = Signature::decode_exact(&[0xaa; 64]).unwrap();
        assert!(decode_vote(&encode_vote(&v, Some(&hs))).is_err());
    }

    #[test]
    fn wrong_tag_rejected() {
        let mut bad = encode_proposal(&sample_proposal());
        bad[0] ^= 0xff;
        assert!(decode_proposal(&bad).is_err());
        let mut bad = encode_vote(&sample_vote(), None);
        bad[0] ^= 0xff;
        assert!(decode_vote(&bad).is_err());
    }

    #[test]
    fn trailing_bytes_rejected() {
        let mut bad = encode_proposal(&sample_proposal());
        bad.push(0x00);
        assert!(decode_proposal(&bad).is_err());
    }

    #[test]
    fn unknown_phase_rejected() {
        let mut bad = encode_vote(&sample_vote(), None);
        // phase byte is at offset 4 + 8 + 4 = 16
        bad[16] = 0x09;
        assert!(decode_vote(&bad).is_err());
    }

    #[test]
    fn truncated_rejected() {
        let enc = encode_vote(&sample_vote(), None);
        for len in [0, 3, 16, 50] {
            assert!(decode_vote(&enc[..len]).is_err(), "len {len}");
        }
    }
}
