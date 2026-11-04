//! External-token mode: no OAuth exchange, persistence, watcher, or retry.
//! Exact-target GETs, approval-bound and goal-bound, revision-bound updates
//! (contract §2.4, §2.5, §12).
use super::{McpError, OnshapeMcpServer, RawResponse};
use crate::dispatch_journal::JournalStore;
use chrono::{DateTime, Utc};
use oauth2::AccessToken;
use onshape_client_core::request::{ApiRequest, ApiResponse};
use onshape_client_io::{ClientAuthConfig, ClientConfig, OnshapeClient};
use onshape_mcp_core::approval::{
    APPROVAL_FILE_MAX_BYTES, ConsumedProposals, admit_mutation, claimed_read_workspace,
    read_admitted,
};
use onshape_mcp_core::config::{AppConfig, ExternalTokenConfig, ResolvedAuth};
use onshape_mcp_core::goal::{
    Capabilities, DispatchOutcome, GOAL_GRANT_FILE_MAX_BYTES, GoalAdmission, GoalDecision,
    GoalGrant, GoalRejection, admit_goal_mutation, approval_active, goal_read_admitted,
    grant_read_workspace, grant_still_active, is_uuid_v4,
};
use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use std::{fs::OpenOptions, io::Read, time::Duration};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Credential {
    access_token: SecretString,
    expires_at: i64,
    scopes: Vec<String>,
}

fn id(value: &str) -> bool {
    value.len() == 24
        && value
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}
/// Absolute and lexically canonical: no empty, `.` or `..` segments and no
/// trailing slash. Checked on the raw bytes, because `Path` normalizes `.`
/// away. Symlinks are rejected when the file or directory is opened.
fn canonical_absolute(path: &std::path::Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let bytes = path.as_os_str().as_bytes();
    bytes.first() == Some(&b'/')
        && bytes[1..]
            .split(|&c| c == b'/')
            .all(|segment| !matches!(segment, b"" | b"." | b".."))
}

pub fn validate_config(config: &ExternalTokenConfig) -> Result<(), &'static str> {
    if config
        .mutation_approval_file
        .as_ref()
        .is_some_and(|p| !p.is_absolute())
        || [&config.goal_grant_file, &config.dispatch_journal_dir]
            .into_iter()
            .flatten()
            .any(|p| !canonical_absolute(p))
        || config
            .read_api_version
            .as_deref()
            .is_some_and(|v| !["v10", "v16"].contains(&v))
        || !config.file.is_absolute()
        || !id(&config.document_id)
        || !id(&config.workspace_id)
        || !id(&config.element_id)
    {
        return Err("external_token_configuration_invalid");
    }
    // Mandatory approval (contract §8.8): refuse to start without it.
    config
        .require_mutation_approval_file()
        .map_err(|_| "target_configuration_invalid")?;
    // Goal grants need the durable journal (contract §12.1).
    config
        .require_dispatch_journal_dir()
        .map_err(|_| "target_configuration_invalid")?;
    if let Some(policy) = &config.mutation {
        policy
            .validate(
                &config.document_id,
                &config.workspace_id,
                &config.element_id,
            )
            .map_err(|_| "external_token_configuration_invalid")?;
    }
    Ok(())
}

fn private_json(
    file_path: &std::path::Path,
    max_bytes: u64,
) -> Result<serde_json::Value, &'static str> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    let fail = |_| "external_private_file_unavailable";
    if std::fs::canonicalize(file_path).map_err(fail)? != file_path {
        return Err("external_private_file_unavailable");
    }
    let parent = file_path
        .parent()
        .ok_or("external_private_file_unavailable")?;
    let dir = std::fs::symlink_metadata(parent).map_err(fail)?;
    let owner = std::fs::metadata("/proc/self").map_err(fail)?.uid();
    if !dir.is_dir() || dir.uid() != owner || dir.mode() & 0o077 != 0 {
        return Err("external_private_file_unavailable");
    }
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(0x20000)
        .open(file_path)
        .map_err(fail)?;
    let meta = file.metadata().map_err(fail)?;
    if !meta.is_file()
        || meta.nlink() != 1
        || meta.uid() != owner
        || meta.mode() & 0o077 != 0
        || meta.len() > max_bytes
    {
        return Err("external_private_file_unavailable");
    }
    let mut bytes = Vec::new();
    file.by_ref()
        .take(max_bytes + 1)
        .read_to_end(&mut bytes)
        .map_err(fail)?;
    if bytes.len() as u64 > max_bytes {
        return Err("external_private_file_unavailable");
    }
    serde_json::from_slice(&bytes).map_err(|_| "external_private_file_unavailable")
}

fn load(config: &ExternalTokenConfig) -> Result<Credential, &'static str> {
    validate_config(config)?;
    let credential: Credential = serde_json::from_value(
        private_json(&config.file, 16384).map_err(|_| "external_token_unavailable")?,
    )
    .map_err(|_| "external_token_unavailable")?;
    let token = credential.access_token.expose_secret();
    if token.is_empty()
        || token.len() > 8192
        || token.chars().any(char::is_whitespace)
        || !credential.scopes.iter().any(|s| s == "OAuth2Read")
        || credential.scopes.len() > 32
    {
        return Err("external_token_unavailable");
    }
    if credential.expires_at <= Utc::now().timestamp_millis() + 30000 {
        return Err("external_token_expired");
    }
    Ok(credential)
}

