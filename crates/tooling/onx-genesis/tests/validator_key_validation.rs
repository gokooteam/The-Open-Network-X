//! Validator-key validation at genesis build time.
//!
//! Explicit 64-hex-char validator keys are real key material, so
//! `build_genesis_document` must hold them to the strict predicate
//! (canonical encoding, on-curve, large-order — the `verify_strict` bar).
//! Label-derived keys are DEV-only (no known private key, can never sign)
//! and keep passing through.

use onx_genesis::{build_genesis_document, Balance, GenesisConfig, Validator, Workchain};

/// Valid Ed25519 public key, derived from seed `[7u8; 32]`
/// (`SecretKey::from_seed(&[7; 32]).public_key().encode()`).
const VALID_KEY_HEX: &str = "ea4a6c63e29c520abef5507b132ec5f9954776aebebe7b92421eea691446d22c";
/// Identity point: canonical on-curve encoding, order 1.
const IDENTITY_KEY_HEX: &str = "0100000000000000000000000000000000000000000000000000000000000000";
/// All-zeros: the order-4 point; canonical and on-curve.
const ORDER_FOUR_KEY_HEX: &str = "0000000000000000000000000000000000000000000000000000000000000000";
/// y = 2: canonical encoding that fails the curve equation.
const OFF_CURVE_KEY_HEX: &str = "0200000000000000000000000000000000000000000000000000000000000000";

fn config_with_validator_key(key: &str) -> GenesisConfig {
    GenesisConfig {
        balances: vec![Balance {
            address: "onx:alice".to_string(),
            amount: 1_000_000,
            public_key: None,
            code_hex: None,
            data_hex: None,
        }],
        validators: vec![Validator {
            public_key: key.to_string(),
            stake: 1_000,
        }],
        workchains: vec![Workchain {
            id: -1,
            name: "masterchain".to_string(),
            enabled: true,
        }],
    }
}

#[test]
fn accepts_valid_hex_validator_key() {
    let cfg = config_with_validator_key(VALID_KEY_HEX);
    let doc = build_genesis_document(&cfg).expect("valid key must build");
    assert_eq!(doc.validators.len(), 1);
    assert_eq!(doc.validators[0].stake, 1_000);
}

#[test]
fn accepts_uppercase_hex_validator_key() {
    let cfg = config_with_validator_key(&VALID_KEY_HEX.to_uppercase());
    build_genesis_document(&cfg).expect("uppercase hex is valid key material");
}

#[test]
fn accepts_label_derived_dev_validator_key() {
    // DEV path: derived keys have no known private key and can never sign,
    // so they are not subject to the strict predicate.
    let cfg = config_with_validator_key("validator-01");
    build_genesis_document(&cfg).expect("label-derived dev key must keep working");
}

#[test]
fn rejects_identity_point_validator_key() {
    let err = build_genesis_document(&config_with_validator_key(IDENTITY_KEY_HEX))
        .expect_err("identity point must be rejected");
    assert!(
        err.contains("validator #0") && err.contains("large-order"),
        "error must name the validator and the rule, got: {err}"
    );
}

#[test]
fn rejects_small_order_validator_key() {
    let err = build_genesis_document(&config_with_validator_key(ORDER_FOUR_KEY_HEX))
        .expect_err("order-4 (all-zeros) point must be rejected");
    assert!(
        err.contains("validator #0") && err.contains("large-order"),
        "error must name the validator and the rule, got: {err}"
    );
}

#[test]
fn rejects_off_curve_validator_key() {
    let err = build_genesis_document(&config_with_validator_key(OFF_CURVE_KEY_HEX))
        .expect_err("off-curve key must be rejected");
    assert!(
        err.contains("validator #0") && err.contains("canonical, on-curve"),
        "error must name the validator and the rule, got: {err}"
    );
}

