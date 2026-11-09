//! The tool catalog is cached by MCP clients: it must equal the Swift app's, and the validator
//! must reject anything outside it before a request reaches the app.

use orbit_automation::catalog::{self, INSTRUCTIONS};
use orbit_automation::{Failure, TransportError};
use serde_json::{Value, json};

/// `tests/golden/tools.json` is the output of the Swift v0.9.15 `OrbitControl tools`.
#[test]
fn catalog_equals_swift_golden() {
    let golden: Value = serde_json::from_str(include_str!("golden/tools.json")).unwrap();
    assert_eq!(golden["instructions"], INSTRUCTIONS);
    let tools = golden["tools"].as_array().unwrap();
    assert_eq!(tools.len(), catalog::tools().len());
    for (expected, actual) in tools.iter().zip(catalog::tools()) {
        assert_eq!(expected, actual, "tool {}", expected["name"]);
    }
}

fn rejected(command: &str, arguments: Value) -> Failure {
    catalog::validate(command, arguments.as_object().unwrap()).expect_err(&arguments.to_string())
}

#[test]
fn validator_rejects_extra_keys_and_wrong_types() {
    let item = json!({"path": "/tmp/a", "identity": "1:2"});
    catalog::validate("collect", json!({"context": "c", "items": [item]}).as_object().unwrap()).unwrap();
    catalog::validate("list", json!({"limit": 200.0}).as_object().unwrap()).unwrap();

    let cases = [
        ("list", json!({"extra": 1})),
        ("collect", json!({"context": "c", "items": [{"path": "/tmp/a", "identity": "1:2", "extra": 1}]})),
        ("collect", json!({"context": "c", "items": [{"path": "/tmp/a"}]})),
        ("collect", json!({"context": "c", "items": []})),
        ("list", json!({"limit": true})),
        ("list", json!({"limit": 1.5})),
        ("list", json!({"limit": "5"})),
        ("list", json!({"cursor": null})),
        ("scan", json!({"path": "nul\0path"})),
        ("search", json!({"context": "c", "query": "é".repeat(4097)})),
        ("prepare_removal", json!({"context": "c", "mode": "delete"})),
        ("remove", json!({"token": "t", "confirmed": false})),
        ("remove", json!({"token": "t", "confirmed": 1})),
    ];
    for (command, arguments) in cases {
        assert_eq!(rejected(command, arguments).code, "invalid_arguments");
    }
    // Lengths count Unicode scalars, not bytes.
    catalog::validate("search", json!({"context": "c", "query": "é".repeat(4096)}).as_object().unwrap()).unwrap();
    assert_eq!(rejected("nothing", json!({})).code, "unknown_command");
}

#[test]
fn transport_failures_after_sending_are_marked_outcome_unknown() {
    for (error, code) in [
        (TransportError::Timeout, "request_timeout"),
        (TransportError::Disconnected, "connection_closed"),
        (TransportError::System("Read automation message", 54), "connection_failed"),
    ] {
        let failure = Failure::from(error);
        assert_eq!((failure.code.as_str(), failure.outcome_unknown), (code, true));
    }
    for error in [TransportError::Unavailable, TransportError::PeerMismatch] {
        assert!(!Failure::from(error).outcome_unknown);
    }
    let response = catalog::failure_response(&Failure::from(TransportError::PeerMismatch));
    assert_eq!(
        response,
        json!({"ok": false, "error": {"code": "peer_mismatch", "message": "The automation connection belongs to another user."}})
    );
}