pub fn resolved_auth(config: &ExternalTokenConfig) -> ResolvedAuth {
    match load(config) {
        Ok(c) => ResolvedAuth::OAuthReady {
            expires_at: DateTime::from_timestamp_millis(c.expires_at),
        },
        Err(code) => ResolvedAuth::NotConfigured {
            configured_method: onshape_client_core::auth::AuthMethod::OAuth,
            detail: code.into(),
        },
    }
}

/// Fixed read set for the configured target (§2.4), without approval claims.
fn allowed(config: &ExternalTokenConfig, request: &ApiRequest) -> bool {
    read_admitted(config, request, None)
}

/// §2.4 including the claimed Candidate of an active v2 approval, and §12.6
/// the Candidate of an unexpired goal grant. The approval and grant files are
/// read only if the fixed set denies the request; an absent, insecure,
/// unreadable, `null`, invalid or expired file adds nothing.
fn read_allowed(config: &ExternalTokenConfig, request: &ApiRequest, now_ms: i64) -> bool {
    if allowed(config, request) {
        return true;
    }
    if let Some(file) = &config.mutation_approval_file
        && let Ok(approval) = private_json(file, APPROVAL_FILE_MAX_BYTES)
        && claimed_read_workspace(config, &approval, now_ms)
            .is_some_and(|workspace| read_admitted(config, request, Some(&workspace)))
    {
        return true;
    }
    let Some(file) = &config.goal_grant_file else {
        return false;
    };
    let Ok(grant) = private_json(file, GOAL_GRANT_FILE_MAX_BYTES) else {
        return false;
    };
    grant_read_workspace(config, &grant, now_ms)
        .is_some_and(|workspace| goal_read_admitted(config, request, &workspace))
}

/// Where admitted requests go. Production always dispatches to the fixed
/// Onshape origin; tests substitute a recording mock.
#[derive(Clone, Default)]
pub enum Provider {
    /// Single-attempt client for `https://cad.onshape.com`.
    #[default]
    Onshape,
    /// Recording test double; never part of a release build.
    #[cfg(test)]
    Mock(std::sync::Arc<mock::MockProvider>),
}

/// A connected provider for exactly one request.
enum Dispatcher {
    Onshape(OnshapeClient),
    #[cfg(test)]
    Mock(std::sync::Arc<mock::MockProvider>),
}

impl Provider {
    fn connect(
        &self,
        base_url: &str,
        access_token: &SecretString,
        timeout: Duration,
    ) -> Result<Dispatcher, ()> {
        match self {
            Self::Onshape => OnshapeClient::new_single_attempt(ClientConfig {
                base_url: base_url.into(),
                auth: ClientAuthConfig::Bearer {
                    access_token: AccessToken::new(access_token.expose_secret().into()),
                },
                timeout: Some(timeout),
            })
            .map(Dispatcher::Onshape)
            .map_err(|_| ()),
            #[cfg(test)]
            Self::Mock(mock) => {
                mock.connected(base_url);
                Ok(Dispatcher::Mock(std::sync::Arc::clone(mock)))
            }
        }
    }
}

impl Dispatcher {
    /// One attempt; `Err` for any transport failure.
    async fn execute(&self, request: &ApiRequest) -> Result<ApiResponse, ()> {
        match self {
            Self::Onshape(client) => client.execute(request).await.map_err(|_| ()),
            #[cfg(test)]
            Self::Mock(mock) => mock.execute(request),
        }
    }
}

/// Fixed API base for a request.
fn base_url(config: &ExternalTokenConfig, mutation: bool) -> &'static str {
    if mutation || config.read_api_version.as_deref() == Some("v16") {
        "https://cad.onshape.com/api/v16"
    } else {
        "https://cad.onshape.com/api/v10"
    }
}

