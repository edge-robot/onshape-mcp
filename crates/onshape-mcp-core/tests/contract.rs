//! Contract tests against the read-only interface fixtures in `tests/contract/`.
//!
//! Contract-Ref: clawonshape/docs/interface/onshape-mcp.md#10-fixtures
//!
//! The fixtures are copied by the architect from the `ClawOnshape` interface
//! contract. They must never be regenerated from this side; a mismatch is a
//! contract deviation that needs a decision, not a snapshot update.

use std::path::PathBuf;

use onshape_client_core::request::{ApiRequest, RequestBody};
use onshape_mcp_core::approval::{
    AdmissionError, ConsumedProposals, admit_mutation, claimed_read_workspace, read_admitted,
};
use onshape_mcp_core::config::{AppConfig, ExternalTokenConfig};
use onshape_mcp_core::goal::{
    Capabilities, DispatchOutcome, GoalAdmissionError, GoalDecision, Journal, admit_goal_mutation,
    approval_active, canonical_json, goal_read_admitted, grant_read_workspace, sha256_hex,
};
use onshape_mcp_core::target_policy::TargetConstraint;
use onshape_mcp_core::tools::{EXTERNAL_TOKEN_TOOL_NAMES, list_tools_external_token};
use serde_json::Value;

/// Contract version this crate is implemented against.
const CONTRACT_VERSION: &str = "1.2.0-draft";

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

// ---------------------------------------------------------------------------
// Channel C: approval file v1/v2 and mutation admission (§2.5, §4, §10)
// ---------------------------------------------------------------------------

fn read_contract_json(name: &str) -> Result<Value, String> {
    serde_json::from_str(&read_contract_file(name)?).map_err(|e| format!("{name} is not JSON: {e}"))
}

fn expectations() -> Result<Value, String> {
    read_contract_json("expectations.json")
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn str_field<'a>(value: &'a Value, pointer: &str) -> Result<&'a str, String> {
    value
        .pointer(pointer)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("expectations.json lacks string {pointer}"))
}

/// The paired request of `expectations.json` `approved-update.pairedRequest`.
fn paired_request(expectations: &Value) -> Result<ApiRequest, String> {
    let name = str_field(expectations, "/approved-update/pairedRequest")?;
    let raw = read_contract_json(&format!("approved-update/{name}"))?;
    let method = str_field(&raw, "/method")?;
    let query = raw
        .get("query_params")
        .and_then(Value::as_array)
        .ok_or("paired request lacks query_params")?;
    if !query.is_empty() {
        return Err("paired request unexpectedly has query parameters".into());
    }
    Ok(ApiRequest {
        method: method
            .parse()
            .map_err(|e| format!("bad method {method}: {e}"))?,
        path: str_field(&raw, "/path")?.to_owned(),
        query_params: vec![],
        headers: http::HeaderMap::new(),
        body: Some(RequestBody::Json(
            raw.get("body")
                .cloned()
                .ok_or("paired request lacks body")?,
        )),
        content_type: Some("application/json".into()),
    })
}

/// Static v2 bounds only (`boundsV2`, no workspace/feature/microversion pins).
fn bounds_policy(expectations: &Value) -> Result<TargetConstraint, String> {
    let mut bounds = expectations
        .get("boundsV2")
        .cloned()
        .ok_or("expectations.json lacks boundsV2")?;
    if let Some(object) = bounds.as_object_mut() {
        object.remove("_comment");
    }
    serde_json::from_value(bounds).map_err(|e| format!("boundsV2 is not a TargetConstraint: {e}"))
}

/// Static bounds plus the v1 `target` pins (`workspace_id`, `expected_microversion`).
fn pinned_policy(expectations: &Value) -> Result<TargetConstraint, String> {
    let mut bounds = expectations
        .get("boundsV2")
        .cloned()
        .ok_or("expectations.json lacks boundsV2")?;
    let target = expectations
        .get("target")
        .and_then(Value::as_object)
        .ok_or("expectations.json lacks target")?;
    let object = bounds.as_object_mut().ok_or("boundsV2 is not an object")?;
    object.remove("_comment");
    for (key, value) in target {
        object.insert(key.clone(), value.clone());
    }
    serde_json::from_value(bounds)
        .map_err(|e| format!("pinned target is not a TargetConstraint: {e}"))
}

