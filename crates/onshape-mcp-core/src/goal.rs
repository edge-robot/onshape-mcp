//! Pure goal authorization (contract §12).
//!
//! Covers the goal grant file v1 (§12.2), the goal mutation admission order
//! (§12.3 steps 1–9), the dispatch journal model (§12.4), goal reads (§12.6)
//! and the capability and journal-summary payloads (§12.7).
//!
//! No I/O happens here. The I/O layer reads the private grant, approval and
//! journal files through the closures passed to [`admit_goal_mutation`],
//! performs the token check (step 10), and owns step 11 (append `PENDING`,
//! re-read the grant, dispatch once, append the outcome).
use std::collections::HashSet;
use std::fmt::Write as _;

use onshape_client_core::request::{ApiRequest, RequestBody};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::config::ExternalTokenConfig;
use crate::target_policy::{
    CadId, OperationId, PolicyError, TargetConstraint, permits_json_response_headers,
};

/// Contract version reported by `GET /capabilities` (§12.7).
pub const CAPABILITIES_CONTRACT_VERSION: &str = "1.1.0";

/// Maximum goal grant file size in bytes (§12.2, §7).
pub const GOAL_GRANT_FILE_MAX_BYTES: u64 = 65_536;

/// Maximum grant lifetime `expiresAt − issuedAt` in ms (8 h, §12.2).
pub const GOAL_MAX_LIFETIME_MS: i64 = 28_800_000;

/// Maximum `maxDispatches` of a grant (§12.2).
pub const GOAL_MAX_DISPATCHES: u32 = 25;

/// Maximum number of `featureIds` of a grant (§12.2).
pub const GOAL_MAX_FEATURE_IDS: usize = 32;

/// Maximum canonical goal-mode request body in bytes (§12.3 step 7).
pub const GOAL_MAX_BODY_BYTES: usize = 900_000;

/// Read limit for one journal file in bytes.
///
/// A goal has at most [`GOAL_MAX_DISPATCHES`] counted entries per grant revision
/// and each line is well below 1 KiB, so a larger file is treated as corrupt.
pub const JOURNAL_FILE_MAX_BYTES: u64 = 4 * 1024 * 1024;

/// Top-level body keys a goal-mode update may carry (§12.3 step 7).
pub const GOAL_BODY_KEYS: [&str; 5] = [
    "feature",
    "sourceMicroversion",
    "rejectMicroversionSkew",
    "serializationVersion",
    "libraryVersion",
];

/// Stable, non-secret goal admission failures, in contract vocabulary.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum GoalAdmissionError {
    /// Static target or revision policy violation (§12.3 steps 1, 6, 8).
    #[error("{0}")]
    Policy(#[from] PolicyError),
    /// Grant file absent, insecure or unreadable (step 2).
    #[error("goal_grant_unavailable")]
    GrantUnavailable,
    /// A grant and an active approval exist at the same time (step 3).
    #[error("mutation_authority_conflict")]
    AuthorityConflict,
    /// Grant structurally invalid, expired or over its caps (steps 4, 9).
    #[error("goal_grant_invalid")]
    GrantInvalid,
    /// Grant `STOPPED`, or STOP observed before dispatch (steps 5, 11).
    #[error("goal_grant_stopped")]
    GrantStopped,
    /// Request outside the grant scope (step 7).
    #[error("goal_scope_denied")]
    ScopeDenied,
    /// `maxDispatches` reached (step 9).
    #[error("goal_budget_exhausted")]
    BudgetExhausted,
    /// An earlier dispatch is uncertain and not reconciled (step 9).
    #[error("goal_reconciliation_required")]
    ReconciliationRequired,
    /// The identical request was already dispatched (step 9).
    #[error("goal_dispatch_duplicate")]
    DispatchDuplicate,
    /// Journal unreadable, insecure or corrupt (step 9, §12.4).
    #[error("dispatch_journal_unavailable")]
    JournalUnavailable,
}

impl GoalAdmissionError {
    /// Every variant, for contract code-list checks.
    pub const ALL: [Self; 12] = [
        Self::Policy(PolicyError::InvalidConfiguration),
        Self::Policy(PolicyError::Denied),
        Self::Policy(PolicyError::Revision),
        Self::GrantUnavailable,
        Self::AuthorityConflict,
        Self::GrantInvalid,
        Self::GrantStopped,
        Self::ScopeDenied,
        Self::BudgetExhausted,
        Self::ReconciliationRequired,
        Self::DispatchDuplicate,
        Self::JournalUnavailable,
    ];

    /// The contract error code returned to MCP clients.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Policy(PolicyError::InvalidConfiguration) => "target_configuration_invalid",
            Self::Policy(PolicyError::Denied) => "target_policy_denied",
            Self::Policy(PolicyError::Revision) => "revision_policy_denied",
            Self::GrantUnavailable => "goal_grant_unavailable",
            Self::AuthorityConflict => "mutation_authority_conflict",
            Self::GrantInvalid => "goal_grant_invalid",
            Self::GrantStopped => "goal_grant_stopped",
            Self::ScopeDenied => "goal_scope_denied",
            Self::BudgetExhausted => "goal_budget_exhausted",
            Self::ReconciliationRequired => "goal_reconciliation_required",
            Self::DispatchDuplicate => "goal_dispatch_duplicate",
            Self::JournalUnavailable => "dispatch_journal_unavailable",
        }
    }
}

/// Whether `value` is a lowercase, hyphenated RFC 4122 version-4 UUID.
///
/// Lowercase only, because the `goalId` names the journal file.
#[must_use]
pub fn is_uuid_v4(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 36
        && bytes.iter().enumerate().all(|(i, &c)| match i {
            8 | 13 | 18 | 23 => c == b'-',
            14 => c == b'4',
            19 => matches!(c, b'8' | b'9' | b'a' | b'b'),
            _ => c.is_ascii_digit() || (b'a'..=b'f').contains(&c),
        })
}

fn is_sha256_hex(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}

fn is_grant_feature_id(value: &str) -> bool {
    (1..=64).contains(&value.len())
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_')
}

/// Grant state (§12.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum GrantState {
    /// Admissions may proceed.
    Active,
    /// STOP or revocation; every admission fails with `goal_grant_stopped`.
    Stopped,
}

/// The goal's Candidate target (§12.2).
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GrantTarget {
    /// Document; must equal the static bound.
    pub document_id: CadId,
    /// The goal's Candidate workspace; must not be excluded.
    pub workspace_id: CadId,
    /// Part Studio; must equal the static bound.
    pub element_id: CadId,
}

