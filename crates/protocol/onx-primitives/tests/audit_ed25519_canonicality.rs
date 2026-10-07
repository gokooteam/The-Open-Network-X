//! AUDIT HARNESS — Ed25519 public-key canonicality at `PublicKey::decode_exact`.
//!
//! Fixed vectors were generated independently from the curve equations
//! (RFC 8032 §5.1.3) by `ed25519_groundtruth.py`, not from this crate or
//! from dalek. They are the complete set of encodings that a permissive
//! decoder (curve25519-dalek's `decompress`) accepts but a strict decoder
//! must reject: 24 non-canonical `y = p + k` encodings, 2 "negative zero"
//! encodings, and the 8 canonical torsion encodings — plus mixed-order,
//! honest and off-curve controls.
//!
//! Expected on `main` (lenient decode): the rejection tests FAIL.
//! Expected on a correct strict branch: every test PASSES.
//!
//! Policy encoded here (from the branch description): reject non-canonical
//! encodings and small-order points; mixed-order keys are reported, not
//! asserted, because "small-order" and "torsion-free" are different rules.

use ed25519_dalek::VerifyingKey;
use onx_primitives::PublicKey;

struct V {
    label: &'static str,
    hex: &'static str,
    category: &'static str,
    permissive_decodes: bool,
    small_order: bool,
    torsion_free: bool,
    canonical_hex: Option<&'static str>,
}

