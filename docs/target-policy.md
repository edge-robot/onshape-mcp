# External-token bounded modification

The default external-token mode remains read-only. Operator provisioning may add
`[external_token.mutation]`. This is generic provider execution authority, not
workflow approval, release authority, or an agent identity check. Keep configuration
and the private network inaccessible to untrusted clients.

```toml
[external_token]
# file, document_id, workspace_id, element_id, read_api_version as before
mutation_approval_file = "/run/onshape-approval/approved-update.json" # mandatory with mutation

[external_token.mutation]
# Static bounds (required)
document_id = "aaaaaaaaaaaaaaaaaaaaaaaa"
element_id = "cccccccccccccccccccccccc"
excluded_workspace_ids = ["dddddddddddddddddddddddd"] # operator supplies Primary ID
allowed_operations = ["updatePartStudioFeature"]
write_mode = "modify"
# Static pins (optional, expand step of contract 1.0.0; required for v1 approvals)
workspace_id = "bbbbbbbbbbbbbbbbbbbbbbbb"
feature_id = "optional_exact_feature"
expected_microversion = "eeeeeeeeeeeeeeeeeeeeeeee"
```

These IDs are examples.

| Key | Required | Rule |
|---|---|---|
| `mutation_approval_file` | yes, if `mutation` is set | absolute path; otherwise the server refuses to start with `target_configuration_invalid` |
| `mutation.document_id`, `mutation.element_id` | yes | must equal `[external_token]` `document_id` / `element_id` |
| `mutation.excluded_workspace_ids` | yes, non-empty | never written; claims naming one are denied |
| `mutation.allowed_operations` | yes, non-empty | only `updatePartStudioFeature` exists |
| `mutation.write_mode` | no (default `read`) | `modify` is needed for any write |
| `mutation.workspace_id` | no | if set: equals `[external_token].workspace_id`, not excluded, and every claim must equal it |
| `mutation.feature_id` | no | if set: every claim and request must name this feature |
| `mutation.expected_microversion` | no | if set: every claim must equal it (`revision_policy_denied`) |

IDs parse into validated types; modes and operation IDs are enums. Unknown fields,
unknown operations, malformed IDs, empty exclusions/operations, conflicting targets
and an excluded pinned workspace fail closed. Omitting `mutation` or using the
(default) `read` ceiling denies all mutations. Without a workspace pin the
mutation workspace is not tied to the read binding: it comes from the approval
claims and may be any workspace of the configured document outside the exclusions. An exclusion identifies a workspace
by immutable ID, not its display name; the control plane must resolve Primary and
supply its ID. The server cannot infer which workspace is Primary from an ID alone.

The initial audited operation is existing-feature replacement only. It supports
arbitrary Part Studio feature definitions, not a project-specific parameter or
feature name. Other create/update/delete or FeatureScript endpoints remain denied
until their atomic revision semantics are audited. An optional feature restriction
narrows the authority; path feature ID must always equal `feature.featureId`.
Only an absent header set or exactly one `Accept: application/json` header is
permitted, matching the OpenAPI builder's automatic JSON response negotiation.
Other headers, duplicate Accept values, query injection, multipart bodies,
noncanonical paths and file references are refused. This pure admission rule
applies to external-token reads and mutations; it does not alter targets or bodies.
The raw `onshape_api_call` tool uses the same admission boundary.

Immediately before provider dispatch the server checks method/route, target,
operation, feature, ceiling, approval, write scope and current external credential
validity. The JSON body must contain the approved `sourceMicroversion` and the
boolean `rejectMicroversionSkew: true`. The payload is preserved, never patched to silently
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
not review, approval, or release. The approved expected microversion lets provider
CAS reject replay after a successful revision-changing write, and the in-process
consumed record rejects a second admission of the same approval; neither is a durable
attempt ledger, and does not prove rejection of simultaneous duplicates, no-op
writes or attempts whose provider outcome is unknown. Workflow receipts and
reconciliation belong to the control plane. Do not enable production mutation
without its exact approval/attempt contract and native runtime acceptance.

