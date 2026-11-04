---
prompt: docs/prompts/2026-10-16-approval-v2-expand.md
branch: claw-onshape/external-token
commits: e894809..344d3e0
status: done
contract-version: 1.0.0-draft
e2e-request: required
---
# Report: Expand: admit approval file v2 with target claims (contract 1.0.0-draft, step 1 of 4)

## Summary

The server now accepts approval files v1 (deprecated) and v2. It admits the mutation target
from the v2 claims within the static bounds and makes the approval file mandatory at startup.
It also rejects replays of an admitted `proposalId` with `mutation_approval_consumed`. While a
valid v2 approval is active, it admits the two Candidate reads.

The pure logic is in a new module, `onshape-mcp-core::approval`:
- approval parsing;
- the §2.5 order for steps 1–8;
- the replay record;
- the §2.4 read set, moved from io.

`onshape-mcp-io` only reads the files, checks the token (step 9), records the proposal and
dispatches (step 10).

The deployed config shape (static pins, v1 from plugin 0.2.10) parses, validates and admits
the v1 fixture unchanged. The `mcp-tools.json` exact match is still green. The fixture copy
and `VERSION` were not touched.

Commits (each with `Refs: edge-robot/onshape-mcp#2, edge-robot/clawonshape#14`):
1. `e894809` test: 🧪 contract 1.0.0-draft approval v2 fixtures and behaviours (red)
2. `53c058c` feat: 🔐 optional pins, mandatory approval file at startup
3. `4eaeb4e` feat: 🔐 admit approval v2 target claims, replay protection, claimed reads
4. `344d3e0` docs: 📖 target policy with approval claims
5. this report + prompt `status: done`

## Changed files

`git diff --stat 7377300..344d3e0`: 11 files, +1756 / −135.

| File | Purpose |
|---|---|
| `crates/onshape-mcp-core/src/approval.rs` (new) | `Approval` v1/v2 parsing, `AdmissionError` + codes, `admit_mutation` (§2.5 steps 1–8), `ConsumedProposals` (replay record), `read_admitted` / `claimed_read_workspace` (§2.4) |
| `crates/onshape-mcp-core/src/target_policy.rs` | `workspace_id` and `expected_microversion` pins become optional (`feature_id` already was). `MutationClaims`. `authorize` is split into `check_request_shape` (step 2) and `authorize_claims` (steps 5–6). `pinned_claims` for v1. `CadId::is_valid` / `as_str`, `OperationId::from_name` |
| `crates/onshape-mcp-core/src/config.rs` | `ExternalTokenConfig::require_mutation_approval_file` (§8.8); doc of `mutation_approval_file` |
| `crates/onshape-mcp-core/src/lib.rs` | `pub mod approval` |
| `crates/onshape-mcp-core/tests/contract.rs` | `CONTRACT_VERSION = "1.0.0-draft"`; fixture-driven approval tests, both pin modes, replay, missingApprovalFile, readsV2, deployed-config regression, error-code list |
| `crates/onshape-mcp-io/src/external_token.rs` | `validate_config` startup refusal. `execute` in §2.5 order with the replay record. `read_allowed` with claimed reads. io tests on real private temp files |
| `crates/onshape-mcp-io/src/lib.rs` | `ApiState::ExternalToken` carries `ConsumedProposals`; match arms |
| `crates/onshape-mcp-io/src/request.rs` | match arm `ExternalToken(..)` |
| `docs/target-policy.md` | mandatory approval, config key table, v2 claims, v1 deprecation, admission order, replay bound, claimed reads |
| `docs/src/project/overview.md` | "approval- and revision-bound" wording |
| `docs/src/project/changelog.md` (`CHANGELOG.md` symlink) | Unreleased entry |

## Checks

| Command | Result |
|---|---|
| `cargo fmt --all --check` | pass |
| `cargo clippy --all-targets --all-features -- -D warnings` | pass |
| `cargo clippy --all-targets -- -D warnings` | pass |
| `cargo nextest run --all-features` | pass: 637 run, 637 passed, 0 skipped |
| `cargo test --workspace` | pass: 638 passed, 0 failed (incl. doctests) |
| `cargo deny check` | pass: `advisories ok, bans ok, licenses ok, sources ok` |
| `mdbook build docs` | pass (HTML written) |
| `python scripts/apply-openapi-overlays.py --check` | not run: OpenAPI and overlays untouched |
| `cargo llvm-cov nextest --workspace --all-features` | `approval.rs` 98.4 % lines; `target_policy.rs` 100 % lines; `external_token.rs` 82.3 % lines (the gap is mostly the existing token/dispatch path, see *Open points*) |