#[rustfmt::skip]
const VECTORS: &[V] = &[
    V { label: "torsion_order4_canonical", hex: "0000000000000000000000000000000000000000000000000000000000000000", category: "torsion_canonical", permissive_decodes: true, small_order: true, torsion_free: false, canonical_hex: Some("0000000000000000000000000000000000000000000000000000000000000000") },
    V { label: "torsion_order4_canonical", hex: "0000000000000000000000000000000000000000000000000000000000000080", category: "torsion_canonical", permissive_decodes: true, small_order: true, torsion_free: false, canonical_hex: Some("0000000000000000000000000000000000000000000000000000000000000080") },
    V { label: "torsion_order1_canonical", hex: "0100000000000000000000000000000000000000000000000000000000000000", category: "torsion_canonical", permissive_decodes: true, small_order: true, torsion_free: true, canonical_hex: Some("0100000000000000000000000000000000000000000000000000000000000000") },
    V { label: "torsion_order8_canonical", hex: "26e8958fc2b227b045c3f489f2ef98f0d5dfac05d3c63339b13802886d53fc05", category: "torsion_canonical", permissive_decodes: true, small_order: true, torsion_free: false, canonical_hex: Some("26e8958fc2b227b045c3f489f2ef98f0d5dfac05d3c63339b13802886d53fc05") },
    V { label: "torsion_order8_canonical", hex: "26e8958fc2b227b045c3f489f2ef98f0d5dfac05d3c63339b13802886d53fc85", category: "torsion_canonical", permissive_decodes: true, small_order: true, torsion_free: false, canonical_hex: Some("26e8958fc2b227b045c3f489f2ef98f0d5dfac05d3c63339b13802886d53fc85") },
    V { label: "torsion_order8_canonical", hex: "c7176a703d4dd84fba3c0b760d10670f2a2053fa2c39ccc64ec7fd7792ac037a", category: "torsion_canonical", permissive_decodes: true, small_order: true, torsion_free: false, canonical_hex: Some("c7176a703d4dd84fba3c0b760d10670f2a2053fa2c39ccc64ec7fd7792ac037a") },
    V { label: "torsion_order8_canonical", hex: "c7176a703d4dd84fba3c0b760d10670f2a2053fa2c39ccc64ec7fd7792ac03fa", category: "torsion_canonical", permissive_decodes: true, small_order: true, torsion_free: false, canonical_hex: Some("c7176a703d4dd84fba3c0b760d10670f2a2053fa2c39ccc64ec7fd7792ac03fa") },
    V { label: "torsion_order2_canonical", hex: "ecffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f", category: "torsion_canonical", permissive_decodes: true, small_order: true, torsion_free: false, canonical_hex: Some("ecffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f") },
    V { label: "noncanon_y_p_plus_0_sign0", hex: "edffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f", category: "noncanonical_y", permissive_decodes: true, small_order: true, torsion_free: false, canonical_hex: Some("0000000000000000000000000000000000000000000000000000000000000000") },
    V { label: "noncanon_y_p_plus_0_sign1", hex: "edffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff", category: "noncanonical_y", permissive_decodes: true, small_order: true, torsion_free: false, canonical_hex: Some("0000000000000000000000000000000000000000000000000000000000000080") },
    V { label: "noncanon_y_p_plus_1_sign0", hex: "eeffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f", category: "noncanonical_y", permissive_decodes: true, small_order: true, torsion_free: true, canonical_hex: Some("0100000000000000000000000000000000000000000000000000000000000000") },
    V { label: "noncanon_y_p_plus_1_sign1", hex: "eeffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff", category: "noncanonical_y", permissive_decodes: true, small_order: true, torsion_free: true, canonical_hex: Some("0100000000000000000000000000000000000000000000000000000000000000") },
    V { label: "noncanon_y_p_plus_2_sign0", hex: "efffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f", category: "noncanonical_y", permissive_decodes: false, small_order: false, torsion_free: false, canonical_hex: None },
    V { label: "noncanon_y_p_plus_2_sign1", hex: "efffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff", category: "noncanonical_y", permissive_decodes: false, small_order: false, torsion_free: false, canonical_hex: None },
    V { label: "noncanon_y_p_plus_3_sign0", hex: "f0ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f", category: "noncanonical_y", permissive_decodes: true, small_order: false, torsion_free: false, canonical_hex: Some("0300000000000000000000000000000000000000000000000000000000000000") },
    V { label: "noncanon_y_p_plus_3_sign1", hex: "f0ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff", category: "noncanonical_y", permissive_decodes: true, small_order: false, torsion_free: false, canonical_hex: Some("0300000000000000000000000000000000000000000000000000000000000080") },
    V { label: "noncanon_y_p_plus_4_sign0", hex: "f1ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f", category: "noncanonical_y", permissive_decodes: true, small_order: false, torsion_free: false, canonical_hex: Some("0400000000000000000000000000000000000000000000000000000000000000") },
    V { label: "noncanon_y_p_plus_4_sign1", hex: "f1ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff", category: "noncanonical_y", permissive_decodes: true, small_order: false, torsion_free: false, canonical_hex: Some("0400000000000000000000000000000000000000000000000000000000000080") },
    V { label: "noncanon_y_p_plus_5_sign0", hex: "f2ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f", category: "noncanonical_y", permissive_decodes: true, small_order: false, torsion_free: false, canonical_hex: Some("0500000000000000000000000000000000000000000000000000000000000000") },
    V { label: "noncanon_y_p_plus_5_sign1", hex: "f2ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff", category: "noncanonical_y", permissive_decodes: true, small_order: false, torsion_free: false, canonical_hex: Some("0500000000000000000000000000000000000000000000000000000000000080") },
    V { label: "noncanon_y_p_plus_6_sign0", hex: "f3ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f", category: "noncanonical_y", permissive_decodes: true, small_order: false, torsion_free: false, canonical_hex: Some("0600000000000000000000000000000000000000000000000000000000000000") },
    V { label: "noncanon_y_p_plus_6_sign1", hex: "f3ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff", category: "noncanonical_y", permissive_decodes: true, small_order: false, torsion_free: false, canonical_hex: Some("0600000000000000000000000000000000000000000000000000000000000080") },
    V { label: "noncanon_y_p_plus_7_sign0", hex: "f4ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f", category: "noncanonical_y", permissive_decodes: false, small_order: false, torsion_free: false, canonical_hex: None },
    V { label: "noncanon_y_p_plus_7_sign1", hex: "f4ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff", category: "noncanonical_y", permissive_decodes: false, small_order: false, torsion_free: false, canonical_hex: None },
    V { label: "noncanon_y_p_plus_8_sign0", hex: "f5ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f", category: "noncanonical_y", permissive_decodes: false, small_order: false, torsion_free: false, canonical_hex: None },
    V { label: "noncanon_y_p_plus_8_sign1", hex: "f5ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff", category: "noncanonical_y", permissive_decodes: false, small_order: false, torsion_free: false, canonical_hex: None },
    V { label: "noncanon_y_p_plus_9_sign0", hex: "f6ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f", category: "noncanonical_y", permissive_decodes: true, small_order: false, torsion_free: false, canonical_hex: Some("0900000000000000000000000000000000000000000000000000000000000000") },
    V { label: "noncanon_y_p_plus_9_sign1", hex: "f6ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff", category: "noncanonical_y", permissive_decodes: true, small_order: false, torsion_free: false, canonical_hex: Some("0900000000000000000000000000000000000000000000000000000000000080") },
    V { label: "noncanon_y_p_plus_10_sign0", hex: "f7ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f", category: "noncanonical_y", permissive_decodes: true, small_order: false, torsion_free: false, canonical_hex: Some("0a00000000000000000000000000000000000000000000000000000000000000") },
    V { label: "noncanon_y_p_plus_10_sign1", hex: "f7ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff", category: "noncanonical_y", permissive_decodes: true, small_order: false, torsion_free: false, canonical_hex: Some("0a00000000000000000000000000000000000000000000000000000000000080") },
    V { label: "noncanon_y_p_plus_11_sign0", hex: "f8ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f", category: "noncanonical_y", permissive_decodes: false, small_order: false, torsion_free: false, canonical_hex: None },
    V { label: "noncanon_y_p_plus_11_sign1", hex: "f8ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff", category: "noncanonical_y", permissive_decodes: false, small_order: false, torsion_free: false, canonical_hex: None },
    V { label: "noncanon_y_p_plus_12_sign0", hex: "f9ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f", category: "noncanonical_y", permissive_decodes: false, small_order: false, torsion_free: false, canonical_hex: None },
    V { label: "noncanon_y_p_plus_12_sign1", hex: "f9ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff", category: "noncanonical_y", permissive_decodes: false, small_order: false, torsion_free: false, canonical_hex: None },
    V { label: "noncanon_y_p_plus_13_sign0", hex: "faffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f", category: "noncanonical_y", permissive_decodes: false, small_order: false, torsion_free: false, canonical_hex: None },
    V { label: "noncanon_y_p_plus_13_sign1", hex: "faffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff", category: "noncanonical_y", permissive_decodes: false, small_order: false, torsion_free: false, canonical_hex: None },
    V { label: "noncanon_y_p_plus_14_sign0", hex: "fbffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f", category: "noncanonical_y", permissive_decodes: true, small_order: false, torsion_free: false, canonical_hex: Some("0e00000000000000000000000000000000000000000000000000000000000000") },
    V { label: "noncanon_y_p_plus_14_sign1", hex: "fbffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff", category: "noncanonical_y", permissive_decodes: true, small_order: false, torsion_free: false, canonical_hex: Some("0e00000000000000000000000000000000000000000000000000000000000080") },
    V { label: "noncanon_y_p_plus_15_sign0", hex: "fcffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f", category: "noncanonical_y", permissive_decodes: true, small_order: false, torsion_free: false, canonical_hex: Some("0f00000000000000000000000000000000000000000000000000000000000000") },
    V { label: "noncanon_y_p_plus_15_sign1", hex: "fcffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff", category: "noncanonical_y", permissive_decodes: true, small_order: false, torsion_free: false, canonical_hex: Some("0f00000000000000000000000000000000000000000000000000000000000080") },
    V { label: "noncanon_y_p_plus_16_sign0", hex: "fdffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f", category: "noncanonical_y", permissive_decodes: true, small_order: false, torsion_free: false, canonical_hex: Some("1000000000000000000000000000000000000000000000000000000000000000") },
    V { label: "noncanon_y_p_plus_16_sign1", hex: "fdffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff", category: "noncanonical_y", permissive_decodes: true, small_order: false, torsion_free: false, canonical_hex: Some("1000000000000000000000000000000000000000000000000000000000000080") },
    V { label: "noncanon_y_p_plus_17_sign0", hex: "feffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f", category: "noncanonical_y", permissive_decodes: false, small_order: false, torsion_free: false, canonical_hex: None },
    V { label: "noncanon_y_p_plus_17_sign1", hex: "feffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff", category: "noncanonical_y", permissive_decodes: false, small_order: false, torsion_free: false, canonical_hex: None },
    V { label: "noncanon_y_p_plus_18_sign0", hex: "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f", category: "noncanonical_y", permissive_decodes: true, small_order: false, torsion_free: false, canonical_hex: Some("1200000000000000000000000000000000000000000000000000000000000000") },
    V { label: "noncanon_y_p_plus_18_sign1", hex: "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff", category: "noncanonical_y", permissive_decodes: true, small_order: false, torsion_free: false, canonical_hex: Some("1200000000000000000000000000000000000000000000000000000000000080") },
    V { label: "negzero_identity", hex: "0100000000000000000000000000000000000000000000000000000000000080", category: "negative_zero_x", permissive_decodes: true, small_order: true, torsion_free: true, canonical_hex: Some("0100000000000000000000000000000000000000000000000000000000000000") },
    V { label: "negzero_order2_0_minus1", hex: "ecffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff", category: "negative_zero_x", permissive_decodes: true, small_order: true, torsion_free: false, canonical_hex: Some("ecffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f") },
    V { label: "mixed_order_A_plus_1T8", hex: "758e4324880bea2d4aec816559f5ae9b3e54d29d6f083bdcda9c5538c94238d2", category: "mixed_order", permissive_decodes: true, small_order: false, torsion_free: false, canonical_hex: Some("758e4324880bea2d4aec816559f5ae9b3e54d29d6f083bdcda9c5538c94238d2") },
    V { label: "mixed_order_A_plus_2T8", hex: "f6492735d1792f56599a477fd11474a287e8919f6aa44c9be1cfa6197b08f845", category: "mixed_order", permissive_decodes: true, small_order: false, torsion_free: false, canonical_hex: Some("f6492735d1792f56599a477fd11474a287e8919f6aa44c9be1cfa6197b08f845") },
    V { label: "mixed_order_A_plus_3T8", hex: "8aee663d5f54fd138481ca26d3117ff47e5a927029641afb4bba95d44faec8fa", category: "mixed_order", permissive_decodes: true, small_order: false, torsion_free: false, canonical_hex: Some("8aee663d5f54fd138481ca26d3117ff47e5a927029641afb4bba95d44faec8fa") },
    V { label: "mixed_order_A_plus_4T8", hex: "010c4a11965beaee632887f72a3375975aae6c45383c28d23c1030dc3ab8cdd0", category: "mixed_order", permissive_decodes: true, small_order: false, torsion_free: false, canonical_hex: Some("010c4a11965beaee632887f72a3375975aae6c45383c28d23c1030dc3ab8cdd0") },
    V { label: "mixed_order_A_plus_5T8", hex: "7871bcdb77f415d2b5137e9aa60a5164c1ab2d6290f7c4232563aac736bdc72d", category: "mixed_order", permissive_decodes: true, small_order: false, torsion_free: false, canonical_hex: Some("7871bcdb77f415d2b5137e9aa60a5164c1ab2d6290f7c4232563aac736bdc72d") },
    V { label: "mixed_order_A_plus_6T8", hex: "f7b5d8ca2e86d0a9a665b8802eeb8b5d78176e60955bb3641e3059e684f707ba", category: "mixed_order", permissive_decodes: true, small_order: false, torsion_free: false, canonical_hex: Some("f7b5d8ca2e86d0a9a665b8802eeb8b5d78176e60955bb3641e3059e684f707ba") },
    V { label: "mixed_order_A_plus_7T8", hex: "631199c2a0ab02ec7b7e35d92cee800b81a56d8fd69be504b4456a2bb0513705", category: "mixed_order", permissive_decodes: true, small_order: false, torsion_free: false, canonical_hex: Some("631199c2a0ab02ec7b7e35d92cee800b81a56d8fd69be504b4456a2bb0513705") },
    V { label: "honest_0", hex: "34b326904548be6a002eaee8d92de5d6f8d698d6808844ae0f5f289a43235e85", category: "honest", permissive_decodes: true, small_order: false, torsion_free: true, canonical_hex: Some("34b326904548be6a002eaee8d92de5d6f8d698d6808844ae0f5f289a43235e85") },
    V { label: "honest_1", hex: "677cec66b47d9e6c9ab2a89dde4f0467d5561ffab2368ffae08044029e9b175c", category: "honest", permissive_decodes: true, small_order: false, torsion_free: true, canonical_hex: Some("677cec66b47d9e6c9ab2a89dde4f0467d5561ffab2368ffae08044029e9b175c") },
    V { label: "honest_2", hex: "38be668d64e080a310710573c07508dcc86ef98e175a27eed76f26e4d2a233d2", category: "honest", permissive_decodes: true, small_order: false, torsion_free: true, canonical_hex: Some("38be668d64e080a310710573c07508dcc86ef98e175a27eed76f26e4d2a233d2") },
    V { label: "honest_3", hex: "e3a56ae1466f5f346fd51c5f57530e83bcd419bf1e93f2a711e67ba6bade5301", category: "honest", permissive_decodes: true, small_order: false, torsion_free: true, canonical_hex: Some("e3a56ae1466f5f346fd51c5f57530e83bcd419bf1e93f2a711e67ba6bade5301") },
    V { label: "honest_4", hex: "894bdaedb0d06e7267d5dee2f1bf2d6ed07b5f14be64d781f61f8b2fd06db062", category: "honest", permissive_decodes: true, small_order: false, torsion_free: true, canonical_hex: Some("894bdaedb0d06e7267d5dee2f1bf2d6ed07b5f14be64d781f61f8b2fd06db062") },
    V { label: "honest_5", hex: "75cc5464819b786f6822c9cab9d83c5f553678fa61dc19d00ed0b6f7fca7465b", category: "honest", permissive_decodes: true, small_order: false, torsion_free: true, canonical_hex: Some("75cc5464819b786f6822c9cab9d83c5f553678fa61dc19d00ed0b6f7fca7465b") },
    V { label: "honest_6", hex: "e77f0dcc607a28bff357f2ae86ba49e9d6c07a61c67dfa574b6e670cc2bb0e78", category: "honest", permissive_decodes: true, small_order: false, torsion_free: true, canonical_hex: Some("e77f0dcc607a28bff357f2ae86ba49e9d6c07a61c67dfa574b6e670cc2bb0e78") },
    V { label: "honest_7", hex: "ada48b3f6088766482815748a3ee26c30bcad0f662344284313ce4d6df748ebd", category: "honest", permissive_decodes: true, small_order: false, torsion_free: true, canonical_hex: Some("ada48b3f6088766482815748a3ee26c30bcad0f662344284313ce4d6df748ebd") },
    V { label: "honest_8", hex: "ead63879badb1c5f643bbb40aa6885039ea21d123f85dfa0213fb3bc242812c1", category: "honest", permissive_decodes: true, small_order: false, torsion_free: true, canonical_hex: Some("ead63879badb1c5f643bbb40aa6885039ea21d123f85dfa0213fb3bc242812c1") },
    V { label: "honest_9", hex: "25837670d076944afc71a204a47d4d6779a3b28c2af6c837cfbbf91c6514ca02", category: "honest", permissive_decodes: true, small_order: false, torsion_free: true, canonical_hex: Some("25837670d076944afc71a204a47d4d6779a3b28c2af6c837cfbbf91c6514ca02") },
    V { label: "honest_10", hex: "f5063dbce685819115d6935efb48ed7fd8c1abda0786f189bf5bc94e16f5a984", category: "honest", permissive_decodes: true, small_order: false, torsion_free: true, canonical_hex: Some("f5063dbce685819115d6935efb48ed7fd8c1abda0786f189bf5bc94e16f5a984") },
    V { label: "honest_11", hex: "5389a39089cbcc00825d1ee9af1e4e7bdf42c5100da3a570d14ffe7d8f0808ac", category: "honest", permissive_decodes: true, small_order: false, torsion_free: true, canonical_hex: Some("5389a39089cbcc00825d1ee9af1e4e7bdf42c5100da3a570d14ffe7d8f0808ac") },
    V { label: "honest_12", hex: "08ba6645bff7eb9966e6e0290f758470a47c904834a454cb9a305bc17b3ab0f8", category: "honest", permissive_decodes: true, small_order: false, torsion_free: true, canonical_hex: Some("08ba6645bff7eb9966e6e0290f758470a47c904834a454cb9a305bc17b3ab0f8") },
    V { label: "honest_13", hex: "fc95ec8e115daf5a250cb6e9f0c127aea456a1f8dd8b2adaef70b6e9d69e5862", category: "honest", permissive_decodes: true, small_order: false, torsion_free: true, canonical_hex: Some("fc95ec8e115daf5a250cb6e9f0c127aea456a1f8dd8b2adaef70b6e9d69e5862") },
    V { label: "honest_14", hex: "f5dd02f700f25b161113f8ab12a12f1972326cd912b2691e9108294439b76f3c", category: "honest", permissive_decodes: true, small_order: false, torsion_free: true, canonical_hex: Some("f5dd02f700f25b161113f8ab12a12f1972326cd912b2691e9108294439b76f3c") },
    V { label: "honest_15", hex: "236438c022801029fb03aa2d41bbdefa2bc7e603156206c27a3cd3f5362a97c6", category: "honest", permissive_decodes: true, small_order: false, torsion_free: true, canonical_hex: Some("236438c022801029fb03aa2d41bbdefa2bc7e603156206c27a3cd3f5362a97c6") },
    V { label: "basepoint", hex: "5866666666666666666666666666666666666666666666666666666666666666", category: "honest", permissive_decodes: true, small_order: false, torsion_free: true, canonical_hex: Some("5866666666666666666666666666666666666666666666666666666666666666") },
    V { label: "off_curve_0", hex: "193f13fc3f7e7f71cd23d6549700377fb4c47476448c09528bc3fcef86c9361d", category: "off_curve", permissive_decodes: false, small_order: false, torsion_free: false, canonical_hex: None },
    V { label: "off_curve_1", hex: "5b7e91c5fcc5f787ee0dbfc8eba2dc6af59b3a64d66c0c24a19bf53af4633f99", category: "off_curve", permissive_decodes: false, small_order: false, torsion_free: false, canonical_hex: None },
    V { label: "off_curve_2", hex: "e1c30272c73d94f0512c878cc1206f7f859f889f7c7a4b41ea812035919cf036", category: "off_curve", permissive_decodes: false, small_order: false, torsion_free: false, canonical_hex: None },
    V { label: "off_curve_3", hex: "996a7ad95193443d604e77720b7fffddab965f05c87dc31307b068b8409a55f3", category: "off_curve", permissive_decodes: false, small_order: false, torsion_free: false, canonical_hex: None },
    V { label: "off_curve_4", hex: "80630428d540fea64d4edb6d57fe4875f45185a149a03dd1f00e8fdb1c871e24", category: "off_curve", permissive_decodes: false, small_order: false, torsion_free: false, canonical_hex: None },
    V { label: "off_curve_5", hex: "6b95904c993e2ad0b183d74a4ded4ff0590f05e694952a9d210280c522a77988", category: "off_curve", permissive_decodes: false, small_order: false, torsion_free: false, canonical_hex: None },
    V { label: "off_curve_6", hex: "b02bcb76ce533654641c6fed7cb080b8ddf0d377a58f135b66a614f00feeb424", category: "off_curve", permissive_decodes: false, small_order: false, torsion_free: false, canonical_hex: None },
    V { label: "off_curve_7", hex: "5571fc12ab10f29ccd2d0256744065e60b76398c64bcd8b180140f280778d5a1", category: "off_curve", permissive_decodes: false, small_order: false, torsion_free: false, canonical_hex: None },
];