/// Goal grant file v1 (§12.2). All fields are required; unknown fields are rejected.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GoalGrant {
    /// Always 1.
    pub version: u8,
    /// Lowercase uuid-v4; names the journal file.
    pub goal_id: String,
    /// Strictly increasing per `goalId`; at least 1.
    pub grant_revision: u64,
    /// SHA-256 hex of the confirmed goal scope (plugin audit field).
    pub goal_hash: String,
    /// `ACTIVE` or `STOPPED`.
    pub state: GrantState,
    /// Issuance (Unix ms).
    pub issued_at: i64,
    /// Expiry (Unix ms); at most [`GOAL_MAX_LIFETIME_MS`] after `issued_at`.
    pub expires_at: i64,
    /// The goal's Candidate target.
    pub target: GrantTarget,
    /// Exactly `["updatePartStudioFeature"]` (phase 1).
    pub operations: Vec<String>,
    /// 1..=32 unique feature IDs, each 1–64 chars `[A-Za-z0-9_]`.
    pub feature_ids: Vec<String>,
    /// Dispatch budget, 1..=25.
    pub max_dispatches: u32,
    /// Highest journal sequence the plugin has reconciled.
    pub reconciled_through: u64,
}

impl GoalGrant {
    /// Parse the structure of a grant value, independent of the clock (§12.2).
    ///
    /// # Errors
    /// [`GoalAdmissionError::GrantInvalid`] for `null`, unknown or missing
    /// fields, wrong types, and every violated §12.2 rule or cap.
    pub fn parse(value: &serde_json::Value) -> Result<Self, GoalAdmissionError> {
        let grant = Self::deserialize(value).map_err(|_| GoalAdmissionError::GrantInvalid)?;
        let lifetime = grant.expires_at.checked_sub(grant.issued_at);
        let mut unique = HashSet::new();
        let feature_ids_valid = (1..=GOAL_MAX_FEATURE_IDS).contains(&grant.feature_ids.len())
            && grant
                .feature_ids
                .iter()
                .all(|id| is_grant_feature_id(id) && unique.insert(id.as_str()));
        if grant.version != 1
            || !is_uuid_v4(&grant.goal_id)
            || grant.grant_revision < 1
            || !is_sha256_hex(&grant.goal_hash)
            || !lifetime.is_some_and(|l| 0 < l && l <= GOAL_MAX_LIFETIME_MS)
            || grant.operations != ["updatePartStudioFeature"]
            || !feature_ids_valid
            || !(1..=GOAL_MAX_DISPATCHES).contains(&grant.max_dispatches)
        {
            return Err(GoalAdmissionError::GrantInvalid);
        }
        Ok(grant)
    }

    /// Parse and check that the grant is unexpired at `now_ms` (§12.3 step 4).
    ///
    /// # Errors
    /// [`GoalAdmissionError::GrantInvalid`] as for [`Self::parse`], or if
    /// `expiresAt ≤ now`.
    pub fn parse_at(value: &serde_json::Value, now_ms: i64) -> Result<Self, GoalAdmissionError> {
        let grant = Self::parse(value)?;
        if grant.expires_at <= now_ms {
            return Err(GoalAdmissionError::GrantInvalid);
        }
        Ok(grant)
    }

    /// Whether the grant target lies within the static bounds (§12.3 step 6),
    /// including the optional expand-step workspace pin.
    fn within_bounds(&self, policy: &TargetConstraint) -> bool {
        self.target.document_id == policy.document_id
            && self.target.element_id == policy.element_id
            && !policy
                .excluded_workspace_ids
                .contains(&self.target.workspace_id)
            && policy
                .workspace_id
                .as_ref()
                .is_none_or(|pin| pin == &self.target.workspace_id)
    }
}

/// Whether an approval file value is a non-`null`, unexpired approval
/// (§12.2 "One authority at a time", §12.3 step 3).
///
/// Any object with an integer `expiresAt > now` counts, whatever its version
/// or other structure: a conflict is reported rather than guessing which
/// authority the publisher meant.
#[must_use]
pub fn approval_active(value: &serde_json::Value, now_ms: i64) -> bool {
    value
        .get("expiresAt")
        .and_then(serde_json::Value::as_i64)
        .is_some_and(|expires_at| expires_at > now_ms)
}

// ---------------------------------------------------------------------------
// Canonical JSON and requestHash (§12.3)
// ---------------------------------------------------------------------------

/// Canonical JSON: object keys sorted (UTF-16 code unit order, as in
/// JavaScript), separators `,` and `:`, no whitespace, UTF-8 without ASCII
/// escaping.
#[must_use]
pub fn canonical_json(value: &serde_json::Value) -> String {
    let mut out = String::new();
    write_canonical(value, &mut out);
    out
}

fn write_canonical(value: &serde_json::Value, out: &mut String) {
    match value {
        serde_json::Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_canonical(item, out);
            }
            out.push(']');
        }
        serde_json::Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
            out.push('{');
            for (i, key) in keys.into_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                // A plain string always serializes; `Value` never fails either.
                out.push_str(&serde_json::Value::String(key.clone()).to_string());
                out.push(':');
                if let Some(item) = map.get(key) {
                    write_canonical(item, out);
                }
            }
            out.push('}');
        }
        scalar => out.push_str(&scalar.to_string()),
    }
}

/// Lowercase SHA-256 hex of `bytes`.
#[must_use]
pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(64);
    for byte in digest {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// `requestHash` = SHA-256 hex of the canonical JSON of `{"path", "body"}` (§12.3).
#[must_use]
pub fn request_hash(path: &str, body: &serde_json::Value) -> String {
    let value = serde_json::json!({"path": path, "body": body});
    sha256_hex(canonical_json(&value).as_bytes())
}

// ---------------------------------------------------------------------------
// Dispatch journal (§12.4)
// ---------------------------------------------------------------------------

/// Outcome of one journal entry (§12.4).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum DispatchOutcome {
    /// Admitted, dispatch in progress; counts as `UNCERTAIN` without a later outcome.
    Pending,
    /// 2xx.
    Ok,
    /// 4xx except 408.
    ProviderRejected,
    /// Transport error, timeout, 3xx, 408 or 5xx.
    Uncertain,
    /// STOP observed at step 11; no provider call. Not counted.
    NotDispatched,
}

impl DispatchOutcome {
    /// Map a provider HTTP status (§12.4): 2xx `OK`, 4xx except 408
    /// `PROVIDER_REJECTED`, everything else `UNCERTAIN`.
    #[must_use]
    pub const fn from_status(status: u16) -> Self {
        match status {
            200..=299 => Self::Ok,
            408 => Self::Uncertain,
            400..=499 => Self::ProviderRejected,
            _ => Self::Uncertain,
        }
    }

    /// `UNCERTAIN`, or `PENDING` (which is treated as `UNCERTAIN`).
    #[must_use]
    pub const fn is_uncertain(self) -> bool {
        matches!(self, Self::Pending | Self::Uncertain)
    }

