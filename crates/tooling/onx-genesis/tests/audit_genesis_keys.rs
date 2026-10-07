//! AUDIT HARNESS — genesis key validation (validator + balance keys).
//!
//! Must-reject vectors (56): all 38 `y = p + k` encodings (24 decode
//! permissively, 14 are off-curve), 2 negative-zero, 8 torsion, 8 off-curve,
//! generated independently from the curve equations.
//!
//! Claims under test (branch description, commit 78c89c2):
//!   * explicit hex validator AND balance keys go through strict decoding;
//!   * label-derived dev keys stay accepted.
//!
//! Plus one RECOMMENDED-POLICY test (not a stated claim): strings that look
//! like hex key material but are not exactly 64 hex chars should be rejected
//! rather than silently re-interpreted as a dev label.

use onx_genesis::{build_genesis_document, GenesisConfig};

#[rustfmt::skip]
const MUST_REJECT: &[(&str, &str)] = &[
    ("torsion_order4_canonical", "0000000000000000000000000000000000000000000000000000000000000000"),
    ("torsion_order4_canonical", "0000000000000000000000000000000000000000000000000000000000000080"),
    ("torsion_order1_canonical", "0100000000000000000000000000000000000000000000000000000000000000"),
    ("torsion_order8_canonical", "26e8958fc2b227b045c3f489f2ef98f0d5dfac05d3c63339b13802886d53fc05"),
    ("torsion_order8_canonical", "26e8958fc2b227b045c3f489f2ef98f0d5dfac05d3c63339b13802886d53fc85"),
    ("torsion_order8_canonical", "c7176a703d4dd84fba3c0b760d10670f2a2053fa2c39ccc64ec7fd7792ac037a"),
    ("torsion_order8_canonical", "c7176a703d4dd84fba3c0b760d10670f2a2053fa2c39ccc64ec7fd7792ac03fa"),
    ("torsion_order2_canonical", "ecffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f"),
    ("noncanon_y_p_plus_0_sign0", "edffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f"),
    ("noncanon_y_p_plus_0_sign1", "edffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"),
    ("noncanon_y_p_plus_1_sign0", "eeffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f"),
    ("noncanon_y_p_plus_1_sign1", "eeffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"),
    ("noncanon_y_p_plus_2_sign0", "efffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f"),
    ("noncanon_y_p_plus_2_sign1", "efffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"),
    ("noncanon_y_p_plus_3_sign0", "f0ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f"),
    ("noncanon_y_p_plus_3_sign1", "f0ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"),
    ("noncanon_y_p_plus_4_sign0", "f1ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f"),
    ("noncanon_y_p_plus_4_sign1", "f1ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"),
    ("noncanon_y_p_plus_5_sign0", "f2ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f"),
    ("noncanon_y_p_plus_5_sign1", "f2ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"),
    ("noncanon_y_p_plus_6_sign0", "f3ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f"),
    ("noncanon_y_p_plus_6_sign1", "f3ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"),
    ("noncanon_y_p_plus_7_sign0", "f4ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f"),
    ("noncanon_y_p_plus_7_sign1", "f4ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"),
    ("noncanon_y_p_plus_8_sign0", "f5ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f"),
    ("noncanon_y_p_plus_8_sign1", "f5ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"),
    ("noncanon_y_p_plus_9_sign0", "f6ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f"),
    ("noncanon_y_p_plus_9_sign1", "f6ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"),
    ("noncanon_y_p_plus_10_sign0", "f7ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f"),
    ("noncanon_y_p_plus_10_sign1", "f7ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"),
    ("noncanon_y_p_plus_11_sign0", "f8ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f"),
    ("noncanon_y_p_plus_11_sign1", "f8ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"),
    ("noncanon_y_p_plus_12_sign0", "f9ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f"),
    ("noncanon_y_p_plus_12_sign1", "f9ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"),
    ("noncanon_y_p_plus_13_sign0", "faffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f"),
    ("noncanon_y_p_plus_13_sign1", "faffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"),
    ("noncanon_y_p_plus_14_sign0", "fbffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f"),
    ("noncanon_y_p_plus_14_sign1", "fbffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"),
    ("noncanon_y_p_plus_15_sign0", "fcffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f"),
    ("noncanon_y_p_plus_15_sign1", "fcffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"),
    ("noncanon_y_p_plus_16_sign0", "fdffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f"),
    ("noncanon_y_p_plus_16_sign1", "fdffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"),
    ("noncanon_y_p_plus_17_sign0", "feffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f"),
    ("noncanon_y_p_plus_17_sign1", "feffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"),
    ("noncanon_y_p_plus_18_sign0", "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f"),
    ("noncanon_y_p_plus_18_sign1", "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"),
    ("negzero_identity", "0100000000000000000000000000000000000000000000000000000000000080"),
    ("negzero_order2_0_minus1", "ecffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"),
    ("off_curve_0", "193f13fc3f7e7f71cd23d6549700377fb4c47476448c09528bc3fcef86c9361d"),
    ("off_curve_1", "5b7e91c5fcc5f787ee0dbfc8eba2dc6af59b3a64d66c0c24a19bf53af4633f99"),
    ("off_curve_2", "e1c30272c73d94f0512c878cc1206f7f859f889f7c7a4b41ea812035919cf036"),
    ("off_curve_3", "996a7ad95193443d604e77720b7fffddab965f05c87dc31307b068b8409a55f3"),
    ("off_curve_4", "80630428d540fea64d4edb6d57fe4875f45185a149a03dd1f00e8fdb1c871e24"),
    ("off_curve_5", "6b95904c993e2ad0b183d74a4ded4ff0590f05e694952a9d210280c522a77988"),
    ("off_curve_6", "b02bcb76ce533654641c6fed7cb080b8ddf0d377a58f135b66a614f00feeb424"),
    ("off_curve_7", "5571fc12ab10f29ccd2d0256744065e60b76398c64bcd8b180140f280778d5a1"),
];

