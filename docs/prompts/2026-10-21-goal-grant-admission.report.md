---
prompt: docs/prompts/2026-10-21-goal-grant-admission.md
branch: feat/5-goal-grant
commits: e68c41d..3348348
status: done
contract-version: 1.1.0-draft
e2e-request: required
---
# Report: Goal grant admission with a durable dispatch journal (#5)

## Summary

This change implements contract `1.1.0-draft` §12 in onshape-mcp:

- The pure logic is in `onshape-mcp-core::goal`: strict grant v1 parsing with all §12.2 caps, admission steps 1–9 in contract order, canonical JSON `requestHash`, the journal model (the last line per sequence wins, corruption is detected, `PENDING` counts as `UNCERTAIN`), the journal summary, goal reads and the capabilities payload.
- The I/O is in `onshape-mcp-io`:
  - authority selection (§12.2);
  - an append-only, fsynced journal store with private directory and file checks;
  - step 10 (token) and step 11 (`PENDING`, then the grant re-read, then exactly one dispatch, then the outcome);
  - error data per §12.4;
  - goal reads (§12.6);
  - `GET /capabilities` and `GET /goals/{goalId}/journal-summary` (§12.7).
- With `goal_grant_file` set and no `dispatch_journal_dir`, the server refuses to start with `target_configuration_invalid`.
- Approval v2 (§2.5) is unchanged. The only new precedence is `mutation_authority_conflict` when both authorities are present.

Precondition check:

- Branch `feat/5-goal-grant` with HEAD `5d9af7a` (later than `c8beefe`).
- `tests/contract/VERSION` = `1.1.0-draft` / `sha256:37aca6a4…ae341`.
- **Deviation:** the clawonshape HEAD is `cc27dc0`, not `f937771`. `f937771` is an ancestor of it. The only commit in between adds `docs/prompts/2026-10-21-goal-authorization-plugin.md`, and `git diff f937771 HEAD -- docs/interface` is empty, so the contract is byte-identical. I therefore continued.

## Changed files

`git diff --stat c8beefe..HEAD`, 13 files, +3746/−48:

| File | Purpose |
|---|---|
| `Cargo.lock`, `crates/onshape-mcp-core/Cargo.toml` | `sha2` (already a workspace dependency) is now also used by core for `requestHash` |
| `crates/onshape-mcp-core/src/config.rs` | `goal_grant_file`, `dispatch_journal_dir`, `require_dispatch_journal_dir()` |
| `crates/onshape-mcp-core/src/goal.rs` | new: pure §12 logic and its unit tests |
| `crates/onshape-mcp-core/src/lib.rs` | `pub mod goal` |
| `crates/onshape-mcp-core/tests/contract.rs` | `CONTRACT_VERSION` = `1.1.0-draft`; ten new fixture-driven goal tests |
| `crates/onshape-mcp-io/src/dispatch_journal.rs` | new: `JournalStore` (private checks, read, append + fsync) and tests |
| `crates/onshape-mcp-io/src/external_token.rs` | goal path, authority selection, `Provider` dispatch seam, goal reads, `local_routes` with `/capabilities` and the journal summary, startup checks, test-only `mock` |
| `crates/onshape-mcp-io/src/external_token/goal_behaviour.rs` | new: named behaviour tests, cases through `execute`, codes check |
| `crates/onshape-mcp-io/src/lib.rs` | `ApiState::ExternalToken` carries the `Provider`; module declaration |
| `docs/target-policy.md` | short goal-grant section that links contract §12 |
| `docs/src/project/changelog.md` (= `CHANGELOG.md`) | entry under Added |
| `docs/prompts/2026-10-21-goal-grant-admission.md` | the prompt (architect commit `5d9af7a`, not mine) |

## Checks