    /// Whether the entry counts against `maxDispatches`.
    #[must_use]
    pub const fn is_counted(self) -> bool {
        !matches!(self, Self::NotDispatched)
    }
}

/// One journal line (§12.4).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct JournalEntry {
    /// Per goal, gapless, from 1.
    pub sequence: u64,
    /// Grant revision that admitted the entry.
    pub grant_revision: u64,
    /// [`request_hash`] of the admitted request.
    pub request_hash: String,
    /// Feature of the admitted request.
    pub feature_id: String,
    /// `body.sourceMicroversion` of the admitted request.
    pub source_microversion: String,
    /// Admission time (Unix ms).
    pub admitted_at: i64,
    /// Outcome; the last line per sequence wins.
    pub outcome: DispatchOutcome,
    /// Provider HTTP status, if a response arrived.
    pub http_status: Option<u16>,
}

impl JournalEntry {
    /// The same entry with a new outcome (a new line with the same sequence).
    #[must_use]
    pub fn with_outcome(&self, outcome: DispatchOutcome, http_status: Option<u16>) -> Self {
        Self {
            outcome,
            http_status,
            ..self.clone()
        }
    }

    /// One JSON line including the trailing newline.
    ///
    /// # Errors
    /// [`GoalAdmissionError::JournalUnavailable`] if serialization fails.
    pub fn to_line(&self) -> Result<String, GoalAdmissionError> {
        let mut line =
            serde_json::to_string(self).map_err(|_| GoalAdmissionError::JournalUnavailable)?;
        line.push('\n');
        Ok(line)
    }

    fn immutable_fields_equal(&self, other: &Self) -> bool {
        self.sequence == other.sequence
            && self.grant_revision == other.grant_revision
            && self.request_hash == other.request_hash
            && self.feature_id == other.feature_id
            && self.source_microversion == other.source_microversion
            && self.admitted_at == other.admitted_at
    }

    fn well_formed(&self) -> bool {
        self.sequence >= 1
            && self.grant_revision >= 1
            && is_sha256_hex(&self.request_hash)
            && is_grant_feature_id(&self.feature_id)
            && CadId::is_valid(&self.source_microversion)
    }
}

/// The effective journal of one goal: one entry per sequence, last line wins.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Journal {
    entries: Vec<JournalEntry>,
}

impl Journal {
    /// Parse a journal file (§12.4).
    ///
    /// The file must be UTF-8, empty or newline-terminated, one well-formed
    /// entry per line. A new sequence must be exactly the next one (gapless).
    /// A line for an existing sequence must repeat its immutable fields and
    /// replaces its outcome. Anything else is corruption.
    ///
    /// # Errors
    /// [`GoalAdmissionError::JournalUnavailable`] for a corrupt journal.
    pub fn parse(bytes: &[u8]) -> Result<Self, GoalAdmissionError> {
        let corrupt = GoalAdmissionError::JournalUnavailable;
        let text = std::str::from_utf8(bytes).map_err(|_| corrupt)?;
        if !text.is_empty() && !text.ends_with('\n') {
            return Err(corrupt);
        }
        let mut journal = Self::default();
        for line in text.lines() {
            let entry: JournalEntry = serde_json::from_str(line).map_err(|_| corrupt)?;
            if !entry.well_formed() {
                return Err(corrupt);
            }
            let next = journal.max_sequence() + 1;
            if entry.sequence == next {
                journal.entries.push(entry);
                continue;
            }
            let index = usize::try_from(entry.sequence - 1).map_err(|_| corrupt)?;
            match journal.entries.get_mut(index) {
                Some(existing) if existing.immutable_fields_equal(&entry) => *existing = entry,
                _ => return Err(corrupt),
            }
        }
        Ok(journal)
    }

    /// Effective entries, ordered by sequence.
    #[must_use]
    pub fn entries(&self) -> &[JournalEntry] {
        &self.entries
    }

    /// Highest sequence, 0 for an empty journal.
    #[must_use]
    pub fn max_sequence(&self) -> u64 {
        self.entries.last().map_or(0, |entry| entry.sequence)
    }

    /// Entries counted against `maxDispatches` (all but `NOT_DISPATCHED`).
    #[must_use]
    pub fn counted(&self) -> usize {
        self.entries
            .iter()
            .filter(|entry| entry.outcome.is_counted())
            .count()
    }

    /// The first `UNCERTAIN`/`PENDING` entry above `reconciled_through`.
    #[must_use]
    pub fn unreconciled(&self, reconciled_through: u64) -> Option<&JournalEntry> {
        self.entries
            .iter()
            .find(|entry| entry.outcome.is_uncertain() && entry.sequence > reconciled_through)
    }

    /// `GET /goals/{goalId}/journal-summary` payload (§12.7).
    #[must_use]
    pub fn summary(&self, goal_id: &str, reconciled_through: u64) -> JournalSummary {
        let last = self.entries.last();
        let unreconciled = self.unreconciled(reconciled_through);
        JournalSummary {
            goal_id: goal_id.to_owned(),
            entries: self.entries.len(),
            counted: self.counted(),
            max_sequence: self.max_sequence(),
            unreconciled_sequence: unreconciled.map(|entry| entry.sequence),
            last_outcome: last.map(|entry| entry.outcome),
            last_admitted_at: last.map(|entry| entry.admitted_at),
            unreconciled_entry: unreconciled.cloned(),
        }
    }
}

/// Journal summary (§12.7). `null`s are serialized, not omitted.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JournalSummary {
    /// The requested goal.
    pub goal_id: String,
    /// Number of sequences.
    pub entries: usize,
    /// Entries counted against the budget.
    pub counted: usize,
    /// Highest sequence, 0 if none.
    pub max_sequence: u64,
    /// Unreconciled `UNCERTAIN`/`PENDING` sequence, if any.
    pub unreconciled_sequence: Option<u64>,
    /// Outcome of the highest sequence.
    pub last_outcome: Option<DispatchOutcome>,
    /// Admission time of the highest sequence.
    pub last_admitted_at: Option<i64>,
    /// The entry named by `unreconciled_sequence` (last line for that
    /// sequence), contract 1.2.0. Carries no request body and no token.
    pub unreconciled_entry: Option<JournalEntry>,
}

// ---------------------------------------------------------------------------
// Goal admission (§12.3 steps 1–9)
// ---------------------------------------------------------------------------

/// Error `data` of goal-mode errors from steps 9–11 and of the unknown-outcome
/// error (§12.4).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GoalErrorData {
    /// The grant's goal.
    pub goal_id: String,
    /// Journal sequence, if the error concerns one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sequence: Option<u64>,
    /// Feature of the request (or of the referenced entry).
    pub feature_id: String,
    /// `sourceMicroversion` of the request (or of the referenced entry).
    pub source_microversion: String,
    /// Admission time of the referenced entry, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub admitted_at: Option<i64>,
}