Contract test binary (`-p onshape-mcp-core --test contract`): 13/13 pass:
- `contract_version_matches`
- `external_token_tools_match_contract_snapshot`
- `external_token_tool_names_match_snapshot`
- `first_difference_names_the_first_differing_key`
- `every_approval_fixture_is_listed_in_expectations`
- `approval_fixtures_with_v1_target_pins`
- `approval_v2_fixtures_with_static_bounds_only`
- `approval_replay_is_consumed_without_dispatch`
- `deployed_config_shape_admits_v1_fixture`
- `missing_approval_file_refuses_start`
- `reads_of_claimed_candidate_while_v2_active`
- `inactive_or_invalid_approval_adds_no_reads`
- `admission_codes_are_listed_in_contract`

### Red → green

| Test | Red at | Green at |
|---|---|---|
| all new tests in `tests/contract.rs` (approval fixtures in both modes, replay, missingApprovalFile, readsV2, deployed config, error codes) | `e894809`: compile error `unresolved import onshape_mcp_core::approval`, `no method require_mutation_approval_file` | `4eaeb4e` |
| `contract_version_matches` (`CONTRACT_VERSION` 0.2.0 → 1.0.0-draft) | before `e894809`: VERSION line 1 `1.0.0-draft` ≠ `0.2.0-draft` | `e894809` (test-only change) |
| `target_policy::tests::pins_are_optional_and_unpinned_bounds_need_claims`, `claims_are_bounded_by_static_bounds_and_pins` | written together with the change, not run red separately | `53c058c` |
| `external_token::tests::mutation_without_approval_file_refuses_to_start` | written together with the change, not run red separately | `53c058c` |
| `approval::tests::*` (6), `external_token::tests::{v1_approval_flow_with_deployed_pins_is_unchanged, v2_claims_admit_candidate_and_v1_needs_pins, replayed_approval_is_consumed_before_token_or_dispatch, claimed_candidate_reads_follow_the_approval_file}` | new API, written with it | `4eaeb4e` |

Changed existing tests:
- `target_policy::tests::configuration_fails_closed`: `expected_microversion: null` is now a valid absent pin. It is replaced by `"bad"` and `workspace_id: 42`.
- `external_token::tests::approval_requires_exact_body_and_current_expiry` is replaced by the io tests above. The core tests `approval::tests::v1_keeps_its_pre_1_0_checks` and `admitted_v2_body_must_match_and_is_consumed_once` now cover the same v1 behaviour.

## Acceptance criteria

| Criterion | Result | Evidence |
|---|---|---|
| Every fixture yields the code in `expectations.json`, in both pin modes where applicable | met | `approval_fixtures_with_v1_target_pins`, `approval_v2_fixtures_with_static_bounds_only`; the table below |
| Replay tested | met | contract `approval_replay_is_consumed_without_dispatch` (v1+v2 share one record; the second admission returns no `AdmittedProposal`). io `replayed_approval_is_consumed_before_token_or_dispatch` (token file missing → `mutation_approval_consumed`, not `external_token_unavailable`, so the request stops before step 9) |
| Startup refusal tested | met | contract `missing_approval_file_refuses_start`; io `mutation_without_approval_file_refuses_to_start` (`validate_config` → `target_configuration_invalid`; `build_api_state` calls it for stdio and HTTP) |
| Read extension positive + negative (expired, `null`, unreadable) | met | contract `reads_of_claimed_candidate_while_v2_active`, `inactive_or_invalid_approval_adds_no_reads`; io `claimed_candidate_reads_follow_the_approval_file` (missing file, expired, mode 0644, `null`, non-JSON) |
| v1 flow with the live config unchanged, regression test | met | contract `deployed_config_shape_admits_v1_fixture` (TOML of the live shape); io `v1_approval_flow_with_deployed_pins_is_unchanged` |
| All existing tests green incl. `mcp-tools.json` exact match | met | 637/637; `external_token_tools_match_contract_snapshot` |
| Admission order §2.5 1–10, no provider I/O before 9/10 | met | `admit_mutation` evaluates steps 1–8 and reads the file only after steps 1–2 (`approval_is_read_only_after_static_request_checks`). In io, `load` (step 9), the client build and `consumed.record` (step 10) come after admission |
| Replay record bounded, explained, not persisted | met | `CONSUMED_PROPOSALS_CAPACITY = 1024`, `replay_record_is_bounded_and_evicts_oldest_first`; see *Replay record* |
| New code in the server's code list; `tools/list` byte-identical | met | `AdmissionError::ApprovalConsumed`; `admission_codes_are_listed_in_contract` checks all admission codes against `error-codes.json`; snapshot test green |
| `CONTRACT_VERSION` bumped, fixtures and `VERSION` untouched | met | `git diff 7377300..HEAD -- crates/onshape-mcp-core/tests/contract/` is empty |
| Docs updated | met | `docs/target-policy.md` |
| No new `unwrap`/`expect` in non-test code | met | none added |