/// Serializes goal admissions process-wide from the grant read (§12.3 step 2)
/// to the recorded outcome (step 11), so that two concurrent calls cannot both
/// pass the journal checks of step 9.
static GOAL_ADMISSION: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Admit and dispatch one request. With `goal_grant_file`, mutations follow
/// contract §12.3 and fall back to §2.5 only for a `null` grant; otherwise
/// they follow §2.5 steps 1–10 in order. `consumed` is the per-process replay
/// record of approvals.
pub async fn execute(
    config: &ExternalTokenConfig,
    timeout: Duration,
    consumed: &mut ConsumedProposals,
    provider: &Provider,
    request: &ApiRequest,
) -> Result<RawResponse, McpError> {
    validate_config(config).map_err(|code| McpError::invalid_request(code, None))?;
    let mutation = request.method != http::Method::GET;
    let now_ms = Utc::now().timestamp_millis();
    if mutation && config.goal_grant_file.is_some() {
        let _admission = GOAL_ADMISSION.lock().await;
        if let GoalDecision::Admit(admission) =
            admit_goal(config, request, now_ms).map_err(goal_error)?
        {
            return execute_goal(config, timeout, provider, request, &admission).await;
        }
    }
    let admitted = if mutation {
        // Steps 1–8; the approval file is read only after steps 1–2.
        let approval_file = config.mutation_approval_file.as_deref();
        let admitted = admit_mutation(
            config.mutation.as_ref(),
            request,
            || approval_file.and_then(|file| private_json(file, APPROVAL_FILE_MAX_BYTES).ok()),
            now_ms,
            consumed,
        )
        .map_err(|error| McpError::invalid_request(error.code(), None))?;
        Some(admitted)
    } else if read_allowed(config, request, now_ms) {
        None
    } else {
        return Err(McpError::invalid_request(
            "external_token_read_policy_denied",
            None,
        ));
    };
    // Step 9.
    let c = load(config).map_err(|code| McpError::invalid_request(code, None))?;
    if mutation && !c.scopes.iter().any(|scope| scope == "OAuth2Write") {
        return Err(McpError::invalid_request(
            "external_token_write_scope_missing",
            None,
        ));
    }
    // Fixed provider origin. Each dispatch reloads the atomically rotated file.
    let client = provider
        .connect(base_url(config, mutation), &c.access_token, timeout)
        .map_err(|()| McpError::internal_error("external_token_client_unavailable", None))?;
    // Step 10: consumed before the single dispatch attempt, whatever its outcome.
    if let Some(admitted) = admitted {
        consumed.record(admitted);
    }
    let response = client
        .execute(request)
        .await
        .map_err(|()| dispatch_error(mutation))?;
    if mutation
        && (response.status.is_server_error()
            || response.status.is_redirection()
            || response.status == http::StatusCode::REQUEST_TIMEOUT)
    {
        return Err(dispatch_error(true));
    }
    Ok(super::raw_response_from_api_response(response))
}

// Once execution has begun, transport failures cannot prove no provider effect.
// Treat every such mutation failure conservatively; never retry here.
fn dispatch_error(mutation: bool) -> McpError {
    McpError::internal_error(
        if mutation { "mutation_outcome_unknown_reconcile_required" } else { "external_token_request_failed" },
        mutation.then(|| serde_json::json!({"outcome":"UNKNOWN","retryAllowed":false,"reconciliationRequired":true})),
    )
}

/// §12.3 steps 1–9. Files are read only when their step is reached.
fn admit_goal(
    config: &ExternalTokenConfig,
    request: &ApiRequest,
    now_ms: i64,
) -> Result<GoalDecision, GoalRejection> {
    let grant_file = config.goal_grant_file.as_deref();
    let journal = config
        .dispatch_journal_dir
        .as_deref()
        .map(JournalStore::new);
    admit_goal_mutation(
        config.mutation.as_ref(),
        request,
        || grant_file.and_then(|file| private_json(file, GOAL_GRANT_FILE_MAX_BYTES).ok()),
        || {
            config
                .mutation_approval_file
                .as_deref()
                .and_then(|file| private_json(file, APPROVAL_FILE_MAX_BYTES).ok())
                .is_some_and(|approval| approval_active(&approval, now_ms))
        },
        |goal_id| {
            journal
                .as_ref()
                .ok_or(onshape_mcp_core::goal::GoalAdmissionError::JournalUnavailable)?
                .read(goal_id)
        },
        now_ms,
    )
}

fn goal_error(rejection: GoalRejection) -> McpError {
    McpError::invalid_request(
        rejection.error.code(),
        rejection.data.map(|data| data.to_value()),
    )
}

/// The unknown-outcome error (§6) with the goal data of §12.4 merged in.
fn goal_unknown_outcome(admission: &GoalAdmission) -> McpError {
    let mut data = admission.sequence_data().to_value();
    if let Some(object) = data.as_object_mut() {
        object.insert("outcome".into(), "UNKNOWN".into());
        object.insert("retryAllowed".into(), false.into());
        object.insert("reconciliationRequired".into(), true.into());
    }
    McpError::internal_error("mutation_outcome_unknown_reconcile_required", Some(data))
}

