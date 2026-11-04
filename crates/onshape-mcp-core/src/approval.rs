//! Pure external-token admission.
//!
//! Covers the approval file (contract §4, v1 and v2), the mutation admission
//! order (§2.5 steps 1–8), the per-process replay record, and the read set
//! including claimed Candidate reads (§2.4).
//!
//! No I/O happens here. The I/O layer reads the private approval file and the
//! credential, and dispatches only an [`AdmittedProposal`].
use std::collections::{HashSet, VecDeque};

use onshape_client_core::request::{ApiRequest, RequestBody};
use serde::Deserialize;

use crate::config::ExternalTokenConfig;
use crate::target_policy::{
    CadId, MutationClaims, PolicyError, TargetConstraint, permits_json_response_headers,
};

/// Maximum v2 approval lifetime `expiresAt − issuedAt` in ms (15 min, §4).
pub const APPROVAL_MAX_LIFETIME_MS: i64 = 900_000;

/// Maximum approval file size in bytes (§4, §7).
pub const APPROVAL_FILE_MAX_BYTES: u64 = 1_048_576;

/// Number of admitted `proposalId`s the replay record remembers.
///
/// The plugin issues at most one approval at a time and blocks new ones until
/// the previous receipt is reconciled (§4 "One-at-a-time"); a v2 approval
/// lives at most 15 minutes. Evicting an ID would take more than 1024
/// admissions within one approval lifetime (over one per second), which the
/// workflow cannot produce. Even after eviction, an expired approval fails at
/// step 4 and the revision CAS (step 6) rejects a second application of the
/// same body. The bound keeps memory at a few tens of KiB.
pub const CONSUMED_PROPOSALS_CAPACITY: usize = 1024;

/// Stable, non-secret mutation admission failures, in contract vocabulary.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum AdmissionError {
    /// Target or revision policy violation (§2.5 steps 1, 2, 5, 6).
    #[error("{0}")]
    Policy(#[from] PolicyError),
    /// Approval file absent, insecure or unreadable (step 3).
    #[error("mutation_approval_unavailable")]
    ApprovalUnavailable,
    /// Approval `null`, expired, structurally invalid or body differs (steps 4, 7).
    #[error("mutation_approval_mismatch")]
    ApprovalMismatch,
    /// The `proposalId` was already admitted by this process (step 8).
    #[error("mutation_approval_consumed")]
    ApprovalConsumed,
}

impl AdmissionError {
    /// Every variant, for contract code-list checks.
    pub const ALL: [Self; 6] = [
        Self::Policy(PolicyError::InvalidConfiguration),
        Self::Policy(PolicyError::Denied),
        Self::Policy(PolicyError::Revision),
        Self::ApprovalUnavailable,
        Self::ApprovalMismatch,
        Self::ApprovalConsumed,
    ];

    /// The contract error code returned to MCP clients.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Policy(PolicyError::InvalidConfiguration) => "target_configuration_invalid",
            Self::Policy(PolicyError::Denied) => "target_policy_denied",
            Self::Policy(PolicyError::Revision) => "revision_policy_denied",
            Self::ApprovalUnavailable => "mutation_approval_unavailable",
            Self::ApprovalMismatch => "mutation_approval_mismatch",
            Self::ApprovalConsumed => "mutation_approval_consumed",
        }
    }
}

/// Claimed target of a v2 approval.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ApprovalTarget {
    /// Claimed document.
    pub document_id: CadId,
    /// Claimed (Candidate) workspace.
    pub workspace_id: CadId,
    /// Claimed Part Studio.
    pub element_id: CadId,
}

/// Approval file v2 (§4). All fields are required; unknown fields are rejected.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ApprovalV2 {
    /// Always 2.
    pub version: u8,
    /// Non-empty proposal identity; the replay key.
    pub proposal_id: String,
    /// Issuance of the approval proof (Unix ms).
    pub issued_at: i64,
    /// Expiry (Unix ms); at most [`APPROVAL_MAX_LIFETIME_MS`] after `issued_at`.
    pub expires_at: i64,
    /// Claimed target.
    pub target: ApprovalTarget,
    /// Claimed feature.
    pub feature_id: String,
    /// Claimed provider-side baseline.
    pub expected_microversion: CadId,
    /// Claimed operation; checked against `allowed_operations` at step 5.
    pub operation: String,
    /// Audit field for the plugin's receipts; ignored by the server (G4).
    pub request_hash: String,
    /// Audit field for the plugin's receipts; ignored by the server (G4).
    pub payload_hash: String,
    /// Exact approved request body.
    pub body: serde_json::Value,
}

