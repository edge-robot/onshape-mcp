//! Named tests for every `expectations.json` `goal-grant.behaviour` entry and
//! the data-driven `goal-grant.cases` (contract §12), through the real
//! [`execute`] with a recording mock provider and a temporary journal.
//!
//! The mock records every connection and request. Each failing admission
//! asserts that no request reached it (no provider I/O before §12.3 step 11).

// Test-only: fixture and file setup failures are test bugs.
#![allow(clippy::unwrap_used)]

use super::mock::{MockProvider, Reply};
use super::*;
use onshape_client_core::request::RequestBody;
use onshape_mcp_core::goal::{Journal, JournalEntry};
use serde_json::{Value, json};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

const GOAL: &str = "3f2a9c1e-5b7d-4e8f-9a0b-1c2d3e4f5a6b";
const V16: &str = "https://cad.onshape.com/api/v16";
const UNKNOWN: &str = "mutation_outcome_unknown_reconcile_required";
const NO_CALLS: [(String, String); 0] = [];
const NO_CONNECTIONS: [String; 0] = [];

fn contract(name: &str) -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../onshape-mcp-core/tests/contract")
        .join(name);
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

/// Atomic private publish, as the plugin does (`writePrivateSecretFileAtomic`).
fn publish(path: &Path, value: &Value) {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, value.to_string()).unwrap();
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600)).unwrap();
    std::fs::rename(tmp, path).unwrap();
}

fn request(name: &str) -> ApiRequest {
    let raw = contract(name);
    ApiRequest {
        method: raw["method"].as_str().unwrap().parse().unwrap(),
        path: raw["path"].as_str().unwrap().to_owned(),
        query_params: vec![],
        headers: http::HeaderMap::new(),
        body: Some(RequestBody::Json(raw["body"].clone())),
        content_type: Some("application/json".into()),
    }
}

fn goal_request(name: &str) -> ApiRequest {
    request(&format!("goal-grant/requests/{name}"))
}

fn depth() -> ApiRequest {
    goal_request("update-depth.request.json")
}