/// Run the real admission (§2.5 steps 1–8) and map the result to the
/// `expectations.json` vocabulary.
fn admission_result(
    policy: &TargetConstraint,
    request: &ApiRequest,
    approval: &Value,
    consumed: &mut ConsumedProposals,
) -> String {
    match admit_mutation(
        Some(policy),
        request,
        || Some(approval.clone()),
        now_ms(),
        consumed,
    ) {
        Ok(admitted) => {
            consumed.record(admitted);
            "accepted".to_owned()
        }
        Err(error) => error.code().to_owned(),
    }
}

/// Fixture names (`valid/…`, `invalid/…`) and their expected results.
fn approval_fixture_expectations(expectations: &Value) -> Result<Vec<(String, String)>, String> {
    let entries = expectations
        .get("approved-update")
        .and_then(Value::as_object)
        .ok_or("expectations.json lacks approved-update")?;
    let mut out = Vec::new();
    for (name, expected) in entries {
        if name.starts_with("valid/") || name.starts_with("invalid/") {
            let expected = expected
                .as_str()
                .ok_or_else(|| format!("expectation for {name} is not a string"))?;
            out.push((name.clone(), expected.to_owned()));
        }
    }
    Ok(out)
}

fn is_v2(fixture: &Value) -> bool {
    fixture.get("version").and_then(Value::as_u64) == Some(2)
}

#[test]
fn every_approval_fixture_is_listed_in_expectations() -> Result<(), String> {
    let listed: Vec<String> = approval_fixture_expectations(&expectations()?)?
        .into_iter()
        .map(|(name, _)| name)
        .collect();
    for kind in ["valid", "invalid"] {
        let dir = contract_path(&format!("approved-update/{kind}"));
        let entries =
            std::fs::read_dir(&dir).map_err(|e| format!("cannot list {}: {e}", dir.display()))?;
        for entry in entries {
            let entry = entry.map_err(|e| e.to_string())?;
            let name = format!("{kind}/{}", entry.file_name().to_string_lossy());
            assert!(listed.contains(&name), "{name} has no expectation");
        }
    }
    Ok(())
}

#[test]
fn approval_fixtures_with_v1_target_pins() -> Result<(), String> {
    let expectations = expectations()?;
    let request = paired_request(&expectations)?;
    let policy = pinned_policy(&expectations)?;
    for (name, expected) in approval_fixture_expectations(&expectations)? {
        let fixture = read_contract_json(&format!("approved-update/{name}"))?;
        let actual = admission_result(
            &policy,
            &request,
            &fixture,
            &mut ConsumedProposals::default(),
        );
        assert_eq!(actual, expected, "pinned mode: {name}");
    }
    Ok(())
}

#[test]
fn approval_v2_fixtures_with_static_bounds_only() -> Result<(), String> {
    let expectations = expectations()?;
    let request = paired_request(&expectations)?;
    let policy = bounds_policy(&expectations)?;
    let mut v2_count = 0;
    for (name, expected) in approval_fixture_expectations(&expectations)? {
        let fixture = read_contract_json(&format!("approved-update/{name}"))?;
        // A v1 approval is admitted only with static pins (§2.5, §4). Without
        // them every v1 (and non-v2) fixture is rejected at step 4.
        let expected = if is_v2(&fixture) || name.contains("/v2-") {
            v2_count += 1;
            expected
        } else {
            "mutation_approval_mismatch".to_owned()
        };
        let actual = admission_result(
            &policy,
            &request,
            &fixture,
            &mut ConsumedProposals::default(),
        );
        assert_eq!(actual, expected, "bounds-only mode: {name}");
    }
    assert_eq!(
        v2_count, 10,
        "expected one valid and nine invalid v2 fixtures"
    );
    Ok(())
}

