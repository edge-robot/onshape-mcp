# External-token bounded modification

The default external-token mode remains read-only. Operator provisioning may add
`[external_token.mutation]`. This is generic provider execution authority, not
workflow approval, release authority, or an agent identity check. Keep configuration
and the private network inaccessible to untrusted clients.

```toml
[external_token.mutation]
document_id = "aaaaaaaaaaaaaaaaaaaaaaaa"
workspace_id = "bbbbbbbbbbbbbbbbbbbbbbbb"
element_id = "cccccccccccccccccccccccc"
excluded_workspace_ids = ["dddddddddddddddddddddddd"] # operator supplies Primary ID
allowed_operations = ["updatePartStudioFeature"]
feature_id = "optional_exact_feature"
expected_microversion = "eeeeeeeeeeeeeeeeeeeeeeee"
write_mode = "modify"
```

These IDs are examples. The target must equal the external-token read binding.
IDs parse into validated types; modes and operation IDs are enums. Unknown fields,
unknown operations, malformed IDs, empty exclusions/operations, conflicting targets
and an excluded assigned workspace fail closed. Omitting `mutation` or using the
(default) `read` ceiling denies all mutations. An exclusion identifies a workspace
by immutable ID, not its display name; the control plane must resolve Primary and
supply its ID. The server cannot infer which workspace is Primary from an ID alone.

The initial audited operation is existing-feature replacement only. It supports
arbitrary Part Studio feature definitions, not a project-specific parameter or
feature name. Other create/update/delete or FeatureScript endpoints remain denied
until their atomic revision semantics are audited. An optional feature restriction
narrows the authority; path feature ID must always equal `feature.featureId`.
Header/query injection, multipart bodies, noncanonical paths and file references
are refused. The raw `onshape_api_call` tool uses the same admission boundary.

Immediately before provider dispatch the server checks method/route, target,
operation, feature, ceiling, write scope and current external credential validity.
The JSON body must contain the configured `sourceMicroversion` and the boolean
`rejectMicroversionSkew: true`. The payload is preserved, never patched to silently
make a caller's unguarded request valid. The vendored OpenAPI catalog declares API
v16 for this operation; bounded updates use v16, existing read-only calls retain
v10. Live v16 compatibility and actual provider skew rejection require separate
operational acceptance; unit tests do not establish them.

Single-attempt clients disable redirects and transport retries. No OAuth flow,
refresh or 401 retry runs in this mode. A transport/response-read failure, HTTP
408, 5xx, or redirect after mutation dispatch returns an MCP error with:

```json
{"outcome":"UNKNOWN","retryAllowed":false,"reconciliationRequired":true}
```

The caller must independently reconcile provider state. A successful response is
not review, approval, or release. The fixed expected microversion lets provider CAS
reject replay after a successful revision-changing write; it is not a durable
attempt ledger, and does not prove rejection of simultaneous duplicates, no-op
writes or attempts whose provider outcome is unknown. Workflow receipts and
reconciliation belong to the control plane. Do not enable production mutation
without its exact approval/attempt contract and native runtime acceptance.

The explicit target scope is a seam for future verified subject, operation,
approval and expiry claims. No VC/VP framework or agent-specific naming is added.
Existing upstream authentication defaults remain unchanged.