fn fillet() -> ApiRequest {
    goal_request("update-fillet-radius.request.json")
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

/// `Ok(status)` or the error code.
fn outcome(result: &Result<RawResponse, McpError>) -> String {
    match result {
        Ok(response) => format!("ok:{}", response.status),
        Err(error) => error.message.to_string(),
    }
}

fn data(result: &Result<RawResponse, McpError>) -> Value {
    result
        .as_ref()
        .err()
        .and_then(|error| error.data.clone())
        .unwrap_or(Value::Null)
}

/// A server configured with `boundsGoal`: private credential, approval and
/// grant files and a private journal directory in one temporary directory.
struct Env {
    _dir: tempfile::TempDir,
    root: PathBuf,
    config: ExternalTokenConfig,
    consumed: ConsumedProposals,
}

impl Env {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let journal = root.join("journal");
        std::fs::create_dir(&journal).unwrap();
        std::fs::set_permissions(&journal, std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut bounds = contract("expectations.json")["boundsGoal"].clone();
        let object = bounds.as_object_mut().unwrap();
        for key in ["_comment", "goal_grant_file", "dispatch_journal_dir"] {
            object.remove(key);
        }
        let config = ExternalTokenConfig {
            file: root.join("access-token.json"),
            document_id: bounds["document_id"].as_str().unwrap().to_owned(),
            // The read binding is the Primary; goal reads must come from the grant.
            workspace_id: "e".repeat(24),
            element_id: bounds["element_id"].as_str().unwrap().to_owned(),
            mutation: Some(serde_json::from_value(bounds).unwrap()),
            mutation_approval_file: Some(root.join("approved-update.json")),
            goal_grant_file: Some(root.join("goal-grant.json")),
            dispatch_journal_dir: Some(journal),
            read_api_version: None,
        };
        let env = Self {
            _dir: dir,
            root,
            config,
            consumed: ConsumedProposals::default(),
        };
        env.token(&["OAuth2Read", "OAuth2Write"]);
        env.approval(&Value::Null);
        env
    }

    fn token(&self, scopes: &[&str]) {
        publish(
            &self.config.file,
            &json!({"accessToken":"fake-test-token",
                "expiresAt":Utc::now().timestamp_millis() + 3_600_000,"scopes":scopes}),
        );
    }

    fn approval(&self, value: &Value) {
        publish(self.config.mutation_approval_file.as_ref().unwrap(), value);
    }

    fn grant_path(&self) -> PathBuf {
        self.config.goal_grant_file.clone().unwrap()
    }

    fn grant_value(&self, value: &Value) {
        publish(&self.grant_path(), value);
    }

    fn grant(&self, fixture: &str) {
        self.grant_value(&contract(&format!("goal-grant/{fixture}")));
    }

    fn journal_dir(&self) -> PathBuf {
        self.config.dispatch_journal_dir.clone().unwrap()
    }

    fn store(&self) -> JournalStore {
        JournalStore::new(&self.journal_dir())
    }

    fn journal(&self) -> Journal {
        self.store().read(GOAL).unwrap()
    }

    fn journal_lines(&self) -> usize {
        std::fs::read_to_string(self.journal_dir().join(format!("{GOAL}.jsonl")))
            .map_or(0, |raw| raw.lines().count())
    }

    async fn call(
        &mut self,
        mock: &Arc<MockProvider>,
        request: &ApiRequest,
    ) -> Result<RawResponse, McpError> {
        execute(
            &self.config,
            Duration::from_secs(1),
            &mut self.consumed,
            &Provider::Mock(Arc::clone(mock)),
            request,
        )
        .await
    }

    async fn route(&self, uri: &str) -> (http::StatusCode, Value) {
        use tower::ServiceExt;
        let response = local_routes(self.config.clone())
            .oneshot(
                http::Request::get(uri)
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
            .await
            .unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }
}

fn hash_of(request: &ApiRequest) -> String {
    match &request.body {
        Some(RequestBody::Json(body)) => onshape_mcp_core::goal::request_hash(&request.path, body),
        _ => String::new(),
    }
}

fn entry(sequence: u64, request_hash: &str, outcome: DispatchOutcome) -> JournalEntry {
    JournalEntry {
        sequence,
        grant_revision: 1,
        request_hash: request_hash.to_owned(),
        feature_id: "FabcDEF123_0".into(),
        source_microversion: "a".repeat(24),
        admitted_at: 1,
        outcome,
        http_status: None,
    }
}

fn post_paths(requests: &[&ApiRequest]) -> Vec<(String, String)> {
    requests
        .iter()
        .map(|r| ("POST".to_owned(), r.path.clone()))
        .collect()
}

/// §12.3 data-driven: every `goal-grant.cases` entry through `execute`;
/// admitted cases dispatch exactly once, all others never reach the provider.
#[tokio::test]
async fn goal_grant_cases_through_execute() {
    let cases = contract("expectations.json")["goal-grant"]["cases"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(cases.len(), 22);
    for case in cases {
        let (grant, req, expect) = (
            case["grant"].as_str().unwrap(),
            case["request"].as_str().unwrap(),
            case["expect"].as_str().unwrap(),
        );
        let mut env = Env::new();
        env.grant(grant);
        let mock = MockProvider::new(&[Reply::Status(200)]);
        let request = goal_request(req);
        let result = env.call(&mock, &request).await;
        if expect == "admitted" {
            assert_eq!(outcome(&result), "ok:200", "{grant} + {req}");
            assert_eq!(mock.calls(), post_paths(&[&request]));
            assert_eq!(mock.connections(), [V16]);
        } else {
            assert_eq!(outcome(&result), expect, "{grant} + {req}");
            assert_eq!(mock.calls(), NO_CALLS, "provider I/O for {grant} + {req}");
            assert_eq!(mock.connections(), NO_CONNECTIONS, "{grant} + {req}");
            assert_eq!(env.journal_lines(), 0, "{grant} + {req}");
        }
    }
}

/// A1, `twoChangesOneGoal`.
#[tokio::test]
async fn two_changes_one_goal() {
    let mut env = Env::new();
    two_changes(&mut env).await;
}

async fn two_changes(env: &mut Env) {
    env.grant("valid/active.json");
    let mock = MockProvider::new(&[Reply::Status(200), Reply::Status(200)]);
    assert_eq!(outcome(&env.call(&mock, &depth()).await), "ok:200");
    assert_eq!(outcome(&env.call(&mock, &fillet()).await), "ok:200");
    assert_eq!(mock.calls(), post_paths(&[&depth(), &fillet()]));
    assert_eq!(mock.connections(), [V16, V16]);
    let journal = env.journal();
    let entries = journal.entries();
    assert_eq!(entries.len(), 2);
    for (i, (entry, feature)) in entries
        .iter()
        .zip(["FabcDEF123_0", "FfedCBA321_1"])
        .enumerate()
    {
        assert_eq!(entry.sequence, i as u64 + 1);
        assert_eq!(entry.outcome, DispatchOutcome::Ok);
        assert_eq!(entry.http_status, Some(200));
        assert_eq!(entry.grant_revision, 1);
        assert_eq!(entry.feature_id, feature);
    }
    // PENDING and OK per dispatch, append-only.
    assert_eq!(env.journal_lines(), 4);
}

/// `journalSummary` after `twoChangesOneGoal`, plus the 404/503 rules (§12.7).
#[tokio::test]
async fn journal_summary() {
    let mut env = Env::new();
    two_changes(&mut env).await;
    let (status, body) = env.route(&format!("/goals/{GOAL}/journal-summary")).await;
    assert_eq!(status, http::StatusCode::OK);
    assert!(body["lastAdmittedAt"].is_i64());
    let mut expected = json!({"goalId":GOAL,"entries":2,"counted":2,"maxSequence":2,
        "unreconciledSequence":null,"lastOutcome":"OK","unreconciledEntry":null});
    expected["lastAdmittedAt"] = body["lastAdmittedAt"].clone();
    assert_eq!(body, expected);
    // Unknown goal: zeros and nulls.
    let other = "0b1c2d3e-4f5a-4b6c-8d7e-9f0a1b2c3d4e";
    let (status, body) = env.route(&format!("/goals/{other}/journal-summary")).await;
    assert_eq!(status, http::StatusCode::OK);
    assert_eq!(
        body,
        json!({"goalId":other,"entries":0,"counted":0,"maxSequence":0,
            "unreconciledSequence":null,"lastOutcome":null,"lastAdmittedAt":null,
            "unreconciledEntry":null})
    );
    // Not a uuid-v4.
    for bad in [
        "not-a-uuid",
        "3F2A9C1E-5B7D-4E8F-9A0B-1C2D3E4F5A6B",
        "..%2F..",
    ] {
        let (status, _) = env.route(&format!("/goals/{bad}/journal-summary")).await;
        assert_eq!(status, http::StatusCode::NOT_FOUND, "{bad}");
    }
    // Unreadable journal.
    std::fs::set_permissions(env.journal_dir(), std::fs::Permissions::from_mode(0o750)).unwrap();
    let (status, body) = env.route(&format!("/goals/{GOAL}/journal-summary")).await;
    assert_eq!(status, http::StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body, json!({"code":"dispatch_journal_unavailable"}));
    // Without goal configuration the route does not exist for this server.
    let mut no_goal = Env::new();
    no_goal.config.goal_grant_file = None;
    no_goal.config.dispatch_journal_dir = None;
    let (status, _) = no_goal
        .route(&format!("/goals/{GOAL}/journal-summary"))
        .await;
    assert_eq!(status, http::StatusCode::NOT_FOUND);
}

/// The summary uses the current grant's `reconciledThrough`.
#[tokio::test]
async fn journal_summary_follows_reconciliation() {
    let env = Env::new();
    env.grant("valid/active.json");
    env.store()
        .append(GOAL, &entry(1, &"1".repeat(64), DispatchOutcome::Uncertain))
        .unwrap();
    let uri = format!("/goals/{GOAL}/journal-summary");
    let before = env.route(&uri).await.1;
    assert_eq!(before["unreconciledSequence"], json!(1));
    assert_eq!(before["unreconciledEntry"]["sequence"], json!(1));
    env.grant("valid/reconciled-through-1.json");
    let after = env.route(&uri).await.1;
    assert_eq!(after["unreconciledSequence"], json!(null));
    assert_eq!(after["unreconciledEntry"], json!(null));
}

/// `journalSummaryUncertain` (1.2.0): `update-depth` ends in HTTP 503; the
/// summary equals `journal-summary/uncertain.json` with the computed
/// `requestHash`. The real clock sets `admittedAt`, so both time fields are
/// taken from the journal; the core contract test checks the fixture value.
#[tokio::test]
async fn journal_summary_uncertain() {
    let mut env = Env::new();
    env.grant("valid/active.json");
    let mock = MockProvider::new(&[Reply::Status(503)]);
    assert_eq!(outcome(&env.call(&mock, &depth()).await), UNKNOWN);
    assert_eq!(mock.calls().len(), 1);
    let admitted_at = env.journal().entries()[0].admitted_at;
    let mut expected = contract("goal-grant/journal-summary/uncertain.json");
    expected["lastAdmittedAt"] = json!(admitted_at);
    expected["unreconciledEntry"]["admittedAt"] = json!(admitted_at);
    expected["unreconciledEntry"]["requestHash"] = json!(hash_of(&depth()));
    let uri = format!("/goals/{GOAL}/journal-summary");
    let (status, body) = env.route(&uri).await;
    assert_eq!(status, http::StatusCode::OK);
    assert_eq!(body, expected);
    // Only the listed fields: no body, token or path leaks into the entry.
    let text = body.to_string();
    assert!(!text.contains("fake-test-token"));
    assert!(!text.contains(&env.root.display().to_string()));
    assert!(!text.contains("\"body\""));
    // reconciled-through-1: both unreconciled fields are null.
    env.grant("valid/reconciled-through-1.json");
    let (_, body) = env.route(&uri).await;
    expected["unreconciledSequence"] = json!(null);
    expected["unreconciledEntry"] = json!(null);
    assert_eq!(body, expected);
    // The summary never reaches the provider.
    assert_eq!(mock.calls().len(), 1);
}

/// A6: `/capabilities` matches both fixtures for the matching config.
#[tokio::test]
async fn capabilities_match_fixtures() {
    let env = Env::new();
    let (status, body) = env.route("/capabilities").await;
    assert_eq!(status, http::StatusCode::OK);
    assert_eq!(body, contract("capabilities/approval-v2-and-goal.json"));
    // Journal directory fails the private checks: goal path not offered.
    std::fs::set_permissions(env.journal_dir(), std::fs::Permissions::from_mode(0o750)).unwrap();
    assert_eq!(
        env.route("/capabilities").await.1,
        contract("capabilities/approval-v2-only.json")
    );
    let mut approval_only = Env::new();
    approval_only.config.goal_grant_file = None;
    approval_only.config.dispatch_journal_dir = None;
    assert_eq!(
        approval_only.route("/capabilities").await.1,
        contract("capabilities/approval-v2-only.json")
    );
    // Health routes are unchanged.
    assert_eq!(env.route("/health").await.0, http::StatusCode::OK);
    assert_eq!(env.route("/health/ready").await.0, http::StatusCode::OK);
}

/// `budget`: `maxDispatches` 2 with 1 OK + 2 `PROVIDER_REJECTED` is exhausted;
/// `NOT_DISPATCHED` does not count.
#[tokio::test]
async fn budget() {
    let mut env = Env::new();
    let mut grant = contract("goal-grant/valid/active.json");
    grant["maxDispatches"] = json!(2);
    env.grant_value(&grant);
    let store = env.store();
    store
        .append(GOAL, &entry(1, &"1".repeat(64), DispatchOutcome::Ok))
        .unwrap();
    store
        .append(
            GOAL,
            &entry(2, &"2".repeat(64), DispatchOutcome::ProviderRejected),
        )
        .unwrap();
    let mock = MockProvider::new(&[Reply::Status(200)]);
    let result = env.call(&mock, &depth()).await;
    assert_eq!(outcome(&result), "goal_budget_exhausted");
    assert_eq!(
        data(&result),
        json!({"goalId":GOAL,"featureId":"FabcDEF123_0","sourceMicroversion":"a".repeat(24)})
    );
    assert_eq!(mock.calls(), NO_CALLS);

    let mut env = Env::new();
    env.grant_value(&grant);
    let store = env.store();
    store
        .append(GOAL, &entry(1, &"1".repeat(64), DispatchOutcome::Ok))
        .unwrap();
    store
        .append(
            GOAL,
            &entry(2, &"2".repeat(64), DispatchOutcome::NotDispatched),
        )
        .unwrap();
    let mock = MockProvider::new(&[Reply::Status(200)]);
    assert_eq!(outcome(&env.call(&mock, &depth()).await), "ok:200");
    assert_eq!(env.journal().entries()[2].sequence, 3);
    assert_eq!(
        outcome(&env.call(&mock, &fillet()).await),
        "goal_budget_exhausted"
    );
    assert_eq!(mock.calls().len(), 1);
}

/// `duplicate`: the identical request after an OK dispatch, no provider I/O.
#[tokio::test]
async fn duplicate() {
    let mut env = Env::new();
    env.grant("valid/active.json");
    let mock = MockProvider::new(&[Reply::Status(200), Reply::Status(200)]);
    assert_eq!(outcome(&env.call(&mock, &depth()).await), "ok:200");
    let result = env.call(&mock, &depth()).await;
    assert_eq!(outcome(&result), "goal_dispatch_duplicate");
    let data = data(&result);
    assert_eq!(data["sequence"], json!(1));
    assert_eq!(data["goalId"], json!(GOAL));
    assert!(data["admittedAt"].is_i64());
    assert_eq!(mock.calls().len(), 1);
}

/// `uncertain`: transport error, 5xx, 3xx and 408 yield the unknown outcome
/// with goal data; the goal stays blocked until `reconciledThrough` covers it.
#[tokio::test]
async fn uncertain() {
    for (reply, status) in [
        (Reply::Transport, None),
        (Reply::Status(500), Some(500)),
        (Reply::Status(503), Some(503)),
        (Reply::Status(302), Some(302)),
        (Reply::Status(408), Some(408)),
    ] {
        let mut env = Env::new();
        env.grant("valid/active.json");
        let mock = MockProvider::new(&[reply]);
        let result = env.call(&mock, &depth()).await;
        assert_eq!(outcome(&result), UNKNOWN, "{reply:?}");
        assert_eq!(
            result.as_ref().err().unwrap().code,
            rmcp::model::ErrorCode::INTERNAL_ERROR
        );
        let data = data(&result);
        assert!(data["admittedAt"].is_i64());
        let mut expected = json!({"goalId":GOAL,"sequence":1,"featureId":"FabcDEF123_0",
            "sourceMicroversion":"a".repeat(24),"outcome":"UNKNOWN",
            "retryAllowed":false,"reconciliationRequired":true});
        expected["admittedAt"] = data["admittedAt"].clone();
        assert_eq!(data, expected, "{reply:?}");
        let journal = env.journal();
        assert_eq!(journal.entries()[0].outcome, DispatchOutcome::Uncertain);
        assert_eq!(journal.entries()[0].http_status, status);
        // Blocked: no further dispatch, not even a different request.
        let blocked = env.call(&mock, &fillet()).await;
        assert_eq!(outcome(&blocked), "goal_reconciliation_required");
        assert_eq!(self::data(&blocked)["sequence"], json!(1));
        assert_eq!(mock.calls().len(), 1, "single attempt for {reply:?}");
        // reconciled-through-1 unblocks sequence 1.
        env.grant("valid/reconciled-through-1.json");
        let next = MockProvider::new(&[Reply::Status(200)]);
        assert_eq!(outcome(&env.call(&next, &fillet()).await), "ok:200");
        let journal = env.journal();
        assert_eq!(journal.entries()[1].grant_revision, 3);
        // The uncertain request itself is never sent again blindly.
        assert_eq!(
            outcome(&env.call(&next, &depth()).await),
            "goal_dispatch_duplicate"
        );
        assert_eq!(next.calls().len(), 1);
    }
}

/// §12.4: 4xx except 408 is `PROVIDER_REJECTED`, returned as the provider
/// response, counted, and does not block the goal.
#[tokio::test]
async fn provider_rejected_is_recorded_and_not_blocking() {
    let mut env = Env::new();
    env.grant("valid/active.json");
    let mock = MockProvider::new(&[Reply::Status(409), Reply::Status(200)]);
    assert_eq!(outcome(&env.call(&mock, &depth()).await), "ok:409");
    let entry = env.journal().entries()[0].clone();
    assert_eq!(entry.outcome, DispatchOutcome::ProviderRejected);
    assert_eq!(entry.http_status, Some(409));
    // The same request may be sent again after a rejection.
    assert_eq!(outcome(&env.call(&mock, &depth()).await), "ok:200");
    assert_eq!(env.journal().counted(), 2);
}

/// `restart`: a trailing `PENDING` is loaded as `UNCERTAIN` by a fresh store;
/// the next request is blocked and the budget counts sequence 1.
#[tokio::test]
async fn restart() {
    let mut env = Env::new();
    env.grant("valid/active.json");
    env.store()
        .append(
            GOAL,
            &entry(1, &hash_of(&depth()), DispatchOutcome::Pending),
        )
        .unwrap();
    // New process: fresh replay record and fresh store over the same directory.
    env.consumed = ConsumedProposals::default();
    let mock = MockProvider::new(&[Reply::Status(200)]);
    let result = env.call(&mock, &fillet()).await;
    assert_eq!(outcome(&result), "goal_reconciliation_required");
    assert_eq!(data(&result)["sequence"], json!(1));
    assert_eq!(mock.calls(), NO_CALLS);
    let (_, summary) = env.route(&format!("/goals/{GOAL}/journal-summary")).await;
    assert_eq!(summary["counted"], json!(1));
    assert_eq!(summary["unreconciledSequence"], json!(1));
    assert_eq!(summary["lastOutcome"], json!("PENDING"));
    // The stored outcome is reported as is (1.2.0 `unreconciledEntry`).
    assert_eq!(
        summary["unreconciledEntry"],
        serde_json::to_value(entry(1, &hash_of(&depth()), DispatchOutcome::Pending)).unwrap()
    );
    assert_eq!(summary["unreconciledEntry"]["outcome"], json!("PENDING"));
    assert_eq!(summary["unreconciledEntry"]["httpStatus"], json!(null));
}

/// A7 via the real path: an outcome that cannot be recorded leaves `PENDING`,
/// is reported as unknown, and blocks the goal after a restart.
#[tokio::test]
async fn unrecorded_outcome_is_uncertain_after_restart() {
    let mut env = Env::new();
    env.grant("valid/active.json");
    let mock = MockProvider::new(&[Reply::Status(200)]);
    let file = env.journal_dir().join(format!("{GOAL}.jsonl"));
    mock.on_recheck(move || {
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o400)).unwrap();
    });
    let result = env.call(&mock, &depth()).await;
    assert_eq!(outcome(&result), UNKNOWN);
    assert_eq!(mock.calls().len(), 1);
    assert_eq!(env.journal_lines(), 1);
    env.consumed = ConsumedProposals::default();
    let next = MockProvider::new(&[Reply::Status(200)]);
    assert_eq!(
        outcome(&env.call(&next, &fillet()).await),
        "goal_reconciliation_required"
    );
    assert_eq!(next.calls(), NO_CALLS);
}

/// A `PENDING` line that cannot be written stops before the re-read and the
/// dispatch.
#[tokio::test]
async fn pending_append_failure_is_unavailable_without_dispatch() {
    let mut env = Env::new();
    env.grant("valid/active.json");
    let file = env.journal_dir().join(format!("{GOAL}.jsonl"));
    std::fs::write(&file, b"").unwrap();
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o400)).unwrap();
    let mock = MockProvider::new(&[Reply::Status(200)]);
    let result = env.call(&mock, &depth()).await;
    assert_eq!(outcome(&result), "dispatch_journal_unavailable");
    assert_eq!(data(&result)["sequence"], json!(1));
    assert_eq!(mock.calls(), NO_CALLS);
    assert_eq!(env.journal_lines(), 0);
}