### Fixture → code (paired request `requests/update-feature.request.json`)

"Pinned" = `boundsV2` plus the `target` pins `workspace_id` and `expected_microversion`.
"Bounds only" = `boundsV2`. The actual result equals the expected value in every row (asserted).

| Fixture | Pinned | Bounds only |
|---|---|---|
| `valid/update-feature.json` (v1) | accepted | mutation_approval_mismatch ¹ |
| `invalid/null-no-approval.json` | mutation_approval_mismatch | mutation_approval_mismatch ¹ |
| `invalid/expired.json` | mutation_approval_mismatch | mutation_approval_mismatch ¹ |
| `invalid/body-mismatch.json` | mutation_approval_mismatch | mutation_approval_mismatch ¹ |
| `invalid/empty-proposal-id.json` | mutation_approval_mismatch | mutation_approval_mismatch ¹ |
| `invalid/version-3.json` | mutation_approval_mismatch | mutation_approval_mismatch ¹ |
| `valid/update-feature-v2.json` | accepted | accepted |
| `invalid/v2-expired.json` | mutation_approval_mismatch | mutation_approval_mismatch |
| `invalid/v2-lifetime-over-15min.json` | mutation_approval_mismatch | mutation_approval_mismatch |
| `invalid/v2-missing-target.json` | mutation_approval_mismatch | mutation_approval_mismatch |
| `invalid/v2-operation-not-allowed.json` | target_policy_denied | target_policy_denied |
| `invalid/v2-document-outside-bounds.json` | target_policy_denied | target_policy_denied |
| `invalid/v2-workspace-claim-differs.json` | target_policy_denied | target_policy_denied |
| `invalid/v2-feature-claim-differs.json` | target_policy_denied | target_policy_denied |
| `invalid/v2-microversion-claim-differs.json` | revision_policy_denied | revision_policy_denied |
| `invalid/v2-body-mismatch.json` | mutation_approval_mismatch | mutation_approval_mismatch |

¹ `expectations.json` lists only the pinned result for non-v2 fixtures. Without pins, v1 is
not admissible (§2.5, §4). I chose `mutation_approval_mismatch` at step 4 (see change
request CR-2).

### Replay record

- In memory in `ApiState::ExternalToken`; one record per process, shared by v1 and v2 and by
  all HTTP sessions (shared `Arc<Mutex<ApiState>>`).
- The mutex is held from admission through dispatch (`call_tool_stdio` path, which
  external-token HTTP uses), so the check (step 8) and the record (step 10) are atomic.
- Recorded after steps 1–9 pass and after the client is built, immediately before the single
  `client.execute`. A step-9 failure, such as an expired token, does not consume the approval.
- **Bound: the last 1024 IDs, FIFO eviction.** The plugin allows one approval at a time and
  blocks new ones until reconciliation, and a v2 approval lives at most 15 min. Evicting a
  still-valid ID would therefore need more than 1024 admissions within one approval lifetime
  (more than one per second), which the workflow cannot produce. Even after eviction, step 4
  (expiry) and the provider CAS (`rejectMicroversionSkew`) still apply. Memory stays at a few
  tens of KiB.
- Not persisted. After a restart the CAS remains the guard (§2.5 note).

### Config keys and optionality

| Key | Optionality |
|---|---|
| `[external_token] file, document_id, workspace_id, element_id` | required (unchanged) |
| `[external_token] read_api_version` | optional (unchanged) |
| `[external_token] mutation_approval_file` | optional without `mutation`; **required with `mutation`**, otherwise the server does not start (`target_configuration_invalid`) |
| `[external_token.mutation] document_id, element_id` | required; must equal `[external_token]` |
| `[external_token.mutation] excluded_workspace_ids` | required, non-empty |
| `[external_token.mutation] allowed_operations` | required, non-empty |
| `[external_token.mutation] write_mode` | optional, default `read` |
| `[external_token.mutation] workspace_id` | **now optional** pin. If set: equals `[external_token].workspace_id`, not excluded, and claims must equal it (`target_policy_denied`) |
| `[external_token.mutation] feature_id` | optional pin (unchanged). Claims and request must equal it (`target_policy_denied`) |
| `[external_token.mutation] expected_microversion` | **now optional** pin. Claims must equal it (`revision_policy_denied`) |
| v1 approvals | admitted only if both `workspace_id` and `expected_microversion` pins are set |

## Contract deviations and change requests

The implementation does not deviate from the contract or the fixture expectations. The
contract leaves three points open or slightly contradictory. I resolved them as follows and
ask the architect to confirm or amend the contract:

