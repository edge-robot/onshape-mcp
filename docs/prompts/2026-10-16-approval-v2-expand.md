---
target-repo: edge-robot/onshape-mcp
branch: claw-onshape/external-token
worktree: /home/kaos/Projects/01_pib/02_sw/onshape-mcp (container: /workspaces/onshape-mcp)
issue: edge-robot/onshape-mcp#2
architecture-ref: clawonshape/docs/interface/onshape-mcp.md#25-mutation-admission-onshape_api_call-method-other-than-get
contract-version: 1.0.0-draft
foreign-repo-commit: c637cc9   # clawonshape, contract 1.0.0-draft (read-only mount)
base-commit: c53d9c9
status: done
---
# Expand: admit approval file v2 with target claims (contract 1.0.0-draft, step 1 of 4)

## Goal

This is step 1, **expand**, of the four-step migration to contract **1.0.0-draft**. P1 of
edge-robot/clawonshape#11 is tracked in edge-robot/clawonshape#14.

The server must accept approval files **v1 and v2**, admit the mutation target from the v2
claims within static bounds, make the approval file mandatory, and reject replays. The
currently deployed configuration and the plugin 0.2.10 (which writes v1) must keep working
unchanged.

## Context

- Contract (read-only mount of clawonshape @ `c637cc9`): `docs/interface/onshape-mcp.md`.
  - §2.4 reads: plus the claimed Candidate while an approval is active.
  - §2.5 admission: new order, steps 1–10.
  - §4 approval file v2 and v1 (deprecated).
  - §6 new code `mutation_approval_consumed`.
  - §8.8 and §8.9.
  - §10 fixtures.
  - The changelog entry `1.0.0-draft`.
- ADR: clawonshape `docs/ADR.md` → "ADR-2026-10-16: Proof-based mutation release …", points 4 and 5.
- Fixture copy: `crates/onshape-mcp-core/tests/contract/`, `VERSION` = `1.0.0-draft` /
  `sha256:5bb47c4dbd0410714e452ba77b807773d580f446e2a2eafc4e1b6784b65ee40f`. `mcp-tools.json` is
  unchanged.
  - `expectations.json` has `target`, the v1 static pins, and `boundsV2`, the static bounds only.
  - It lists the per-fixture codes.
  - It has the behavioural entries `replay`, `missingApprovalFile` and `readsV2`.
- Code:
  - `crates/onshape-mcp-core/src/config.rs`: `ExternalTokenConfig`, with `mutation_approval_file`
    optional today.
  - `crates/onshape-mcp-core/src/target_policy.rs`: `TargetConstraint`. Its `validate` requires the
    mutation workspace to equal the read workspace, and the pins are mandatory.
  - `crates/onshape-mcp-io/src/external_token.rs`: `allowed` (reads), `approval_matches` (v1 only) and
    `execute`, where the approval check runs only `if let Some(file)`.
- Tests: `crates/onshape-mcp-core/tests/contract.rs` (`CONTRACT_VERSION = "0.2.0-draft"`).
- Current live config shape (must stay valid):
  - `[external_token]` with `document_id`, `workspace_id`, `element_id`, `read_api_version = "v16"` and
    `mutation_approval_file`;
  - `[external_token.mutation]` with `document_id`, `workspace_id`, `element_id`,
    `excluded_workspace_ids`, `allowed_operations = ["updatePartStudioFeature"]`, `feature_id`,
    `expected_microversion` and `write_mode = "modify"`.

## Binding constraints and prohibitions

- **Admission order exactly as in §2.5 steps 1–10.** No provider I/O before step 9 (token) and
  step 10 (dispatch). The first failure wins, with the listed code.
- **v2 schema:**
  - all fields are required and unknown fields are rejected;
  - `target.*` and `expectedMicroversion` are 24-hex;
  - `operation == "updatePartStudioFeature"`;
  - `0 < expiresAt − issuedAt ≤ 900_000`;
  - `expiresAt > now`.
- **v1** is admitted only when the static pins `workspace_id` and `expected_microversion` are configured.
  It then behaves exactly as today.