| Command | Baseline (`5d9af7a`) | After (`3348348`) |
|---|---|---|
| `cargo fmt --all --check` | pass | pass |
| `cargo clippy --all-targets --all-features -- -D warnings` | pass | pass |
| `cargo nextest run --all-features` | 669 run: 668 passed, 1 failed (`contract_version_matches`, expected: `VERSION` already at 1.1.0-draft) | **717 run, 717 passed** |
| `cargo deny check` | **fail**: advisories RUSTSEC-2026-0382 (`rustls-webpki` 0.103.15 via rustls 0.23.46 / reqwest) | **fail**, same pre-existing advisory; bans, licenses and sources ok |
| `mdbook build docs` | n/a | pass |
| `python scripts/apply-openapi-overlays.py --check` | n/a, OpenAPI not touched | n/a |
| `cargo llvm-cov nextest --workspace --all-features` | — | `goal.rs` lines 97.8 % / functions 94.9 %; `dispatch_journal.rs` lines 96.0 %; `external_token.rs` lines 95.4 % |

I did not touch the deny advisory, because the solution is a lockfile bump of `rustls-webpki` to ≥ 0.103.16. That needs its own prompt.

Coverage gaps in new code:

- `goal.rs:727`: the step-4 check "grant operation ∉ static `allowed_operations`". It cannot be reached after step 1 plus the exact operations rule; it is kept as defence in depth.
- `dispatch_journal.rs:131`: the metadata re-check after open (race only).
- `external_token.rs` `Provider::Onshape`, `Dispatcher::Onshape` and the goal-path `external_token_client_unavailable`: the production client; there is no network in tests.
- The defensive `target_configuration_invalid` in `execute_goal`, which `validate_config` makes unreachable.

## Acceptance criteria

| # | Result | Evidence |
|---|---|---|
| A1 | met | `goal_behaviour::two_changes_one_goal`: one grant revision; `update-depth` and then `update-fillet-radius` each 2xx; journal sequences 1 and 2 are `OK` with `httpStatus` 200 and `grantRevision` 1; 4 append-only lines; 2 provider calls, both to v16 |
| A2 | met | `contract::goal_grant_cases_follow_admission_order` (pure, asserts 22 cases) and `goal_behaviour::goal_grant_cases_through_execute` (all 22 through the real `execute` with mock and journal) |
| A3 | met | named tests, see the table below |
| A4 | met | every failing case in `goal_grant_cases_through_execute` and in each behaviour test asserts `mock.calls() == []`, and where relevant `connections() == []` and no journal line |
| A5 | met | approval suite unchanged: `approval.rs` 6 → 6 tests, `target_policy.rs` 7 → 7, `external_token.rs::tests` 12 → 12, contract approval tests 13 → 13 (plus 10 new goal tests). Existing test bodies changed only to pass `&Provider::Onshape` and the two new `None` config fields. `null_grant_falls_back` shows that §2.5 still applies with a `null` grant |
| A6 | met | `goal_behaviour::capabilities_match_fixtures` (HTTP route against both fixtures; insecure journal dir → `approval-v2-only`) and `contract::capabilities_match_fixtures` |
| A7 | met | `goal_behaviour::restart` (fresh store and replay record over the same directory, trailing `PENDING` → `goal_reconciliation_required`, counted 1); `unrecorded_outcome_is_uncertain_after_restart`; `dispatch_journal::tests::append_is_durable_private_and_append_only` |
| A8 | met, with one exception | fmt, clippy and nextest pass; `cargo deny check` fails only on the pre-existing RUSTSEC-2026-0382 (see Checks) |

### Tests per `goal-grant.cases` and `behaviour` key

`goal-grant.cases`: all 22 entries run data-driven from `expectations.json` in `contract::goal_grant_cases_follow_admission_order` (core, pure) and in `goal_behaviour::goal_grant_cases_through_execute` (io, end-to-end). `contract::every_goal_fixture_is_used_by_a_case` ensures that every grant and request fixture is exercised.

