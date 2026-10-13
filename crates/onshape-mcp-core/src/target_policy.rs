//! Pure admission policy for externally authorized, revision-bound mutations.
//! Authority is provisioned by the operator, never inferred from tool annotations.
use std::collections::BTreeSet;

use onshape_client_core::request::{ApiRequest, RequestBody};
use serde::Deserialize;

/// Permit only the fixed JSON response negotiation used by the API builder.
/// Other headers, alternate representations and duplicate values fail closed.
pub fn permits_json_response_headers(headers: &http::HeaderMap) -> bool {
    headers.is_empty()
        || (headers.len() == 1
            && headers.get(http::header::ACCEPT)
                == Some(&http::HeaderValue::from_static("application/json")))
}

/// A validated provider ID (document, workspace, element, or microversion).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(try_from = "String")]
pub struct CadId(String);

impl TryFrom<String> for CadId {
    type Error = PolicyError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        if value.len() == 24
            && value
                .bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
        {
            Ok(Self(value))
        } else {
            Err(PolicyError::InvalidConfiguration)
        }
    }
}

/// Operations with audited revision semantics. Other operations fail closed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
pub enum OperationId {
    /// Replace an existing Part Studio feature definition.
    #[serde(rename = "updatePartStudioFeature")]
    UpdatePartStudioFeature,
}

/// Maximum mutation authority; read is the default.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WriteMode {
    /// Deny all writes.
    #[default]
    Read,
    /// Allow only configured operations on the exact target and revision.
    Modify,
}

/// Operator-issued target scope. Authentication remains a separate boundary.
/// This scope can later be derived from verified subject/approval/expiry claims.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TargetConstraint {
    /// Exact document.
    pub document_id: CadId,
    /// Exact assigned workspace.
    pub workspace_id: CadId,
    /// Exact Part Studio.
    pub element_id: CadId,
    /// Workspaces that must never be written (include the Primary workspace).
    pub excluded_workspace_ids: BTreeSet<CadId>,
    /// Explicit operation allowlist.
    pub allowed_operations: BTreeSet<OperationId>,
    /// Optional exact feature restriction; path and payload must always agree.
    pub feature_id: Option<String>,
    /// Required provider-side baseline for every permitted mutation.
    pub expected_microversion: CadId,
    /// Ceiling applied independently of MCP annotations.
    #[serde(default)]
    pub write_mode: WriteMode,
}

/// Stable, non-secret admission failure categories.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum PolicyError {
    /// Invalid or contradictory authority.
    #[error("target_configuration_invalid")]
    InvalidConfiguration,
    /// Wrong target, unsupported operation, or write ceiling exceeded.
    #[error("target_policy_denied")]
    Denied,
    /// Required provider-side compare-and-swap fields absent or mismatched.
    #[error("revision_policy_denied")]
    Revision,
}

fn feature_id_valid(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
}

impl TargetConstraint {
    /// Check configuration before any provider I/O.
    ///
    /// # Errors
    /// Rejects conflicting targets, missing exclusions or malformed feature IDs.
    pub fn validate(
        &self,
        document: &str,
        workspace: &str,
        element: &str,
    ) -> Result<(), PolicyError> {
        if self.document_id.0 != document
            || self.workspace_id.0 != workspace
            || self.element_id.0 != element
            || self.excluded_workspace_ids.is_empty()
            || self.excluded_workspace_ids.contains(&self.workspace_id)
            || self.allowed_operations.is_empty()
            || self
                .feature_id
                .as_deref()
                .is_some_and(|id| !feature_id_valid(id))
        {
            return Err(PolicyError::InvalidConfiguration);
        }
        Ok(())
    }

