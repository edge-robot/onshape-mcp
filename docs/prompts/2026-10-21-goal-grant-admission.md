---
target-repo: edge-robot/onshape-mcp
branch: feat/5-goal-grant
worktree: <workspace>/onshape-mcp (container: /workspaces/onshape-mcp)
issue: edge-robot/onshape-mcp#5
architecture-ref: clawonshape/docs/interface/onshape-mcp.md#12-goal-authorization-110
contract-version: 1.1.0-draft
foreign-repo-commit: f937771   # clawonshape, branch feat/18-goal-authorization (read-only mount)
base-commit: c8beefe
status: done
---
# Goal grant admission with a durable dispatch journal (#5)

## Goal

Implement contract `1.1.0-draft` §12 in onshape-mcp. One human-confirmed goal grant must admit
several agent-authored `updatePartStudioFeature` calls in one Candidate, and only within the grant.
The server enforces the scope, the budget, STOP and expiry, and the "at most one uncertain
dispatch" rule. A durable journal makes these rules survive restarts.
Approval v2 (§2.5, §4) keeps working unchanged.

## Context

- Contract (read-only mount): `clawonshape/docs/interface/onshape-mcp.md` §12 (normative), plus
  §2.4, §2.5, §6, §7, §8.10 and §10. Plugin side for orientation only: `goal-workflow.md`.
- ADR: `clawonshape/docs/ADR.md` → "ADR-2026-10-21: Goal-scoped mutation authorization" (accepted,
  decisions D1–D5).
- Fixtures (already copied, committed in `c8beefe`): `crates/onshape-mcp-core/tests/contract/`.
  Use `goal-grant/{valid,invalid,requests}/`, `capabilities/`, `expectations.json` → `boundsGoal`,
  `goal-grant.cases` and `goal-grant.behaviour` (13 behaviour entries), and `error-codes.json`.
  `VERSION` = `1.1.0-draft` / `sha256:37aca6a4323ae2c26b5826539ef2d607e532773ae1f7caf44d7d92c8020ae341`.
- Existing code to extend:
  - `crates/onshape-mcp-core/src/approval.rs`: approval v2, `ConsumedProposals`, admission order.
  - `crates/onshape-mcp-core/src/target_policy.rs`: static bounds.
  - `crates/onshape-mcp-core/src/config.rs`: `mutation_approval_file`,
    `require_mutation_approval_file`.
  - `crates/onshape-mcp-io/src/external_token.rs`: dispatch, `private_json`, `run_http`, `/health`
    routes.
  - `crates/onshape-mcp-core/tests/contract.rs`: `CONTRACT_VERSION`, fixture-driven tests.

## Binding constraints and prohibitions

- Contract-first: implement exactly §12. **Do not edit** the contract, the fixtures or `VERSION`.
  If the contract is wrong, ambiguous or unimplementable, stop, set the report to
  `status: blocked` and describe a change request (CR-n).
- Keep pure decision logic in `onshape-mcp-core` (grant parsing, admission order, journal
  evaluation, `requestHash`). Keep file and HTTP I/O in `onshape-mcp-io`.
- **No provider I/O before §12.3 step 11.** Step 10 reads only the local token file.
- Single attempt per admitted dispatch. No retry anywhere, for any outcome.
- Journal writes are append-only with `fsync` after every line. The journal is never truncated or
  rewritten. The journal directory and files use the same private-file checks as the channel B/C
  files, but the directory is rw and owned by the server.
- Approval v2 behaviour and all existing tests stay unchanged. The only allowed change is the new
  `mutation_authority_conflict` precedence (§12.2) when both authorities are present.
- Do not touch `Containerfile`, `.devcontainer/`, `scripts/dev.sh`, `.claude/` or deployment files.
  They belong to the architect. If you need a container change, describe it in the report.
- No new dependencies without a reason in the report. Prefer the existing `serde_json`, `sha2`,
  `uuid` and similar. Canonical JSON for `requestHash` = sorted keys, no whitespace (§12.3).
- Work only on branch `feat/5-goal-grant` in `/workspaces/onshape-mcp`. No new branches, worktrees
  or stashes. Never push.

## Steps

1. Verify the branch and HEAD (`c8beefe` or later on `feat/5-goal-grant`) and that `VERSION`
   matches this prompt. Record the baseline checks.
2. Bump `CONTRACT_VERSION` in `tests/contract.rs` to `1.1.0-draft`. Keep every existing contract
   test green. The new fixture directories must not break the existing iteration.
3. Config (§12.1): optional `goal_grant_file` and `dispatch_journal_dir` in `[external_token]`.
   Startup fails with `target_configuration_invalid` if `goal_grant_file` is set without
   `dispatch_journal_dir` (`behaviour.missingJournalDir`). Paths must be absolute and canonical.