- **CR-1, §2.5 step 5 vs. `expectations.json` (wording, patch).** Step 5 says "every
  configured static pin equals its claim" → `target_policy_denied`. That would include the
  `expected_microversion` pin. But `invalid/v2-microversion-claim-differs.json` must yield
  `revision_policy_denied` in pinned mode too (boundsV2 `_comment`), and the prompt maps that
  pin to `revision_policy_denied`. Implemented: workspace and feature pins at step 5,
  microversion pin at step 6. Proposed diff:
  - step 5: "every configured static pin equals its claim" → "the configured `workspace_id`
    and `feature_id` pins equal their claims"
  - step 6: add "and a configured `expected_microversion` pin equals claim
    `expectedMicroversion`"

  No impact on ClawOnshape.
- **CR-2, v1 without static pins (clarification, patch).** §2.5 and §4 say v1 is admitted
  "only with these static pins" but name no code. Implemented: step 4,
  `mutation_approval_mismatch` (the consumer action "new proposal and approval", i.e. a v2
  approval, fits). Proposed diff for step 4: "…, and a v1 approval only if the pins
  `workspace_id` and `expected_microversion` are configured". It is additive and only
  matters in the migrate step.
- **CR-3, §2.4 claimed reads (clarification, patch).** The contract says "while a valid v2
  approval is active". Implemented more strictly, failing closed: the claim adds reads only if
  `[external_token.mutation]` is configured and the claim lies within the static bounds. That
  means document and element equal the configured target, the workspace is not excluded, and
  it equals the workspace pin if one is set. A claim that could never be admitted for the
  mutation enables no reads. Proposed wording: "a valid v2 approval (§4) whose claims lie
  within the static bounds (§2.5 step 5, §8.9)". There is no impact on a correct plugin.

## Live E2E request

- **Image:** build from `344d3e0` or the report commit (code is identical).
- **Config:** the currently deployed config, unchanged. It contains `mutation_approval_file`,
  the static pins and `read_api_version = "v16"`. If a deployed config lacks
  `mutation_approval_file`, the server now refuses to start with `target_configuration_invalid`.
  That is intended (§8.8), but check it before the rollout.
- **Plugin:** 0.2.10 (writes v1).

Verify:
1. The server starts. `/health` returns 200 and `/health/ready` returns 200 with a valid
   token.
2. The v1 proposal → approve → `onshape_mutation_context` → `onshape_api_call` → reconcile
   flow ends in `VERIFIED`.
3. Send the same `apiCall` again with the same approval file (before the plugin writes
   `null`, or by restoring it) → `mutation_approval_consumed`. Onshape receives no request:
   the microversion is unchanged and there is no new server log entry for a dispatch.
4. Optional: start with `[external_token.mutation]` and without `mutation_approval_file` →
   the process exits with `target_configuration_invalid`.

v2 claims and claimed reads can only be tested live with the P3 plugin (migrate step).

## Open points and risks

- **Step 10 is not covered by a test.** No test reaches `consumed.record(admitted)` directly
  before `client.execute`, because the dispatch origin is fixed to `https://cad.onshape.com`
  and tests may not reach the network. The record-before-dispatch order is visible in
  `execute`, and the core record semantics are tested. E2E step 3 checks it live.
- **Order change for v1.** The approval file is now read before the target and revision checks
  (§2.5 order). A wrong-target request that used to return `target_policy_denied` now returns
  `mutation_approval_unavailable` or `mutation_approval_mismatch` when the approval file is
  missing or invalid. Requests matching their approval are unaffected.
- **v1 replay is now rejected** inside one process (intended, §2.5 step 8). A plugin that
  re-sends the same v1 approval after an `UNKNOWN` outcome gets `mutation_approval_consumed`
  and must reconcile.
- Approval reads for GETs: an extra private-file read only when the fixed read set denies the
  request. This adds no provider I/O.
- New `#[allow]`: one, `#[allow(clippy::unwrap_used)]` on `approval::tests` (test module only,
  with a reason comment; same pattern as `target_policy::tests`). No new `// Invariant:`
  exceptions, no `unsafe`, no new dependencies.
- The moved read-set code has one branch without coverage: `/partstudios/d/{d}/m/{mid}/e/{e}/features`.
  It was moved verbatim from io.

## Out-of-scope findings

- `clawonshape/docs/interface/onshape-mcp.md:215-216` (§6): `target_policy_denied` still cites
  "§2.5 steps 1–3" and `revision_policy_denied` cites "§2.5 step 4". After the 1.0.0 rewrite
  these should be steps 1, 2, 5 and step 6.
- `clawonshape/docs/interface/onshape-mcp.md:121` (§2.5): "no provider I/O happens before step 8".
  Dispatch is now step 10; the prompt says "before step 9 (token) and step 10". It should
  probably read "before step 10".
- `clawonshape/docs/interface/onshape-mcp.md:245` (§7): "Approval lifetime 300 000 ms" does not
  mention the v2 bound of ≤ 900 000 ms (§4).