/// `stopRace`: the grant turns `STOPPED` (or `null`) between step 9 and the
/// step-11 re-read: `NOT_DISPATCHED`, no provider call, `goal_grant_stopped`.
#[tokio::test]
async fn stop_race() {
    for replacement in [contract("goal-grant/valid/stopped.json"), Value::Null] {
        let mut env = Env::new();
        env.grant("valid/active.json");
        let mock = MockProvider::new(&[Reply::Status(200)]);
        let grant_path = env.grant_path();
        let swap = replacement.clone();
        mock.on_recheck(move || publish(&grant_path, &swap));
        let result = env.call(&mock, &depth()).await;
        assert_eq!(outcome(&result), "goal_grant_stopped", "{replacement}");
        assert_eq!(data(&result)["sequence"], json!(1));
        assert_eq!(mock.calls(), NO_CALLS, "{replacement}");
        let journal = env.journal();
        assert_eq!(journal.entries()[0].outcome, DispatchOutcome::NotDispatched);
        assert_eq!(journal.counted(), 0);
        assert_eq!(env.journal_lines(), 2);
    }
    // A STOPPED grant then fails at step 5, before the journal.
    let mut env = Env::new();
    env.grant("valid/stopped.json");
    let mock = MockProvider::new(&[]);
    assert_eq!(
        outcome(&env.call(&mock, &depth()).await),
        "goal_grant_stopped"
    );
    assert_eq!(env.journal_lines(), 0);
}