impl ApprovalV2 {
    /// The claims to check against the static bounds.
    #[must_use]
    pub fn claims(&self) -> MutationClaims<'_> {
        MutationClaims {
            document_id: &self.target.document_id,
            workspace_id: &self.target.workspace_id,
            element_id: &self.target.element_id,
            feature_id: Some(&self.feature_id),
            expected_microversion: &self.expected_microversion,
            operation: &self.operation,
        }
    }
}

/// A structurally valid, unexpired approval (§2.5 step 4).
#[derive(Clone, Debug)]
pub enum Approval {
    /// Deprecated v1: identity, expiry and body only; target from static pins.
    V1 {
        /// Non-empty proposal identity.
        proposal_id: String,
        /// Exact approved request body.
        body: serde_json::Value,
    },
    /// v2 with target claims.
    V2(Box<ApprovalV2>),
}

impl Approval {
    /// Parse and check an approval file value at `now_ms` (§2.5 step 4).
    ///
    /// v1 keeps its pre-1.0.0 checks (`version`, `expiresAt > now`, non-empty
    /// `proposalId`, `body` present). v2 is strict: all fields, no unknown
    /// fields, 24-hex IDs, `expiresAt > now` and `0 < expiresAt − issuedAt ≤ 900 000`.
    ///
    /// # Errors
    /// [`AdmissionError::ApprovalMismatch`] for `null`, unknown versions,
    /// structural errors and expired or over-long approvals.
    pub fn parse(value: &serde_json::Value, now_ms: i64) -> Result<Self, AdmissionError> {
        match value.get("version").and_then(serde_json::Value::as_u64) {
            Some(1) => {
                let proposal_id = value
                    .get("proposalId")
                    .and_then(serde_json::Value::as_str)
                    .filter(|id| !id.is_empty())
                    .ok_or(AdmissionError::ApprovalMismatch)?;
                let unexpired = value
                    .get("expiresAt")
                    .and_then(serde_json::Value::as_i64)
                    .is_some_and(|t| t > now_ms);
                let body = value.get("body").ok_or(AdmissionError::ApprovalMismatch)?;
                if !unexpired {
                    return Err(AdmissionError::ApprovalMismatch);
                }
                Ok(Self::V1 {
                    proposal_id: proposal_id.to_owned(),
                    body: body.clone(),
                })
            }
            Some(2) => {
                let approval =
                    ApprovalV2::deserialize(value).map_err(|_| AdmissionError::ApprovalMismatch)?;
                let lifetime = approval.expires_at.checked_sub(approval.issued_at);
                if approval.version != 2
                    || approval.proposal_id.is_empty()
                    || approval.expires_at <= now_ms
                    || !lifetime.is_some_and(|l| 0 < l && l <= APPROVAL_MAX_LIFETIME_MS)
                {
                    return Err(AdmissionError::ApprovalMismatch);
                }
                Ok(Self::V2(Box::new(approval)))
            }
            _ => Err(AdmissionError::ApprovalMismatch),
        }
    }

    /// The replay key.
    #[must_use]
    pub fn proposal_id(&self) -> &str {
        match self {
            Self::V1 { proposal_id, .. } => proposal_id,
            Self::V2(approval) => &approval.proposal_id,
        }
    }

    /// The exact approved request body.
    #[must_use]
    pub fn body(&self) -> &serde_json::Value {
        match self {
            Self::V1 { body, .. } => body,
            Self::V2(approval) => &approval.body,
        }
    }
}

/// A `proposalId` that passed §2.5 steps 1–8. Only [`admit_mutation`] creates
/// one; record it with [`ConsumedProposals::record`] right before dispatch.
#[derive(Debug, PartialEq, Eq)]
pub struct AdmittedProposal(String);

impl AdmittedProposal {
    /// The admitted `proposalId`.
    #[must_use]
    pub fn proposal_id(&self) -> &str {
        &self.0
    }
}

/// Per-process, non-persisted record of admitted `proposalId`s (v1 and v2),
/// bounded to [`CONSUMED_PROPOSALS_CAPACITY`] entries, oldest evicted first.
#[derive(Clone, Debug, Default)]
pub struct ConsumedProposals {
    order: VecDeque<String>,
    ids: HashSet<String>,
}