impl GoalErrorData {
    fn for_entry(goal_id: &str, entry: &JournalEntry) -> Self {
        Self {
            goal_id: goal_id.to_owned(),
            sequence: Some(entry.sequence),
            feature_id: entry.feature_id.clone(),
            source_microversion: entry.source_microversion.clone(),
            admitted_at: Some(entry.admitted_at),
        }
    }

    /// The data as a JSON object.
    #[must_use]
    pub fn to_value(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or(serde_json::Value::Null)
    }
}

/// A failed goal admission: the code and, from step 9 on, the error data.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{error}")]
pub struct GoalRejection {
    /// The first failing check.
    pub error: GoalAdmissionError,
    /// Present for steps 9–11 (§12.4 "Error data").
    pub data: Option<GoalErrorData>,
}

impl From<GoalAdmissionError> for GoalRejection {
    fn from(error: GoalAdmissionError) -> Self {
        Self { error, data: None }
    }
}

impl From<PolicyError> for GoalRejection {
    fn from(error: PolicyError) -> Self {
        GoalAdmissionError::Policy(error).into()
    }
}

/// A request that passed §12.3 steps 1–9, with the `PENDING` entry to append
/// at step 11. Only [`admit_goal_mutation`] creates one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GoalAdmission {
    grant: GoalGrant,
    pending: JournalEntry,
}

impl GoalAdmission {
    /// The admitting grant.
    #[must_use]
    pub const fn grant(&self) -> &GoalGrant {
        &self.grant
    }

    /// The `PENDING` entry to append before the grant re-read (step 11).
    #[must_use]
    pub const fn pending(&self) -> &JournalEntry {
        &self.pending
    }

    /// Error data without a sequence (step 10 failures).
    #[must_use]
    pub fn request_data(&self) -> GoalErrorData {
        GoalErrorData {
            goal_id: self.grant.goal_id.clone(),
            sequence: None,
            feature_id: self.pending.feature_id.clone(),
            source_microversion: self.pending.source_microversion.clone(),
            admitted_at: None,
        }
    }

    /// Error data for the pending sequence (step 11 and the outcome).
    #[must_use]
    pub fn sequence_data(&self) -> GoalErrorData {
        GoalErrorData::for_entry(&self.grant.goal_id, &self.pending)
    }
}

/// Result of a successful goal admission.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GoalDecision {
    /// The grant file holds `null`: the §2.5 approval path applies unchanged.
    ApprovalPath,
    /// Steps 1–9 passed.
    Admit(Box<GoalAdmission>),
}

/// The goal re-read at step 11: whether `value` is still a valid, unexpired
/// `ACTIVE` grant for `goal_id`.
#[must_use]
pub fn grant_still_active(value: &serde_json::Value, goal_id: &str, now_ms: i64) -> bool {
    GoalGrant::parse_at(value, now_ms)
        .is_ok_and(|grant| grant.state == GrantState::Active && grant.goal_id == goal_id)
}

/// Evaluate §12.3 steps 1–9 for a non-GET request; the first failure wins.
///
/// The closures run only when their step is reached, so no file is read
/// before the static checks pass:
/// - `read_grant` (step 2): the grant file's JSON, or `None` if it is
///   insecure or unreadable;
/// - `approval_active` (step 3): whether the approval file holds an active
///   approval (see [`approval_active`]); called only for a non-`null` grant;
/// - `read_journal` (step 9): the journal of the given `goalId`, or an error
///   if it is unreadable, insecure or corrupt.
///
/// Steps 10 (token) and 11 (journal append, re-read, dispatch) belong to the caller.
///
/// # Errors
/// The [`GoalRejection`] of the first failing step.
pub fn admit_goal_mutation<G, A, J>(
    policy: Option<&TargetConstraint>,
    request: &ApiRequest,
    read_grant: G,
    approval_active: A,
    read_journal: J,
    now_ms: i64,
) -> Result<GoalDecision, GoalRejection>
where
    G: FnOnce() -> Option<serde_json::Value>,
    A: FnOnce() -> bool,
    J: FnOnce(&str) -> Result<Journal, GoalAdmissionError>,
{
    // Step 1: §2.5 steps 1–2.
    let policy = policy.ok_or(PolicyError::Denied)?;
    policy.check_request_shape(request)?;
    // Step 2.
    let value = read_grant().ok_or(GoalAdmissionError::GrantUnavailable)?;
    // Step 3.
    if value.is_null() {
        return Ok(GoalDecision::ApprovalPath);
    }
    if approval_active() {
        return Err(GoalAdmissionError::AuthorityConflict.into());
    }
    // Step 4.
    let grant = GoalGrant::parse_at(&value, now_ms)?;
    if !grant.operations.iter().all(|op| {
        OperationId::from_name(op).is_some_and(|op| policy.allowed_operations.contains(&op))
    }) {
        return Err(GoalAdmissionError::GrantInvalid.into());
    }
    // Step 5.
    if grant.state != GrantState::Active {
        return Err(GoalAdmissionError::GrantStopped.into());
    }
    // Step 6.
    if !grant.within_bounds(policy) {
        return Err(PolicyError::Denied.into());
    }
    // Step 7.
    let (feature_id, body) = goal_scope(policy, &grant, request)?;
    // Step 8.
    let source_microversion = body
        .get("sourceMicroversion")
        .and_then(serde_json::Value::as_str)
        .filter(|mv| CadId::is_valid(mv))
        .ok_or(PolicyError::Revision)?;
    if body
        .get("rejectMicroversionSkew")
        .and_then(serde_json::Value::as_bool)
        != Some(true)
        || policy
            .expected_microversion
            .as_ref()
            .is_some_and(|pin| pin.as_str() != source_microversion)
    {
        return Err(PolicyError::Revision.into());
    }
    // Step 9.
    let pending = JournalEntry {
        sequence: 0,
        grant_revision: grant.grant_revision,
        request_hash: request_hash(&request.path, body),
        feature_id: feature_id.to_owned(),
        source_microversion: source_microversion.to_owned(),
        admitted_at: now_ms,
        outcome: DispatchOutcome::Pending,
        http_status: None,
    };
    let reject = |error, data| GoalRejection {
        error,
        data: Some(data),
    };
    let request_data = GoalErrorData {
        goal_id: grant.goal_id.clone(),
        sequence: None,
        feature_id: pending.feature_id.clone(),
        source_microversion: pending.source_microversion.clone(),
        admitted_at: None,
    };
    let journal = read_journal(&grant.goal_id)
        .map_err(|_| reject(GoalAdmissionError::JournalUnavailable, request_data.clone()))?;
    if grant.reconciled_through > journal.max_sequence() {
        return Err(reject(GoalAdmissionError::GrantInvalid, request_data));
    }
    if let Some(entry) = journal.unreconciled(grant.reconciled_through) {
        return Err(reject(
            GoalAdmissionError::ReconciliationRequired,
            GoalErrorData::for_entry(&grant.goal_id, entry),
        ));
    }
    if let Some(entry) = journal.entries().iter().find(|entry| {
        entry.request_hash == pending.request_hash
            && matches!(
                entry.outcome,
                DispatchOutcome::Ok | DispatchOutcome::Uncertain | DispatchOutcome::Pending
            )
    }) {
        return Err(reject(
            GoalAdmissionError::DispatchDuplicate,
            GoalErrorData::for_entry(&grant.goal_id, entry),
        ));
    }
    let budget = usize::try_from(grant.max_dispatches).unwrap_or(usize::MAX);
    if journal.counted() >= budget {
        return Err(reject(GoalAdmissionError::BudgetExhausted, request_data));
    }
    let pending = JournalEntry {
        sequence: journal.max_sequence() + 1,
        ..pending
    };
    Ok(GoalDecision::Admit(Box::new(GoalAdmission {
        grant,
        pending,
    })))
}