| `behaviour` key | Test(s) |
|---|---|
| `twoChangesOneGoal` | `goal_behaviour::two_changes_one_goal` |
| `budget` | `goal_behaviour::budget` |
| `duplicate` | `goal_behaviour::duplicate` |
| `uncertain` | `goal_behaviour::uncertain` (transport, 500, 503, 302, 408), `provider_rejected_is_recorded_and_not_blocking` |
| `restart` | `goal_behaviour::restart`, `unrecorded_outcome_is_uncertain_after_restart` |
| `stopRace` | `goal_behaviour::stop_race` (swap to `stopped.json` and to `null` through the test hook) |
| `authorityConflict` | `goal_behaviour::authority_conflict`, `contract::goal_authority_conflict_and_null_fallback` |
| `nullGrantFallsBack` | `goal_behaviour::null_grant_falls_back` |
| `journalUnavailable` | `goal_behaviour::journal_unavailable`, `pending_append_failure_is_unavailable_without_dispatch`, `dispatch_journal::tests::insecure_or_corrupt_journal_fails_closed` |
| `missingJournalDir` | `goal_behaviour::missing_journal_dir`, `contract::missing_journal_dir_refuses_start` |
| `readsGoal` | `goal_behaviour::reads_goal`, `contract::reads_of_goal_candidate_while_grant_exists` |
| `reconciledBeyondJournal` | `goal_behaviour::reconciled_beyond_journal`, `contract::goal_reconciled_beyond_journal_is_invalid` |
| `journalSummary` | `goal_behaviour::journal_summary`, `journal_summary_follows_reconciliation` |

`contract::goal_behaviour_entries_are_known` fails if the architect adds a behaviour key without a matching test.

The "foreign owner" variant of `journalUnavailable` cannot be produced as an unprivileged user (UID 1000) in the devcontainer. The owner check uses the same code path as the mode check.

Other new tests:

- `concurrent_identical_requests_dispatch_once`: multi-thread runtime; exactly one dispatch, the other call gets `goal_dispatch_duplicate`.
- `token_failures_carry_goal_data_and_write_nothing`.
- `emitted_codes_are_listed_in_contract`: scans the string literals in the production part of `external_token.rs` against `error-codes.json` `server`.
- `contract::goal_codes_are_listed_in_contract`.
- `contract::canonical_json_matches_goal_hash_vector`: my canonical JSON reproduces `goal-workflow/goal-hash/vector.json` byte for byte, including its SHA-256.
- 14 core unit tests in `goal::tests`.

### Concurrency choice

- **Admission lock:** one process-wide `tokio::sync::Mutex<()>` (`GOAL_ADMISSION`). It is held from the grant read (step 2) to the recorded outcome (step 11), across the dispatch await. Two concurrent goal mutations cannot both pass step 9. A process-wide lock is enough because the plugin runs one goal at a time and goal dispatches are sequential by design. It only serializes goal-path mutations: reads and the §2.5 path do not take it. (MCP tool calls are also serialized today by the existing `api_state` mutex, but the goal path does not rely on that.)
- **File lock:** a separate process-wide `std::sync::Mutex<()>` (`JOURNAL_IO`). It is held only for the duration of one file read or append and never across `.await`. This keeps the unauthenticated journal-summary route from observing a half-written line without blocking it for the length of a dispatch.
- **Journal state:** the journal is re-read from disk on every admission. There is no in-memory journal state, so a restart cannot diverge from the file.

### New dependencies

- None new to the workspace.
- `onshape-mcp-core` now uses the existing workspace dependency `sha2` (`Cargo.lock`: +1 line). Reason: `requestHash` must be computed in core (§12.3, prompt step 5). `cargo deny` bans, licenses and sources are ok.
- There is no `uuid` crate in the workspace. The uuid-v4 check is written by hand (`goal::is_uuid_v4`).

### New `#[allow]`s

All four are test-only. There is no new `allow` in production code, no `unwrap`/`expect`/`panic` outside tests, and no new `// Invariant:` exception.