/// `authorityConflict`: a grant and an unexpired approval at the same time.
#[tokio::test]
async fn authority_conflict() {
    let mut env = Env::new();
    env.grant("valid/active.json");
    let approval = contract("approved-update/valid/update-feature-v2.json");
    env.approval(&approval);
    let mock = MockProvider::new(&[Reply::Status(200)]);
    for request in [
        depth(),
        request("approved-update/requests/update-feature.request.json"),
    ] {
        assert_eq!(
            outcome(&env.call(&mock, &request).await),
            "mutation_authority_conflict"
        );
    }
    // Also a (deprecated) v1 approval, and even an invalid grant (step 3 before 4).
    env.approval(&contract("approved-update/valid/update-feature.json"));
    assert_eq!(
        outcome(&env.call(&mock, &depth()).await),
        "mutation_authority_conflict"
    );
    env.grant("invalid/unknown-field.json");
    assert_eq!(
        outcome(&env.call(&mock, &depth()).await),
        "mutation_authority_conflict"
    );
    assert_eq!(mock.calls(), NO_CALLS);
    // An expired approval is no authority.
    env.grant("valid/active.json");
    env.approval(&contract("approved-update/invalid/v2-expired.json"));
    assert_eq!(outcome(&env.call(&mock, &depth()).await), "ok:200");
}

