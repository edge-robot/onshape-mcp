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

impl CadId {
    /// The validated 24-hex ID.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

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

impl OperationId {
    /// Parse the contract operation name; unknown names yield `None`.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "updatePartStudioFeature" => Some(Self::UpdatePartStudioFeature),
            _ => None,
        }
    }
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

/// Operator-issued static mutation bounds.
///
/// Authentication remains a separate boundary. The dynamic target (workspace, feature, expected microversion)
/// comes from approval claims ([`MutationClaims`]); the optional static pins
/// can only narrow it further.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TargetConstraint {
    /// Exact document.
    pub document_id: CadId,
    /// Optional static workspace pin (expand step). Required for v1 approvals;
    /// if present, the workspace claim must equal it.
    #[serde(default)]
    pub workspace_id: Option<CadId>,
    /// Exact Part Studio.
    pub element_id: CadId,
    /// Workspaces that must never be written (include the Primary workspace).
    pub excluded_workspace_ids: BTreeSet<CadId>,
    /// Explicit operation allowlist.
    pub allowed_operations: BTreeSet<OperationId>,
    /// Optional exact feature restriction; path and payload must always agree.
    #[serde(default)]
    pub feature_id: Option<String>,
    /// Optional static provider-side baseline pin (expand step). Required for
    /// v1 approvals; if present, the microversion claim must equal it.
    #[serde(default)]
    pub expected_microversion: Option<CadId>,
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

