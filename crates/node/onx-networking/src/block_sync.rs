//! M5 block-sync wire protocol: how blocks move between nodes.
//!
//! Layering: ADNL carries encrypted datagrams ([`crate::adnl_transport`]),
//! RLDP carries multi-datagram payloads ([`crate::rldp`]). This module
//! defines WHAT is carried: the three sync messages, their strict codec,
//! the producer-side block server, and follower-side response parsing.
//!
//! Transport mapping (ADR-0042, ADR-0043):
//! - [`BlockAnnouncement`] is broadcast to every connected peer (the
//!   M5 "overlay" is the producer's peer list; a real broadcast overlay
//!   is M6 work).
//! - [`BlockRequest`] / [`BlockResponse`] travel point-to-point over an
//!   ADNL channel. Responses larger than one ADNL datagram (~64KB) MUST
//!   be chunked through RLDP; the codec only bounds the total size.
//!
//! Trust boundary: NOTHING received here is trusted. For every fetched
//! block the follower checks, in order: the `ONXBLK05` magic, strict
//! decode of the file, that `onx_stf::BlockHeader::hash()` of the decoded
//! block matches the announcement (cheap filter before the expensive
//! check), then `verify_block_auth` (ONXBLK05 signatures against the
//! genesis validator set, chain-bound preimage) — and only then applies
//! it. The verification order lives in the `onxd` follower loop, not
//! here. This module guarantees framing integrity only: a peer that sends
//! garbage gets a deterministic rejection, never a partial parse.

use crate::NetworkError;
use std::fmt;
use std::path::{Path, PathBuf};

/// Sync protocol version. Bump on any wire-incompatible change; decoders
/// reject anything else fail-closed.
pub const SYNC_PROTOCOL_VERSION: u8 = 1;

/// Maximum total encoded message size accepted by the decoder (8 MiB).
/// Bounds the decoder's upfront reservation against a malicious
/// `payload_len`. Block bodies above this are a protocol violation, not
/// a transport problem — real M5 blocks are kilobytes.
pub const MAX_SYNC_MESSAGE_BYTES: usize = 8 * 1024 * 1024;

/// Wire framing added around the raw block file bytes in a
/// [`BlockResponse`]: the 6-byte envelope plus the 8-byte response
/// prefix (`seq_no:u32be` + `block_len:u32be`).
const RESPONSE_FRAMING_OVERHEAD: usize = 6 + 8;

/// Largest block file the sync server will serve. This is the decoder's
/// message cap minus the response framing overhead: a file of exactly
/// [`MAX_SYNC_MESSAGE_BYTES`] would encode to a message no follower can
/// decode (the 14 framing bytes push the total over the cap), so the
/// server fails closed here and every served response is guaranteed
/// decodable. The producer never commits a block whose worst-case
/// encoded file exceeds this (ADR-0045), so the refusal is unreachable
/// on an honest chain — it is the backstop, not the mechanism.
pub const MAX_BLOCK_FILE_BYTES: usize = MAX_SYNC_MESSAGE_BYTES - RESPONSE_FRAMING_OVERHEAD;

/// Message type tags. Unknown tags are rejected; the tag space is not
/// extensible by convention — a new message means a version bump.
const MSG_ANNOUNCE: u8 = 0x01;
const MSG_REQUEST: u8 = 0x02;
const MSG_RESPONSE: u8 = 0x03;

/// Canonical block file name for a sequence number, matching the
/// producer's `block-{seqno:08}.blk` layout (`onx::blockfile`). The name
/// is fully determined by the `u32` seqno, so no path traversal is
/// possible through it.
fn block_file_name(seq_no: u32) -> String {
    format!("block-{seq_no:08}.blk")
}

/// A producer's announcement that a block was committed: its sequence
/// number, block hash, and resulting state root. The follower uses the
/// hash to check the fetched bytes before paying for signature
/// verification; the state root is informational (it becomes a
/// checkpoint once the block verifies and applies).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockAnnouncement {
    pub seq_no: u32,
    pub block_hash: [u8; 32],
    pub state_root: [u8; 32],
}

/// A follower's request for one block's bytes by sequence number.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockRequest {
    pub seq_no: u32,
}