/// `nullGrantFallsBack`: a `null` grant leaves §2.5 unchanged.
#[tokio::test]
async fn null_grant_falls_back() {
    let mut env = Env::new();
    env.grant_value(&Value::Null);
    let paired = request("approved-update/requests/update-feature.request.json");
    let mock = MockProvider::new(&[Reply::Status(200), Reply::Status(200)]);
    // No approval: §2.5 step 4.
    assert_eq!(
        outcome(&env.call(&mock, &paired).await),
        "mutation_approval_mismatch"
    );
    env.approval(&contract("approved-update/valid/update-feature-v2.json"));
    assert_eq!(outcome(&env.call(&mock, &paired).await), "ok:200");
    assert_eq!(
        outcome(&env.call(&mock, &paired).await),
        "mutation_approval_consumed"
    );
    // The approval claims another feature: §2.5 step 5.
    assert_eq!(
        outcome(&env.call(&mock, &fillet()).await),
        "target_policy_denied"
    );
    assert_eq!(mock.calls(), post_paths(&[&paired]));
    assert_eq!(env.journal_lines(), 0);
    // A §2.5 dispatch failure still yields the 1.0.0 unknown outcome, no goal data.
    let mut env = Env::new();
    env.grant_value(&Value::Null);
    env.approval(&contract("approved-update/valid/update-feature-v2.json"));
    let failing = MockProvider::new(&[Reply::Status(502)]);
    let result = env.call(&failing, &paired).await;
    assert_eq!(outcome(&result), UNKNOWN);
    assert_eq!(
        data(&result),
        json!({"outcome":"UNKNOWN","retryAllowed":false,"reconciliationRequired":true})
    );
    // A missing grant file is not `null`: step 2.
    std::fs::remove_file(env.grant_path()).unwrap();
    assert_eq!(
        outcome(&env.call(&failing, &paired).await),
        "goal_grant_unavailable"
    );
    assert_eq!(failing.calls().len(), 1);
}

