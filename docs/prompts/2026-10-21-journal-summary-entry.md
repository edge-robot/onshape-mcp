---
target-repo: edge-robot/onshape-mcp
branch: feat/5-goal-grant
worktree: /home/kaos/Projects/01_pib/02_sw/onshape-mcp (container: /workspaces/onshape-mcp)
issue: edge-robot/onshape-mcp#5
architecture-ref: clawonshape/docs/interface/onshape-mcp.md#127-capability-reporting
contract-version: 1.2.0-draft
foreign-repo-commit: 68dfbc6   # clawonshape, branch feat/18-goal-authorization (read-only mount)
base-commit: 162e2c5
status: done
---
# Journal summary returns the unreconciled entry (contract 1.2.0-draft)

## Goal

Extend `GET /goals/{goalId}/journal-summary` with `unreconciledEntry` as specified in contract
`1.2.0-draft` §12.7. Operators (dashboard) and agents can then reconcile an uncertain dispatch
without the original MCP error data. This is an additive change; nothing else changes.

## Context

- Contract §12.7 (journal summary row) and its changelog entry for `1.2.0-draft`.
- Fixtures (committed `162e2c5`):
  - `goal-grant/journal-summary/uncertain.json`; its `requestHash` is a placeholder, so compute it;
  - `expectations.json` `goal-grant.behaviour.journalSummaryUncertain`, plus the updated
    `journalSummary` entry.
  - `VERSION` `1.2.0-draft` / `sha256:f60bfa0df9e3d76bb78f6a13e1231f64ac9c15c71d54465287b9ebd54e771cba`.
- Your implementation from `2026-10-21-goal-grant-admission.md`: `onshape-mcp-core::goal` (summary
  model) and `external_token.rs` `local_routes`.

## Binding constraints and prohibitions

- No contract, fixture or `VERSION` edits. On a problem, stop and write a CR.
- `unreconciledEntry` is the last line of the journal entry named by `unreconciledSequence`. It is
  `null` whenever `unreconciledSequence` is `null`. Only the listed fields; never request bodies,
  tokens or paths.
- No other behaviour change. The constraints of the previous prompt (no provider I/O on these
  routes, private-file rules, branch only, never push, no container files) apply.

## Steps

1. Bump `CONTRACT_VERSION` to `1.2.0-draft`. Keep all tests green.
2. Add `unreconciledEntry` to the summary model and the route output.
3. Tests:
   - `journalSummaryUncertain`, compared with the fixture except the computed `requestHash`;
   - the updated `journalSummary`;
   - `unreconciledEntry` is `null` after reconciliation;
   - a `PENDING` entry after a restart appears with `outcome: "PENDING"`;
   - extend `contract::goal_behaviour_entries_are_known`.
4. Add one line to `docs/target-policy.md`, if it describes the summary.

## Acceptance criteria and tests

| # | Criterion | Evidence |
|---|---|---|
| A1 | Summary matches `uncertain.json` (computed `requestHash`) | test |
| A2 | `null` when nothing is unreconciled; `PENDING` after a restart is reported | tests |
| A3 | No other output or behaviour changed | test diff limited to the summary |
| A4 | fmt, clippy `-D warnings` and nextest pass; deny as before (#6) | checks table |

## Branch and worktree rules

Commit on `feat/5-goal-grant` with the trailers `Refs: edge-robot/onshape-mcp#5`,
`Prompt: docs/prompts/2026-10-21-journal-summary-entry.md`, `Agent: rust-mcp-dev` and
`Co-Authored-By`. Never push. The uncommitted `README.md`, `spec.md` and `docs/src/project/*` changes
are not yours (edge-robot/onshape-mcp#8). Do not touch, stage or commit them.

## Expected report / handoff format

`docs/prompts/2026-10-21-journal-summary-entry.report.md` with frontmatter `status`,
`contract-version: 1.2.0-draft` and `e2e-request: required`. It must contain:

- the checks table;
- A1–A4;
- a sample summary output;
- the commits.