/// The mutation target asserted for one request: approval v2 claims, or the
/// static pins for a v1 approval.
#[derive(Clone, Copy, Debug)]
pub struct MutationClaims<'a> {
    /// Claimed document; must equal the static bound.
    pub document_id: &'a CadId,
    /// Claimed workspace; must not be excluded.
    pub workspace_id: &'a CadId,
    /// Claimed Part Studio; must equal the static bound.
    pub element_id: &'a CadId,
    /// Claimed feature, or `None` for any feature (v1 without feature pin).
    pub feature_id: Option<&'a str>,
    /// Claimed provider-side baseline.
    pub expected_microversion: &'a CadId,
    /// Claimed operation name; must be allowlisted.
    pub operation: &'a str,
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
    /// The static workspace pin, if configured, must equal the read binding.
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
            || self
                .workspace_id
                .as_ref()
                .is_some_and(|pin| pin.0 != workspace)
            || self.element_id.0 != element
        {
            return Err(PolicyError::InvalidConfiguration);
        }
        self.validate_bounds()
    }

    /// Check the bounds for internal consistency, independent of the read binding.
    fn validate_bounds(&self) -> Result<(), PolicyError> {
        if self.excluded_workspace_ids.is_empty()
            || self
                .workspace_id
                .as_ref()
                .is_some_and(|pin| self.excluded_workspace_ids.contains(pin))
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

    /// The claims a v1 approval implies: the static pins. `None` if the
    /// workspace or microversion pin is absent (v1 is then not admissible).
    #[must_use]
    pub fn pinned_claims(&self) -> Option<MutationClaims<'_>> {
        Some(MutationClaims {
            document_id: &self.document_id,
            workspace_id: self.workspace_id.as_ref()?,
            element_id: &self.element_id,
            feature_id: self.feature_id.as_deref(),
            expected_microversion: self.expected_microversion.as_ref()?,
            operation: "updatePartStudioFeature",
        })
    }

    /// Admit an exact structured request without modifying its payload, with
    /// the static pins as claims (v1 semantics).
    ///
    /// # Errors
    /// Rejects all operations except allowlisted, revision-bound feature
    /// updates on the pinned target; `Denied` if a pin is absent.
    pub fn authorize(&self, request: &ApiRequest) -> Result<(), PolicyError> {
        self.check_request_shape(request)?;
        let claims = self.pinned_claims().ok_or(PolicyError::Denied)?;
        self.authorize_claims(request, &claims)
    }

    /// Contract §2.5 step 2: write ceiling, method, operation allowlist, no
    /// query parameters and JSON response headers only. Needs no approval.
    /// The operation is identified by canonical HTTP method/route, so aliases
    /// or raw endpoint invocation cannot bypass the same execution boundary.
    ///
    /// # Errors
    /// `InvalidConfiguration` for inconsistent bounds, otherwise `Denied`.
    pub fn check_request_shape(&self, request: &ApiRequest) -> Result<(), PolicyError> {
        self.validate_bounds()?;
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
        Ok(())
    }

    /// Contract §2.5 steps 5 (target) and 6 (revision): the claims must lie
    /// within the static bounds, equal every configured pin, and match the
    /// request path and body exactly. The request is never modified.
    ///
    /// # Errors
    /// `Denied` for target violations, `Revision` for baseline or skew-flag
    /// violations (including a microversion pin that differs from its claim).
    pub fn authorize_claims(
        &self,
        request: &ApiRequest,
        claims: &MutationClaims<'_>,
    ) -> Result<(), PolicyError> {
        let operation_allowed = OperationId::from_name(claims.operation)
            .is_some_and(|op| self.allowed_operations.contains(&op));
        if claims.document_id != &self.document_id
            || claims.element_id != &self.element_id
            || self.excluded_workspace_ids.contains(claims.workspace_id)
            || self
                .workspace_id
                .as_ref()
                .is_some_and(|pin| pin != claims.workspace_id)
            || !operation_allowed
        {
            return Err(PolicyError::Denied);
        }
        let prefix = format!(
            "/partstudios/d/{}/w/{}/e/{}/features/featureid/",
            self.document_id.0, claims.workspace_id.0, self.element_id.0
        );
        let feature_id = request
            .path
            .strip_prefix(&prefix)
            .ok_or(PolicyError::Denied)?;
        if !feature_id_valid(feature_id)
            || claims.feature_id.is_some_and(|id| id != feature_id)
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
            != Some(claims.expected_microversion.0.as_str())
            || self
                .expected_microversion
                .as_ref()
                .is_some_and(|pin| pin != claims.expected_microversion)
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
            ("expected_microversion", json!("bad")),
            ("workspace_id", json!(42)),
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
    fn bounds() -> TargetConstraint {
        let mut v = value();
        let object = v.as_object_mut().unwrap();
        for pin in ["workspace_id", "feature_id", "expected_microversion"] {
            object.remove(pin);
        }
        serde_json::from_value(v).unwrap()
    }
    fn id(c: char) -> CadId {
        CadId(c.to_string().repeat(24))
    }
    fn claims<'a>(d: &'a CadId, w: &'a CadId, e: &'a CadId, mv: &'a CadId) -> MutationClaims<'a> {
        MutationClaims {
            document_id: d,
            workspace_id: w,
            element_id: e,
            feature_id: Some("feature_1"),
            expected_microversion: mv,
            operation: "updatePartStudioFeature",
        }
    }
    #[test]
    fn pins_are_optional_and_unpinned_bounds_need_claims() {
        let p = bounds();
        assert!(p.workspace_id.is_none() && p.expected_microversion.is_none());
        // Without a workspace pin the read workspace is unconstrained.
        assert_eq!(
            p.validate(&"a".repeat(24), &"d".repeat(24), &"c".repeat(24)),
            Ok(())
        );
        assert!(p.pinned_claims().is_none());
        assert_eq!(p.authorize(&request()), Err(PolicyError::Denied));
        let (d, w, e, mv) = (id('a'), id('b'), id('c'), id('e'));
        assert_eq!(
            p.authorize_claims(&request(), &claims(&d, &w, &e, &mv)),
            Ok(())
        );
        // A configured workspace pin must still equal the read binding.
        assert!(
            policy()
                .validate(&"a".repeat(24), &"f".repeat(24), &"c".repeat(24))
                .is_err()
        );
    }
    #[test]
    fn claims_are_bounded_by_static_bounds_and_pins() {
        let (d, w, e, mv) = (id('a'), id('b'), id('c'), id('e'));
        let other = id('f');
        let excluded = id('d');
        for p in [bounds(), policy()] {
            let ok = claims(&d, &w, &e, &mv);
            assert_eq!(p.authorize_claims(&request(), &ok), Ok(()));
            for bad in [
                MutationClaims {
                    document_id: &other,
                    ..ok
                },
                MutationClaims {
                    element_id: &other,
                    ..ok
                },
                MutationClaims {
                    workspace_id: &other,
                    ..ok
                },
                MutationClaims {
                    feature_id: Some("feature_2"),
                    ..ok
                },
                MutationClaims {
                    operation: "deletePartStudioFeature",
                    ..ok
                },
            ] {
                assert_eq!(
                    p.authorize_claims(&request(), &bad),
                    Err(PolicyError::Denied)
                );
            }
            assert_eq!(
                p.authorize_claims(
                    &request(),
                    &MutationClaims {
                        expected_microversion: &other,
                        ..ok
                    }
                ),
                Err(PolicyError::Revision)
            );
        }
        // An excluded workspace is never a valid claim, even if the path matches.
        let mut r = request();
        r.path = r.path.replace(&"b".repeat(24), &"d".repeat(24));
        assert_eq!(
            bounds().authorize_claims(&r, &claims(&d, &excluded, &e, &mv)),
            Err(PolicyError::Denied)
        );
        // Claims may name another workspace only without a workspace pin.
        let mut r = request();
        r.path = r.path.replace(&"b".repeat(24), &"9".repeat(24));
        let candidate = id('9');
        assert_eq!(
            bounds().authorize_claims(&r, &claims(&d, &candidate, &e, &mv)),
            Ok(())
        );
        assert_eq!(
            policy().authorize_claims(&r, &claims(&d, &candidate, &e, &mv)),
            Err(PolicyError::Denied)
        );
        // A feature claim of `None` accepts any valid feature (v1 semantics),
        // but a feature pin still applies.
        let mut r = request();
        r.path = r.path.replace("feature_1", "feature_2");
        if let Some(RequestBody::Json(body)) = &mut r.body {
            body["feature"]["featureId"] = json!("feature_2");
        }
        let any = MutationClaims {
            feature_id: None,
            ..claims(&d, &w, &e, &mv)
        };
        assert_eq!(bounds().authorize_claims(&r, &any), Ok(()));
        assert_eq!(
            policy().authorize_claims(&r, &any),
            Err(PolicyError::Denied)
        );
        // A microversion pin that differs from its claim is a revision failure.
        let mut r = request();
        if let Some(RequestBody::Json(body)) = &mut r.body {
            body["sourceMicroversion"] = json!("9".repeat(24));
        }
        assert_eq!(
            policy().authorize_claims(&r, &claims(&d, &w, &e, &candidate)),
            Err(PolicyError::Revision)
        );
    }
}
