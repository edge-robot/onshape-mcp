---
prompt: docs/prompts/2026-10-21-journal-summary-entry.md
branch: feat/5-goal-grant
commits: 25fedb8..f6ef9c6
status: done
contract-version: 1.2.0-draft
e2e-request: required
---
# Report: Journal summary returns the unreconciled entry (contract 1.2.0-draft)

## Summary

`GET /goals/{goalId}/journal-summary` now also returns `unreconciledEntry` (contract 1.2.0-draft
§12.7):

- It is the effective journal entry for `unreconciledSequence`. The effective entry is the last
  line for that sequence.
- It is `null` whenever `unreconciledSequence` is `null`.
- It carries exactly the eight listed fields, because it is the serialized `JournalEntry`:
  `sequence`, `grantRevision`, `requestHash`, `featureId`, `sourceMicroversion`, `admittedAt`,
  `outcome` and `httpStatus`.
- It never carries request bodies, tokens or paths.

`CONTRACT_VERSION` is now `1.2.0-draft`. Nothing else changed:

- the existing summary fields;
- the `404` and `503` rules;
- `/capabilities`, which still reports `contractVersion: "1.1.0"` as the contract text says;
- admission;
- the journal format.

Preconditions:

- Branch `feat/5-goal-grant`.
- `VERSION` `1.2.0-draft` / `sha256:f60bfa0d…1cba`.
- clawonshape `68dfbc6`.
- Base `162e2c5`, plus the prompt commit `8f53f70`.

All four matched.

## Changed files

```
 crates/onshape-mcp-core/src/goal.rs                | 21 ++++--   JournalSummary.unreconciled_entry + unit test
 crates/onshape-mcp-core/tests/contract.rs          | 77 +++++-   version bump, journal_summary_uncertain_matches_fixture, behaviour key
 .../src/external_token/goal_behaviour.rs           | 55 ++++--   journal_summary_uncertain, updated summary/restart/reconciliation tests
 docs/src/project/changelog.md                      |  3 +     Unreleased entry
 docs/target-policy.md                              |  2 +     one line on unreconciledEntry
```

The route code (`external_token.rs` `journal_summary`) did not need to change: it serializes
`Journal::summary`.

