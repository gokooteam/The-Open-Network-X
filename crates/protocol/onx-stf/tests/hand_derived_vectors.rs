//! Hand-derived golden vectors for ONX transaction and block encodings.
//!
//! Every expected value below was computed INDEPENDENTLY of this Rust
//! implementation, straight from the spec documents:
//!   - `docs/specification/protocol-primitives.md` (SHA-256, big-endian ints,
//!     32-byte zero-padded domain tags)
//!   - `hidden_files/tx-auth-report.md` (V2 field layout — historical)
//!   - `hidden_files/tvm-integration-report.md` (ONX_TX_V3 field layout, domain tags)
//!   - `hidden_files/phase3-report.md` (block header field layout)
//!
//! Derivation script: `/tmp/hand_derive_vectors_v3.py` (reproducible;
//! V3 adaptation of `hidden_files/hand_derive_vectors.py`).
//! Parameters confirmed from code (parameters only, no logic copied):
//! SHA-256; `domain_hash(tag,msg)=SHA256(pad32(tag)||msg)`;
//! tags `ONX_TX_V3`, `ONX_TX_V3_SIGN`, `ONX_TXS_ROOT_V3`, `ONX_BLOCK_HDR_V1`;
//! `Transaction::hash()` covers the full wire (body || signature);
//! signature domain message is `pad32(tag) || body_bytes`.
//!
//! RULE: if any test here fails, INVESTIGATE. Do not "fix" the vector to
//! match the implementation — either the spec interpretation or the
//! implementation is wrong, and the report must say which.

use onx_data_structures::AccountId;
use onx_primitives::PublicKey;
use onx_stf::{
    block::{Transaction, TxKind},
    txs_root, BlockHeader,
};

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

// Vector A inputs (human-chosen): kind = Transfer(0), from = 0x11*32,
// to = 0x22*32, amount = 1000, fee = 10, nonce = 7, message empty,
// signature = 0x33*64 (pattern only, NOT a valid signature — encoding
// vector, not a signature vector).
fn hand_tx() -> Transaction {
    Transaction {
        kind: TxKind::Transfer,
        from: AccountId::from_bytes([0x11; 32]),
        to: AccountId::from_bytes([0x22; 32]),
        amount_nanos: 1000,
        fee_nanos: 10,
        nonce: 7,
        message: Vec::new(),
        signature: [0x33; 64],
    }
}

const HAND_TX_WIRE_HEX: &str = "0011111111111111111111111111111111111111111111111111111111111111112222222222222222222222222222222222222222222222222222222222222222000000000000000000000000000003e80000000000000000000000000000000a00000000000000070000000033333333333333333333333333333333333333333333333333333333333333333333333333333333333333333333333333333333333333333333333333333333";
const HAND_TX_BODY_HEX: &str = "0011111111111111111111111111111111111111111111111111111111111111112222222222222222222222222222222222222222222222222222222222222222000000000000000000000000000003e80000000000000000000000000000000a000000000000000700000000";
const HAND_TX_HASH_HEX: &str = "eeae569fd8af2db1a67850e40c005e2dc59938581085b4f4ea809b4653b2426f";
const HAND_TXS_ROOT_HEX: &str = "45e407c06e7363e68e7e8743cabaca8b4039bd5919c84493cc7c04421ac8815e";
const HAND_HEADER_HEX: &str = "0000002aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa45e407c06e7363e68e7e8743cabaca8b4039bd5919c84493cc7c04421ac8815ebbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb00000000000f4240ffffffffcccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc00000001";
const HAND_HEADER_HASH_HEX: &str =
    "d25809964169f44cdd812fe75087f5ee9c020951b0a3463d1d9464e32a50e979";

#[test]
fn hand_derived_tx_wire_bytes() {
    // Spec: kind(1) || from(32) || to(32) || amount u128be(16) ||
    //       fee u128be(16) || nonce u64be(8) || msg_len u32be(4) || msg ||
    //       signature(64) = 173 bytes for an empty message.
    let tx = hand_tx();
    assert_eq!(tx.to_bytes(), h(HAND_TX_WIRE_HEX));
    assert_eq!(tx.body_bytes(), h(HAND_TX_BODY_HEX));
    // Spot-check the layout by hand: kind byte first, then amount=1000
    // is 0x03e8 in the last two bytes of its 16-byte field (offset 65..81).
    let body = tx.body_bytes();
    assert_eq!(body[0], 0x00); // kind = Transfer
    assert_eq!(&body[79..81], &[0x03, 0xe8]); // amount = 1000
    assert_eq!(&body[95..97], &[0x00, 0x0a]); // fee = 10
    assert_eq!(&body[97..105], &[0, 0, 0, 0, 0, 0, 0, 7]); // nonce = 7
    assert_eq!(&body[105..109], &[0, 0, 0, 0]); // msg_len = 0
}