- **Pins in `[external_token.mutation]`:** `workspace_id`, `feature_id` and `expected_microversion`
  become optional.
  - If a pin is present, the matching v2 claim must equal it (`target_policy_denied` /
    `revision_policy_denied`).
  - `validate` must no longer require the mutation workspace to equal `[external_token].workspace_id`
    when the pin is absent.
  - `excluded_workspace_ids` stays mandatory and non-empty.
- **Mandatory approval:** `[external_token.mutation]` without `mutation_approval_file` must refuse to
  start with `target_configuration_invalid`.
- **Replay:**
  - Keep an in-process record of admitted `proposalId`s (v1 and v2). Record it after steps 1–9
    pass and **before** dispatch.
  - A repeat yields `mutation_approval_consumed`, with no provider I/O.
  - Bound the record, for example the last 1024 IDs. Explain the bound you chose.
  - The record is not persisted.
- **Reads (§2.4):** while a valid v2 approval is active, also admit
  `/documents/d/{d}/w/{claimed}/currentmicroversion` and `/partstudios/d/{d}/w/{claimed}/e/{e}/features`.
  - `{d}` and `{e}` stay the configured target.
  - An unreadable, `null` or expired approval adds nothing.
- Add the new error code wherever the server's code list lives. `tools/list` must stay byte-identical,
  and the `mcp-tools.json` exact-match test must stay green.
- Bump `CONTRACT_VERSION` to `1.0.0-draft`. Do not edit the fixture copy or `VERSION`; they are the
  architect's.
- Update the server docs that describe the policy (for example `docs/target-policy.md`) to state that
  the approval file is mandatory and to describe the claims.
- No changes to auth, OAuth, the HTTP transport or the timeouts, and nothing outside external-token mode.
- No new `unwrap`/`expect` in non-test code. Justify any new `#[allow]` in the report.

## Steps

1. **Red.** Add contract tests driven by `expectations.json`:
   - every `approved-update` fixture against the paired request with the `target` pins, and v2
     fixtures also with `boundsV2`;
   - `replay`, `missingApprovalFile` and `readsV2`;
   - the deployed config shape still parses and admits the v1 fixture.
2. **Green, core.** Change the `TargetConstraint`/config so that the pins are optional; add the
   startup check and a claims type with validation.
3. **Green, io.** Implement `execute` in the §2.5 order, the v2 parsing, the replay record and the
   read extension.
4. Run `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test --workspace`
   and `cargo deny check`.

## Acceptance criteria and tests

- Every fixture yields the code listed in `expectations.json`, in both pin modes where applicable.
- Replay and startup refusal are tested, and the read extension has a positive and a negative test
  (expired, `null`, unreadable).
- The v1 flow with the current live config is unchanged. A regression test covers it.
- All existing tests are green, including the `mcp-tools.json` exact match.

## Branch and worktree rules

- Only `claw-onshape/external-token` in `/workspaces/onshape-mcp`, on top of `c53d9c9` plus the
  architect's fixture/prompt commit. No branches, worktrees or pushes.
- This branch goes to edge-robot/onshape-mcp, never to `altendky/onshape-mcp`. Prompts and reports may
  live here as before.
- Commits with `Refs: edge-robot/onshape-mcp#2, edge-robot/clawonshape#14`,
  `Prompt: docs/prompts/2026-10-16-approval-v2-expand.md`, `Agent: rust-mcp-dev`,
  `Co-Authored-By: …`:
  1. `test: 🧪 contract 1.0.0-draft approval v2 fixtures and behaviours`
  2. `feat: 🔐 optional pins, mandatory approval file at startup`
  3. `feat: 🔐 admit approval v2 target claims, replay protection, claimed reads`
  4. `docs: 📖 target policy with approval claims`
  5. `docs: 📖 report for approval-v2-expand prompt` (report + mark this prompt done)

## Expected report / handoff format

`docs/prompts/2026-10-16-approval-v2-expand.report.md` per your agent definition. It must contain:
- `status`;
- a red→green table;
- the fixture → code results for both pin modes;
- the replay-record bound and why;
- the exact config keys and their optionality;
- any contract deviation as a change request;
- `e2e-request: required`.

The live E2E the architect will run:
- build the image from your last commit;
- the deployed config with plugin 0.2.10 → the v1 proposal flow is still `VERIFIED`;
- a second dispatch of the same approval → `mutation_approval_consumed`.