#[rustfmt::skip]
const HONEST: &[(&str, &str)] = &[
    ("honest_0", "34b326904548be6a002eaee8d92de5d6f8d698d6808844ae0f5f289a43235e85"),
    ("honest_1", "677cec66b47d9e6c9ab2a89dde4f0467d5561ffab2368ffae08044029e9b175c"),
    ("honest_2", "38be668d64e080a310710573c07508dcc86ef98e175a27eed76f26e4d2a233d2"),
    ("honest_3", "e3a56ae1466f5f346fd51c5f57530e83bcd419bf1e93f2a711e67ba6bade5301"),
    ("honest_4", "894bdaedb0d06e7267d5dee2f1bf2d6ed07b5f14be64d781f61f8b2fd06db062"),
    ("honest_5", "75cc5464819b786f6822c9cab9d83c5f553678fa61dc19d00ed0b6f7fca7465b"),
    ("honest_6", "e77f0dcc607a28bff357f2ae86ba49e9d6c07a61c67dfa574b6e670cc2bb0e78"),
    ("honest_7", "ada48b3f6088766482815748a3ee26c30bcad0f662344284313ce4d6df748ebd"),
    ("honest_8", "ead63879badb1c5f643bbb40aa6885039ea21d123f85dfa0213fb3bc242812c1"),
    ("honest_9", "25837670d076944afc71a204a47d4d6779a3b28c2af6c837cfbbf91c6514ca02"),
    ("honest_10", "f5063dbce685819115d6935efb48ed7fd8c1abda0786f189bf5bc94e16f5a984"),
    ("honest_11", "5389a39089cbcc00825d1ee9af1e4e7bdf42c5100da3a570d14ffe7d8f0808ac"),
    ("honest_12", "08ba6645bff7eb9966e6e0290f758470a47c904834a454cb9a305bc17b3ab0f8"),
    ("honest_13", "fc95ec8e115daf5a250cb6e9f0c127aea456a1f8dd8b2adaef70b6e9d69e5862"),
    ("honest_14", "f5dd02f700f25b161113f8ab12a12f1972326cd912b2691e9108294439b76f3c"),
    ("honest_15", "236438c022801029fb03aa2d41bbdefa2bc7e603156206c27a3cd3f5362a97c6"),
    ("basepoint", "5866666666666666666666666666666666666666666666666666666666666666"),
];

fn unhex(s: &str) -> [u8; 32] {
    let mut out = [0u8; 32];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap();
    }
    out
}

fn with_validator_key(key: &str) -> GenesisConfig {
    let mut cfg = GenesisConfig::default();
    cfg.validators[0].public_key = key.to_string();
    cfg
}

fn with_balance_key(key: &str) -> GenesisConfig {
    let mut cfg = GenesisConfig::default();
    cfg.balances[0].public_key = Some(key.to_string());
    cfg
}