fn unhex(s: &str) -> [u8; 32] {
    let mut out = [0u8; 32];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap();
    }
    out
}

fn tohex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn accepted(b: &[u8; 32]) -> bool {
    PublicKey::decode_exact(b).is_ok()
}

fn of(cat: &str) -> impl Iterator<Item = &'static V> + '_ {
    VECTORS.iter().filter(move |v| v.category == cat)
}

/// Independent canonicality predicate, no field arithmetic: an encoding of
/// a valid point is canonical iff its 255-bit y is < p and it does not set
/// the sign bit when x == 0 (x == 0 exactly when y == 1 or y == p - 1).
fn is_canonical_bytes(b: &[u8; 32]) -> bool {
    let mut y = *b;
    let sign = y[31] >> 7;
    y[31] &= 0x7f;
    // p = 2^255 - 19, little-endian: ed ff .. ff 7f
    let mut p = [0xffu8; 32];
    p[0] = 0xed;
    p[31] = 0x7f;
    // y < p ?  compare little-endian from the top byte down
    let mut lt = false;
    for i in (0..32).rev() {
        if y[i] != p[i] {
            lt = y[i] < p[i];
            break;
        }
    }
    if !lt {
        return false;
    }
    let mut one = [0u8; 32];
    one[0] = 1;
    let mut pm1 = p;
    pm1[0] = 0xec;
    !(sign == 1 && (y == one || y == pm1))
}