/// §12.3 steps 10–11 for an admitted goal request: token, `PENDING` (fsync),
/// grant re-read, one dispatch, outcome (fsync). Never retries.
async fn execute_goal(
    config: &ExternalTokenConfig,
    timeout: Duration,
    provider: &Provider,
    request: &ApiRequest,
    admission: &GoalAdmission,
) -> Result<RawResponse, McpError> {
    let goal_id = admission.grant().goal_id.as_str();
    let request_data = || Some(admission.request_data().to_value());
    // Step 10.
    let c = load(config).map_err(|code| McpError::invalid_request(code, request_data()))?;
    if !c.scopes.iter().any(|scope| scope == "OAuth2Write") {
        return Err(McpError::invalid_request(
            "external_token_write_scope_missing",
            request_data(),
        ));
    }
    let client = provider
        .connect(base_url(config, true), &c.access_token, timeout)
        .map_err(|()| {
            McpError::internal_error("external_token_client_unavailable", request_data())
        })?;
    // Step 11.
    let (Some(grant_file), Some(journal_dir)) = (
        config.goal_grant_file.as_deref(),
        config.dispatch_journal_dir.as_deref(),
    ) else {
        return Err(McpError::invalid_request(
            "target_configuration_invalid",
            None,
        ));
    };
    let journal = JournalStore::new(journal_dir);
    let pending = admission.pending();
    journal.append(goal_id, pending).map_err(|error| {
        McpError::invalid_request(error.code(), Some(admission.sequence_data().to_value()))
    })?;
    // Test seam between the PENDING append and the grant re-read.
    #[cfg(test)]
    if let Provider::Mock(mock) = provider {
        mock.before_recheck();
    }
    let still_active = private_json(grant_file, GOAL_GRANT_FILE_MAX_BYTES)
        .is_ok_and(|grant| grant_still_active(&grant, goal_id, Utc::now().timestamp_millis()));
    if !still_active {
        // If this append fails, the PENDING line stays and blocks the goal as
        // UNCERTAIN until reconciled: fail closed, no dispatch either way.
        let _ = journal.append(
            goal_id,
            &pending.with_outcome(DispatchOutcome::NotDispatched, None),
        );
        return Err(McpError::invalid_request(
            "goal_grant_stopped",
            Some(admission.sequence_data().to_value()),
        ));
    }
    let result = client.execute(request).await;
    let (outcome, status) = match &result {
        Ok(response) => {
            let status = response.status.as_u16();
            (DispatchOutcome::from_status(status), Some(status))
        }
        Err(()) => (DispatchOutcome::Uncertain, None),
    };
    let recorded = journal
        .append(goal_id, &pending.with_outcome(outcome, status))
        .is_ok();
    match result {
        // An unrecorded outcome leaves PENDING (= UNCERTAIN) in the journal,
        // so the caller must reconcile whatever the provider answered.
        Ok(response) if recorded && outcome != DispatchOutcome::Uncertain => {
            Ok(super::raw_response_from_api_response(response))
        }
        _ => Err(goal_unknown_outcome(admission)),
    }
}

/// `GET /capabilities` (§12.7). Local only; no provider I/O.
fn capabilities(config: &ExternalTokenConfig) -> Capabilities {
    let goal = config.goal_grant_file.is_some()
        && config
            .dispatch_journal_dir
            .as_deref()
            .is_some_and(|dir| JournalStore::new(dir).check_dir().is_ok());
    Capabilities::new(config.mutation_approval_file.is_some(), goal)
}

/// `GET /goals/{goalId}/journal-summary` (§12.7). Local only; no provider I/O.
///
/// `404` for a non-uuid-v4 `goalId` or a server without goal configuration;
/// `503` with `dispatch_journal_unavailable` for an unreadable journal.
/// `unreconciledSequence` is computed against `reconciledThrough` of the
/// current grant if it is structurally valid and names this goal, else 0.
fn journal_summary(
    config: &ExternalTokenConfig,
    goal_id: &str,
) -> (http::StatusCode, serde_json::Value) {
    let (Some(grant_file), Some(journal_dir)) = (
        config.goal_grant_file.as_deref(),
        config.dispatch_journal_dir.as_deref(),
    ) else {
        return (http::StatusCode::NOT_FOUND, serde_json::Value::Null);
    };
    if !is_uuid_v4(goal_id) {
        return (http::StatusCode::NOT_FOUND, serde_json::Value::Null);
    }
    let Ok(journal) = JournalStore::new(journal_dir).read(goal_id) else {
        return (
            http::StatusCode::SERVICE_UNAVAILABLE,
            serde_json::json!({"code": "dispatch_journal_unavailable"}),
        );
    };
    let reconciled_through = private_json(grant_file, GOAL_GRANT_FILE_MAX_BYTES)
        .ok()
        .and_then(|value| GoalGrant::parse(&value).ok())
        .filter(|grant| grant.goal_id == goal_id)
        .map_or(0, |grant| grant.reconciled_through);
    (
        http::StatusCode::OK,
        serde_json::to_value(journal.summary(goal_id, reconciled_through))
            .unwrap_or(serde_json::Value::Null),
    )
}

/// The local, unauthenticated routes of channel D and §12.7 (no provider I/O).
fn local_routes(credential: ExternalTokenConfig) -> axum::Router {
    use axum::{
        Json, Router, extract::Path, http::StatusCode, response::IntoResponse, routing::get,
    };
    let ready = credential.clone();
    let caps = credential.clone();
    let summary = credential;
    Router::new()
        .route("/health", get(|| async { StatusCode::OK }))
        .route(
            "/health/ready",
            get(move || {
                let c = ready.clone();
                async move {
                    if load(&c).is_ok() {
                        StatusCode::OK
                    } else {
                        StatusCode::SERVICE_UNAVAILABLE
                    }
                }
            }),
        )
        .route(
            "/capabilities",
            get(move || {
                let c = caps.clone();
                async move { Json(capabilities(&c)) }
            }),
        )
        .route(
            "/goals/{goal_id}/journal-summary",
            get(move |Path(goal_id): Path<String>| {
                let c = summary.clone();
                async move {
                    let (status, body) = journal_summary(&c, &goal_id);
                    if status == StatusCode::NOT_FOUND {
                        return status.into_response();
                    }
                    (status, Json(body)).into_response()
                }
            }),
        )
}