/// §12.3 step 7: path, feature and body shape within the grant scope.
/// Returns the feature ID and the JSON body.
fn goal_scope<'r>(
    policy: &TargetConstraint,
    grant: &GoalGrant,
    request: &'r ApiRequest,
) -> Result<(&'r str, &'r serde_json::Value), GoalAdmissionError> {
    let scope = GoalAdmissionError::ScopeDenied;
    let prefix = format!(
        "/partstudios/d/{}/w/{}/e/{}/features/featureid/",
        grant.target.document_id.as_str(),
        grant.target.workspace_id.as_str(),
        grant.target.element_id.as_str()
    );
    let feature_id = request.path.strip_prefix(&prefix).ok_or(scope)?;
    if !grant.feature_ids.iter().any(|id| id == feature_id)
        || policy
            .feature_id
            .as_deref()
            .is_some_and(|pin| pin != feature_id)
    {
        return Err(scope);
    }
    let Some(RequestBody::Json(body)) = &request.body else {
        return Err(scope);
    };
    let object = body.as_object().ok_or(scope)?;
    if !object
        .keys()
        .all(|key| GOAL_BODY_KEYS.contains(&key.as_str()))
        || body
            .pointer("/feature/featureId")
            .and_then(serde_json::Value::as_str)
            != Some(feature_id)
        || canonical_json(body).len() > GOAL_MAX_BODY_BYTES
    {
        return Err(scope);
    }
    Ok((feature_id, body))
}

// ---------------------------------------------------------------------------
// Reads (§12.6) and capabilities (§12.7)
// ---------------------------------------------------------------------------

/// The Candidate workspace of a valid, unexpired grant (`ACTIVE` or
/// `STOPPED`) whose target lies within the static bounds (§12.6).
///
/// `None` without `[external_token.mutation]`, for `null`, invalid or expired
/// grants, and for targets outside the configured document and element or on
/// an excluded workspace.
#[must_use]
pub fn grant_read_workspace(
    config: &ExternalTokenConfig,
    grant: &serde_json::Value,
    now_ms: i64,
) -> Option<CadId> {
    let policy = config.mutation.as_ref()?;
    let grant = GoalGrant::parse_at(grant, now_ms).ok()?;
    (grant.target.document_id.as_str() == config.document_id
        && grant.target.element_id.as_str() == config.element_id
        && grant.within_bounds(policy))
    .then_some(grant.target.workspace_id)
}

/// Goal read admission (§12.6): `GET`, no body, no query parameters, JSON
/// response headers only, and one of the three Candidate paths of `workspace`.
#[must_use]
pub fn goal_read_admitted(
    config: &ExternalTokenConfig,
    request: &ApiRequest,
    workspace: &CadId,
) -> bool {
    if request.method != http::Method::GET
        || request.body.is_some()
        || !permits_json_response_headers(&request.headers)
        || !request.query_params.is_empty()
    {
        return false;
    }
    let d = &config.document_id;
    let e = &config.element_id;
    let w = workspace.as_str();
    [
        format!("/documents/d/{d}/w/{w}/currentmicroversion"),
        format!("/documents/d/{d}/w/{w}/elements"),
        format!("/partstudios/d/{d}/w/{w}/e/{e}/features"),
    ]
    .contains(&request.path)
}

/// `GET /capabilities` payload (§12.7).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Capabilities {
    /// Always [`CAPABILITIES_CONTRACT_VERSION`].
    pub contract_version: &'static str,
    /// `approval-v2` and/or `goal-grant-v1`.
    pub mutation_authorities: Vec<&'static str>,
}

impl Capabilities {
    /// Capabilities for a server with `mutation_approval_file` configured
    /// (`approval`) and with a usable goal configuration (`goal`: grant file
    /// and journal directory configured, directory passes the private checks).
    #[must_use]
    pub fn new(approval: bool, goal: bool) -> Self {
        let mut mutation_authorities = Vec::new();
        if approval {
            mutation_authorities.push("approval-v2");
        }
        if goal {
            mutation_authorities.push("goal-grant-v1");
        }
        Self {
            contract_version: CAPABILITIES_CONTRACT_VERSION,
            mutation_authorities,
        }
    }
}