/// `journalUnavailable`: insecure directory, corrupt line, symlinked file or
/// missing directory fail closed with no provider I/O.
///
/// A foreign owner cannot be produced without privileges in the test
/// environment; [`JournalStore`] checks the owner with the same comparison as
/// the mode bits.
#[tokio::test]
async fn journal_unavailable() {
    let expect = |result: &Result<RawResponse, McpError>| {
        assert_eq!(outcome(result), "dispatch_journal_unavailable");
        assert_eq!(
            data(result),
            json!({"goalId":GOAL,"featureId":"FabcDEF123_0","sourceMicroversion":"a".repeat(24)})
        );
    };
    let mut env = Env::new();
    env.grant("valid/active.json");
    let mock = MockProvider::new(&[]);
    let dir = env.journal_dir();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o750)).unwrap();
    expect(&env.call(&mock, &depth()).await);
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    let file = dir.join(format!("{GOAL}.jsonl"));
    std::fs::write(&file, b"{\"sequence\":1}\n").unwrap();
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
    expect(&env.call(&mock, &depth()).await);
    std::fs::remove_file(&file).unwrap();
    let elsewhere = env.root.join("elsewhere.jsonl");
    std::fs::write(&elsewhere, b"").unwrap();
    std::os::unix::fs::symlink(&elsewhere, &file).unwrap();
    expect(&env.call(&mock, &depth()).await);
    std::fs::remove_file(&file).unwrap();
    std::fs::remove_dir(&dir).unwrap();
    expect(&env.call(&mock, &depth()).await);
    assert_eq!(mock.calls(), NO_CALLS);
    assert_eq!(mock.connections(), NO_CONNECTIONS);
}