fn small_order_encodings() -> Vec<[u8; 32]> {
    VECTORS
        .iter()
        .filter(|v| v.permissive_decodes && v.small_order)
        .map(|v| unhex(v.hex))
        .collect()
}

/// What a strict decoder must do, computed without the code under test:
/// on-curve (dalek's permissive decompress) AND canonical AND not small-order.
fn oracle_accepts(b: &[u8; 32], small: &[[u8; 32]]) -> bool {
    VerifyingKey::from_bytes(b).is_ok() && is_canonical_bytes(b) && !small.contains(b)
}

// ---------------------------------------------------------------------------
// Fixed-vector tests
// ---------------------------------------------------------------------------

#[test]
fn vector_set_is_complete_and_self_consistent() {
    let nc: Vec<_> = VECTORS
        .iter()
        .filter(|v| {
            matches!(v.category, "noncanonical_y" | "negative_zero_x") && v.permissive_decodes
        })
        .collect();
    assert_eq!(
        nc.len(),
        26,
        "expected all 26 decodable non-canonical encodings"
    );
    assert_eq!(nc.iter().filter(|v| v.small_order).count(), 6);
    assert_eq!(of("torsion_canonical").count(), 8);
    // The Python ground truth and dalek must agree on which encodings are
    // curve points at all — otherwise the oracle below is meaningless.
    for v in VECTORS {
        let b = unhex(v.hex);
        assert_eq!(
            VerifyingKey::from_bytes(&b).is_ok(),
            v.permissive_decodes,
            "ground truth vs dalek disagree on {}",
            v.label
        );
        if let Some(c) = v.canonical_hex {
            assert_eq!(
                is_canonical_bytes(&b),
                c == v.hex,
                "canonical predicate wrong on {}",
                v.label
            );
            let cb = unhex(c);
            assert!(
                is_canonical_bytes(&cb),
                "{}: re-encoding not canonical",
                v.label
            );
        }
    }
}