The following uncommitted changes are not mine (edge-robot/onshape-mcp#8). They are untouched,
unstaged and uncommitted: `README.md`, `docs/src/project/{implementation,index,overview,requirements}.md`
and `spec.md`.

## Checks

| Command | Result |
|---|---|
| `cargo fmt --all --check` | pass |
| `cargo clippy --all-targets --all-features -- -D warnings` | pass |
| `cargo nextest run --all-features` | pass: 719/719 (previously 717; +1 core contract test, +1 IO behaviour test) |
| `cargo deny check` | fail as before (#6): `advisories FAILED` RUSTSEC-2026-0382 (rustls-webpki); bans, licenses and sources ok |
| `mdbook build docs` | pass |
| `cargo llvm-cov nextest --workspace --all-features` | goal.rs 97.96 % lines; dispatch_journal.rs 96.28 %; external_token.rs 95.88 %. The new code has no branches of its own (`Option::cloned` of the existing `unreconciled` lookup); both the `Some` and the `None` paths are tested |
| `python scripts/apply-openapi-overlays.py --check` | not run (OpenAPI not touched) |

New `#[allow]`, `unwrap` or `expect` outside tests: none. New dependencies: none.

Note: the test commit `25fedb8` on its own fails `journal_summary_uncertain_matches_fixture`. This
is intentional (test first). `d2ce3df` makes it pass.

## Acceptance criteria

| # | Criterion | Status | Evidence |
|---|---|---|---|
| A1 | Summary matches `uncertain.json` (computed `requestHash`) | met | See below |
| A2 | `null` when nothing is unreconciled; `PENDING` after a restart is reported | met | See below |
| A3 | No other output or behaviour changed | met | See below |
| A4 | fmt, clippy `-D warnings` and nextest pass; deny as before (#6) | met | Checks table |

**A1 evidence**

- `contract::journal_summary_uncertain_matches_fixture` runs the real `admit_goal_mutation` at the
  fixture's `admittedAt` (4102444600000) with `valid/active.json`.
- It then builds the `PENDING` line and the `UNCERTAIN`/503 line, parses the journal and compares
  the summary with the fixture.
- The fixture's `requestHash` is replaced by sha256(canonical `{path, body}`) of
  `update-depth.request.json`. The test computes it itself from `canonical_json` and `sha256_hex`.
- `goal_behaviour::journal_summary_uncertain` runs the same case through the real `execute`, the
  mock provider (503) and the HTTP route.
  - The real clock sets `admittedAt`, so `lastAdmittedAt` and `admittedAt` come from the journal.
  - Every other field is compared exactly.
  - The test also checks that no token, temporary path or `"body"` key appears in the output, and
    that the route made no provider call.

**A2 evidence**

- **`null` after reconciliation:**
  - Both new tests check that with `reconciled-through-1.json`, `unreconciledSequence` and
    `unreconciledEntry` are `null`.
  - `journal_summary_follows_reconciliation` is extended.
  - `journal_summary` (unknown goal, and `twoChangesOneGoal` per the updated `journalSummary`
    entry) expects `unreconciledEntry: null`.
  - The core unit test `summary_reports_effective_state` covers the empty journal and
    `reconciledThrough ≥ sequence`.
- **`PENDING` after a restart:** `goal_behaviour::restart` checks that a trailing `PENDING` line,
  loaded by a fresh store, appears with `outcome: "PENDING"` and `httpStatus: null`, and equals
  the stored entry.

**A3 evidence**

- In the test diff, only the summary expectations gained the new key. No other assertion changed.
- `contract::goal_behaviour_entries_are_known` is extended by `journalSummaryUncertain`.

## Contract deviations and change requests

None. No contract, fixture or `VERSION` edits.

## Sample summary output

After `update-depth` with `valid/active.json` ends in HTTP 503 (values as in the fixture):

```json
{
  "goalId": "3f2a9c1e-5b7d-4e8f-9a0b-1c2d3e4f5a6b",
  "entries": 1,
  "counted": 1,
  "maxSequence": 1,
  "unreconciledSequence": 1,
  "lastOutcome": "UNCERTAIN",
  "lastAdmittedAt": 4102444600000,
  "unreconciledEntry": {
    "sequence": 1,
    "grantRevision": 1,
    "requestHash": "903d47baabe6f6912615c1df76c520ca2da8bd320bdd4c74581292e893460c58",
    "featureId": "FabcDEF123_0",
    "sourceMicroversion": "aaaaaaaaaaaaaaaaaaaaaaaa",
    "admittedAt": 4102444600000,
    "outcome": "UNCERTAIN",
    "httpStatus": 503
  }
}
```

The server returns compact JSON; the field order is the struct order. The `requestHash` shown was
computed independently with Python, using the contract's canonicalization (keys sorted by UTF-16
code units, no whitespace). It was not printed from the Rust test; the Rust test computes and
compares the value at runtime.

After `reconciled-through-1.json`: the same, with `"unreconciledSequence": null` and
`"unreconciledEntry": null`.

## Live E2E request

Run this after the goal-grant E2E from `2026-10-21-goal-grant-admission.md`, on the same image and
configuration: `goal_grant_file` and `dispatch_journal_dir` are set, and the journal volume is
`0700`, owned by the server UID.

1. Force an uncertain outcome. In live use this is mainly a dispatch interrupted by a timeout. If
   that cannot be provoked, leave a journal with a `PENDING` line from a killed server.
2. Check `GET /goals/{goalId}/journal-summary`:
   - `unreconciledEntry` names the same `sequence`, `requestHash`, `featureId` and
     `sourceMicroversion` as the MCP error data;
   - `outcome` is `UNCERTAIN`, or `PENDING` after a restart.
3. Publish a grant with `reconciledThrough ≥ sequence`. Expect `unreconciledEntry: null`.
4. Check that the plugin or dashboard can reconcile from the summary alone.

## Open points and risks

- The plugin side (`goal-workflow.md` `progress.unreconciledEntry`) is not part of this repo.
- Older servers (1.1.0) omit the key. The plugin has to treat a missing key like `null`, as the
  additive contract change implies.

## Out-of-scope findings

None new. RUSTSEC-2026-0382 is still open (#6).