#[cfg(test)]
// Test-only: fixture construction failures are test bugs (same as approval).
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use serde_json::json;

    const GOAL: &str = "3f2a9c1e-5b7d-4e8f-9a0b-1c2d3e4f5a6b";

    fn id(c: char) -> String {
        c.to_string().repeat(24)
    }

    fn grant(now: i64) -> serde_json::Value {
        json!({"version":1,"goalId":GOAL,"grantRevision":1,"goalHash":"a".repeat(64),
            "state":"ACTIVE","issuedAt":now - 1_000,"expiresAt":now + 60_000,
            "target":{"documentId":id('a'),"workspaceId":id('9'),"elementId":id('c')},
            "operations":["updatePartStudioFeature"],"featureIds":["f1","f2"],
            "maxDispatches":3,"reconciledThrough":0})
    }

    fn bounds() -> TargetConstraint {
        serde_json::from_value(json!({"document_id":id('a'),"element_id":id('c'),
            "excluded_workspace_ids":[id('d')],
            "allowed_operations":["updatePartStudioFeature"],"write_mode":"modify"}))
        .unwrap()
    }

    fn request(feature: &str, depth: &str) -> ApiRequest {
        ApiRequest {
            method: http::Method::POST,
            path: format!(
                "/partstudios/d/{}/w/{}/e/{}/features/featureid/{feature}",
                id('a'),
                id('9'),
                id('c')
            ),
            query_params: vec![],
            headers: http::HeaderMap::new(),
            content_type: None,
            body: Some(RequestBody::Json(json!({"feature":{"featureId":feature,
                "parameters":[{"expression":depth}]},
                "sourceMicroversion":id('e'),"rejectMicroversionSkew":true}))),
        }
    }

    fn admit(
        grant: &serde_json::Value,
        request: &ApiRequest,
        journal: &Journal,
        now: i64,
    ) -> Result<GoalDecision, GoalRejection> {
        admit_goal_mutation(
            Some(&bounds()),
            request,
            || Some(grant.clone()),
            || false,
            |_| Ok(journal.clone()),
            now,
        )
    }

    fn code(result: Result<GoalDecision, GoalRejection>) -> &'static str {
        match result {
            Ok(GoalDecision::Admit(_)) => "admitted",
            Ok(GoalDecision::ApprovalPath) => "approval",
            Err(rejection) => rejection.error.code(),
        }
    }

    fn entry(sequence: u64, hash: &str, outcome: DispatchOutcome) -> JournalEntry {
        JournalEntry {
            sequence,
            grant_revision: 1,
            request_hash: hash.to_owned(),
            feature_id: "f1".into(),
            source_microversion: id('e'),
            admitted_at: 5,
            outcome,
            http_status: None,
        }
    }

    fn journal(entries: &[JournalEntry]) -> Journal {
        let text: String = entries.iter().map(|e| e.to_line().unwrap()).collect();
        Journal::parse(text.as_bytes()).unwrap()
    }

    #[test]
    fn uuid_v4_is_strict() {
        assert!(is_uuid_v4(GOAL));
        for bad in [
            "3F2A9C1E-5B7D-4E8F-9A0B-1C2D3E4F5A6B",
            "3f2a9c1e-5b7d-3e8f-9a0b-1c2d3e4f5a6b",
            "3f2a9c1e-5b7d-4e8f-7a0b-1c2d3e4f5a6b",
            "3f2a9c1e5b7d4e8f9a0b1c2d3e4f5a6b",
            "../../../../etc/passwd-aaaaaaaaaaaaa",
            "",
        ] {
            assert!(!is_uuid_v4(bad), "{bad}");
        }
    }

    #[test]
    fn grant_structure_and_caps_fail_closed() {
        let now = 1_000_000;
        assert!(GoalGrant::parse_at(&grant(now), now).is_ok());
        let cases: [(&str, serde_json::Value); 16] = [
            ("version", json!(2)),
            ("goalId", json!("not-a-uuid")),
            ("grantRevision", json!(0)),
            ("goalHash", json!("A".repeat(64))),
            ("state", json!("PAUSED")),
            ("issuedAt", json!(now + 60_000)),
            ("issuedAt", json!(now + 60_000 - GOAL_MAX_LIFETIME_MS - 1)),
            ("expiresAt", json!(now)),
            ("operations", json!(["updatePartStudioFeature", "x"])),
            ("featureIds", json!([])),
            ("featureIds", json!(["f1", "f1"])),
            ("featureIds", json!(["bad-id"])),
            ("featureIds", json!(["x".repeat(65)])),
            ("maxDispatches", json!(26)),
            ("reconciledThrough", json!(-1)),
            ("extra", json!(true)),
        ];
        for (key, bad) in cases {
            let mut value = grant(now);
            value[key] = bad;
            assert_eq!(
                GoalGrant::parse_at(&value, now).err(),
                Some(GoalAdmissionError::GrantInvalid),
                "{key}"
            );
        }
        let mut value = grant(now);
        value["featureIds"] = json!((0..33).map(|i| format!("f{i}")).collect::<Vec<_>>());
        assert!(GoalGrant::parse(&value).is_err());
        let mut value = grant(now);
        value["featureIds"] = json!((0..32).map(|i| format!("f{i}")).collect::<Vec<_>>());
        value["maxDispatches"] = json!(25);
        value["expiresAt"] = json!(now - 1_000 + GOAL_MAX_LIFETIME_MS);
        assert!(GoalGrant::parse_at(&value, now).is_ok());
        assert!(GoalGrant::parse(&json!(null)).is_err());
    }

    #[test]
    fn canonical_json_sorts_keys_without_whitespace_or_ascii_escaping() {
        let value = json!({"b":[1,{"z":null,"a":"ä\"\n"}],"a":true,"é":1.5,"Z":-2});
        assert_eq!(
            canonical_json(&value),
            "{\"Z\":-2,\"a\":true,\"b\":[1,{\"a\":\"ä\\\"\\n\",\"z\":null}],\"é\":1.5}"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        let body = json!({"x":1});
        assert_eq!(
            request_hash("/p", &body),
            sha256_hex(br#"{"body":{"x":1},"path":"/p"}"#)
        );
    }

    #[test]
    fn outcome_mapping_follows_the_contract() {
        for (status, outcome) in [
            (200, DispatchOutcome::Ok),
            (204, DispatchOutcome::Ok),
            (400, DispatchOutcome::ProviderRejected),
            (409, DispatchOutcome::ProviderRejected),
            (408, DispatchOutcome::Uncertain),
            (302, DispatchOutcome::Uncertain),
            (500, DispatchOutcome::Uncertain),
            (503, DispatchOutcome::Uncertain),
            (101, DispatchOutcome::Uncertain),
        ] {
            assert_eq!(DispatchOutcome::from_status(status), outcome, "{status}");
        }
    }

    #[test]
    fn journal_last_line_wins_and_corruption_is_detected() {
        let hash = "1".repeat(64);
        let pending = entry(1, &hash, DispatchOutcome::Pending);
        let ok = pending.with_outcome(DispatchOutcome::Ok, Some(200));
        let text = format!("{}{}", pending.to_line().unwrap(), ok.to_line().unwrap());
        let parsed = Journal::parse(text.as_bytes()).unwrap();
        assert_eq!(parsed.entries(), std::slice::from_ref(&ok));
        assert_eq!(Journal::parse(b"").unwrap(), Journal::default());
        let bad_hash = entry(2, "x", DispatchOutcome::Ok).to_line().unwrap();
        let gap = entry(3, &hash, DispatchOutcome::Ok).to_line().unwrap();
        let mut changed = ok.clone();
        changed.request_hash = "2".repeat(64);
        let changed = changed.to_line().unwrap();
        let unterminated = ok.to_line().unwrap().trim_end().to_owned();
        let extra = ok.to_line().unwrap().replacen('{', "{\"extra\":1,", 1);
        for corrupt in [
            "garbage\n".to_owned(),
            "\n".to_owned(),
            gap,
            bad_hash,
            format!("{}{changed}", ok.to_line().unwrap()),
            unterminated,
            extra,
        ] {
            assert_eq!(
                Journal::parse(corrupt.as_bytes()),
                Err(GoalAdmissionError::JournalUnavailable),
                "{corrupt}"
            );
        }
        assert!(Journal::parse(&[0xff, b'\n']).is_err());
    }

    #[test]
    fn admission_steps_in_order() {
        let now = 1_000_000;
        let empty = Journal::default();
        let r = request("f1", "1 mm");
        assert_eq!(code(admit(&grant(now), &r, &empty, now)), "admitted");
        assert_eq!(code(admit(&json!(null), &r, &empty, now)), "approval");
        // Step 1 before any file read.
        let reads = std::cell::Cell::new(0);
        let mut get = r.clone();
        get.method = http::Method::DELETE;
        let result = admit_goal_mutation(
            Some(&bounds()),
            &get,
            || {
                reads.set(reads.get() + 1);
                None
            },
            || true,
            |_| Ok(Journal::default()),
            now,
        );
        assert_eq!(code(result), "target_policy_denied");
        assert_eq!(reads.get(), 0);
        assert_eq!(
            code(admit_goal_mutation(
                None,
                &r,
                || None,
                || false,
                |_| Ok(Journal::default()),
                now
            )),
            "target_policy_denied"
        );
        // Step 2.
        assert_eq!(
            code(admit_goal_mutation(
                Some(&bounds()),
                &r,
                || None,
                || false,
                |_| Ok(Journal::default()),
                now
            )),
            "goal_grant_unavailable"
        );
        // Step 3 wins over an invalid grant (step 4).
        assert_eq!(
            code(admit_goal_mutation(
                Some(&bounds()),
                &r,
                || Some(json!({"version":9})),
                || true,
                |_| Ok(Journal::default()),
                now
            )),
            "mutation_authority_conflict"
        );
    }

    #[test]
    fn admission_steps_4_to_8_in_order() {
        let now = 1_000_000;
        let empty = Journal::default();
        let r = request("f1", "1 mm");
        // Step 4 (expiry), step 5 (stopped wins over scope), step 6.
        assert_eq!(
            code(admit(&grant(now), &r, &empty, now + 60_000)),
            "goal_grant_invalid"
        );
        let mut stopped = grant(now);
        stopped["state"] = json!("STOPPED");
        assert_eq!(
            code(admit(&stopped, &request("other", "1 mm"), &empty, now)),
            "goal_grant_stopped"
        );
        let mut excluded = grant(now);
        excluded["target"]["workspaceId"] = json!(id('d'));
        assert_eq!(
            code(admit(&excluded, &r, &empty, now)),
            "target_policy_denied"
        );
        // Step 7 before step 8.
        let mut both = request("f1", "1 mm");
        if let Some(RequestBody::Json(body)) = &mut both.body {
            body["rejectMicroversionSkew"] = json!(false);
            body["extra"] = json!(1);
        }
        assert_eq!(
            code(admit(&grant(now), &both, &empty, now)),
            "goal_scope_denied"
        );
        let mut no_body = r.clone();
        no_body.body = None;
        assert_eq!(
            code(admit(&grant(now), &no_body, &empty, now)),
            "goal_scope_denied"
        );
        let mut huge = r.clone();
        if let Some(RequestBody::Json(body)) = &mut huge.body {
            body["feature"]["blob"] = json!("x".repeat(GOAL_MAX_BODY_BYTES));
        }
        assert_eq!(
            code(admit(&grant(now), &huge, &empty, now)),
            "goal_scope_denied"
        );
        let mut bad_mv = r;
        if let Some(RequestBody::Json(body)) = &mut bad_mv.body {
            body["sourceMicroversion"] = json!("E".repeat(24));
        }
        assert_eq!(
            code(admit(&grant(now), &bad_mv, &empty, now)),
            "revision_policy_denied"
        );
    }

    #[test]
    fn expand_step_pins_also_bound_goal_admission() {
        let now = 1_000_000;
        let empty = Journal::default();
        let r = request("f1", "1 mm");
        let admit_with = |key: &str, value: serde_json::Value| {
            let mut policy = bounds();
            match key {
                "workspace_id" => {
                    policy.workspace_id =
                        Some(value.as_str().unwrap().to_owned().try_into().unwrap());
                }
                "feature_id" => policy.feature_id = Some(value.as_str().unwrap().to_owned()),
                _ => {
                    policy.expected_microversion =
                        Some(value.as_str().unwrap().to_owned().try_into().unwrap());
                }
            }
            code(admit_goal_mutation(
                Some(&policy),
                &r,
                || Some(grant(now)),
                || false,
                |_| Ok(empty.clone()),
                now,
            ))
        };
        assert_eq!(admit_with("workspace_id", json!(id('9'))), "admitted");
        assert_eq!(
            admit_with("workspace_id", json!(id('b'))),
            "target_policy_denied"
        );
        assert_eq!(admit_with("feature_id", json!("f1")), "admitted");
        assert_eq!(admit_with("feature_id", json!("f2")), "goal_scope_denied");
        assert_eq!(
            admit_with("expected_microversion", json!(id('e'))),
            "admitted"
        );
        assert_eq!(
            admit_with("expected_microversion", json!(id('f'))),
            "revision_policy_denied"
        );
    }

    #[test]
    fn journal_rules_reconcile_duplicate_budget() {
        let now = 1_000_000;
        let r = request("f1", "1 mm");
        let hash = match &r.body {
            Some(RequestBody::Json(body)) => request_hash(&r.path, body),
            _ => String::new(),
        };
        let other = "2".repeat(64);
        // Unavailable journal carries request data.
        let unavailable = admit_goal_mutation(
            Some(&bounds()),
            &r,
            || Some(grant(now)),
            || false,
            |_| Err(GoalAdmissionError::JournalUnavailable),
            now,
        )
        .unwrap_err();
        assert_eq!(unavailable.error, GoalAdmissionError::JournalUnavailable);
        assert_eq!(
            unavailable.data.unwrap().to_value(),
            json!({"goalId":GOAL,"featureId":"f1","sourceMicroversion":id('e')})
        );
        // reconciledThrough beyond the journal.
        let mut ahead = grant(now);
        ahead["reconciledThrough"] = json!(1);
        assert_eq!(
            code(admit(&ahead, &r, &Journal::default(), now)),
            "goal_grant_invalid"
        );
        // PENDING counts as UNCERTAIN; reconciliation wins over duplicate.
        for outcome in [DispatchOutcome::Pending, DispatchOutcome::Uncertain] {
            let j = journal(&[entry(1, &hash, outcome)]);
            let rejection = admit(&grant(now), &r, &j, now).unwrap_err();
            assert_eq!(rejection.error, GoalAdmissionError::ReconciliationRequired);
            assert_eq!(rejection.data.unwrap().sequence, Some(1));
            // Reconciled: the identical request is still a duplicate.
            assert_eq!(code(admit(&ahead, &r, &j, now)), "goal_dispatch_duplicate");
        }
        let j = journal(&[entry(1, &hash, DispatchOutcome::Ok)]);
        let duplicate = admit(&grant(now), &r, &j, now).unwrap_err();
        assert_eq!(duplicate.error, GoalAdmissionError::DispatchDuplicate);
        assert_eq!(
            duplicate.data.unwrap().to_value(),
            json!({"goalId":GOAL,"sequence":1,"featureId":"f1",
                "sourceMicroversion":id('e'),"admittedAt":5})
        );
        // A rejected or not dispatched request may be sent again.
        for outcome in [
            DispatchOutcome::ProviderRejected,
            DispatchOutcome::NotDispatched,
        ] {
            let j = journal(&[entry(1, &hash, outcome)]);
            match admit(&grant(now), &r, &j, now) {
                Ok(GoalDecision::Admit(a)) => assert_eq!(a.pending().sequence, 2),
                other => assert_eq!(code(other), "admitted"),
            }
        }
        // Budget 3: NOT_DISPATCHED does not count.
        let j = journal(&[
            entry(1, &other, DispatchOutcome::Ok),
            entry(2, &other, DispatchOutcome::NotDispatched),
            entry(3, &"3".repeat(64), DispatchOutcome::ProviderRejected),
        ]);
        assert_eq!(code(admit(&grant(now), &r, &j, now)), "admitted");
        let j = journal(&[
            entry(1, &other, DispatchOutcome::Ok),
            entry(2, &"3".repeat(64), DispatchOutcome::ProviderRejected),
            entry(3, &"4".repeat(64), DispatchOutcome::Ok),
        ]);
        assert_eq!(
            code(admit(&grant(now), &r, &j, now)),
            "goal_budget_exhausted"
        );
    }

    #[test]
    fn admitted_entry_and_recheck() {
        let now = 1_000_000;
        let admission = match admit(
            &grant(now),
            &request("f2", "1 mm"),
            &Journal::default(),
            now,
        ) {
            Ok(GoalDecision::Admit(admission)) => admission,
            other => return assert_eq!(code(other), "admitted"),
        };
        assert_eq!(admission.pending().sequence, 1);
        assert_eq!(admission.pending().outcome, DispatchOutcome::Pending);
        assert_eq!(admission.pending().admitted_at, now);
        assert_eq!(admission.grant().goal_id, GOAL);
        assert_eq!(admission.request_data().sequence, None);
        assert_eq!(admission.sequence_data().admitted_at, Some(now));
        assert!(grant_still_active(&grant(now), GOAL, now));
        let mut stopped = grant(now);
        stopped["state"] = json!("STOPPED");
        assert!(!grant_still_active(&stopped, GOAL, now));
        let mut other = grant(now);
        other["goalId"] = json!("3f2a9c1e-5b7d-4e8f-9a0b-1c2d3e4f5a6c");
        assert!(!grant_still_active(&other, GOAL, now));
        assert!(!grant_still_active(&json!(null), GOAL, now));
    }

    #[test]
    fn summary_reports_effective_state() {
        let h = "1".repeat(64);
        let j = journal(&[
            entry(1, &h, DispatchOutcome::Ok),
            entry(2, &h, DispatchOutcome::NotDispatched),
            entry(3, &h, DispatchOutcome::Pending),
        ]);
        assert_eq!(
            serde_json::to_value(j.summary(GOAL, 0)).unwrap(),
            json!({"goalId":GOAL,"entries":3,"counted":2,"maxSequence":3,
                "unreconciledSequence":3,"lastOutcome":"PENDING","lastAdmittedAt":5,
                "unreconciledEntry":{"sequence":3,"grantRevision":1,"requestHash":h,
                    "featureId":"f1","sourceMicroversion":"e".repeat(24),"admittedAt":5,
                    "outcome":"PENDING","httpStatus":null}})
        );
        let reconciled = j.summary(GOAL, 3);
        assert_eq!(reconciled.unreconciled_sequence, None);
        assert_eq!(reconciled.unreconciled_entry, None);
        assert_eq!(
            serde_json::to_value(Journal::default().summary(GOAL, 0)).unwrap(),
            json!({"goalId":GOAL,"entries":0,"counted":0,"maxSequence":0,
                "unreconciledSequence":null,"lastOutcome":null,"lastAdmittedAt":null,
                "unreconciledEntry":null})
        );
    }

    #[test]
    fn approval_activity_and_capabilities() {
        assert!(approval_active(&json!({"expiresAt": 2}), 1));
        assert!(!approval_active(&json!({"expiresAt": 1}), 1));
        assert!(!approval_active(&json!(null), 1));
        assert!(!approval_active(&json!({"expiresAt": "2"}), 1));
        assert_eq!(
            serde_json::to_value(Capabilities::new(true, false)).unwrap(),
            json!({"contractVersion":"1.1.0","mutationAuthorities":["approval-v2"]})
        );
        assert_eq!(
            Capabilities::new(false, true).mutation_authorities,
            ["goal-grant-v1"]
        );
    }

    #[test]
    fn goal_reads_require_the_plain_get_shape() {
        let config = ExternalTokenConfig {
            file: "/run/token.json".into(),
            document_id: id('a'),
            workspace_id: id('d'),
            element_id: id('c'),
            mutation: Some(bounds()),
            mutation_approval_file: None,
            goal_grant_file: None,
            dispatch_journal_dir: None,
            read_api_version: None,
        };
        let now = 1_000_000;
        let workspace = grant_read_workspace(&config, &grant(now), now).unwrap();
        let ok = ApiRequest {
            method: http::Method::GET,
            path: format!("/documents/d/{}/w/{}/elements", id('a'), id('9')),
            query_params: vec![],
            headers: http::HeaderMap::new(),
            content_type: None,
            body: None,
        };
        assert!(goal_read_admitted(&config, &ok, &workspace));
        let mut post = ok.clone();
        post.method = http::Method::POST;
        let mut query = ok.clone();
        query.query_params.push(("x".into(), "1".into()));
        let mut body = ok.clone();
        body.body = Some(RequestBody::Json(json!({})));
        let mut header = ok;
        header
            .headers
            .insert(http::header::ACCEPT, http::HeaderValue::from_static("*/*"));
        for request in [post, query, body, header] {
            assert!(!goal_read_admitted(&config, &request, &workspace));
        }
        let mut read_only = config;
        read_only.mutation = None;
        assert_eq!(grant_read_workspace(&read_only, &grant(now), now), None);
    }

    #[test]
    fn codes_match_display() {
        for error in GoalAdmissionError::ALL {
            assert_eq!(error.to_string(), error.code());
        }
    }
}
