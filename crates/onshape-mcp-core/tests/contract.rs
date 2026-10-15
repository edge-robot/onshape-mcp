//! Contract tests against the read-only interface fixtures in `tests/contract/`.
//!
//! Contract-Ref: clawonshape/docs/interface/onshape-mcp.md#10-fixtures
//!
//! The fixtures are copied by the architect from the `ClawOnshape` interface
//! contract. They must never be regenerated from this side; a mismatch is a
//! contract deviation that needs a decision, not a snapshot update.

use std::path::PathBuf;

use onshape_mcp_core::tools::{EXTERNAL_TOKEN_TOOL_NAMES, list_tools_external_token};
use serde_json::Value;

/// Contract version this crate is implemented against.
const CONTRACT_VERSION: &str = "0.2.0-draft";

fn contract_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("contract")
        .join(name)
}

fn read_contract_file(name: &str) -> Result<String, String> {
    let path = contract_path(name);
    std::fs::read_to_string(&path).map_err(|e| format!("cannot read {}: {e}", path.display()))
}

fn load_tools_snapshot() -> Result<Vec<Value>, String> {
    let raw = read_contract_file("mcp-tools.json")?;
    let value: Value =
        serde_json::from_str(&raw).map_err(|e| format!("mcp-tools.json is not JSON: {e}"))?;
    match value {
        Value::Array(tools) => Ok(tools),
        other => Err(format!("mcp-tools.json must be an array, got {other}")),
    }
}

fn tool_name(tool: &Value) -> &str {
    tool.get("name")
        .and_then(Value::as_str)
        .unwrap_or("<unnamed>")
}

/// Returns the JSON pointer of the first difference between `actual` and
/// `expected`, or `None` if they are equal.
fn first_difference(actual: &Value, expected: &Value, path: &str) -> Option<String> {
    match (actual, expected) {
        (Value::Object(a), Value::Object(e)) => {
            let mut keys: Vec<&String> = a.keys().chain(e.keys()).collect();
            keys.sort();
            keys.dedup();
            keys.into_iter().find_map(|key| {
                let child = format!("{path}/{key}");
                match (a.get(key), e.get(key)) {
                    (Some(av), Some(ev)) => first_difference(av, ev, &child),
                    (Some(_), None) => Some(format!("{child} (only in code output)")),
                    (None, Some(_)) => Some(format!("{child} (only in snapshot)")),
                    (None, None) => None,
                }
            })
        }
        (Value::Array(a), Value::Array(e)) => {
            if a.len() != e.len() {
                return Some(format!(
                    "{path} (array length: code {} vs snapshot {})",
                    a.len(),
                    e.len()
                ));
            }
            a.iter()
                .zip(e)
                .enumerate()
                .find_map(|(i, (av, ev))| first_difference(av, ev, &format!("{path}/{i}")))
        }
        _ if actual == expected => None,
        _ => Some(format!("{path} (code {actual} vs snapshot {expected})")),
    }
}

#[test]
fn contract_version_matches() -> Result<(), String> {
    let raw = read_contract_file("VERSION")?;
    let mut lines = raw.lines();

    let version = lines.next().unwrap_or_default();
    assert_eq!(
        version, CONTRACT_VERSION,
        "tests/contract/VERSION line 1 must be the contract version this code implements"
    );

    let digest_line = lines.next().unwrap_or_default();
    let hex = digest_line.strip_prefix("sha256:").unwrap_or_default();
    assert!(
        hex.len() == 64
            && hex
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
        "tests/contract/VERSION line 2 must be `sha256:` + 64 lowercase hex characters, got {digest_line:?}"
    );
    Ok(())
}

#[test]
fn external_token_tools_match_contract_snapshot() -> Result<(), String> {
    let expected = load_tools_snapshot()?;
    let actual = match serde_json::to_value(list_tools_external_token()) {
        Ok(Value::Array(tools)) => tools,
        Ok(other) => return Err(format!("tool list serialized to non-array: {other}")),
        Err(e) => return Err(format!("tool list serialization failed: {e}")),
    };

    let actual_names: Vec<&str> = actual.iter().map(tool_name).collect();
    let expected_names: Vec<&str> = expected.iter().map(tool_name).collect();
    assert_eq!(
        actual_names, expected_names,
        "external-token tool names/order differ from tests/contract/mcp-tools.json"
    );

    for (code, snapshot) in actual.iter().zip(&expected) {
        if let Some(diff) = first_difference(code, snapshot, "") {
            return Err(format!(
                "tool `{}` differs from tests/contract/mcp-tools.json at {diff}",
                tool_name(snapshot)
            ));
        }
    }

    // Belt and braces: the whole document must be identical.
    assert_eq!(Value::Array(actual), Value::Array(expected));
    Ok(())
}

#[test]
fn external_token_tool_names_match_snapshot() -> Result<(), String> {
    let snapshot = load_tools_snapshot()?;
    let snapshot_names: Vec<&str> = snapshot.iter().map(tool_name).collect();
    assert_eq!(
        EXTERNAL_TOKEN_TOOL_NAMES.as_slice(),
        snapshot_names.as_slice()
    );
    Ok(())
}

#[test]
fn first_difference_names_the_first_differing_key() {
    let code = serde_json::json!({"name": "t", "annotations": {"readOnlyHint": true}});
    let snapshot = serde_json::json!({"name": "t", "annotations": {"readOnlyHint": false}});
    assert_eq!(first_difference(&code, &code, ""), None);
    assert_eq!(
        first_difference(&code, &snapshot, "").as_deref(),
        Some("/annotations/readOnlyHint (code true vs snapshot false)")
    );
    let extra = serde_json::json!({"name": "t", "title": "x"});
    assert_eq!(
        first_difference(&extra, &serde_json::json!({"name": "t"}), "").as_deref(),
        Some("/title (only in code output)")
    );
    assert_eq!(
        first_difference(&serde_json::json!([1]), &serde_json::json!([1, 2]), "").as_deref(),
        Some(" (array length: code 1 vs snapshot 2)")
    );
}