#[test]
fn explicit_hex_validator_keys_reject_every_bad_encoding() {
    let accepted: Vec<_> = MUST_REJECT
        .iter()
        .filter(|(_, h)| build_genesis_document(&with_validator_key(h)).is_ok())
        .map(|(l, _)| *l)
        .collect();
    assert!(
        accepted.is_empty(),
        "genesis ACCEPTED {}/{} bad explicit VALIDATOR keys: {:?}",
        accepted.len(),
        MUST_REJECT.len(),
        accepted
    );
}

#[test]
fn explicit_hex_balance_keys_reject_every_bad_encoding() {
    let accepted: Vec<_> = MUST_REJECT
        .iter()
        .filter(|(_, h)| build_genesis_document(&with_balance_key(h)).is_ok())
        .map(|(l, _)| *l)
        .collect();
    assert!(
        accepted.is_empty(),
        "genesis ACCEPTED {}/{} bad explicit BALANCE keys: {:?}",
        accepted.len(),
        MUST_REJECT.len(),
        accepted
    );
}

#[test]
fn explicit_hex_honest_keys_accepted_and_stored_verbatim() {
    for (l, h) in HONEST {
        let doc = build_genesis_document(&with_validator_key(h))
            .unwrap_or_else(|e| panic!("validator {l} rejected: {e}"));
        assert_eq!(
            doc.validators[0].pubkey,
            unhex(h),
            "validator {l} not stored verbatim"
        );
        build_genesis_document(&with_balance_key(h))
            .unwrap_or_else(|e| panic!("balance {l} rejected: {e}"));
        // Upper-case hex is the same key material.
        build_genesis_document(&with_validator_key(&h.to_uppercase()))
            .unwrap_or_else(|e| panic!("upper-case validator {l} rejected: {e}"));
    }
}

fn labels() -> impl Iterator<Item = String> {
    (0..256).map(|i| format!("dev-key-{i:03}"))
}

#[test]
fn label_derived_validator_keys_stay_accepted() {
    let rejected: Vec<_> = labels()
        .filter(|l| build_genesis_document(&with_validator_key(l)).is_err())
        .collect();
    assert!(
        rejected.is_empty(),
        "{} of 256 validator labels rejected: {:?}",
        rejected.len(),
        &rejected[..rejected.len().min(8)]
    );
}

#[test]
fn label_derived_balance_keys_are_validated() {
    // F4: balance keys clear `decode_exact` again, as on main — a
    // label-derived key whose domain-hash output is off-curve is rejected,
    // an on-curve one accepted. (Roughly half the labels land off-curve;
    // main rejected exactly those.) Validator labels stay unvalidated
    // DEV-only; see `label_derived_validator_keys_stay_accepted`.
    use onx_primitives::PublicKey;
    use onx_state_model::derive_validator_pubkey;
    let mut accepted = 0;
    let mut rejected = 0;
    for l in labels() {
        let bytes = derive_validator_pubkey(&l);
        let expect_ok = PublicKey::decode_exact(&bytes).is_ok();
        let got_ok = build_genesis_document(&with_balance_key(&l)).is_ok();
        assert_eq!(
            got_ok, expect_ok,
            "label {l:?}: decode_exact says {expect_ok}, builder says {got_ok}"
        );
        if got_ok {
            accepted += 1;
        } else {
            rejected += 1;
        }
    }
    assert!(
        accepted > 0 && rejected > 0,
        "expected a mix of on/off-curve labels, got {accepted} accepted / {rejected} rejected"
    );
}

#[test]
fn recommended_policy_near_hex_strings_are_not_silently_derived() {
    let h = HONEST[0].1;
    let near_misses = [
        format!("0x{h}"),    // 66 chars: common prefix
        h[..63].to_string(), // 63 chars: dropped digit
        format!("{h}0"),     // 65 chars: extra digit
        format!(" {h}"),     // 65 chars: stray whitespace
        h[..62].to_string(), // 62 chars
    ];
    let silently_derived: Vec<_> = near_misses
        .iter()
        .filter(|s| build_genesis_document(&with_validator_key(s)).is_ok())
        .map(|s| format!("{}..({} chars)", &s[..6], s.len()))
        .collect();
    assert!(
        silently_derived.is_empty(),
        "near-hex validator keys silently became DEV LABELS (validator with no known private key): {silently_derived:?}"
    );
}