4. Core: grant v1 type with strict parsing (unknown fields rejected) and every structural rule from
   §12.2: caps 8 h / 25 / 32 feature IDs, feature ID pattern, unique IDs, operations exactly
   `["updatePartStudioFeature"]`, state enum.
5. Core: goal admission as a pure function over (static bounds, grant, approval-v2 presence,
   request, journal view, now) that returns the first failing code in §12.3 order, or `Admit`
   together with the journal entry to append. Include:
   - the `reconciledThrough ≤ maxSequence` rule;
   - the duplicate rule over `requestHash`;
   - the budget rule (`NOT_DISPATCHED` does not count);
   - `PENDING` is treated as `UNCERTAIN`.
6. Core: a journal model with line parsing ("last line per sequence wins"), corruption detection
   (→ `dispatch_journal_unavailable`) and the summary for `GET /goals/{goalId}/journal-summary`.
7. IO: journal store. Steps:
   1. Private-file checks.
   2. Append `PENDING` with `fsync`.
   3. Re-read the grant before dispatch (§12.3 step 11). If it is no longer `ACTIVE` for this
      `goalId`, append `NOT_DISPATCHED` and return `goal_grant_stopped`.
   4. Dispatch once.
   5. Append the outcome per the §12.4 mapping.
   6. Unknown-outcome and goal errors carry `data` per §12.4.

   Serialize admissions per goal so that two concurrent calls cannot both pass step 9. A process-wide
   mutex is acceptable; describe your choice.
8. IO: authority selection (§12.2):
   - non-`null` grant + active approval v2 → `mutation_authority_conflict`;
   - non-`null` grant → goal path;
   - otherwise §2.5 unchanged.
9. IO: read admission extension (§12.6) for the grant's Candidate workspace.
10. HTTP: `GET /capabilities` (§12.7, `capabilities/*.json`) and
    `GET /goals/{goalId}/journal-summary` (uuid-v4 check, `404` otherwise, `503` with
    `dispatch_journal_unavailable` for an unreadable journal). No auth, no provider I/O.
11. Tests:
    - every `goal-grant.cases` entry, data-driven from `expectations.json`;
    - every `goal-grant.behaviour` entry as a named test. Use a mock provider for dispatch outcomes
      (2xx, 4xx, 408, 5xx, transport error) and a temp journal dir. Prove "no provider I/O" with a
      mock that fails the test on any call;
    - a restart test: drop and recreate the store over the same directory;
    - a STOP-race test: swap the grant file between step 9 and step 11 through a test hook;
    - the error codes in `error-codes.json` `server` stay a superset of everything emitted.
12. Update `docs/` in this repo where the external-token mode is described (target policy /
    external token docs) with a short goal-grant section that links the contract. Do not copy the
    contract.

## Acceptance criteria and tests

| # | Criterion | Evidence |
|---|---|---|
| A1 | Two different permitted updates (`update-depth`, `update-fillet-radius`) are admitted under one grant revision; journal sequences 1 and 2 are `OK` | test `twoChangesOneGoal` |
| A2 | All 22 `goal-grant.cases` yield the expected code in §12.3 order | data-driven test |
| A3 | Budget, duplicate, uncertain, restart, STOP race, authority conflict, null fallback, journal unavailable, missing journal dir, reads, reconciledBeyondJournal, journalSummary: each behaviour has a named passing test | test names in the report |
| A4 | No provider I/O before step 11 in any failing case | mock assertion |
| A5 | Approval v2 suite unchanged and green | test count before/after |
| A6 | `/capabilities` matches both fixtures for the matching config | test |
| A7 | Journal survives a restart; `PENDING` is loaded as `UNCERTAIN` | restart test |
| A8 | All repo checks pass (agent definition list: fmt, clippy `-D warnings`, tests, deny/audit if configured) | checks table |

## Branch and worktree rules

Commit on `feat/5-goal-grant` only, in small logical commits with the required trailers
(`Refs: edge-robot/onshape-mcp#5`, `Prompt: docs/prompts/2026-10-21-goal-grant-admission.md`,
`Agent: rust-mcp-dev`, `Co-Authored-By`). Never push. The worktree stays assigned to you until the
architect accepts the report.

## Expected report / handoff format

`docs/prompts/2026-10-21-goal-grant-admission.report.md` in the agent-definition format
(frontmatter `status`, `contract-version: 1.1.0-draft`, `e2e-request: required`). It must contain:

- the checks table (baseline vs. after);
- A1–A8 with evidence;
- the test list per `goal-grant.cases` and `behaviour` key;
- your concurrency choice;
- new dependencies with reasons;
- new `#[allow]`s with reasons;
- container or deployment needs: the rw journal mount (expected path and UID requirements for
  Clawake);
- CRs, if any;
- the commit list with trailers.
