//! Hand-derived golden vectors for ONX message and block encodings.
//!
//! Every expected value below was computed INDEPENDENTLY of this Rust
//! implementation, straight from the spec documents:
//!   - `docs/specification/protocol-primitives.md` (SHA-256, big-endian ints,
//!     32-byte zero-padded domain tags)
//!   - `docs/adr/0002-message-encoding-and-domain-tags.md` (external message
//!     field layout, domain tags, msgs_root, 148-byte legacy header layout)
//!   - `docs/adr/0032-onxblk05-authenticated-headers.md` (ONXBLK05: the header
//!     grew to 160 bytes — protocol_version u32be + block_time u64be
//!     appended after msg_count; the hand header vector was re-derived for
//!     160 bytes in `reference/history/hand_derive_onxblk05_header.py`)
//!
//! Derivation script: `hidden_files/hand_derive_vectors_msg.py` (stdlib
//! `struct` + `hashlib` only; Ed25519 via PyNaCl/libsodium, NOT the Rust
//! ed25519-dalek code — the signature test below cross-checks the
//! independent signature against the implementation's own verifier, so both
//! sides must agree).
//! Parameters confirmed from code (parameters only, no logic copied):
//! SHA-256; `domain_hash(tag,msg)=SHA256(pad32(tag)||msg)`;
//! tags `ONX_MSG_EXT_V1`, `ONX_MSG_EXT_SIGN_V1`, `ONX_MSGS_ROOT_V1`,
//! `ONX_BLOCK_HDR_V1`, `ONX_ADDR_V1`;
//! `ExternalMessage::hash()` covers the full wire (body || signature);
//! the signature domain message is `pad32(tag) || body_bytes`.
//!
//! RULE: if any test here fails, INVESTIGATE. Do not "fix" the vector to
//! match the implementation — either the spec interpretation or the
//! implementation is wrong, and the report must say which.

use onx_data_structures::AccountId;
use onx_primitives::PublicKey;
use onx_stf::{derive_address, msgs_root, BlockHeader, ExternalMessage, MsgKind};

/// Hex decoder (avoids adding a dependency for test constants).
fn h(s: &str) -> Vec<u8> {
    assert!(s.len().is_multiple_of(2), "odd hex length");
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("bad hex"))
        .collect()
}

fn h32(s: &str) -> [u8; 32] {
    h(s).try_into().expect("expected 32 bytes")
}

fn h64(s: &str) -> [u8; 64] {
    h(s).try_into().expect("expected 64 bytes")
}

// Vector A inputs (human-chosen): chain_id = 0xDD*32, kind = Transfer(0),
// from = 0x11*32, nonce = 7, to = 0x22*32, amount = 1000, fee = 10,
// message empty, pubkey = 0x44*32 (nonzero reveal field, exercised in the
// encoding vector), signature = 0x33*64 (pattern only, NOT a valid
// signature — encoding vector, not a signature vector).
fn hand_msg() -> ExternalMessage {
    ExternalMessage {
        chain_id: [0xDD; 32],
        kind: MsgKind::Transfer,
        from: AccountId::from_bytes([0x11; 32]),
        nonce: 7,
        to: AccountId::from_bytes([0x22; 32]),
        amount_nanos: 1000,
        fee_nanos: 10,
        message: Vec::new(),
        pubkey: [0x44; 32],
        signature: [0x33; 64],
    }
}