/// A producer's answer: the raw `.blk` file bytes for the requested
/// sequence number. The caller MUST verify these bytes — magic, strict
/// decode, announcement-hash match, `verify_block_auth` — before use;
/// see the module docs. Framing integrity is not content validity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockResponse {
    pub seq_no: u32,
    pub block_bytes: Vec<u8>,
}

/// Reasons a sync message or server operation is rejected. All
/// fail-closed: a bad message never yields a partial parse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncError {
    /// Fewer bytes than the envelope header needs.
    TruncatedEnvelope { got_len: usize },
    /// Wrong protocol version — never negotiated down.
    BadVersion { got: u8 },
    /// Unknown message type tag.
    UnknownMessageType { got: u8 },
    /// `payload_len` disagrees with the actual trailing bytes.
    PayloadLengthMismatch { claimed: usize, actual: usize },
    /// Total message exceeds [`MAX_SYNC_MESSAGE_BYTES`].
    MessageTooLarge { len: usize },
    /// Fixed-size payload with the wrong length.
    BadPayloadLength { msg_type: u8, got: usize },
    /// Response payload too short for its fixed prefix, or `block_len`
    /// disagrees with the trailing bytes.
    MalformedResponse { got_len: usize },
    /// Expected one message type, got another.
    UnexpectedMessage { expected: u8, got: u8 },
    /// Response seqno does not match the request.
    SeqNoMismatch { expected: u32, got: u32 },
    /// No block file for this sequence number.
    UnknownBlock(u32),
    /// Block file exceeds [`MAX_BLOCK_FILE_BYTES`].
    BlockTooLarge { seq_no: u32, len: u64 },
    /// Block file could not be read.
    BlockReadFailed { seq_no: u32, reason: String },
    /// Underlying datagram transport failure (UDP I/O, closed channel...).
    /// Never a framing or content fault.
    Transport(String),
    /// Peer did not answer within the configured timeout. Expected in
    /// normal operation (e.g. block not produced yet); the caller retries.
    Timeout { what: &'static str },
}

impl fmt::Display for SyncError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TruncatedEnvelope { got_len } => {
                write!(
                    f,
                    "sync message shorter than the 6-byte envelope: {got_len} bytes"
                )
            }
            Self::BadVersion { got } => {
                write!(
                    f,
                    "unsupported sync protocol version {got}, expected {SYNC_PROTOCOL_VERSION}"
                )
            }
            Self::UnknownMessageType { got } => {
                write!(f, "unknown sync message type 0x{got:02x}")
            }
            Self::PayloadLengthMismatch { claimed, actual } => {
                write!(
                    f,
                    "sync payload length mismatch: claimed {claimed}, got {actual}"
                )
            }
            Self::MessageTooLarge { len } => {
                write!(
                    f,
                    "sync message too large: {len} bytes, max {MAX_SYNC_MESSAGE_BYTES}"
                )
            }
            Self::BadPayloadLength { msg_type, got } => {
                write!(
                    f,
                    "sync message 0x{msg_type:02x} has wrong payload length: {got}"
                )
            }
            Self::MalformedResponse { got_len } => {
                write!(f, "malformed block response payload: {got_len} bytes")
            }
            Self::UnexpectedMessage { expected, got } => {
                write!(f, "expected sync message 0x{expected:02x}, got 0x{got:02x}")
            }
            Self::SeqNoMismatch { expected, got } => {
                write!(
                    f,
                    "block response seqno {got} does not match request {expected}"
                )
            }
            Self::UnknownBlock(seq_no) => {
                write!(f, "no block file for seqno {seq_no}")
            }
            Self::BlockTooLarge { seq_no, len } => {
                write!(
                    f,
                    "block {seq_no} is {len} bytes, over the {MAX_SYNC_MESSAGE_BYTES}-byte sync cap"
                )
            }
            Self::BlockReadFailed { seq_no, reason } => {
                write!(f, "could not read block {seq_no}: {reason}")
            }
            Self::Transport(reason) => {
                write!(f, "sync transport failure: {reason}")
            }
            Self::Timeout { what } => {
                write!(f, "sync timeout waiting for {what}")
            }
        }
    }
}

impl std::error::Error for SyncError {}