/// `missingJournalDir` and the §12.1 path rules.
#[tokio::test]
async fn missing_journal_dir() {
    let mut env = Env::new();
    env.config.dispatch_journal_dir = None;
    assert_eq!(
        validate_config(&env.config),
        Err("target_configuration_invalid")
    );
    let mock = MockProvider::new(&[]);
    assert_eq!(
        outcome(&env.call(&mock, &depth()).await),
        "target_configuration_invalid"
    );
    for bad in [
        "relative/journal",
        "/var/lib/../journal",
        "/var/./journal",
        "/var/lib/journal/",
        "//var/lib/journal",
    ] {
        let mut config = Env::new().config;
        config.dispatch_journal_dir = Some(bad.into());
        assert_eq!(
            validate_config(&config),
            Err("external_token_configuration_invalid"),
            "{bad}"
        );
        config.dispatch_journal_dir = Some("/var/lib/onshape-mcp/journal".into());
        config.goal_grant_file = Some(bad.into());
        assert!(validate_config(&config).is_err(), "{bad}");
    }
    // Neither key: 1.0.0 behaviour.
    env.config.goal_grant_file = None;
    assert_eq!(validate_config(&env.config), Ok(()));
    assert_eq!(mock.calls(), NO_CALLS);
}

/// `readsGoal`: the three Candidate reads while an unexpired grant exists.
#[tokio::test]
async fn reads_goal() {
    let mut env = Env::new();
    let grant = contract("goal-grant/valid/active.json");
    let w = grant["target"]["workspaceId"].as_str().unwrap().to_owned();
    let (d, e) = (
        env.config.document_id.clone(),
        env.config.element_id.clone(),
    );
    let reads = [
        format!("/documents/d/{d}/w/{w}/currentmicroversion"),
        format!("/documents/d/{d}/w/{w}/elements"),
        format!("/partstudios/d/{d}/w/{w}/e/{e}/features"),
    ];
    for fixture in ["valid/active.json", "valid/stopped.json"] {
        env.grant(fixture);
        let mock = MockProvider::new(&[Reply::Status(200); 3]);
        for path in &reads {
            assert_eq!(
                outcome(&env.call(&mock, &get(path.clone())).await),
                "ok:200",
                "{fixture} {path}"
            );
        }
        assert_eq!(mock.calls().len(), 3);
        assert!(
            mock.connections()
                .iter()
                .all(|base| base == "https://cad.onshape.com/api/v10")
        );
    }
    // Expired, null or out-of-scope paths add nothing.
    let mut expired = grant;
    expired["issuedAt"] = json!(1_000);
    expired["expiresAt"] = json!(2_000);
    let mock = MockProvider::new(&[]);
    for value in [expired, Value::Null] {
        env.grant_value(&value);
        for path in &reads {
            assert_eq!(
                outcome(&env.call(&mock, &get(path.clone())).await),
                "external_token_read_policy_denied"
            );
        }
    }
    env.grant("valid/active.json");
    assert_eq!(
        outcome(
            &env.call(&mock, &get(format!("/documents/d/{d}/w/{w}/workspaces")))
                .await
        ),
        "external_token_read_policy_denied"
    );
    assert_eq!(mock.calls(), NO_CALLS);
}