#[test]
fn approval_replay_is_consumed_without_dispatch() -> Result<(), String> {
    let expectations = expectations()?;
    assert!(
        str_field(&expectations, "/approved-update/replay")?.contains("mutation_approval_consumed")
    );
    let request = paired_request(&expectations)?;
    let v1 = read_contract_json("approved-update/valid/update-feature.json")?;
    let v2 = read_contract_json("approved-update/valid/update-feature-v2.json")?;
    for (policy, approvals) in [
        (pinned_policy(&expectations)?, vec![&v1, &v2]),
        (bounds_policy(&expectations)?, vec![&v2]),
    ] {
        // One record per server process, shared by v1 and v2.
        let mut consumed = ConsumedProposals::default();
        for approval in approvals {
            assert_eq!(
                admission_result(&policy, &request, approval, &mut consumed),
                "accepted"
            );
            let mut file_reads = 0;
            let second = admit_mutation(
                Some(&policy),
                &request,
                || {
                    file_reads += 1;
                    Some(approval.clone())
                },
                now_ms(),
                &consumed,
            );
            // No AdmittedProposal is handed out, so the caller cannot reach
            // the token check (step 9) or the dispatch (step 10).
            assert_eq!(
                second.err().map(|e| e.code()),
                Some("mutation_approval_consumed")
            );
            assert_eq!(file_reads, 1);
        }
    }
    Ok(())
}

fn deployed_config_toml(target: &Value, approval_file: bool) -> Result<String, String> {
    let d = str_field(target, "/document_id")?;
    let w = str_field(target, "/workspace_id")?;
    let e = str_field(target, "/element_id")?;
    let mv = str_field(target, "/expected_microversion")?;
    let approval = if approval_file {
        "mutation_approval_file = \"/run/onshape-approval/approved-update.json\"\n"
    } else {
        ""
    };
    Ok(format!(
        r#"[external_token]
file = "/run/onshape-credentials/access-token.json"
document_id = "{d}"
workspace_id = "{w}"
element_id = "{e}"
read_api_version = "v16"
{approval}
[external_token.mutation]
document_id = "{d}"
workspace_id = "{w}"
element_id = "{e}"
excluded_workspace_ids = ["eeeeeeeeeeeeeeeeeeeeeeee"]
allowed_operations = ["updatePartStudioFeature"]
feature_id = "FabcDEF123_0"
expected_microversion = "{mv}"
write_mode = "modify"
"#
    ))
}

fn external_token(toml_text: &str) -> Result<ExternalTokenConfig, String> {
    let config: AppConfig =
        toml::from_str(toml_text).map_err(|e| format!("config does not parse: {e}"))?;
    config
        .external_token
        .ok_or_else(|| "no [external_token]".to_owned())
}

/// Regression: the currently deployed config shape (static pins, v1 approvals
/// from plugin 0.2.10) still parses, validates and admits the v1 fixture.
#[test]
fn deployed_config_shape_admits_v1_fixture() -> Result<(), String> {
    let expectations = expectations()?;
    let target = expectations.get("target").ok_or("no target")?;
    let config = external_token(&deployed_config_toml(target, true)?)?;
    assert_eq!(config.require_mutation_approval_file(), Ok(()));
    let policy = config.mutation.as_ref().ok_or("no mutation policy")?;
    assert_eq!(
        policy.validate(
            &config.document_id,
            &config.workspace_id,
            &config.element_id
        ),
        Ok(())
    );
    let request = paired_request(&expectations)?;
    let v1 = read_contract_json("approved-update/valid/update-feature.json")?;
    assert_eq!(
        admission_result(policy, &request, &v1, &mut ConsumedProposals::default()),
        "accepted"
    );
    Ok(())
}

#[test]
fn missing_approval_file_refuses_start() -> Result<(), String> {
    let expectations = expectations()?;
    assert!(
        str_field(&expectations, "/approved-update/missingApprovalFile")?
            .contains("target_configuration_invalid")
    );
    let target = expectations.get("target").ok_or("no target")?;
    let config = external_token(&deployed_config_toml(target, false)?)?;
    assert_eq!(
        config
            .require_mutation_approval_file()
            .map_err(|e| e.to_string()),
        Err("target_configuration_invalid".to_owned())
    );
    Ok(())
}

