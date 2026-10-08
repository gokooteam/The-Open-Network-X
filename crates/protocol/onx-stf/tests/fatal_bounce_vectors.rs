//! Fatal-vs-bounce agreement: the Rust taxonomy (`onx_stf::is_fatal_exception`,
//! ADR-0037, extended by ADR-0039) must agree with the independent Python
//! reference (`reference/gen_fatal_bounce_vectors.py`,
//! `reference/vectors/fatal_bounce.json`).
//!
//! The vectors were generated from the spec text, not from the Rust code.
//! `kind_name` matches every `ExceptionKind` variant explicitly: adding a
//! seventh kind breaks compilation here until the taxonomy (and the vectors)
//! decide its fate.

use onx_execution::ExceptionKind;
use onx_stf::is_fatal_exception;
use serde_json::Value;

fn vectors() -> Value {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../reference/vectors/fatal_bounce.json"
    );
    let text = std::fs::read_to_string(path).expect("fatal_bounce.json must exist");
    serde_json::from_str(&text).expect("fatal_bounce.json must parse")
}

/// Names every variant of the closed `ExceptionKind` set explicitly — a new
/// variant fails compilation until it is named here AND the taxonomy and
/// vectors decide its outcome.
fn kind_name(kind: &ExceptionKind) -> &'static str {
    match kind {
        ExceptionKind::OutOfGas => "OutOfGas",
        ExceptionKind::IntegerOverflow => "IntegerOverflow",
        ExceptionKind::AbsentNode => "AbsentNode",
        ExceptionKind::MalformedCell => "MalformedCell",
        ExceptionKind::TypeMismatch => "TypeMismatch",
        ExceptionKind::CallStackOverflow => "CallStackOverflow",
    }
}

fn all_kinds() -> [ExceptionKind; 6] {
    [
        ExceptionKind::OutOfGas,
        ExceptionKind::IntegerOverflow,
        ExceptionKind::AbsentNode,
        ExceptionKind::MalformedCell,
        ExceptionKind::TypeMismatch,
        ExceptionKind::CallStackOverflow,
    ]
}

#[test]
fn taxonomy_agrees_with_python() {
    let v = vectors();
    assert_eq!(v["adr"].as_str(), Some("ADR-0037 + ADR-0039"));
    let table: std::collections::BTreeMap<&str, &str> = v["exception_outcomes"]
        .as_array()
        .expect("exception_outcomes array")
        .iter()
        .map(|c| {
            (
                c["exception_kind"].as_str().expect("kind"),
                c["outcome"].as_str().expect("outcome"),
            )
        })
        .collect();
    for kind in all_kinds() {
        let name = kind_name(&kind);
        let expected = table
            .get(name)
            .unwrap_or_else(|| panic!("{name} missing from fatal_bounce.json"));
        let fatal = is_fatal_exception(&kind);
        assert_eq!(
            *expected,
            if fatal { "fatal" } else { "bounce" },
            "taxonomy mismatch for {name}"
        );
    }
}

#[test]
fn vectors_cover_the_closed_set() {
    // The JSON must name exactly the closed ExceptionKind set — no silent
    // extras, no silent gaps.
    let v = vectors();
    let names: std::collections::BTreeSet<&str> = v["exception_outcomes"]
        .as_array()
        .expect("exception_outcomes array")
        .iter()
        .map(|c| c["exception_kind"].as_str().expect("kind"))
        .collect();
    let expected: std::collections::BTreeSet<&str> = all_kinds().iter().map(kind_name).collect();
    assert_eq!(names, expected, "vector set != closed ExceptionKind set");
}

#[test]
fn structural_cases_all_bounce() {
    // ADR-0004 structural delivery cases, unchanged by ADR-0037: none of
    // them is an execution failure, so none is fatal.
    let v = vectors();
    for c in v["situation_outcomes"].as_array().expect("array") {
        assert_eq!(
            c["outcome"].as_str(),
            Some("bounce"),
            "structural case {} must bounce",
            c["situation"]
        );
    }
}