impl ConsumedProposals {
    /// Whether `proposal_id` was already admitted by this process.
    #[must_use]
    pub fn contains(&self, proposal_id: &str) -> bool {
        self.ids.contains(proposal_id)
    }

    /// Mark an admitted proposal as consumed (§2.5 step 10, before dispatch).
    pub fn record(&mut self, admitted: AdmittedProposal) {
        if !self.ids.insert(admitted.0.clone()) {
            return;
        }
        self.order.push_back(admitted.0);
        while self.order.len() > CONSUMED_PROPOSALS_CAPACITY {
            if let Some(oldest) = self.order.pop_front() {
                self.ids.remove(&oldest);
            }
        }
    }

    /// Number of remembered proposals.
    #[must_use]
    pub fn len(&self) -> usize {
        self.order.len()
    }

    /// Whether no proposal was admitted yet.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.order.is_empty()
    }
}

/// Evaluate §2.5 steps 1–8 for a non-GET request, first failure wins.
///
/// `read_approval` is called only after steps 1–2 passed and returns the
/// approval file's JSON, or `None` if it is unconfigured, insecure or
/// unreadable (step 3). Steps 9 (token) and 10 (record, dispatch) belong to
/// the caller and must follow in that order.
///
/// # Errors
/// The [`AdmissionError`] of the first failing step.
pub fn admit_mutation<F>(
    policy: Option<&TargetConstraint>,
    request: &ApiRequest,
    read_approval: F,
    now_ms: i64,
    consumed: &ConsumedProposals,
) -> Result<AdmittedProposal, AdmissionError>
where
    F: FnOnce() -> Option<serde_json::Value>,
{
    let policy = policy.ok_or(PolicyError::Denied)?;
    policy.check_request_shape(request)?;
    let value = read_approval().ok_or(AdmissionError::ApprovalUnavailable)?;
    let approval = Approval::parse(&value, now_ms)?;
    let claims = match &approval {
        // v1 is admitted only with the static pins (§2.5, §4).
        Approval::V1 { .. } => policy
            .pinned_claims()
            .ok_or(AdmissionError::ApprovalMismatch)?,
        Approval::V2(v2) => v2.claims(),
    };
    policy.authorize_claims(request, &claims)?;
    if !matches!(&request.body, Some(RequestBody::Json(body)) if body == approval.body()) {
        return Err(AdmissionError::ApprovalMismatch);
    }
    if consumed.contains(approval.proposal_id()) {
        return Err(AdmissionError::ApprovalConsumed);
    }
    Ok(AdmittedProposal(approval.proposal_id().to_owned()))
}

/// The Candidate workspace a valid, active v2 approval claims, if it lies
/// within the static mutation bounds (§2.4, §8.9).
///
/// `None` without `[external_token.mutation]`, for `null`, v1, expired or
/// invalid approvals, and for claims outside the configured document and
/// element, on an excluded workspace, or differing from a workspace pin.
/// Consumption does not matter: a consumed approval still enables the reads
/// needed to verify the Candidate.
#[must_use]
pub fn claimed_read_workspace(
    config: &ExternalTokenConfig,
    approval: &serde_json::Value,
    now_ms: i64,
) -> Option<CadId> {
    let policy = config.mutation.as_ref()?;
    let Ok(Approval::V2(approval)) = Approval::parse(approval, now_ms) else {
        return None;
    };
    let target = &approval.target;
    let within_bounds = target.document_id.as_str() == config.document_id
        && target.element_id.as_str() == config.element_id
        && target.document_id == policy.document_id
        && target.element_id == policy.element_id
        && !policy.excluded_workspace_ids.contains(&target.workspace_id)
        && policy
            .workspace_id
            .as_ref()
            .is_none_or(|pin| pin == &target.workspace_id);
    within_bounds.then(|| target.workspace_id.clone())
}