/// Transport-level failures surface as [`SyncError::Transport`]; the sync
/// layer never invents framing faults from I/O errors.
impl From<NetworkError> for SyncError {
    fn from(err: NetworkError) -> Self {
        Self::Transport(err.to_string())
    }
}

impl From<SyncError> for NetworkError {
    /// Sync framing faults surface as transport-level decryption-agnostic
    /// failures: they are peer misbehavior, never crypto faults.
    fn from(err: SyncError) -> Self {
        match err {
            SyncError::TruncatedEnvelope { .. }
            | SyncError::PayloadLengthMismatch { .. }
            | SyncError::BadPayloadLength { .. }
            | SyncError::MalformedResponse { .. } => Self::TruncatedPacket,
            SyncError::Transport(reason) => Self::TransportIo(reason),
            SyncError::Timeout { .. } => Self::RldpTimeout,
            _ => Self::MalformedRldpFrame,
        }
    }
}

/// Encode one message: `version:u8 || type:u8 || payload_len:u32be || payload`.
fn encode_envelope(msg_type: u8, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(6 + payload.len());
    out.push(SYNC_PROTOCOL_VERSION);
    out.push(msg_type);
    out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    out.extend_from_slice(payload);
    out
}

/// Split and validate one envelope. Returns `(msg_type, payload)`.
fn decode_envelope(bytes: &[u8]) -> Result<(u8, &[u8]), SyncError> {
    if bytes.len() > MAX_SYNC_MESSAGE_BYTES {
        return Err(SyncError::MessageTooLarge { len: bytes.len() });
    }
    if bytes.len() < 6 {
        return Err(SyncError::TruncatedEnvelope {
            got_len: bytes.len(),
        });
    }
    if bytes[0] != SYNC_PROTOCOL_VERSION {
        return Err(SyncError::BadVersion { got: bytes[0] });
    }
    let msg_type = bytes[1];
    if !matches!(msg_type, MSG_ANNOUNCE | MSG_REQUEST | MSG_RESPONSE) {
        return Err(SyncError::UnknownMessageType { got: msg_type });
    }
    let claimed = u32::from_be_bytes(bytes[2..6].try_into().expect("four bytes")) as usize;
    let actual = bytes.len() - 6;
    if claimed != actual {
        return Err(SyncError::PayloadLengthMismatch { claimed, actual });
    }
    Ok((msg_type, &bytes[6..]))
}

fn encode_u32(v: u32) -> [u8; 4] {
    v.to_be_bytes()
}

fn decode_u32(bytes: &[u8]) -> u32 {
    u32::from_be_bytes(bytes.try_into().expect("four bytes"))
}

/// Encode a [`BlockAnnouncement`] as a wire message.
pub fn encode_announcement(ann: &BlockAnnouncement) -> Vec<u8> {
    let mut payload = Vec::with_capacity(68);
    payload.extend_from_slice(&encode_u32(ann.seq_no));
    payload.extend_from_slice(&ann.block_hash);
    payload.extend_from_slice(&ann.state_root);
    encode_envelope(MSG_ANNOUNCE, &payload)
}

/// Decode and strictly validate a [`BlockAnnouncement`] wire message.
pub fn decode_announcement(bytes: &[u8]) -> Result<BlockAnnouncement, SyncError> {
    let (msg_type, payload) = decode_envelope(bytes)?;
    if msg_type != MSG_ANNOUNCE {
        return Err(SyncError::UnexpectedMessage {
            expected: MSG_ANNOUNCE,
            got: msg_type,
        });
    }
    if payload.len() != 68 {
        return Err(SyncError::BadPayloadLength {
            msg_type,
            got: payload.len(),
        });
    }
    let mut block_hash = [0u8; 32];
    let mut state_root = [0u8; 32];
    block_hash.copy_from_slice(&payload[4..36]);
    state_root.copy_from_slice(&payload[36..68]);
    Ok(BlockAnnouncement {
        seq_no: decode_u32(&payload[..4]),
        block_hash,
        state_root,
    })
}

/// Encode a [`BlockRequest`] as a wire message.
pub fn encode_request(req: &BlockRequest) -> Vec<u8> {
    encode_envelope(MSG_REQUEST, &encode_u32(req.seq_no))
}