/// `reconciledBeyondJournal`.
#[tokio::test]
async fn reconciled_beyond_journal() {
    let mut env = Env::new();
    env.grant("valid/reconciled-through-1.json");
    let mock = MockProvider::new(&[Reply::Status(200)]);
    let result = env.call(&mock, &depth()).await;
    assert_eq!(outcome(&result), "goal_grant_invalid");
    assert_eq!(data(&result)["goalId"], json!(GOAL));
    assert_eq!(mock.calls(), NO_CALLS);
}

/// Step 10 failures carry goal data and write nothing to the journal.
#[tokio::test]
async fn token_failures_carry_goal_data_and_write_nothing() {
    let mut env = Env::new();
    env.grant("valid/active.json");
    let mock = MockProvider::new(&[]);
    env.token(&["OAuth2Read"]);
    let result = env.call(&mock, &depth()).await;
    assert_eq!(outcome(&result), "external_token_write_scope_missing");
    assert_eq!(
        data(&result),
        json!({"goalId":GOAL,"featureId":"FabcDEF123_0","sourceMicroversion":"a".repeat(24)})
    );
    std::fs::remove_file(&env.config.file).unwrap();
    assert_eq!(
        outcome(&env.call(&mock, &depth()).await),
        "external_token_unavailable"
    );
    assert_eq!(env.journal_lines(), 0);
    assert_eq!(mock.connections(), NO_CONNECTIONS);
}

/// Two concurrent identical requests: the admission lock lets exactly one
/// pass step 9; the other sees its journal entry.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_identical_requests_dispatch_once() {
    let env = Env::new();
    env.grant("valid/active.json");
    let mock = MockProvider::new(&[Reply::Status(200), Reply::Status(200)]);
    let tasks: Vec<_> = (0..2)
        .map(|_| {
            let config = env.config.clone();
            let provider = Provider::Mock(Arc::clone(&mock));
            tokio::spawn(async move {
                outcome(
                    &execute(
                        &config,
                        Duration::from_secs(1),
                        &mut ConsumedProposals::default(),
                        &provider,
                        &depth(),
                    )
                    .await,
                )
            })
        })
        .collect();
    let mut results = Vec::new();
    for task in tasks {
        results.push(task.await.unwrap());
    }
    results.sort();
    assert_eq!(results, ["goal_dispatch_duplicate", "ok:200"]);
    assert_eq!(mock.calls().len(), 1);
}

/// Every code string the external-token module can emit is a contract server
/// code (`error-codes.json` `server` is a superset).
#[test]
fn emitted_codes_are_listed_in_contract() {
    let codes = contract("error-codes.json");
    let server: Vec<&str> = codes["server"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect();
    let source = include_str!("../external_token.rs");
    let production = source
        .split("/// Recording provider double")
        .next()
        .unwrap();
    let mut literals = Vec::new();
    for piece in production.split('"').skip(1).step_by(2) {
        let is_code = piece.contains('_')
            && piece
                .bytes()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_');
        if is_code {
            literals.push(piece);
        }
    }
    assert!(literals.contains(&"goal_grant_stopped"));
    for code in literals {
        assert!(server.contains(&code), "{code} not in error-codes.json");
    }
    for error in onshape_mcp_core::goal::GoalAdmissionError::ALL {
        assert!(server.contains(&error.code()), "{}", error.code());
    }
}