#[test]
fn rejects_all_24_noncanonical_y_encodings() {
    let accepted_labels: Vec<_> = of("noncanonical_y")
        .filter(|v| v.permissive_decodes && accepted(&unhex(v.hex)))
        .map(|v| v.label)
        .collect();
    assert!(
        accepted_labels.is_empty(),
        "decode_exact ACCEPTED {} non-canonical y >= p encodings: {:?}",
        accepted_labels.len(),
        accepted_labels
    );
}

#[test]
fn rejects_negative_zero_x_encodings() {
    let accepted_labels: Vec<_> = of("negative_zero_x")
        .filter(|v| accepted(&unhex(v.hex)))
        .map(|v| v.label)
        .collect();
    assert!(
        accepted_labels.is_empty(),
        "accepted negative-zero encodings: {accepted_labels:?}"
    );
}

#[test]
fn rejects_all_eight_torsion_points() {
    let accepted_labels: Vec<_> = of("torsion_canonical")
        .filter(|v| accepted(&unhex(v.hex)))
        .map(|v| v.label)
        .collect();
    assert!(
        accepted_labels.is_empty(),
        "accepted small-order points: {accepted_labels:?}"
    );
}

#[test]
fn rejects_off_curve_encodings() {
    for v in of("off_curve") {
        assert!(!accepted(&unhex(v.hex)), "accepted off-curve {}", v.label);
    }
}