/// External-token read admission (§2.4).
///
/// Requires `GET`, no body, no query parameters, JSON response headers only,
/// and a path of the fixed read set for the configured target. With `claimed_workspace` (from
/// [`claimed_read_workspace`]) the Candidate's `currentmicroversion` and
/// `features` paths are admitted too; `{d}` and `{e}` stay configured.
#[must_use]
pub fn read_admitted(
    config: &ExternalTokenConfig,
    request: &ApiRequest,
    claimed_workspace: Option<&CadId>,
) -> bool {
    if request.method != http::Method::GET
        || request.body.is_some()
        || !permits_json_response_headers(&request.headers)
        || !request.query_params.is_empty()
    {
        return false;
    }
    let d = &config.document_id;
    let w = &config.workspace_id;
    let e = &config.element_id;
    let fixed = [
        format!("/documents/{d}"),
        format!("/documents/d/{d}/workspaces"),
        format!("/documents/d/{d}/w/{w}/currentmicroversion"),
        format!("/documents/d/{d}/w/{w}/elements"),
        format!("/partstudios/d/{d}/w/{w}/e/{e}/features"),
    ];
    if fixed.contains(&request.path) {
        return true;
    }
    if let Some(claimed) = claimed_workspace {
        let c = claimed.as_str();
        if request.path == format!("/documents/d/{d}/w/{c}/currentmicroversion")
            || request.path == format!("/partstudios/d/{d}/w/{c}/e/{e}/features")
        {
            return true;
        }
    }
    let parts: Vec<_> = request.path.split('/').collect();
    (parts.len() == 7
        && parts[1..4] == ["documents", "d", d]
        && parts[4] == "m"
        && CadId::is_valid(parts[5])
        && parts[6] == "elements")
        || (parts.len() == 9
            && parts[1..4] == ["partstudios", "d", d]
            && parts[4] == "m"
            && CadId::is_valid(parts[5])
            && parts[6] == "e"
            && parts[7] == e
            && parts[8] == "features")
}