/// Decode and strictly validate a [`BlockRequest`] wire message.
pub fn decode_request(bytes: &[u8]) -> Result<BlockRequest, SyncError> {
    let (msg_type, payload) = decode_envelope(bytes)?;
    if msg_type != MSG_REQUEST {
        return Err(SyncError::UnexpectedMessage {
            expected: MSG_REQUEST,
            got: msg_type,
        });
    }
    if payload.len() != 4 {
        return Err(SyncError::BadPayloadLength {
            msg_type,
            got: payload.len(),
        });
    }
    Ok(BlockRequest {
        seq_no: decode_u32(payload),
    })
}

/// Encode a [`BlockResponse`] as a wire message.
pub fn encode_response(resp: &BlockResponse) -> Vec<u8> {
    let mut payload = Vec::with_capacity(8 + resp.block_bytes.len());
    payload.extend_from_slice(&encode_u32(resp.seq_no));
    payload.extend_from_slice(&(resp.block_bytes.len() as u32).to_be_bytes());
    payload.extend_from_slice(&resp.block_bytes);
    encode_envelope(MSG_RESPONSE, &payload)
}

/// Decode a [`BlockResponse`] and check it answers `expected_seq_no`.
///
/// Returns the raw block bytes. The caller MUST still verify them
/// (magic, strict decode, announcement-hash match, `verify_block_auth`)
/// before use — framing integrity is not content validity.
pub fn decode_response(bytes: &[u8], expected_seq_no: u32) -> Result<Vec<u8>, SyncError> {
    let (msg_type, payload) = decode_envelope(bytes)?;
    if msg_type != MSG_RESPONSE {
        return Err(SyncError::UnexpectedMessage {
            expected: MSG_RESPONSE,
            got: msg_type,
        });
    }
    if payload.len() < 8 {
        return Err(SyncError::MalformedResponse {
            got_len: payload.len(),
        });
    }
    let seq_no = decode_u32(&payload[..4]);
    let block_len = u32::from_be_bytes(payload[4..8].try_into().expect("four bytes")) as usize;
    if block_len != payload.len() - 8 {
        return Err(SyncError::MalformedResponse {
            got_len: payload.len(),
        });
    }
    if seq_no != expected_seq_no {
        return Err(SyncError::SeqNoMismatch {
            expected: expected_seq_no,
            got: seq_no,
        });
    }
    Ok(payload[8..].to_vec())
}

/// Producer side of block sync: serves `.blk` files from a blocks
/// directory. The directory layout matches the producer's
/// (`<storage>/blocks/block-{seqno:08}.blk`, written atomically — a
/// crash never leaves a torn file, so a served file is always whole).
#[derive(Debug, Clone)]
pub struct BlockServer {
    blocks_dir: PathBuf,
}

impl BlockServer {
    /// Serve block files from `blocks_dir`.
    pub fn new(blocks_dir: PathBuf) -> Self {
        Self { blocks_dir }
    }