pub async fn run_http(
    name: &str,
    version: &str,
    config: AppConfig,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    use rmcp::transport::streamable_http_server::{
        StreamableHttpService, session::local::LocalSessionManager,
    };
    use std::sync::Arc;
    let host = config.http.host.clone();
    let port = config.http.port;
    let public_url: url::Url = config
        .http
        .public_url
        .as_deref()
        .ok_or("external_token_local_url_required")?
        .parse()?;
    if public_url.scheme() != "http"
        || public_url.path() != "/"
        || public_url.query().is_some()
        || !public_url.username().is_empty()
        || public_url.password().is_some()
    {
        return Err("external_token_local_url_invalid".into());
    }
    let cancel = tokio_util::sync::CancellationToken::new();
    let transport_config = super::streamable_http_server_config(&public_url, cancel.clone())?;
    let server = OnshapeMcpServer::new(name, version, config)?;
    let credential = server
        .config
        .external_token
        .clone()
        .ok_or("external_token_configuration_invalid")?;
    // Clone the existing shared handler: no additional OAuth issuer/proxy/client.
    let validation = server
        .validation
        .shared()
        .ok_or("external_token_validation_unavailable")?;
    let service = StreamableHttpService::new(
        move || {
            Ok(OnshapeMcpServer {
                info: server.info.clone(),
                config: Arc::clone(&server.config),
                spec: Arc::clone(&server.spec),
                api_state: Arc::clone(&server.api_state),
                validation: super::ValidationStore::Shared(Arc::clone(&validation)),
                login_state: Arc::clone(&server.login_state),
                oauth_state: None,
            })
        },
        Arc::new(LocalSessionManager::default()),
        transport_config,
    );
    let app = local_routes(credential).nest_service("/mcp", service);
    let listener = tokio::net::TcpListener::bind((host.as_str(), port)).await?;
    super::serve_until_shutdown(listener, app, cancel).await?;
    Ok(())
}

/// Recording provider double for goal and approval dispatch tests.
#[cfg(test)]
// Test-only: a poisoned test mutex is a test bug.
#[allow(clippy::unwrap_used)]
pub mod mock {
    use onshape_client_core::request::{ApiRequest, ApiResponse, ResponseBody};
    use std::collections::VecDeque;
    use std::sync::Mutex;

    /// Scripted provider answer for one dispatch.
    #[derive(Clone, Copy, Debug)]
    pub enum Reply {
        /// An HTTP response with this status.
        Status(u16),
        /// A transport error (connection reset, timeout).
        Transport,
    }

    type Hook = Box<dyn FnOnce() + Send>;

    /// Records every connection and request; replies from a script. A call
    /// without a scripted reply is recorded and fails as a transport error, so
    /// tests that expect no provider I/O assert `calls().is_empty()`.
    #[derive(Default)]
    pub struct MockProvider {
        replies: Mutex<VecDeque<Reply>>,
        calls: Mutex<Vec<(String, String)>>,
        connections: Mutex<Vec<String>>,
        hook: Mutex<Option<Hook>>,
    }

    impl MockProvider {
        pub fn new(replies: &[Reply]) -> std::sync::Arc<Self> {
            let mock = Self::default();
            mock.replies.lock().unwrap().extend(replies);
            std::sync::Arc::new(mock)
        }

        /// Run `hook` once between the PENDING append and the grant re-read.
        pub fn on_recheck(&self, hook: impl FnOnce() + Send + 'static) {
            *self.hook.lock().unwrap() = Some(Box::new(hook));
        }

        /// `(method, path)` of every dispatched request.
        pub fn calls(&self) -> Vec<(String, String)> {
            self.calls.lock().unwrap().clone()
        }

        /// Base URLs of every connection.
        pub fn connections(&self) -> Vec<String> {
            self.connections.lock().unwrap().clone()
        }

        pub fn connected(&self, base_url: &str) {
            self.connections.lock().unwrap().push(base_url.to_owned());
        }

        pub fn before_recheck(&self) {
            let hook = self.hook.lock().unwrap().take();
            if let Some(hook) = hook {
                hook();
            }
        }

        pub fn execute(&self, request: &ApiRequest) -> Result<ApiResponse, ()> {
            self.calls
                .lock()
                .unwrap()
                .push((request.method.to_string(), request.path.clone()));
            let reply = self.replies.lock().unwrap().pop_front();
            match reply {
                Some(Reply::Status(status)) => Ok(ApiResponse {
                    status: http::StatusCode::from_u16(status).unwrap(),
                    headers: http::HeaderMap::new(),
                    body: ResponseBody::from("{}"),
                }),
                Some(Reply::Transport) | None => Err(()),
            }
        }
    }
}