The explicit target scope is a seam for future verified subject, operation,
approval and expiry claims. No VC/VP framework or agent-specific naming is added.
Existing upstream authentication defaults remain unchanged.

## Reviewed payload admission and native v16 reads

For the assigned full-feature read, call `getPartStudioFeatures` with target path
parameters and omit query parameters. External-token read admission rejects query
options, including optional options advertised by API explanation.
`external_token_read_policy_denied` is local admission failure before provider I/O;
repeating OAuth consent cannot resolve it. Correct the request shape or have the
operator review the specifically required operation without bypassing the policy.

External-token deployments can set `read_api_version = "v16"` to retrieve feature
payloads with the serialization/library metadata required by native updates.
The default remains v10. Mutation dispatch remains v16 and single-attempt.

## Approval file and claims

`mutation_approval_file` points to a private JSON file published by the approval
owner (same private-file rules as the token file, at most 1 MiB). It is mandatory
whenever `[external_token.mutation]` is configured. The server only reads it; its
owner issues approvals after reviewing a concrete payload. This is generic execution
admission, not an engineering workflow or an OAuth owner.

Version 2 carries the mutation target as claims. All fields are required and
unknown fields are rejected:

```json
{"version": 2, "proposalId": "<uuid>", "issuedAt": 0, "expiresAt": 0,
 "target": {"documentId": "<24-hex>", "workspaceId": "<24-hex>", "elementId": "<24-hex>"},
 "featureId": "<feature>", "expectedMicroversion": "<24-hex>",
 "operation": "updatePartStudioFeature", "requestHash": "…", "payloadHash": "…",
 "body": {}}
```

`expiresAt > now` and `0 < expiresAt − issuedAt ≤ 900000` (15 minutes). The claims
only narrow the static bounds: the claimed document and element must equal the
configured ones, the claimed workspace must not be excluded, the operation must be
allowlisted, and every configured pin must equal its claim. `requestHash` and
`payloadHash` are audit fields and are not checked.

Version 1 (`{version:1, proposalId, expiresAt, body}`) is deprecated. It is admitted
only while the pins `workspace_id` and `expected_microversion` are configured, which
then act as its claims; otherwise it is rejected with `mutation_approval_mismatch`.

A mutation is admitted in this order; the first failure wins and nothing reaches
the provider before step 10:

1. `[external_token.mutation]` present (`target_policy_denied`).
2. `modify` ceiling, `POST`, allowlisted operation, no query, JSON headers only
   (`target_policy_denied`).
3. Approval file readable (`mutation_approval_unavailable`).
4. Approval structurally valid for its version and unexpired (`mutation_approval_mismatch`).
5. Path, feature and operation equal the claims within the bounds (`target_policy_denied`).
6. `body.sourceMicroversion` equals the claimed baseline and
   `rejectMicroversionSkew` is `true` (`revision_policy_denied`).
7. Approved body equals the request body exactly (`mutation_approval_mismatch`).
8. `proposalId` not yet admitted by this process (`mutation_approval_consumed`).
9. Credential valid with `OAuth2Write` (`external_token_*`).
10. Record `proposalId` as consumed, then a single dispatch attempt.

The consumed record is in memory, shared by v1 and v2, and bounded to the last
1024 admitted proposal IDs. It is not persisted; after a restart the provider CAS
(`rejectMicroversionSkew`) still rejects a second application of the same body.
A consumed approval is never admitted again, even if its dispatch outcome was
`UNKNOWN`; reconcile instead.

While a valid v2 approval is active (`now < expiresAt`, consumed or not) and its
claims lie within the static bounds, the reads
`/documents/d/{d}/w/{claimed}/currentmicroversion` and
`/partstudios/d/{d}/w/{claimed}/e/{e}/features` are admitted in addition to the
configured read set, so the Candidate can be verified after the update. `{d}` and
`{e}` stay the configured target. A missing, insecure, unreadable, `null`, v1 or
expired approval adds nothing.