#[cfg(test)]
// Test-only: fixture construction failures are test bugs (same as target_policy).
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use serde_json::json;

    fn admitted(id: &str) -> AdmittedProposal {
        AdmittedProposal(id.to_owned())
    }

    #[test]
    fn replay_record_is_bounded_and_evicts_oldest_first() {
        let mut consumed = ConsumedProposals::default();
        assert!(consumed.is_empty());
        for i in 0..CONSUMED_PROPOSALS_CAPACITY {
            consumed.record(admitted(&format!("p{i}")));
        }
        consumed.record(admitted("p1"));
        assert_eq!(consumed.len(), CONSUMED_PROPOSALS_CAPACITY);
        consumed.record(admitted("new"));
        assert_eq!(consumed.len(), CONSUMED_PROPOSALS_CAPACITY);
        assert!(!consumed.contains("p0"));
        assert!(consumed.contains("p1"));
        assert!(consumed.contains("new"));
    }

    fn v2(now: i64) -> serde_json::Value {
        json!({"version":2,"proposalId":"p","issuedAt":now,"expiresAt":now + 1,
            "target":{"documentId":"a".repeat(24),"workspaceId":"b".repeat(24),
                "elementId":"c".repeat(24)},
            "featureId":"f","expectedMicroversion":"e".repeat(24),
            "operation":"updatePartStudioFeature","requestHash":"r","payloadHash":"h",
            "body":{}})
    }

    #[test]
    fn v2_structure_and_lifetime_fail_closed() {
        let now = 1_000;
        assert!(matches!(
            Approval::parse(&v2(now), now),
            Ok(Approval::V2(_))
        ));
        let mutations: [(&str, serde_json::Value); 9] = [
            ("proposalId", json!("")),
            ("issuedAt", json!(now + 1)),
            ("issuedAt", json!(now - APPROVAL_MAX_LIFETIME_MS)),
            ("issuedAt", json!(i64::MIN)),
            ("expiresAt", json!(now)),
            ("expectedMicroversion", json!("E".repeat(24))),
            ("target", json!({"documentId":"a".repeat(24)})),
            ("requestHash", json!(null)),
            ("unknown", json!(true)),
        ];
        for (key, bad) in mutations {
            let mut value = v2(now);
            value[key] = bad;
            assert_eq!(
                Approval::parse(&value, now - 1).err(),
                Some(AdmissionError::ApprovalMismatch),
                "{key}"
            );
        }
        // Exactly 15 minutes is the longest admitted lifetime.
        let mut value = v2(now);
        value["expiresAt"] = json!(now + APPROVAL_MAX_LIFETIME_MS);
        assert!(Approval::parse(&value, now).is_ok());
        let mut value = v2(now);
        value["target"]["unknown"] = json!(1);
        assert!(Approval::parse(&value, now).is_err());
    }

    #[test]
    fn v1_keeps_its_pre_1_0_checks() {
        let ok = json!({"version":1,"proposalId":"p","expiresAt":2_000,"body":{"x":1}});
        let parsed = Approval::parse(&ok, 1_000).unwrap();
        assert_eq!(parsed.proposal_id(), "p");
        assert_eq!(parsed.body(), &json!({"x":1}));
        for (key, bad) in [
            ("proposalId", json!("")),
            ("proposalId", json!(1)),
            ("expiresAt", json!(1_000)),
            ("expiresAt", json!("2000")),
        ] {
            let mut value = ok.clone();
            value[key] = bad;
            assert!(Approval::parse(&value, 1_000).is_err(), "{key}");
        }
        let mut value = ok;
        value.as_object_mut().unwrap().remove("body");
        assert!(Approval::parse(&value, 1_000).is_err());
        assert!(Approval::parse(&json!(null), 1_000).is_err());
        assert!(Approval::parse(&json!({"version":"1"}), 1_000).is_err());
    }

    fn request() -> ApiRequest {
        ApiRequest {
            method: http::Method::POST,
            path: format!(
                "/partstudios/d/{}/w/{}/e/{}/features/featureid/f",
                "a".repeat(24),
                "b".repeat(24),
                "c".repeat(24)
            ),
            query_params: vec![],
            headers: http::HeaderMap::new(),
            content_type: None,
            body: Some(RequestBody::Json(json!({"feature":{"featureId":"f"},
                "sourceMicroversion":"e".repeat(24),"rejectMicroversionSkew":true}))),
        }
    }

    fn bounds() -> TargetConstraint {
        serde_json::from_value(json!({"document_id":"a".repeat(24),
            "element_id":"c".repeat(24),"excluded_workspace_ids":["d".repeat(24)],
            "allowed_operations":["updatePartStudioFeature"],"write_mode":"modify"}))
        .unwrap()
    }

    #[test]
    fn approval_is_read_only_after_static_request_checks() {
        let consumed = ConsumedProposals::default();
        let reads = std::cell::Cell::new(0);
        let read = || {
            reads.set(reads.get() + 1);
            None
        };
        // Step 1: no mutation policy.
        assert_eq!(
            admit_mutation(None, &request(), read, 0, &consumed).err(),
            Some(AdmissionError::Policy(PolicyError::Denied))
        );
        // Step 2: wrong method.
        let mut r = request();
        r.method = http::Method::DELETE;
        assert_eq!(
            admit_mutation(Some(&bounds()), &r, read, 0, &consumed).err(),
            Some(AdmissionError::Policy(PolicyError::Denied))
        );
        assert_eq!(reads.get(), 0);
        // Step 3: unavailable.
        assert_eq!(
            admit_mutation(Some(&bounds()), &request(), read, 0, &consumed).err(),
            Some(AdmissionError::ApprovalUnavailable)
        );
        assert_eq!(reads.get(), 1);
    }

    #[test]
    fn admitted_v2_body_must_match_and_is_consumed_once() {
        let now = 1_000;
        let mut approval = v2(now);
        approval["body"] = json!({"feature":{"featureId":"f"},
            "sourceMicroversion":"e".repeat(24),"rejectMicroversionSkew":true});
        approval["featureId"] = json!("f");
        let mut consumed = ConsumedProposals::default();
        let admit = |consumed: &ConsumedProposals, approval: &serde_json::Value| {
            admit_mutation(
                Some(&bounds()),
                &request(),
                || Some(approval.clone()),
                now,
                consumed,
            )
        };
        let ok = admit(&consumed, &approval).unwrap();
        assert_eq!(ok.proposal_id(), "p");
        consumed.record(ok);
        assert_eq!(
            admit(&consumed, &approval).err(),
            Some(AdmissionError::ApprovalConsumed)
        );
        // Body mismatch (step 7) wins over consumed (step 8).
        approval["body"]["extra"] = json!(1);
        assert_eq!(
            admit(&consumed, &approval).err(),
            Some(AdmissionError::ApprovalMismatch)
        );
    }

    #[test]
    fn codes_match_display() {
        for error in AdmissionError::ALL {
            assert_eq!(error.to_string(), error.code());
        }
    }
}