#[test]
fn accepts_honest_keys_and_reencodes_them_exactly() {
    for v in of("honest") {
        let b = unhex(v.hex);
        let pk =
            PublicKey::decode_exact(&b).unwrap_or_else(|e| panic!("{} rejected: {e:?}", v.label));
        assert_eq!(pk.encode(), b, "{} did not round-trip", v.label);
    }
}

#[test]
fn mixed_order_policy_report() {
    // Informational: mixed-order points are not small-order, so a
    // small-order check does not reject them; only `is_torsion_free` does.
    let n = of("mixed_order").count();
    let acc = of("mixed_order")
        .filter(|v| accepted(&unhex(v.hex)))
        .count();
    assert!(of("mixed_order").all(|v| !v.small_order && !v.torsion_free));
    eprintln!("MIXED-ORDER POLICY: decode_exact accepts {acc}/{n} mixed-order keys");
}

// ---------------------------------------------------------------------------
// Floods (oracle = on-curve AND canonical AND not small-order)
// ---------------------------------------------------------------------------

struct XorShift(u64);
impl XorShift {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    fn bytes32(&mut self) -> [u8; 32] {
        let mut b = [0u8; 32];
        for c in b.chunks_mut(8) {
            c.copy_from_slice(&self.next().to_le_bytes());
        }
        b
    }
}