#[test]
fn rejects_64_char_non_hex_validator_key() {
    // The three-way rule treats 64-char non-hex as a probable typo.
    let bad = "zz".repeat(32);
    let err = build_genesis_document(&config_with_validator_key(&bad))
        .expect_err("64-char non-hex must be rejected");
    assert!(err.contains("not valid hex"), "got: {err}");
}

#[test]
fn validator_index_in_error_names_the_culprit() {
    let cfg = GenesisConfig {
        balances: vec![Balance {
            address: "onx:alice".to_string(),
            amount: 1_000_000,
            public_key: None,
            code_hex: None,
            data_hex: None,
        }],
        validators: vec![
            Validator {
                public_key: VALID_KEY_HEX.to_string(),
                stake: 1_000,
            },
            Validator {
                public_key: IDENTITY_KEY_HEX.to_string(),
                stake: 1_000,
            },
        ],
        workchains: vec![Workchain {
            id: -1,
            name: "masterchain".to_string(),
            enabled: true,
        }],
    };
    let err = build_genesis_document(&cfg).expect_err("second key must be rejected");
    assert!(err.contains("validator #1"), "got: {err}");
}

fn config_with_balance_key(key: Option<&str>) -> GenesisConfig {
    GenesisConfig {
        balances: vec![Balance {
            address: "onx:alice".to_string(),
            amount: 1_000_000,
            public_key: key.map(str::to_string),
            code_hex: None,
            data_hex: None,
        }],
        validators: vec![Validator {
            public_key: "validator-01".to_string(),
            stake: 1_000,
        }],
        workchains: vec![Workchain {
            id: -1,
            name: "masterchain".to_string(),
            enabled: true,
        }],
    }
}

#[test]
fn accepts_valid_hex_balance_key() {
    let cfg = config_with_balance_key(Some(VALID_KEY_HEX));
    build_genesis_document(&cfg).expect("valid balance key must build");
}

#[test]
fn rejects_small_order_balance_key() {
    // A small-order balance key would lock funds forever: unspendable
    // under `verify_strict`, so genesis must refuse to create it.
    for (name, key) in [
        ("identity", IDENTITY_KEY_HEX),
        ("order-4", ORDER_FOUR_KEY_HEX),
        ("off-curve", OFF_CURVE_KEY_HEX),
    ] {
        let err = build_genesis_document(&config_with_balance_key(Some(key)))
            .expect_err(&format!("{name} balance key must be rejected"));
        assert!(
            err.contains("onx:alice") && err.contains("large-order"),
            "error must name the balance and the rule, got: {err}"
        );
    }
}

#[test]
fn accepts_label_derived_dev_balance_key() {
    let cfg = config_with_balance_key(Some("onx:alice-key"));
    build_genesis_document(&cfg).expect("label-derived dev balance key must keep working");
}

/// y = 3 is on-curve; this is its NON-canonical `y + p` encoding
/// (p = 2^255 - 19, so `y + p` LE = `ef ff .. ff 7f`). It decodes to a
/// valid large-order point but must be rejected: one point, one encoding.
const NONCANONICAL_YP_KEY_HEX: &str =
    "efffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f";

#[test]
fn rejects_noncanonical_yp_validator_key() {
    // The hole this closes: the `y + p` encoding passed both decompress
    // and the small-order check, so the old "canonical + on-curve +
    // large-order" predicate let it through.
    let err = build_genesis_document(&config_with_validator_key(NONCANONICAL_YP_KEY_HEX))
        .expect_err("non-canonical y+p validator key must be rejected");
    assert!(
        err.contains("validator #0") && err.contains("canonical"),
        "error must name the validator and the rule, got: {err}"
    );
}

#[test]
fn rejects_noncanonical_yp_balance_key() {
    let err = build_genesis_document(&config_with_balance_key(Some(NONCANONICAL_YP_KEY_HEX)))
        .expect_err("non-canonical y+p balance key must be rejected");
    assert!(
        err.contains("onx:alice") && err.contains("canonical"),
        "error must name the balance and the rule, got: {err}"
    );
}