#[test]
fn hand_derived_tx_hash() {
    // Spec: tx identity = SHA256(pad32("ONX_TX_V3") || wire_bytes).
    // Covers the signature too (173 bytes, not just the body).
    let tx = hand_tx();
    assert_eq!(tx.hash(), h32(HAND_TX_HASH_HEX));
}

#[test]
fn hand_derived_txs_root_single_tx() {
    // Spec: txs_root = SHA256(pad32("ONX_TXS_ROOT_V3") || concat(tx hashes)).
    let tx = hand_tx();
    assert_eq!(txs_root(&[tx]), h32(HAND_TXS_ROOT_HEX));
}

fn hand_header() -> BlockHeader {
    BlockHeader {
        seqno: 42,
        prev_hash: [0xAA; 32],
        txs_root: h32(HAND_TXS_ROOT_HEX),
        state_root: [0xBB; 32],
        lt: 1_000_000,
        workchain: -1, // masterchain; i32be(-1) = 0xFFFFFFFF
        fee_collector: AccountId::from_bytes([0xCC; 32]),
        tx_count: 1,
    }
}

#[test]
fn hand_derived_block_header_bytes() {
    // Spec: seqno u32be(4) || prev_hash(32) || txs_root(32) ||
    // state_root(32) || lt u64be(8) || workchain i32be(4) ||
    // fee_collector(32) || tx_count u32be(4) = 148 bytes.
    let hdr = hand_header();
    let bytes = hdr.to_bytes();
    assert_eq!(bytes.len(), 148);
    assert_eq!(bytes, h(HAND_HEADER_HEX));
    // Spot-checks: seqno 42, workchain -1, tx_count 1.
    assert_eq!(&bytes[0..4], &[0, 0, 0, 42]);
    assert_eq!(&bytes[108..112], &[0xFF, 0xFF, 0xFF, 0xFF]);
    assert_eq!(&bytes[144..148], &[0, 0, 0, 1]);
}

#[test]
fn hand_derived_block_header_hash() {
    // Spec: header hash = SHA256(pad32("ONX_BLOCK_HDR_V1") || header_bytes).
    let hdr = hand_header();
    assert_eq!(hdr.hash(), h32(HAND_HEADER_HASH_HEX));
}

// Vector G: Ed25519 interop with an INDEPENDENT implementation.
// Seed 0x42*32 -> keypair via PyNaCl/libsodium (not the Rust code).
// Signature over pad32("ONX_TX_V3_SIGN") || body_bytes (141 bytes).
const HAND_G_PUBKEY_HEX: &str = "2152f8d19b791d24453242e15f2eab6cb7cffa7b6a5ed30097960e069881db12";
const HAND_G_SIGNATURE_HEX: &str = "8c5237d2432120d2f520c276f263f83c7b22135bacad8a650742d84b11326d2995da728d2b8d451c1fb38b345b6d17b66f85d69bc228f25176e2ff2cd1adbc0c";
const HAND_G_TX_HASH_HEX: &str = "c7cf89b03fc724cb65073820d4429e1f86b19c261b95fbaa07dd3724688eb98e";

#[test]
fn hand_derived_signature_verifies_across_implementations() {
    let mut tx = hand_tx();
    tx.signature = h64(HAND_G_SIGNATURE_HEX);
    let pubkey = PublicKey::decode_exact(&h(HAND_G_PUBKEY_HEX)).expect("valid pubkey");
    // The independently-produced signature MUST verify against the
    // implementation's own body_bytes and domain tag.
    tx.verify_signature(&pubkey)
        .expect("PyNaCl-produced signature must verify");
    // And the tx hash commits to the wire carrying that signature.
    assert_eq!(tx.hash(), h32(HAND_G_TX_HASH_HEX));
}

#[test]
fn hand_derived_signature_wrong_message_rejected() {
    // Same independent signature, but over a tampered body -> must fail.
    // (Proves the test above is actually checking the message binding,
    // not vacuously passing.)
    let mut tx = hand_tx();
    tx.signature = h64(HAND_G_SIGNATURE_HEX);
    tx.amount_nanos = 999; // tampered after signing
    let pubkey = PublicKey::decode_exact(&h(HAND_G_PUBKEY_HEX)).expect("valid pubkey");
    assert!(tx.verify_signature(&pubkey).is_err());
}