fn check_flood(inputs: impl Iterator<Item = [u8; 32]>) -> (usize, usize, Vec<String>) {
    let small = small_order_encodings();
    let (mut n, mut acc, mut bad) = (0usize, 0usize, Vec::new());
    for b in inputs {
        n += 1;
        let got = accepted(&b);
        let want = oracle_accepts(&b, &small);
        acc += got as usize;
        if got != want && bad.len() < 20 {
            bad.push(format!("{} got={got} want={want}", tohex(&b)));
        }
        if got {
            // Canonical round trip: an accepted key must re-encode to its input.
            let pk = PublicKey::decode_exact(&b).unwrap();
            if pk.encode() != b && bad.len() < 20 {
                bad.push(format!("{} accepted but re-encodes differently", tohex(&b)));
            }
        }
    }
    (n, acc, bad)
}

#[test]
fn flood_exhaustive_near_both_field_boundaries() {
    // y in [0, 255] and y in [p - 256, 2^255 - 1], both sign bits.
    let mut inputs = Vec::new();
    for sign in [0u8, 0x80] {
        for k in 0u16..256 {
            let mut lo = [0u8; 32];
            lo[0] = k as u8;
            lo[1] = (k >> 8) as u8;
            lo[31] |= sign;
            inputs.push(lo);
        }
        // 2^255 - 1 - j for j in 0..275 covers [p - 256, 2^255 - 1]
        for j in 0u16..275 {
            let mut hi = [0xffu8; 32];
            hi[31] = 0x7f;
            // low two bytes are 0xffff; subtracting j < 0xffff never borrows further
            let low = 0xffffu16 - j;
            hi[0] = low as u8;
            hi[1] = (low >> 8) as u8;
            hi[31] |= sign;
            inputs.push(hi);
        }
    }
    let (n, acc, bad) = check_flood(inputs.into_iter());
    eprintln!("boundary flood: {n} encodings, {acc} accepted");
    assert!(
        bad.is_empty(),
        "boundary flood mismatches:\n{}",
        bad.join("\n")
    );
}

#[test]
fn flood_random_encodings() {
    let mut rng = XorShift(0x9E37_79B9_7F4A_7C15);
    let (n, acc, bad) = check_flood((0..200_000).map(|_| rng.bytes32()));
    eprintln!("random flood: {n} encodings, {acc} accepted");
    assert!(
        bad.is_empty(),
        "random flood mismatches:\n{}",
        bad.join("\n")
    );
}

#[test]
fn flood_single_bit_malleations_of_honest_and_torsion_keys() {
    let mut inputs = Vec::new();
    for v in VECTORS
        .iter()
        .filter(|v| matches!(v.category, "honest" | "torsion_canonical" | "mixed_order"))
    {
        let base = unhex(v.hex);
        for bit in 0..256 {
            let mut m = base;
            m[bit / 8] ^= 1 << (bit % 8);
            inputs.push(m);
        }
    }
    let (n, acc, bad) = check_flood(inputs.into_iter());
    eprintln!("bit-malleation flood: {n} encodings, {acc} accepted");
    assert!(
        bad.is_empty(),
        "malleation flood mismatches:\n{}",
        bad.join("\n")
    );
}