- `crates/onshape-mcp-core/src/goal.rs` `mod tests`: `#[allow(clippy::unwrap_used)]` (fixture construction failures are test bugs).
- `crates/onshape-mcp-io/src/dispatch_journal.rs` `mod tests`: `#[allow(clippy::unwrap_used)]`, same reason.
- `crates/onshape-mcp-io/src/external_token.rs` `#[cfg(test)] mod mock`: `#[allow(clippy::unwrap_used)]` (a poisoned test mutex is a test bug).
- `crates/onshape-mcp-io/src/external_token/goal_behaviour.rs`: `#![allow(clippy::unwrap_used)]`, same reason.

## Contract deviations and change requests

There is no deviation from the contract, the fixtures and `VERSION` are untouched, and `error-codes.json` `server` stays a superset of every emitted code (tested). No contract snapshot or `onshape-api-tools.json` changed.

Where §12 leaves room, I made the choices below. I ask the architect to confirm them or write them into the contract. All of them are additive clarifications; none is breaking or blocking.

- **CR-1 Expand-step pins in the goal path.**
  - Choice: if the optional `[external_token.mutation]` pins are configured, they also bound goal admission. `workspace_id` ≠ grant workspace → step 6 `target_policy_denied`. `feature_id` ≠ `{fid}` → step 7 `goal_scope_denied`. `expected_microversion` ≠ `sourceMicroversion` → step 8 `revision_policy_denied`.
  - Rationale: §12.1 "a grant can only narrow" the static bounds (§8.9).
  - Proposed text for §12.3 steps 6–8: "configured expand-step pins (§2.5) must equal the grant target / `{fid}` / `sourceMicroversion`".
  - Impact on ClawOnshape: none (Clawake no longer sets pins for goals).
- **CR-2 "Active approval" at step 3.**
  - Choice: an approval file counts as active if it is readable under the private-file rules and holds a non-`null` value with an integer `expiresAt > now`, whatever its version or other structure. An unreadable or insecure approval file counts as no approval.
  - Proposed text for §12.2: "non-`null` value with `expiresAt > now`".
- **CR-3 Step 11 re-read.**
  - Choice: "still `ACTIVE` with the same `goalId`" is evaluated with the full step-4 validation, including `expiresAt > now`. A grant that expires between step 9 and step 11 therefore yields `NOT_DISPATCHED` + `goal_grant_stopped`, not `goal_grant_invalid`. This is fail-closed; please confirm.
- **CR-4 Journal summary details.**
  - (a) `404` also when the server has no goal configuration.
  - (b) The `503` body is `{"code":"dispatch_journal_unavailable"}`.
  - (c) `lastOutcome` reports the stored outcome, so it can be `PENDING` during an in-flight dispatch or after a crash. `unreconciledSequence` treats `PENDING` as uncertain.
  - (d) `reconciledThrough` comes from the current grant if it is structurally valid (expiry ignored) and names the same `goalId`; otherwise 0.
- **CR-5 Error data.**
  - `goal_reconciliation_required` and `goal_dispatch_duplicate` carry the referenced journal entry's `sequence`, `featureId`, `sourceMicroversion` and `admittedAt`. That is what the plugin needs to reconcile.
  - Other step 9–10 errors carry the request's `goalId`, `featureId` and `sourceMicroversion`, without `sequence`.
  - Step-11 and unknown-outcome errors carry the new sequence and `admittedAt`.
  - The unknown-outcome `data` is the §6 object merged with the §12.4 fields.
- **CR-6 Unrecorded outcome.**
  - If the outcome line cannot be appended after a dispatch, the call returns `mutation_outcome_unknown_reconcile_required`, even when the provider answered 2xx or 4xx. The remaining `PENDING` then blocks the goal like `UNCERTAIN`.
  - If the `NOT_DISPATCHED` append fails at step 11, `goal_grant_stopped` is still returned and the `PENDING` line blocks the goal until it is reconciled.
- **CR-7 Identifier and path strictness.**
  - `goalId` must be a **lowercase** uuid-v4, because it names a file.
  - `goal_grant_file` and `dispatch_journal_dir` must be absolute with no empty, `.` or `..` segments and no trailing slash. Otherwise startup fails with `external_token_configuration_invalid`, the same code a relative `mutation_approval_file` gets today.
  - Journal files over 4 MiB are treated as corrupt (`dispatch_journal_unavailable`).