#[cfg(test)]
mod goal_behaviour;

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    fn config(file: std::path::PathBuf) -> ExternalTokenConfig {
        ExternalTokenConfig {
            file,
            document_id: "a".repeat(24),
            workspace_id: "b".repeat(24),
            element_id: "c".repeat(24),
            mutation: None,
            mutation_approval_file: None,
            goal_grant_file: None,
            dispatch_journal_dir: None,
            read_api_version: None,
        }
    }
    fn publish(file: &std::path::Path, value: &serde_json::Value) {
        std::fs::write(file, value.to_string()).unwrap();
        std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    #[test]
    fn credentials_missing_expired_rotated_and_refresh_isolated() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let c = config(dir.path().join("token.json"));
        assert!(load(&c).is_err());
        let mut value = serde_json::json!({"accessToken":"secret-a", "expiresAt":Utc::now().timestamp_millis()+120_000, "scopes":["OAuth2Read"]});
        publish(&c.file, &value);
        assert_eq!(load(&c).unwrap().access_token.expose_secret(), "secret-a");
        value["accessToken"] = "secret-b".into();
        let rotated = dir.path().join("rotated");
        publish(&rotated, &value);
        std::fs::rename(rotated, &c.file).unwrap();
        assert_eq!(load(&c).unwrap().access_token.expose_secret(), "secret-b");
        value["refreshToken"] = "never-accepted".into();
        publish(&c.file, &value);
        assert!(load(&c).is_err());
        value.as_object_mut().unwrap().remove("refreshToken");
        value["expiresAt"] = 1.into();
        publish(&c.file, &value);
        assert_eq!(load(&c).err(), Some("external_token_expired"));
    }
    #[test]
    fn wrong_permissions_symlink_and_secret_errors() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let c = config(dir.path().join("token.json"));
        publish(
            &c.file,
            &serde_json::json!({"accessToken":"secret-a","expiresAt":Utc::now().timestamp_millis()+120_000,"scopes":["OAuth2Read"]}),
        );
        std::fs::set_permissions(&c.file, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(load(&c).err(), Some("external_token_unavailable"));
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&c.file, &link).unwrap();
        assert!(load(&config(link)).is_err());
    }
    #[test]
    fn ambiguous_write_is_explicit_and_never_retryable() {
        let error = dispatch_error(true);
        assert_eq!(
            error.data.unwrap(),
            serde_json::json!({"outcome":"UNKNOWN","retryAllowed":false,"reconciliationRequired":true})
        );
        assert!(dispatch_error(false).data.is_none());
    }
    #[tokio::test]
    async fn write_rejected_before_credentials_or_provider_io() {
        let c = config("/nonexistent/token.json".into());
        let r = ApiRequest {
            method: http::Method::POST,
            path: "/anything".into(),
            query_params: vec![],
            headers: http::HeaderMap::new(),
            body: None,
            content_type: None,
        };
        let error = execute(
            &c,
            Duration::from_secs(1),
            &mut ConsumedProposals::default(),
            &Provider::Onshape,
            &r,
        )
        .await
        .err()
        .unwrap();
        assert_eq!(error.message, "target_policy_denied");
    }
    #[test]
    fn requests_cannot_escape_target_or_write() {
        let c = config("/run/onshape/token.json".into());
        let mut r = ApiRequest {
            method: http::Method::GET,
            path: format!(
                "/partstudios/d/{}/w/{}/e/{}/features",
                c.document_id, c.workspace_id, c.element_id
            ),
            query_params: vec![],
            headers: http::HeaderMap::new(),
            body: None,
            content_type: None,
        };
        assert!(allowed(&c, &r));
        r.method = http::Method::POST;
        assert!(!allowed(&c, &r));
        r.method = http::Method::GET;
        r.path = r.path.replace(&c.workspace_id, &"d".repeat(24));
        assert!(!allowed(&c, &r));
        r.path = format!(
            "/documents/d/{}/m/{}/elements",
            c.document_id,
            "e".repeat(24)
        );
        assert!(allowed(&c, &r));
        r.path.push_str("/../workspaces");
        assert!(!allowed(&c, &r));
    }

    #[test]
    fn catalog_built_feature_read_accepts_json_negotiation_only() {
        let c = config("/nonexistent/token.json".into());
        let spec = openapi_mcp_spec::OpenApiSpec::from_json_with_server_url_fallback(
            crate::OPENAPI_SPEC_JSON,
            crate::OPENAPI_SERVER_URL_FALLBACK,
        )
        .unwrap();
        let r = spec
            .build_request(
                "getPartStudioFeatures",
                &std::collections::HashMap::from([
                    ("did".into(), c.document_id.clone()),
                    ("wvm".into(), "w".into()),
                    ("wvmid".into(), c.workspace_id.clone()),
                    ("eid".into(), c.element_id.clone()),
                ]),
                &std::collections::HashMap::new(),
                &http::HeaderMap::new(),
                None,
            )
            .unwrap();
        let mut r = crate::request::into_onshape_request(r);
        assert_eq!(
            r.headers.get(http::header::ACCEPT).unwrap(),
            "application/json"
        );
        assert!(allowed(&c, &r));
        r.headers.insert(
            http::header::AUTHORIZATION,
            http::HeaderValue::from_static("Bearer caller-controlled"),
        );
        assert!(!allowed(&c, &r));
        r.headers.remove(http::header::AUTHORIZATION);
        r.headers
            .insert(http::header::ACCEPT, http::HeaderValue::from_static("*/*"));
        assert!(!allowed(&c, &r));
    }
    fn private_dir() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        dir
    }
    fn update_body() -> serde_json::Value {
        serde_json::json!({"feature":{"featureId":"plate"},"sourceMicroversion":"e".repeat(24),"rejectMicroversionSkew":true})
    }
    fn update(workspace: &str) -> ApiRequest {
        ApiRequest {
            method: http::Method::POST,
            path: format!(
                "/partstudios/d/{}/w/{workspace}/e/{}/features/featureid/plate",
                "a".repeat(24),
                "c".repeat(24)
            ),
            query_params: vec![],
            headers: http::HeaderMap::new(),
            body: Some(onshape_client_core::request::RequestBody::Json(
                update_body(),
            )),
            content_type: None,
        }
    }
    fn v1(expires_at: i64) -> serde_json::Value {
        serde_json::json!({"version":1,"proposalId":"reviewed-proposal","expiresAt":expires_at,"body":update_body()})
    }
    fn v2(workspace: &str, now: i64) -> serde_json::Value {
        serde_json::json!({"version":2,"proposalId":"7c1d2e3f-4a5b-4c6d-8e7f-9a0b1c2d3e4f",
            "issuedAt":now - 1_000,"expiresAt":now + 60_000,
            "target":{"documentId":"a".repeat(24),"workspaceId":workspace,"elementId":"c".repeat(24)},
            "featureId":"plate","expectedMicroversion":"e".repeat(24),
            "operation":"updatePartStudioFeature","requestHash":"fake","payloadHash":"fake",
            "body":update_body()})
    }
    /// The deployed shape: static pins on the read binding, v1 approvals.
    /// The token file never exists, so a request that passes admission stops
    /// at step 9 with `external_token_unavailable` and never reaches a client.
    fn mutation_config(dir: &std::path::Path, pinned: bool) -> ExternalTokenConfig {
        let mut c = config(dir.join("missing-token.json"));
        let mut bounds = bounds();
        if pinned {
            bounds.workspace_id = Some("b".repeat(24).try_into().unwrap());
            bounds.expected_microversion = Some("e".repeat(24).try_into().unwrap());
        }
        c.mutation = Some(bounds);
        c.mutation_approval_file = Some(dir.join("approved-update.json"));
        c
    }
    async fn run(
        c: &ExternalTokenConfig,
        consumed: &mut ConsumedProposals,
        r: &ApiRequest,
    ) -> String {
        execute(c, Duration::from_secs(1), consumed, &Provider::Onshape, r)
            .await
            .err()
            .unwrap()
            .message
            .into_owned()
    }
    #[tokio::test]
    async fn v1_approval_flow_with_deployed_pins_is_unchanged() {
        let dir = private_dir();
        let c = mutation_config(dir.path(), true);
        let file = c.mutation_approval_file.clone().unwrap();
        let now = Utc::now().timestamp_millis();
        let mut consumed = ConsumedProposals::default();
        let r = update(&"b".repeat(24));
        assert_eq!(
            run(&c, &mut consumed, &r).await,
            "mutation_approval_unavailable"
        );
        publish(&file, &v1(now + 60_000));
        assert_eq!(
            run(&c, &mut consumed, &r).await,
            "external_token_unavailable"
        );
        // A failed step 9 does not consume the approval.
        assert!(consumed.is_empty());
        assert_eq!(
            run(&c, &mut consumed, &r).await,
            "external_token_unavailable"
        );
        publish(&file, &v1(now - 1));
        assert_eq!(
            run(&c, &mut consumed, &r).await,
            "mutation_approval_mismatch"
        );
        publish(&file, &serde_json::Value::Null);
        assert_eq!(
            run(&c, &mut consumed, &r).await,
            "mutation_approval_mismatch"
        );
        let mut other = v1(now + 60_000);
        other["body"]["sourceMicroversion"] = "f".repeat(24).into();
        publish(&file, &other);
        assert_eq!(
            run(&c, &mut consumed, &r).await,
            "mutation_approval_mismatch"
        );
        // Insecure file.
        publish(&file, &v1(now + 60_000));
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(
            run(&c, &mut consumed, &r).await,
            "mutation_approval_unavailable"
        );
        // Static checks (steps 1–2) still win before the file is read.
        let mut r = update(&"b".repeat(24));
        r.query_params.push(("x".into(), "1".into()));
        assert_eq!(run(&c, &mut consumed, &r).await, "target_policy_denied");
    }
    #[tokio::test]
    async fn v2_claims_admit_candidate_and_v1_needs_pins() {
        let dir = private_dir();
        let c = mutation_config(dir.path(), false);
        let file = c.mutation_approval_file.clone().unwrap();
        let now = Utc::now().timestamp_millis();
        let candidate = "9".repeat(24);
        let mut consumed = ConsumedProposals::default();
        publish(&file, &v2(&candidate, now));
        assert_eq!(
            run(&c, &mut consumed, &update(&candidate)).await,
            "external_token_unavailable"
        );
        assert_eq!(
            run(&c, &mut consumed, &update(&"b".repeat(24))).await,
            "target_policy_denied"
        );
        // An excluded (Primary) workspace claim never passes.
        publish(&file, &v2(&"d".repeat(24), now));
        assert_eq!(
            run(&c, &mut consumed, &update(&"d".repeat(24))).await,
            "target_policy_denied"
        );
        publish(&file, &v1(now + 60_000));
        assert_eq!(
            run(&c, &mut consumed, &update(&"b".repeat(24))).await,
            "mutation_approval_mismatch"
        );
    }
    #[tokio::test]
    async fn replayed_approval_is_consumed_before_token_or_dispatch() {
        let dir = private_dir();
        let now = Utc::now().timestamp_millis();
        for (pinned, approval, workspace) in [
            (true, v1(now + 60_000), "b".repeat(24)),
            (false, v2(&"9".repeat(24), now), "9".repeat(24)),
        ] {
            let c = mutation_config(dir.path(), pinned);
            let file = c.mutation_approval_file.clone().unwrap();
            publish(&file, &approval);
            let r = update(&workspace);
            // Simulate the first, dispatched admission of this process: the
            // real dispatch target is fixed to the provider, so record the
            // admitted proposal directly as execute does before dispatch.
            let mut consumed = ConsumedProposals::default();
            let admitted = admit_mutation(
                c.mutation.as_ref(),
                &r,
                || Some(approval.clone()),
                now,
                &consumed,
            )
            .unwrap();
            consumed.record(admitted);
            // The token file is missing: reaching step 9 would yield
            // external_token_unavailable instead.
            assert_eq!(
                run(&c, &mut consumed, &r).await,
                "mutation_approval_consumed"
            );
            assert_eq!(consumed.len(), 1);
        }
    }
    #[test]
    fn claimed_candidate_reads_follow_the_approval_file() {
        let dir = private_dir();
        let c = mutation_config(dir.path(), false);
        let file = c.mutation_approval_file.clone().unwrap();
        let now = Utc::now().timestamp_millis();
        let candidate = "9".repeat(24);
        let get = |path: String| ApiRequest {
            method: http::Method::GET,
            path,
            query_params: vec![],
            headers: http::HeaderMap::new(),
            body: None,
            content_type: None,
        };
        let (d, e) = ("a".repeat(24), "c".repeat(24));
        let features = get(format!("/partstudios/d/{d}/w/{candidate}/e/{e}/features"));
        let microversion = get(format!(
            "/documents/d/{d}/w/{candidate}/currentmicroversion"
        ));
        let elements = get(format!("/documents/d/{d}/w/{candidate}/elements"));
        // Missing file.
        assert!(!read_allowed(&c, &features, now));
        publish(&file, &v2(&candidate, now));
        assert!(read_allowed(&c, &features, now));
        assert!(read_allowed(&c, &microversion, now));
        assert!(!read_allowed(&c, &elements, now));
        // Expired.
        assert!(!read_allowed(&c, &features, now + 60_000));
        // Unreadable (insecure mode) and null.
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(!read_allowed(&c, &features, now));
        publish(&file, &serde_json::Value::Null);
        assert!(!read_allowed(&c, &features, now));
        std::fs::write(&file, b"not json").unwrap();
        assert!(!read_allowed(&c, &features, now));
        // Configured reads never depend on the approval file.
        assert!(read_allowed(
            &c,
            &get(format!(
                "/partstudios/d/{d}/w/{}/e/{e}/features",
                "b".repeat(24)
            )),
            now
        ));
    }
    #[test]
    fn approval_file_permissions_and_read_version_are_validated() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let file = dir.path().join("approval.json");
        publish(&file, &serde_json::json!({"version":1}));
        assert!(private_json(&file, 1024).is_ok());
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(private_json(&file, 1024).is_err());
        let mut c = config(dir.path().join("token.json"));
        c.read_api_version = Some("v16".into());
        assert!(validate_config(&c).is_ok());
        c.read_api_version = Some("v17".into());
        assert!(validate_config(&c).is_err());
        c.read_api_version = None;
        c.mutation_approval_file = Some("relative.json".into());
        assert!(validate_config(&c).is_err());
    }
    fn bounds() -> onshape_mcp_core::target_policy::TargetConstraint {
        serde_json::from_value(serde_json::json!({"document_id":"a".repeat(24),
            "element_id":"c".repeat(24),"excluded_workspace_ids":["d".repeat(24)],
            "allowed_operations":["updatePartStudioFeature"],"write_mode":"modify"}))
        .unwrap()
    }
    #[test]
    fn mutation_without_approval_file_refuses_to_start() {
        let mut c = config("/run/onshape/token.json".into());
        c.mutation = Some(bounds());
        assert_eq!(validate_config(&c), Err("target_configuration_invalid"));
        c.mutation_approval_file = Some("/run/onshape/approved-update.json".into());
        assert_eq!(validate_config(&c), Ok(()));
        // Unpinned bounds no longer require the read workspace to match.
        c.workspace_id = "d".repeat(24);
        assert_eq!(validate_config(&c), Ok(()));
    }
}