A future verified VC/VP can replace local-file issuer trust and supply subject,
audience, target, operation, payload digest, source revision, approval and expiry
claims to the existing typed policy. No DID resolver, credential issuer, proof
format or revocation service is introduced in this increment. Provider CAS and
single-attempt dispatch do not constitute a durable exactly-once execution ledger;
uncertain writes require control-plane reconciliation rather than automatic retry.

## Goal grants (contract 1.1.0)

A goal grant lets one human-confirmed goal admit several agent-authored
`updatePartStudioFeature` calls in one Candidate workspace, within a scope and
budget that this server enforces. The normative rules are in the interface
contract, `clawonshape/docs/interface/onshape-mcp.md` §12 (grant schema, admission
order, journal, STOP, reads, `/capabilities`); this section only describes the
server configuration and behaviour.

```toml
[external_token]
mutation_approval_file = "/run/onshape-approval/approved-update.json"
goal_grant_file = "/run/onshape-approval/goal-grant.json"       # optional
dispatch_journal_dir = "/var/lib/onshape-mcp/journal"          # required with goal_grant_file
```

| Key | Required | Rule |
|---|---|---|
| `goal_grant_file` | no | absolute, canonical; same private-file rules as the approval file, at most 64 KiB |
| `dispatch_journal_dir` | yes, if `goal_grant_file` is set | absolute, canonical; otherwise the server refuses to start with `target_configuration_invalid`. Server-owned, rw, mode `0700`, persistent across restarts and upgrades; not shared with any other container |

Authority selection on every mutation: a non-`null` grant together with an
unexpired approval yields `mutation_authority_conflict`; a non-`null` grant
selects goal admission; a `null` grant keeps the approval path above unchanged.
A configured but missing or insecure grant file yields `goal_grant_unavailable`.

Goal admission checks, in contract order and before any provider I/O: the static
request shape, the grant (structure, expiry, 8 h lifetime, budget ≤ 25, state),
the static bounds (including the optional expand-step pins), the grant scope
(Candidate path, listed feature IDs, allowed body keys, ≤ 900 000 B canonical
body), the revision fields, and the journal (reconciliation, duplicate
`requestHash`, budget). Then the token is checked, a `PENDING` line is appended
and fsynced, and the grant is re-read: if it is no longer `ACTIVE` for the same
goal, the entry becomes `NOT_DISPATCHED` and the call fails with
`goal_grant_stopped`. Otherwise the request is dispatched exactly once and the
outcome is appended (`OK`, `PROVIDER_REJECTED`, or `UNCERTAIN` with the usual
`UNKNOWN` error plus goal data). An outcome that cannot be recorded is reported
as `UNKNOWN`; the remaining `PENDING` line blocks the goal like `UNCERTAIN`.

The journal is `<dispatch_journal_dir>/<goalId>.jsonl`, append-only, never
truncated by the server. It is the only server state; it makes budget, duplicate
and uncertain-outcome blocking survive restarts. Goal admissions are serialized
process-wide by one lock, held from the grant read to the recorded outcome. An
unreadable, insecure or corrupt journal fails closed with
`dispatch_journal_unavailable`.

While an unexpired grant (`ACTIVE` or `STOPPED`) within the static bounds exists,
GETs of `currentmicroversion`, `elements` and `features` of its Candidate are
admitted in addition to the configured read set.

Two unauthenticated local routes support the plugin; neither touches the
provider:

- `GET /capabilities` lists `approval-v2` when `mutation_approval_file` is set and
  `goal-grant-v1` when the goal keys are set and the journal directory passes the
  private checks.
- `GET /goals/{goalId}/journal-summary` returns entries, counted entries,
  highest sequence, the unreconciled sequence (against the current grant's
  `reconciledThrough`), last outcome and admission time. A non-uuid-v4 `goalId`
  or a server without goal keys yields `404`; an unreadable journal yields `503`
  with `{"code":"dispatch_journal_unavailable"}`.