    /// Admit an exact structured request without modifying its payload.
    /// The operation is identified by canonical HTTP method/route, so aliases
    /// or raw endpoint invocation cannot bypass the same execution boundary.
    ///
    /// # Errors
    /// Rejects all operations except allowlisted, revision-bound feature updates.
    pub fn authorize(&self, request: &ApiRequest) -> Result<(), PolicyError> {
        self.validate(
            &self.document_id.0,
            &self.workspace_id.0,
            &self.element_id.0,
        )?;
        if self.write_mode != WriteMode::Modify
            || request.method != http::Method::POST
            || !self
                .allowed_operations
                .contains(&OperationId::UpdatePartStudioFeature)
            || !request.query_params.is_empty()
            || !permits_json_response_headers(&request.headers)
        {
            return Err(PolicyError::Denied);
        }
        let prefix = format!(
            "/partstudios/d/{}/w/{}/e/{}/features/featureid/",
            self.document_id.0, self.workspace_id.0, self.element_id.0
        );
        let feature_id = request
            .path
            .strip_prefix(&prefix)
            .ok_or(PolicyError::Denied)?;
        if !feature_id_valid(feature_id)
            || self
                .feature_id
                .as_deref()
                .is_some_and(|id| id != feature_id)
        {
            return Err(PolicyError::Denied);
        }
        let Some(RequestBody::Json(body)) = &request.body else {
            return Err(PolicyError::Denied);
        };
        if body
            .pointer("/feature/featureId")
            .and_then(serde_json::Value::as_str)
            != Some(feature_id)
        {
            return Err(PolicyError::Denied);
        }
        if body
            .get("sourceMicroversion")
            .and_then(serde_json::Value::as_str)
            != Some(self.expected_microversion.0.as_str())
            || body
                .get("rejectMicroversionSkew")
                .and_then(serde_json::Value::as_bool)
                != Some(true)
        {
            return Err(PolicyError::Revision);
        }
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use serde_json::json;

    fn value() -> serde_json::Value {
        json!({"document_id":"a".repeat(24),"workspace_id":"b".repeat(24),
            "element_id":"c".repeat(24),"excluded_workspace_ids":["d".repeat(24)],
            "allowed_operations":["updatePartStudioFeature"],"feature_id":"feature_1",
            "expected_microversion":"e".repeat(24),"write_mode":"modify"})
    }
    fn policy() -> TargetConstraint {
        serde_json::from_value(value()).unwrap()
    }
    fn request() -> ApiRequest {
        ApiRequest {
            method: http::Method::POST,
            path: format!(
                "/partstudios/d/{}/w/{}/e/{}/features/featureid/feature_1",
                "a".repeat(24),
                "b".repeat(24),
                "c".repeat(24)
            ),
            query_params: vec![],
            headers: http::HeaderMap::new(),
            content_type: None,
            body: Some(RequestBody::Json(
                json!({"feature":{"featureId":"feature_1"},
                "sourceMicroversion":"e".repeat(24),"rejectMicroversionSkew":true}),
            )),
        }
    }
    #[test]
    fn configuration_fails_closed() {
        for (key, bad) in [
            ("document_id", json!("bad")),
            ("expected_microversion", json!(null)),
            ("allowed_operations", json!(["deletePartStudioFeature"])),
            ("write_mode", json!("destroy")),
            ("unknown", json!(true)),
        ] {
            let mut v = value();
            v[key] = bad;
            assert!(serde_json::from_value::<TargetConstraint>(v).is_err());
        }
        for excluded in [json!([]), json!(["b".repeat(24)])] {
            let mut v = value();
            v["excluded_workspace_ids"] = excluded;
            let p: TargetConstraint = serde_json::from_value(v).unwrap();
            assert!(
                p.validate(&"a".repeat(24), &"b".repeat(24), &"c".repeat(24))
                    .is_err()
            );
        }
        let mut v = value();
        v.as_object_mut().unwrap().remove("write_mode");
        assert_eq!(
            serde_json::from_value::<TargetConstraint>(v)
                .unwrap()
                .authorize(&request()),
            Err(PolicyError::Denied)
        );
    }
    #[test]
    fn exact_target_and_revision_preserved() {
        let p = policy();
        let r = request();
        let before = serde_json::to_value(&r).unwrap();
        assert_eq!(p.authorize(&r), Ok(()));
        assert_eq!(serde_json::to_value(&r).unwrap(), before);
        assert!(
            p.validate(&"f".repeat(24), &"b".repeat(24), &"c".repeat(24))
                .is_err()
        );
    }
    #[test]
    fn json_response_header_admitted_without_payload_changes() {
        let p = policy();
        let mut r = request();
        r.headers.insert(
            http::header::ACCEPT,
            http::HeaderValue::from_static("application/json"),
        );
        let before = serde_json::to_value(&r).unwrap();
        assert_eq!(p.authorize(&r), Ok(()));
        assert_eq!(serde_json::to_value(&r).unwrap(), before);
        for value in ["*/*", "text/plain", "application/json, text/plain"] {
            r.headers
                .insert(http::header::ACCEPT, http::HeaderValue::from_static(value));
            assert_eq!(p.authorize(&r), Err(PolicyError::Denied));
        }
        r.headers.insert(
            http::header::ACCEPT,
            http::HeaderValue::from_static("application/json"),
        );
        r.headers.append(
            http::header::ACCEPT,
            http::HeaderValue::from_static("application/json"),
        );
        assert_eq!(p.authorize(&r), Err(PolicyError::Denied));
    }
    #[test]
    fn wrong_targets_operations_and_escape_rejected() {
        let p = policy();
        for id in ['a', 'b', 'c'] {
            let mut r = request();
            r.path = r.path.replace(&id.to_string().repeat(24), &"f".repeat(24));
            assert_eq!(p.authorize(&r), Err(PolicyError::Denied));
        }
        for suffix in ["/../feature_1", "%2ffeature_1", "?x=1"] {
            let mut r = request();
            r.path.push_str(suffix);
            assert!(p.authorize(&r).is_err());
        }
        let mut r = request();
        r.method = http::Method::DELETE;
        assert!(p.authorize(&r).is_err());
        r = request();
        r.path = r.path.replace(&"b".repeat(24), &"d".repeat(24));
        assert!(p.authorize(&r).is_err());
        r = request();
        r.path = r.path.replace("feature_1", "feature_2");
        assert!(p.authorize(&r).is_err());
        r = request();
        r.query_params.push(("x".into(), "1".into()));
        assert!(p.authorize(&r).is_err());
        r = request();
        r.headers
            .insert("authorization", http::HeaderValue::from_static("bad"));
        assert!(p.authorize(&r).is_err());
        let mut p = policy();
        p.allowed_operations.clear();
        assert!(p.authorize(&request()).is_err());
    }
    #[test]
    fn revision_fields_required_and_typed() {
        for (key, bad) in [
            ("sourceMicroversion", json!("f".repeat(24))),
            ("sourceMicroversion", json!(null)),
            ("rejectMicroversionSkew", json!(false)),
            ("rejectMicroversionSkew", json!("true")),
            ("rejectMicroversionSkew", json!(null)),
        ] {
            let mut r = request();
            if let Some(RequestBody::Json(body)) = &mut r.body {
                body[key] = bad;
            }
            assert_eq!(policy().authorize(&r), Err(PolicyError::Revision));
        }
        let mut r = request();
        r.body = None;
        assert!(policy().authorize(&r).is_err());
        r = request();
        if let Some(RequestBody::Json(body)) = &mut r.body {
            body["feature"]["featureId"] = json!("other");
        }
        assert!(policy().authorize(&r).is_err());
    }
}