/// Read target is the Primary (excluded) workspace; the v2 claim names the
/// Candidate, so the claimed reads are distinguishable from the fixed set.
fn reads_config(expectations: &Value) -> Result<ExternalTokenConfig, String> {
    let bounds = bounds_policy(expectations)?;
    let mut config = external_token(&format!(
        r#"[external_token]
file = "/run/onshape-credentials/access-token.json"
document_id = "{d}"
workspace_id = "eeeeeeeeeeeeeeeeeeeeeeee"
element_id = "{e}"
mutation_approval_file = "/run/onshape-approval/approved-update.json"
"#,
        d = str_field(expectations, "/boundsV2/document_id")?,
        e = str_field(expectations, "/boundsV2/element_id")?,
    ))?;
    config.mutation = Some(bounds);
    Ok(config)
}

fn get(path: String) -> ApiRequest {
    ApiRequest {
        method: http::Method::GET,
        path,
        query_params: vec![],
        headers: http::HeaderMap::new(),
        body: None,
        content_type: None,
    }
}

#[test]
fn reads_of_claimed_candidate_while_v2_active() -> Result<(), String> {
    let expectations = expectations()?;
    assert!(str_field(&expectations, "/approved-update/readsV2")?.contains("currentmicroversion"));
    let config = reads_config(&expectations)?;
    let d = config.document_id.clone();
    let e = config.element_id.clone();
    let v2 = read_contract_json("approved-update/valid/update-feature-v2.json")?;
    let claimed = str_field(&v2, "/target/workspaceId")?.to_owned();
    let features = get(format!("/partstudios/d/{d}/w/{claimed}/e/{e}/features"));
    let microversion = get(format!("/documents/d/{d}/w/{claimed}/currentmicroversion"));

    // Without an active approval only the configured read target is admitted.
    assert!(!read_admitted(&config, &features, None));
    assert!(!read_admitted(&config, &microversion, None));

    let workspace =
        claimed_read_workspace(&config, &v2, now_ms()).ok_or("valid v2 must yield a claim")?;
    assert_eq!(workspace.as_str(), claimed);
    assert!(read_admitted(&config, &features, Some(&workspace)));
    assert!(read_admitted(&config, &microversion, Some(&workspace)));
    // Only those two paths; {d} and {e} stay the configured target.
    for path in [
        format!("/documents/d/{d}/w/{claimed}/elements"),
        format!(
            "/partstudios/d/{d}/w/{claimed}/e/{}/features",
            "1".repeat(24)
        ),
        format!(
            "/partstudios/d/{}/w/{claimed}/e/{e}/features",
            "1".repeat(24)
        ),
        format!("/partstudios/d/{d}/w/{claimed}/e/{e}/features/featureid/FabcDEF123_0"),
    ] {
        assert!(
            !read_admitted(&config, &get(path.clone()), Some(&workspace)),
            "{path}"
        );
    }
    // Configured reads stay admitted with or without the claim.
    let configured = get(format!(
        "/partstudios/d/{d}/w/{}/e/{e}/features",
        config.workspace_id
    ));
    assert!(read_admitted(&config, &configured, None));
    assert!(read_admitted(&config, &configured, Some(&workspace)));
    Ok(())
}

#[test]
fn inactive_or_invalid_approval_adds_no_reads() -> Result<(), String> {
    let expectations = expectations()?;
    let config = reads_config(&expectations)?;
    for name in [
        "invalid/null-no-approval.json",
        "invalid/v2-expired.json",
        "invalid/v2-lifetime-over-15min.json",
        "invalid/v2-missing-target.json",
        "invalid/v2-document-outside-bounds.json",
        "valid/update-feature.json",
    ] {
        let fixture = read_contract_json(&format!("approved-update/{name}"))?;
        assert_eq!(
            claimed_read_workspace(&config, &fixture, now_ms()),
            None,
            "{name}"
        );
    }
    // Unreadable files reach the core as non-JSON values or not at all.
    assert_eq!(
        claimed_read_workspace(&config, &Value::String("garbage".into()), now_ms()),
        None
    );
    // `now == expiresAt` is no longer active.
    let v2 = read_contract_json("approved-update/valid/update-feature-v2.json")?;
    let expires_at = v2
        .get("expiresAt")
        .and_then(Value::as_i64)
        .ok_or("no expiresAt")?;
    assert_eq!(claimed_read_workspace(&config, &v2, expires_at), None);
    // Without a mutation policy no claim is honoured.
    let mut read_only = config;
    read_only.mutation = None;
    assert_eq!(claimed_read_workspace(&read_only, &v2, now_ms()), None);
    Ok(())
}