const HAND_MSG_WIRE_HEX: &str = "dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd11111111111111111111111111111111111111111111111111111111111111110000000000000007002222222222222222222222222222222222222222222222222222222222222222000000000000000000000000000003e80000000000000000000000000000000a00000000444444444444444444444444444444444444444444444444444444444444444433333333333333333333333333333333333333333333333333333333333333333333333333333333333333333333333333333333333333333333333333333333";
const HAND_MSG_BODY_HEX: &str = "dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd11111111111111111111111111111111111111111111111111111111111111110000000000000007002222222222222222222222222222222222222222222222222222222222222222000000000000000000000000000003e80000000000000000000000000000000a000000004444444444444444444444444444444444444444444444444444444444444444";
const HAND_MSG_HASH_HEX: &str = "bef51624d10e0bcd1f93e7edee27f832621d4aa59d91c611435898d92ddd3b9d";
const HAND_MSGS_ROOT_HEX: &str = "2e63c6aa14a2b13de273f57ae0e8071fcad84badcdbf0f1e0a7e597c7c49515e";
const HAND_HEADER_HEX: &str = "0000002aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa2e63c6aa14a2b13de273f57ae0e8071fcad84badcdbf0f1e0a7e597c7c49515ebbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb00000000000f4240ffffffffcccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc0000000100000001000000006553f100";
const HAND_HEADER_HASH_HEX: &str =
    "384ff6dd999455d3516c2a1cb76cf5f762188ebb317308de71a5eff1fe153f8a";

#[test]
fn hand_derived_msg_wire_bytes() {
    // Spec (ADR-0002): chain_id(32) || from(32) || nonce u64be(8) ||
    // kind(1) || to(32) || amount u128be(16) || fee u128be(16) ||
    // msg_len u32be(4) || message || pubkey(32), then signature(64):
    // 141 + 0 + 32 + 64 = 237 bytes for an empty-message transfer.
    let msg = hand_msg();
    assert_eq!(msg.to_bytes(), h(HAND_MSG_WIRE_HEX));
    assert_eq!(msg.body_bytes(), h(HAND_MSG_BODY_HEX));
    // Spot-check the layout by hand: chain_id first (new vs the V3
    // transaction era), kind byte at offset 72, pubkey before the
    // signature.
    let body = msg.body_bytes();
    assert_eq!(&body[0..32], &[0xDD; 32]); // chain_id
    assert_eq!(&body[32..64], &[0x11; 32]); // from
    assert_eq!(&body[64..72], &[0, 0, 0, 0, 0, 0, 0, 7]); // nonce = 7
    assert_eq!(body[72], 0x00); // kind = Transfer
    assert_eq!(&body[73..105], &[0x22; 32]); // to
    assert_eq!(&body[119..121], &[0x03, 0xe8]); // amount = 1000 (u128be tail)
    assert_eq!(&body[135..137], &[0x00, 0x0a]); // fee = 10 (u128be tail)
    assert_eq!(&body[137..141], &[0, 0, 0, 0]); // msg_len = 0
    assert_eq!(&body[141..173], &[0x44; 32]); // pubkey reveal field
}

#[test]
fn hand_derived_msg_hash() {
    // Spec: message identity = SHA256(pad32("ONX_MSG_EXT_V1") || wire_bytes).
    // Covers the signature too (237 bytes, not just the body).
    let msg = hand_msg();
    assert_eq!(msg.hash(), h32(HAND_MSG_HASH_HEX));
}

#[test]
fn hand_derived_msgs_root_single_msg() {
    // Spec: msgs_root = SHA256(pad32("ONX_MSGS_ROOT_V1") || concat(msg hashes)).
    let msg = hand_msg();
    assert_eq!(msgs_root(&[msg]), h32(HAND_MSGS_ROOT_HEX));
}

fn hand_header() -> BlockHeader {
    BlockHeader {
        seqno: 42,
        prev_hash: [0xAA; 32],
        msgs_root: h32(HAND_MSGS_ROOT_HEX),
        state_root: [0xBB; 32],
        lt: 1_000_000,
        workchain: -1, // masterchain; i32be(-1) = 0xFFFFFFFF
        fee_collector: AccountId::from_bytes([0xCC; 32]),
        msg_count: 1,
        // ONXBLK05 tail (ADR-0032): protocol_version u32be at [148..152],
        // block_time u64be at [152..160]. Human-chosen vector values
        // (1, 1_700_000_000); see hand_derive_onxblk05_header.py.
        protocol_version: 1,
        block_time: 1_700_000_000,
    }
}