## Live E2E request

`e2e-request: required`: new externally visible behaviour (routes, codes), a change to admission and the authority choice, and new `UNKNOWN`/fail-closed paths.

**Preconditions**

- An image built from `feat/5-goal-grant` @ `3348348`, pinned by digest.
- Clawake config:
  - adds `goal_grant_file = "/run/onshape-approval/goal-grant.json"` (same ro mount as the approval file) and `dispatch_journal_dir = "/var/lib/onshape-mcp/journal"`;
  - `[api] timeout = 30`;
  - keeps the static bounds unchanged and sets no pins.
- **Container/deployment need (for Clawake):**
  - a **persistent rw volume** at `/var/lib/onshape-mcp/journal`;
  - mounted only into `clawonshape-mcp` and kept across restarts and image upgrades;
  - the directory owned by the **server's runtime UID** (the same UID that owns the token and approval files, G7) with mode `0700`;
  - no group or other bits; not a symlink; the path exactly as configured;
  - the files inside are created by the server with `0600`.
  - A wrong owner or mode does not stop startup: the server starts, `/capabilities` omits `goal-grant-v1`, and goal mutations fail with `dispatch_journal_unavailable`.
- Plugin with the goal workflow (clawonshape#18).

**Verify**

1. `GET /capabilities` → `{"contractVersion":"1.1.0","mutationAuthorities":["approval-v2","goal-grant-v1"]}`.
2. Grant `null` + approval v2 → the existing v2 flow is unchanged (one dispatch → `VERIFIED`; replay → `mutation_approval_consumed`).
3. One ACTIVE grant, two different feature updates in the Candidate → two dispatches, `journal-summary` reports `entries 2, counted 2, maxSequence 2, unreconciledSequence null, lastOutcome OK`.
4. Re-send of an identical request → `goal_dispatch_duplicate`, no provider call.
5. STOP (grant `STOPPED`) → the next mutation fails with `goal_grant_stopped`.
6. Restart the container → the summary is unchanged and the budget is preserved.
7. Grant + active approval → `mutation_authority_conflict`.
8. Reads of the Candidate `currentmicroversion`/`elements`/`features` are admitted while the grant exists.

## Open points and risks

- **Unrelated worktree changes:** while I was working, uncommitted changes appeared in the shared worktree that are not mine: `README.md`, `docs/src/project/{implementation,index,overview,requirements}.md` modified and `spec.md` untracked (dated 2026-10-21 11:30). I did not touch, stage or commit them.
- The production dispatch (`Provider::Onshape`) is not exercised by tests (no network). Live E2E is required.
- The admission lock is per process. Two server processes over the same journal directory are not supported; the contract assumes one server.
- The pre-existing `cargo deny` advisory RUSTSEC-2026-0382 needs a separate lockfile-bump prompt.
- `dispatch_journal_unavailable` for a foreign-owner directory is only covered structurally (no privileges in the devcontainer).

## Out-of-scope findings

- `crates/onshape-mcp-io/src/external_token.rs:109`: `private_json` opens with `custom_flags(0x20000)` as `O_NOFOLLOW`. That value is only correct on x86_64; on aarch64 `0x20000` is `O_LARGEFILE` and `O_NOFOLLOW` is `0x8000`. The canonical-path check before it still rejects symlinks, but it is not atomic (TOCTOU). The new `dispatch_journal.rs` uses per-architecture constants. I did not fix the existing code.
- `crates/onshape-mcp-io/src/external_token.rs:96`: `canonicalize(file_path) != file_path` compares `Path`s, which ignores interior `.` components (`/a/./b` == `/a/b`). This is harmless for the open itself but less strict than "canonical". For the new goal paths, startup rejects such paths (CR-7). Not fixed.
- The clawonshape HEAD (`cc27dc0`) differs from the prompt's `f937771`; the interface is byte-identical (see Summary).