#[test]
fn admission_codes_are_listed_in_contract() -> Result<(), String> {
    let codes = read_contract_json("error-codes.json")?;
    let server: Vec<&str> = codes
        .get("server")
        .and_then(Value::as_array)
        .ok_or("error-codes.json lacks server")?
        .iter()
        .filter_map(Value::as_str)
        .collect();
    for error in AdmissionError::ALL {
        assert!(
            server.contains(&error.code()),
            "{} not in contract",
            error.code()
        );
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Goal authorization (§12, 1.1.0)
// ---------------------------------------------------------------------------

/// `boundsGoal` without the `_comment` and the §12.1 file keys, as the static
/// `[external_token.mutation]` bounds.
fn goal_bounds(expectations: &Value) -> Result<TargetConstraint, String> {
    let mut bounds = expectations
        .get("boundsGoal")
        .cloned()
        .ok_or("expectations.json lacks boundsGoal")?;
    let object = bounds
        .as_object_mut()
        .ok_or("boundsGoal is not an object")?;
    for key in ["_comment", "goal_grant_file", "dispatch_journal_dir"] {
        object.remove(key);
    }
    serde_json::from_value(bounds).map_err(|e| format!("boundsGoal is not a TargetConstraint: {e}"))
}

/// A `goal-grant/requests/*.request.json` fixture as an `ApiRequest`.
fn goal_request(name: &str) -> Result<ApiRequest, String> {
    let raw = read_contract_json(&format!("goal-grant/requests/{name}"))?;
    let method = str_field(&raw, "/method")?;
    let query = raw
        .get("query_params")
        .and_then(Value::as_array)
        .ok_or_else(|| format!("{name} lacks query_params"))?;
    if !query.is_empty() {
        return Err(format!("{name} unexpectedly has query parameters"));
    }
    Ok(ApiRequest {
        method: method
            .parse()
            .map_err(|e| format!("bad method {method}: {e}"))?,
        path: str_field(&raw, "/path")?.to_owned(),
        query_params: vec![],
        headers: http::HeaderMap::new(),
        body: Some(RequestBody::Json(
            raw.get("body")
                .cloned()
                .ok_or_else(|| format!("{name} lacks body"))?,
        )),
        content_type: Some("application/json".into()),
    })
}

/// Run the real goal admission (§12.3 steps 1–9) against an empty journal and
/// no active approval; map the result to the `expectations.json` vocabulary.
fn goal_result(
    policy: &TargetConstraint,
    request: &ApiRequest,
    grant: &Value,
    journal: &Journal,
) -> String {
    match admit_goal_mutation(
        Some(policy),
        request,
        || Some(grant.clone()),
        || false,
        |_| Ok(journal.clone()),
        now_ms(),
    ) {
        Ok(GoalDecision::Admit(_)) => "admitted".to_owned(),
        Ok(GoalDecision::ApprovalPath) => "approval-path".to_owned(),
        Err(rejection) => rejection.error.code().to_owned(),
    }
}

fn goal_cases(expectations: &Value) -> Result<Vec<(String, String, String)>, String> {
    let cases = expectations
        .pointer("/goal-grant/cases")
        .and_then(Value::as_array)
        .ok_or("expectations.json lacks goal-grant.cases")?;
    cases
        .iter()
        .map(|case| {
            Ok((
                str_field(case, "/grant")?.to_owned(),
                str_field(case, "/request")?.to_owned(),
                str_field(case, "/expect")?.to_owned(),
            ))
        })
        .collect()
}

#[test]
fn goal_grant_cases_follow_admission_order() -> Result<(), String> {
    let expectations = expectations()?;
    let policy = goal_bounds(&expectations)?;
    let cases = goal_cases(&expectations)?;
    assert_eq!(cases.len(), 22, "goal-grant.cases count");
    for (grant, request, expected) in cases {
        let fixture = read_contract_json(&format!("goal-grant/{grant}"))?;
        let actual = goal_result(
            &policy,
            &goal_request(&request)?,
            &fixture,
            &Journal::default(),
        );
        assert_eq!(actual, expected, "{grant} + {request}");
    }
    Ok(())
}

#[test]
fn every_goal_fixture_is_used_by_a_case() -> Result<(), String> {
    let expectations = expectations()?;
    let cases = goal_cases(&expectations)?;
    for kind in ["valid", "invalid", "requests"] {
        let dir = contract_path(&format!("goal-grant/{kind}"));
        let entries =
            std::fs::read_dir(&dir).map_err(|e| format!("cannot list {}: {e}", dir.display()))?;
        for entry in entries {
            let entry = entry.map_err(|e| e.to_string())?;
            let file = entry.file_name().to_string_lossy().into_owned();
            let used = cases.iter().any(|(grant, request, _)| {
                grant == &format!("{kind}/{file}") || (kind == "requests" && request == &file)
            }) || (kind == "valid" && file == "reconciled-through-1.json");
            assert!(used, "goal-grant/{kind}/{file} is not exercised");
        }
    }
    Ok(())
}

#[test]
fn goal_behaviour_entries_are_known() -> Result<(), String> {
    let expectations = expectations()?;
    let behaviour = expectations
        .pointer("/goal-grant/behaviour")
        .and_then(Value::as_object)
        .ok_or("expectations.json lacks goal-grant.behaviour")?;
    let mut keys: Vec<&str> = behaviour.keys().map(String::as_str).collect();
    keys.sort_unstable();
    // Each key has a named test in onshape-mcp-io `goal_behaviour`; a new key
    // needs a new test there.
    assert_eq!(
        keys,
        [
            "authorityConflict",
            "budget",
            "duplicate",
            "journalSummary",
            "journalSummaryUncertain",
            "journalUnavailable",
            "missingJournalDir",
            "nullGrantFallsBack",
            "readsGoal",
            "reconciledBeyondJournal",
            "restart",
            "stopRace",
            "twoChangesOneGoal",
            "uncertain",
        ]
    );
    Ok(())
}

/// `journalSummaryUncertain` (1.2.0): the real admission at the fixture's
/// `admittedAt`, an HTTP 503 outcome, and the summary against the fixture with
/// the computed `requestHash`. The IO crate runs the same case through the
/// route with the real clock.
#[test]
fn journal_summary_uncertain_matches_fixture() -> Result<(), String> {
    let expectations = expectations()?;
    assert!(
        str_field(
            &expectations,
            "/goal-grant/behaviour/journalSummaryUncertain"
        )?
        .contains("goal-grant/journal-summary/uncertain.json")
    );
    let mut expected = read_contract_json("goal-grant/journal-summary/uncertain.json")?;
    let admitted_at = expected
        .pointer("/unreconciledEntry/admittedAt")
        .and_then(Value::as_i64)
        .ok_or("uncertain.json lacks unreconciledEntry.admittedAt")?;
    let goal_id = str_field(&expected, "/goalId")?.to_owned();
    let raw = read_contract_json("goal-grant/requests/update-depth.request.json")?;
    let hashed = serde_json::json!({
        "path": str_field(&raw, "/path")?,
        "body": raw.get("body").ok_or("request lacks body")?,
    });
    let request_hash = sha256_hex(canonical_json(&hashed).as_bytes());
    let slot = expected
        .pointer_mut("/unreconciledEntry/requestHash")
        .ok_or("uncertain.json lacks unreconciledEntry.requestHash")?;
    *slot = Value::String(request_hash);

    let policy = goal_bounds(&expectations)?;
    let active = read_contract_json("goal-grant/valid/active.json")?;
    let admission = match admit_goal_mutation(
        Some(&policy),
        &goal_request("update-depth.request.json")?,
        || Some(active.clone()),
        || false,
        |_| Ok(Journal::default()),
        admitted_at,
    ) {
        Ok(GoalDecision::Admit(admission)) => admission,
        Ok(GoalDecision::ApprovalPath) => return Err("unexpected approval path".into()),
        Err(rejection) => return Err(format!("rejected: {}", rejection.error.code())),
    };
    let pending = admission.pending();
    let uncertain = pending.with_outcome(DispatchOutcome::from_status(503), Some(503));
    let lines = format!(
        "{}{}",
        pending.to_line().map_err(|e| e.code().to_owned())?,
        uncertain.to_line().map_err(|e| e.code().to_owned())?
    );
    let journal = Journal::parse(lines.as_bytes()).map_err(|e| e.code().to_owned())?;
    let summary = |grant: &Value| -> Result<Value, String> {
        let reconciled = grant
            .get("reconciledThrough")
            .and_then(Value::as_u64)
            .ok_or("grant lacks reconciledThrough")?;
        serde_json::to_value(journal.summary(&goal_id, reconciled)).map_err(|e| e.to_string())
    };
    assert_eq!(summary(&active)?, expected);
    // Reconciled through sequence 1: nothing unreconciled any more.
    let reconciled = read_contract_json("goal-grant/valid/reconciled-through-1.json")?;
    let mut cleared = expected.clone();
    cleared["unreconciledSequence"] = Value::Null;
    cleared["unreconciledEntry"] = Value::Null;
    assert_eq!(summary(&reconciled)?, cleared);
    Ok(())
}

#[test]
fn goal_reconciled_beyond_journal_is_invalid() -> Result<(), String> {
    let expectations = expectations()?;
    assert!(
        str_field(
            &expectations,
            "/goal-grant/behaviour/reconciledBeyondJournal"
        )?
        .contains("goal_grant_invalid")
    );
    let grant = read_contract_json("goal-grant/valid/reconciled-through-1.json")?;
    let request = goal_request("update-depth.request.json")?;
    let policy = goal_bounds(&expectations)?;
    assert_eq!(
        goal_result(&policy, &request, &grant, &Journal::default()),
        "goal_grant_invalid"
    );
    Ok(())
}

#[test]
fn goal_authority_conflict_and_null_fallback() -> Result<(), String> {
    let expectations = expectations()?;
    let policy = goal_bounds(&expectations)?;
    let request = goal_request("update-depth.request.json")?;
    let grant = read_contract_json("goal-grant/valid/active.json")?;
    let approval = read_contract_json("approved-update/valid/update-feature-v2.json")?;
    assert!(approval_active(&approval, now_ms()));
    let conflict = admit_goal_mutation(
        Some(&policy),
        &request,
        || Some(grant.clone()),
        || approval_active(&approval, now_ms()),
        |_| Ok(Journal::default()),
        now_ms(),
    );
    assert_eq!(
        conflict.err().map(|r| r.error.code()),
        Some("mutation_authority_conflict")
    );
    // Expired or `null` approvals are no authority.
    for name in ["invalid/v2-expired.json", "invalid/null-no-approval.json"] {
        let fixture = read_contract_json(&format!("approved-update/{name}"))?;
        assert!(!approval_active(&fixture, now_ms()), "{name}");
    }
    assert_eq!(
        goal_result(&policy, &request, &Value::Null, &Journal::default()),
        "approval-path"
    );
    Ok(())
}

#[test]
fn missing_journal_dir_refuses_start() -> Result<(), String> {
    let expectations = expectations()?;
    assert!(
        str_field(&expectations, "/goal-grant/behaviour/missingJournalDir")?
            .contains("target_configuration_invalid")
    );
    let grant_file = str_field(&expectations, "/boundsGoal/goal_grant_file")?;
    let journal_dir = str_field(&expectations, "/boundsGoal/dispatch_journal_dir")?;
    let base = r#"[external_token]
file = "/run/onshape-credentials/access-token.json"
document_id = "0123456789abcdef01234567"
workspace_id = "eeeeeeeeeeeeeeeeeeeeeeee"
element_id = "fedcba9876543210fedcba98"
mutation_approval_file = "/run/onshape-approval/approved-update.json"
"#;
    let without = external_token(&format!("{base}goal_grant_file = \"{grant_file}\"\n"))?;
    assert_eq!(
        without
            .require_dispatch_journal_dir()
            .map_err(|e| e.to_string()),
        Err("target_configuration_invalid".to_owned())
    );
    let with = external_token(&format!(
        "{base}goal_grant_file = \"{grant_file}\"\ndispatch_journal_dir = \"{journal_dir}\"\n"
    ))?;
    assert_eq!(with.require_dispatch_journal_dir(), Ok(()));
    assert_eq!(external_token(base)?.require_dispatch_journal_dir(), Ok(()));
    Ok(())
}

#[test]
fn reads_of_goal_candidate_while_grant_exists() -> Result<(), String> {
    let expectations = expectations()?;
    let mut config = reads_config(&expectations)?;
    config.mutation = Some(goal_bounds(&expectations)?);
    let d = config.document_id.clone();
    let e = config.element_id.clone();
    for name in ["valid/active.json", "valid/stopped.json"] {
        let grant = read_contract_json(&format!("goal-grant/{name}"))?;
        let w = str_field(&grant, "/target/workspaceId")?.to_owned();
        let workspace =
            grant_read_workspace(&config, &grant, now_ms()).ok_or("grant must yield reads")?;
        for path in [
            format!("/documents/d/{d}/w/{w}/currentmicroversion"),
            format!("/documents/d/{d}/w/{w}/elements"),
            format!("/partstudios/d/{d}/w/{w}/e/{e}/features"),
        ] {
            assert!(!read_admitted(&config, &get(path.clone()), None), "{path}");
            assert!(
                goal_read_admitted(&config, &get(path.clone()), &workspace),
                "{name}: {path}"
            );
        }
        for path in [
            format!("/documents/d/{d}/w/{w}/workspaces"),
            format!("/partstudios/d/{d}/w/{w}/e/{e}/features/featureid/FabcDEF123_0"),
            format!("/partstudios/d/{}/w/{w}/e/{e}/features", "1".repeat(24)),
        ] {
            assert!(
                !goal_read_admitted(&config, &get(path.clone()), &workspace),
                "{path}"
            );
        }
        // Expired grants add nothing.
        let expires_at = grant
            .get("expiresAt")
            .and_then(Value::as_i64)
            .ok_or("no expiresAt")?;
        assert_eq!(grant_read_workspace(&config, &grant, expires_at), None);
    }
    for name in [
        "invalid/expired.json",
        "invalid/document-outside-bounds.json",
        "invalid/workspace-excluded-primary.json",
        "invalid/unknown-field.json",
    ] {
        let grant = read_contract_json(&format!("goal-grant/{name}"))?;
        assert_eq!(
            grant_read_workspace(&config, &grant, now_ms()),
            None,
            "{name}"
        );
    }
    assert_eq!(grant_read_workspace(&config, &Value::Null, now_ms()), None);
    Ok(())
}

#[test]
fn capabilities_match_fixtures() -> Result<(), String> {
    for (name, goal) in [
        ("approval-v2-only.json", false),
        ("approval-v2-and-goal.json", true),
    ] {
        let fixture = read_contract_json(&format!("capabilities/{name}"))?;
        let actual =
            serde_json::to_value(Capabilities::new(true, goal)).map_err(|e| e.to_string())?;
        assert_eq!(actual, fixture, "{name}");
    }
    Ok(())
}

#[test]
fn canonical_json_matches_goal_hash_vector() -> Result<(), String> {
    let vector = read_contract_json("goal-workflow/goal-hash/vector.json")?;
    let scope = vector.get("scope").ok_or("vector lacks scope")?;
    let canonical = str_field(&vector, "/canonical")?;
    assert_eq!(canonical_json(scope), canonical);
    assert_eq!(
        sha256_hex(canonical.as_bytes()),
        str_field(&vector, "/goalHash")?
    );
    Ok(())
}

#[test]
fn goal_codes_are_listed_in_contract() -> Result<(), String> {
    let codes = read_contract_json("error-codes.json")?;
    let server: Vec<&str> = codes
        .get("server")
        .and_then(Value::as_array)
        .ok_or("error-codes.json lacks server")?
        .iter()
        .filter_map(Value::as_str)
        .collect();
    for error in GoalAdmissionError::ALL {
        assert!(
            server.contains(&error.code()),
            "{} not in contract",
            error.code()
        );
    }
    Ok(())
}