#[test]
fn hand_derived_block_header_bytes() {
    // Spec: seqno u32be(4) || prev_hash(32) || msgs_root(32) ||
    // state_root(32) || lt u64be(8) || workchain i32be(4) ||
    // fee_collector(32) || msg_count u32be(4) ||
    // protocol_version u32be(4) || block_time u64be(8) = 160 bytes
    // (ONXBLK05, ADR-0032; was 148 before the tail was appended).
    let hdr = hand_header();
    let bytes = hdr.to_bytes();
    assert_eq!(bytes.len(), 160);
    assert_eq!(bytes, h(HAND_HEADER_HEX));
    // Spot-checks: seqno 42, workchain -1, msg_count 1, protocol_version 1,
    // block_time 1_700_000_000 (0x6553F100).
    assert_eq!(&bytes[0..4], &[0, 0, 0, 42]);
    assert_eq!(&bytes[108..112], &[0xFF, 0xFF, 0xFF, 0xFF]);
    assert_eq!(&bytes[144..148], &[0, 0, 0, 1]);
    assert_eq!(&bytes[148..152], &[0, 0, 0, 1]);
    assert_eq!(&bytes[152..160], &[0, 0, 0, 0, 0x65, 0x53, 0xF1, 0x00]);
}

#[test]
fn hand_derived_block_header_hash() {
    // Spec: header hash = SHA256(pad32("ONX_BLOCK_HDR_V1") || header_bytes).
    let hdr = hand_header();
    assert_eq!(hdr.hash(), h32(HAND_HEADER_HASH_HEX));
}

// Vector G: Ed25519 interop with an INDEPENDENT implementation.
// Seed 0x42*32 -> keypair via PyNaCl/libsodium (not the Rust code).
// Signature over pad32("ONX_MSG_EXT_SIGN_V1") || body_bytes (173 bytes).
const HAND_G_PUBKEY_HEX: &str = "2152f8d19b791d24453242e15f2eab6cb7cffa7b6a5ed30097960e069881db12";
const HAND_G_SIGNATURE_HEX: &str = "7d6851faa2de6347f59853c7410339664eaf111ba5e65a2417eeabfdfc0322087d66a04e55be8081afc8d78affa751c5f2b45c1c411839fc6d08b1d671ffa305";
const HAND_G_MSG_HASH_HEX: &str =
    "f9b035272494a9d7ecf577f0fb3a8b6ca3d97b1753c73dbeec90ac2859cde160";
const HAND_G_ADDRESS_HEX: &str = "82972854269005891682cfedf715bde97112f9fbe159238b4fe098c5b90c10e8";

#[test]
fn hand_derived_signature_verifies_across_implementations() {
    let mut msg = hand_msg();
    msg.signature = h64(HAND_G_SIGNATURE_HEX);
    let pubkey = PublicKey::decode_exact(&h(HAND_G_PUBKEY_HEX)).expect("valid pubkey");
    // The independently-produced signature MUST verify against the
    // implementation's own body_bytes and domain tag.
    msg.verify_signature(&pubkey)
        .expect("PyNaCl-produced signature must verify");
    // And the message hash commits to the wire carrying that signature.
    assert_eq!(msg.hash(), h32(HAND_G_MSG_HASH_HEX));
}

#[test]
fn hand_derived_signature_wrong_message_rejected() {
    // Same independent signature, but over a tampered body -> must fail.
    // (Proves the test above is actually checking the message binding,
    // not vacuously passing.)
    let mut msg = hand_msg();
    msg.signature = h64(HAND_G_SIGNATURE_HEX);
    msg.amount_nanos = 999; // tampered after signing
    let pubkey = PublicKey::decode_exact(&h(HAND_G_PUBKEY_HEX)).expect("valid pubkey");
    assert!(msg.verify_signature(&pubkey).is_err());
}

#[test]
fn hand_derived_address_matches_spec() {
    // Spec (ADR-0002/ADR-0006): address = SHA256(pad32("ONX_ADDR_V1") || pubkey).
    // Uses the independent Vector G pubkey, so the preimage is fixed by an
    // implementation the Rust code never touched.
    let pubkey: [u8; 32] = h(HAND_G_PUBKEY_HEX).try_into().unwrap();
    assert_eq!(
        derive_address(&pubkey),
        AccountId::from_bytes(h32(HAND_G_ADDRESS_HEX))
    );
}