    /// Handle one raw request envelope, returning the raw response
    /// envelope. Fails closed on anything that is not a well-formed
    /// [`BlockRequest`], and on missing or oversized block files.
    pub fn handle_request(&self, request_bytes: &[u8]) -> Result<Vec<u8>, SyncError> {
        let req = decode_request(request_bytes)?;
        let path = Path::new(&self.blocks_dir).join(block_file_name(req.seq_no));
        let metadata = std::fs::metadata(&path).map_err(|_| SyncError::UnknownBlock(req.seq_no))?;
        // The cap is MAX_BLOCK_FILE_BYTES, not MAX_SYNC_MESSAGE_BYTES:
        // encode_response adds 14 framing bytes, so a file between the
        // two caps would produce a response the follower's decoder
        // rejects with MessageTooLarge — served but never syncable.
        if metadata.len() > MAX_BLOCK_FILE_BYTES as u64 {
            return Err(SyncError::BlockTooLarge {
                seq_no: req.seq_no,
                len: metadata.len(),
            });
        }
        let block_bytes = std::fs::read(&path).map_err(|e| SyncError::BlockReadFailed {
            seq_no: req.seq_no,
            reason: e.to_string(),
        })?;
        Ok(encode_response(&BlockResponse {
            seq_no: req.seq_no,
            block_bytes,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ann() -> BlockAnnouncement {
        BlockAnnouncement {
            seq_no: 7,
            block_hash: [0xab; 32],
            state_root: [0xcd; 32],
        }
    }

    #[test]
    fn announcement_round_trip() {
        let a = ann();
        let back = decode_announcement(&encode_announcement(&a)).unwrap();
        assert_eq!(a, back);
    }

    #[test]
    fn request_round_trip() {
        let r = BlockRequest { seq_no: 1 };
        let back = decode_request(&encode_request(&r)).unwrap();
        assert_eq!(r, back);
    }

    #[test]
    fn response_round_trip() {
        let resp = BlockResponse {
            seq_no: 42,
            block_bytes: b"ONXBLK05fake-block-bytes".to_vec(),
        };
        let back = decode_response(&encode_response(&resp), 42).unwrap();
        assert_eq!(back, resp.block_bytes);
    }

    #[test]
    fn envelope_rejects_garbage() {
        // Empty and short inputs.
        assert!(decode_announcement(&[]).is_err());
        assert!(decode_announcement(&[1, 2, 3]).is_err());
        // Bad version.
        let mut bad = encode_announcement(&ann());
        bad[0] = 2;
        assert!(matches!(
            decode_announcement(&bad),
            Err(SyncError::BadVersion { got: 2 })
        ));
        // Unknown type tag.
        let mut bad = encode_announcement(&ann());
        bad[1] = 0x77;
        assert!(matches!(
            decode_announcement(&bad),
            Err(SyncError::UnknownMessageType { got: 0x77 })
        ));
        // Length lie: claimed payload longer than actual.
        let mut bad = encode_announcement(&ann());
        bad[5] = bad[5].wrapping_add(10);
        assert!(matches!(
            decode_announcement(&bad),
            Err(SyncError::PayloadLengthMismatch { .. })
        ));
        // Trailing bytes after a good message.
        let mut bad = encode_announcement(&ann());
        bad.extend_from_slice(&[0u8; 3]);
        assert!(matches!(
            decode_announcement(&bad),
            Err(SyncError::PayloadLengthMismatch { .. })
        ));
    }

    #[test]
    fn wrong_type_rejected_per_decoder() {
        let req = encode_request(&BlockRequest { seq_no: 3 });
        assert!(matches!(
            decode_announcement(&req),
            Err(SyncError::UnexpectedMessage { .. })
        ));
        let ann = encode_announcement(&ann());
        assert!(matches!(
            decode_request(&ann),
            Err(SyncError::UnexpectedMessage { .. })
        ));
    }

    #[test]
    fn fixed_size_payloads_reject_wrong_length() {
        // Announcement with a 4-byte payload (a request's shape).
        let bad = encode_envelope(MSG_ANNOUNCE, &[0u8; 4]);
        assert!(matches!(
            decode_announcement(&bad),
            Err(SyncError::BadPayloadLength { got: 4, .. })
        ));
        // Request with a 68-byte payload (an announcement's shape).
        let bad = encode_envelope(MSG_REQUEST, &[0u8; 68]);
        assert!(matches!(
            decode_request(&bad),
            Err(SyncError::BadPayloadLength { got: 68, .. })
        ));
    }

    #[test]
    fn response_rejects_seqno_mismatch_and_length_lies() {
        let resp = BlockResponse {
            seq_no: 9,
            block_bytes: vec![1, 2, 3, 4],
        };
        let wire = encode_response(&resp);
        // Wrong expected seqno: the peer answered a different request.
        assert!(matches!(
            decode_response(&wire, 10),
            Err(SyncError::SeqNoMismatch {
                expected: 10,
                got: 9
            })
        ));
        // block_len lies about the trailing bytes.
        let mut bad = wire.clone();
        let len_pos = 6 + 4;
        bad[len_pos + 3] = bad[len_pos + 3].wrapping_add(1);
        assert!(matches!(
            decode_response(&bad, 9),
            Err(SyncError::MalformedResponse { .. })
        ));
        // Truncated response payload.
        let bad = encode_envelope(MSG_RESPONSE, &[0u8; 5]);
        assert!(matches!(
            decode_response(&bad, 9),
            Err(SyncError::MalformedResponse { .. })
        ));
    }

    #[test]
    fn oversize_message_rejected_before_allocation() {
        let huge = vec![0u8; MAX_SYNC_MESSAGE_BYTES + 1];
        assert!(matches!(
            decode_envelope(&huge),
            Err(SyncError::MessageTooLarge { .. })
        ));
    }

    fn test_blocks_dir(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("onx-block-sync-test-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn server_serves_block_file() {
        let dir = test_blocks_dir("serve");
        let body = b"ONXBLK05".to_vec();
        std::fs::write(dir.join("block-00000007.blk"), &body).unwrap();
        let server = BlockServer::new(dir.clone());
        let req = encode_request(&BlockRequest { seq_no: 7 });
        let resp_bytes = server.handle_request(&req).unwrap();
        let got = decode_response(&resp_bytes, 7).unwrap();
        assert_eq!(got, body);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn server_rejects_unknown_block_and_wrong_message() {
        let dir = test_blocks_dir("reject");
        let server = BlockServer::new(dir.clone());
        let req = encode_request(&BlockRequest { seq_no: 99 });
        assert!(matches!(
            server.handle_request(&req),
            Err(SyncError::UnknownBlock(99))
        ));
        // An announcement is not a request.
        let ann = encode_announcement(&ann());
        assert!(matches!(
            server.handle_request(&ann),
            Err(SyncError::UnexpectedMessage { .. })
        ));
        // Garbage is not a request.
        assert!(server.handle_request(b"not a message").is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn framing_overhead_arithmetic_is_exact() {
        // The servable-file cap plus the response framing must reconstruct
        // the decoder's message cap exactly — no off-by-one in either
        // direction.
        assert_eq!(
            MAX_BLOCK_FILE_BYTES + RESPONSE_FRAMING_OVERHEAD,
            MAX_SYNC_MESSAGE_BYTES
        );
    }

    #[test]
    fn server_serves_file_at_exact_cap_and_it_decodes() {
        // Regression test for the 14-byte gap the review bots caught: a
        // file of exactly MAX_BLOCK_FILE_BYTES must be served AND the
        // response must decode. (Before the fix the server accepted files
        // up to MAX_SYNC_MESSAGE_BYTES, whose responses the follower's
        // decoder rejects with MessageTooLarge — served but never
        // syncable.)
        let dir = test_blocks_dir("cap-exact");
        let body = vec![0x42u8; MAX_BLOCK_FILE_BYTES];
        std::fs::write(dir.join("block-00000011.blk"), &body).unwrap();
        let server = BlockServer::new(dir.clone());
        let req = encode_request(&BlockRequest { seq_no: 11 });
        let resp_bytes = server.handle_request(&req).unwrap();
        let got = decode_response(&resp_bytes, 11).unwrap();
        assert_eq!(got, body);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn server_rejects_file_one_byte_over_cap() {
        let dir = test_blocks_dir("cap-over");
        let body = vec![0x42u8; MAX_BLOCK_FILE_BYTES + 1];
        std::fs::write(dir.join("block-00000012.blk"), &body).unwrap();
        let server = BlockServer::new(dir.clone());
        let req = encode_request(&BlockRequest { seq_no: 12 });
        assert!(matches!(
            server.handle_request(&req),
            Err(SyncError::BlockTooLarge { seq_no: 12, .. })
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn server_file_name_is_traversal_safe() {
        // seqno is formatted as zero-padded digits only; no input can
        // smuggle a path separator into the file name.
        assert_eq!(block_file_name(0), "block-00000000.blk");
        assert_eq!(block_file_name(1), "block-00000001.blk");
        assert_eq!(block_file_name(u32::MAX), "block-4294967295.blk");
        for n in [0u32, 1, 7, 123456, u32::MAX] {
            let name = block_file_name(n);
            assert!(!name.contains('/') && !name.contains('\\') && !name.contains(".."));
        }
    }

    #[test]
    fn sync_error_converts_to_network_error() {
        let net: NetworkError = SyncError::TruncatedEnvelope { got_len: 2 }.into();
        assert_eq!(net, NetworkError::TruncatedPacket);
        let net: NetworkError = SyncError::UnknownBlock(5).into();
        assert_eq!(net, NetworkError::MalformedRldpFrame);
    }
}
