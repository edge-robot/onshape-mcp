---
prompt: docs/prompts/2026-10-15-g1-external-token-tools-contract.md
branch: claw-onshape/external-token
commits: 2ab2424..be15c5e
status: partial
contract-version: 0.2.0-draft
e2e-request: required
---
# Report: G1 single-source external-token tool allowlist and contract snapshot test

## Summary

- The external-token tool allowlist now exists once, in `onshape-mcp-core::tools`
  (`EXTERNAL_TOKEN_TOOL_NAMES`, `is_external_token_tool`, `list_tools_external_token`).
  `onshape-mcp-io` uses it for both `tools/list` and `external_token_tool_denied`.
  Behaviour is unchanged.
- New contract test `crates/onshape-mcp-core/tests/contract.rs`. It passes against the
  read-only fixture copy `0.2.0-draft`, and the snapshot matches exactly. There is no
  contract deviation.
- G12: a test pins the current behaviour. `onshape_auth_status {"validate": true}` in
  external-token mode is rejected with `external_token_read_policy_denied` (invalid_request,
  no data). The rejection happens before the credential is loaded and before any client is
  built, so there is no provider I/O.
- **Why `status: partial`:** all deliverables are done. Only the mandatory check
  `cargo deny check` fails, on 4 new RustSec advisories against `rustls 0.23.45` in the
  unchanged `Cargo.lock`. The failure also reproduces on the base commit without my
  changes. The fix needs a lockfile update (`cargo update -p rustls`, ≥ 0.23.46). That is
  a dependency update, which is out of scope and needs approval, so I did not run it.

## Changed files

`git diff --stat 29f3461..be15c5e` (base = prompt-issued commit):

```
 crates/onshape-mcp-core/src/tools.rs      |  76 ++++++++++++++
 crates/onshape-mcp-core/tests/contract.rs | 163 ++++++++++++++++++++++++++++++
 crates/onshape-mcp-io/src/lib.rs          |  82 ++++++++++-----
 3 files changed, 298 insertions(+), 23 deletions(-)
```

- `crates/onshape-mcp-core/src/tools.rs`: adds the allowlist const, the predicate and the
  filtered catalog (with `Contract-Ref` comment and `///` docs), plus 3 inline unit tests.
- `crates/onshape-mcp-core/tests/contract.rs`: new contract tests (VERSION, exact
  snapshot match with a diff hint, name order) and a unit test for the diff helper.
- `crates/onshape-mcp-io/src/lib.rs`: replaces the two hard-coded name lists with the core
  functions, and adds the G12 pin test.

Commits:
- `2ab2424` refactor: ♻️ single-source external-token tool allowlist in core
- `8c49fa9` test: 🧪 add external-token tools/list contract snapshot test
- `be15c5e` test: 🧪 pin onshape_auth_status validate=true in external-token mode (G12)
- plus the commit containing this report

## Checks

| Command | Result |
|---|---|
| `cargo fmt --all --check` | pass |
| `cargo clippy --all-targets --all-features -- -D warnings` | pass |
| `cargo nextest run --all-features` | pass: 616 run, 616 passed, 0 skipped |
| `cargo nextest run -p onshape-mcp-core --test contract` | pass: 4 run, 4 passed (see below) |
| `cargo deny check` | **fail**: `advisories FAILED, bans ok, licenses ok, sources ok` (pre-existing, see below) |
| `python scripts/apply-openapi-overlays.py --check` | not run: OpenAPI and overlays untouched |
| `mdbook build docs` | not run: `docs/src/` untouched (only `docs/prompts/` report added) |
| `cargo llvm-cov nextest --workspace --all-features` | run: new branches covered (see below) |

Contract test output:
```
    Starting 4 tests across 1 binary
        PASS (1/4) onshape-mcp-core::contract contract_version_matches
        PASS (2/4) onshape-mcp-core::contract first_difference_names_the_first_differing_key
        PASS (3/4) onshape-mcp-core::contract external_token_tool_names_match_snapshot
        PASS (4/4) onshape-mcp-core::contract external_token_tools_match_contract_snapshot
     Summary 4 tests run: 4 passed, 0 skipped
```

`cargo deny check` failure (all against `Cargo.lock:151` `rustls 0.23.45`, solution
"Upgrade to >=0.23.46"):
- RUSTSEC-2026-0346: `Debug` impls for client session values expose TLS secrets
- RUSTSEC-2026-0347: buffered application data sent to outer server after ECH rejection
- RUSTSEC-2026-0348: Brotli certificate decompression can allocate up to 1 GiB before authentication
- RUSTSEC-2026-0349: unauthenticated ServerHello can downgrade later connections to classical key exchange

I confirmed it is pre-existing: with my changes stashed, `cargo deny check advisories`
also reports `advisories FAILED`. `Cargo.lock` and `Cargo.toml` are unchanged by this task.

Coverage (`cargo llvm-cov`, workspace):
- `is_external_token_tool`: 57 hits.
- `list_tools_external_token`: 4 hits.
- `onshape-mcp-io/src/lib.rs`:
  - `list_tools` external branch (l. 481): 1 hit; full branch (l. 483): 4 hits.
  - `call_tool_stdio` deny branch (l. 559–563): 1 hit; allow path: 31 hits.

  The `onshape-mcp-io` branches are covered by the existing CLI tests in
  `crates/onshape-mcp/tests/cli.rs`.

## Acceptance criteria

| # | Status | Evidence |
|---|---|---|
| A1 | met | `grep -rn '"onshape_api_search"' crates --include=*.rs` → `onshape-mcp-core/src/tools.rs:579` (the allowlist const). The other hits are tests (`tools.rs:1595,1682,2000,2023,2044`, `onshape-mcp/tests/cli.rs:1035,1058`) or the metadata definition (`tools/onshape/metadata.rs:15`). There are no hits in `onshape-mcp-io`. |
| A2 | met | `crates/onshape-mcp-io/src/lib.rs:480-484` `let catalog = if self.config.external_token.is_some() { tools::list_tools_external_token() } else { tools::list_tools() };`. `crates/onshape-mcp-io/src/lib.rs:559` `if self.config.external_token.is_some() && !tools::is_external_token_tool(&request.name) {` → unchanged `external_token_tool_denied` |
| A3 | met | nextest contract output above (4/4 pass) against `tests/contract/VERSION` = `0.2.0-draft` |
| A4 | met | `external_token_tools_match_contract_snapshot` compares the whole `Value` array (order, annotations, schemas). `git diff --stat 29f3461..HEAD -- crates/onshape-mcp-core/tests/contract crates/onshape-mcp-core/tests/fixtures` is empty. `onshape_api_metadata_matches_published_contract` is untouched. |
| A5 | met | `PASS onshape-mcp-core tools::tests::list_tools_has_eleven_tools`. `list_tools()` is unchanged. The non-external branch still returns the full catalog. |
| A6 | met | test `onshape-mcp-io tests::external_token_auth_status_validate_is_read_policy_denied_before_io` (`crates/onshape-mcp-io/src/lib.rs:3396`) plus the trace under *Open points* |
| A7 | partial | fmt, clippy and nextest pass. `cargo deny check` fails on the pre-existing rustls advisories (see *Checks*). |
| A8 | met | No dependency changes (Cargo.toml and Cargo.lock untouched). No new `#[allow]`, and `contract.rs` needs none because its tests return `Result<(), String>`. The only new `expect`/`expect_err`/`panic!` are inside the existing `#[cfg(test)] mod tests` of `onshape-mcp-io/src/lib.rs` (G12 test). There are no new `unwrap`/`expect` in non-test code. |

## Contract deviations and change requests

None. The code output matches `tests/contract/mcp-tools.json` exactly. No fixture,
snapshot or contract file was changed.

## Live E2E request

Preconditions: an image built from `claw-onshape/external-token` @ `be15c5e` or later.
Run the server in external-token mode (`[external_token]` configured, Streamable HTTP),
with the Engineer `toolFilter` from contract §2.2.

1. `openclaw mcp probe onshape --json` in the Engineer.
   - Expected: exactly the four `toolFilter` tools: `onshape_auth_status`,
     `onshape_api_explain`, `onshape_api_schema` and `onshape_api_call`.
   - The server itself still lists five, including `onshape_api_search`.
2. Call a non-allowlisted tool (e.g. `onshape_screenshot` or `onshape_auth_login`)
   directly against `/mcp`.
   - Expected: an MCP error `invalid_request` with message `external_token_tool_denied`.
3. Optional (G12): call `onshape_auth_status` with `{"validate": true}`.
   - Expected: an MCP error `invalid_request` with message
     `external_token_read_policy_denied`, and no outgoing request to Onshape.

## Open points and risks

- **G12 finding (observed behaviour, pinned by test):**
  1. `crates/onshape-mcp-core/src/tools.rs:941`: `validate != Some(true)` would return the
     cached state. With `validate: true`, `:952-972` builds operation `sessionInfo` →
     `ToolEffect::ApiRequest { GET /users/sessioninfo }`. This happens regardless of
     `resolved_auth`.
  2. `crates/onshape-mcp-io/src/lib.rs:736`: `executor.execute(api_req)`.
     `crates/onshape-mcp-io/src/request.rs:28-34`: `ApiState::ExternalToken` passes the
     state gate. `crates/onshape-mcp-io/src/lib.rs:908-909`: `external_token::execute`.
  3. `crates/onshape-mcp-io/src/external_token.rs:188`: `validate_config`. `:189`: GET →
     not a mutation. `:198-202`: `allowed()` (`:131`) is false because the path is not in
     the §2.4 read set. The call returns `McpError::invalid_request("external_token_read_policy_denied", None)`.
  4. The credential `load()` (`:214`) and the client build come only after this check, so
     there is no file read of the token and no provider I/O. The test proves this with a
     non-existent token file: a later check would have produced `external_token_unavailable`.
     It also asserts that the validation state is unchanged (`update_implicit_validation`
     is not reached).
  5. Consequence for the contract: in external-token mode, `validate: true` is never
     useful. It always fails with a code whose documented consumer action ("correct the
     request shape") does not fit an auth-status call. `validate: false`/omitted works and
     reports the token-file state.
  6. Decision for the architect: either document this in §2.3/§6, or make a contract
     change. One option: in external-token mode, `validate: true` returns the local status
     with a hint. I changed nothing here, as instructed.
- **cargo deny:** the rustls advisories block a clean check on this branch. Some of them
  are TLS-relevant (downgrade, secret exposure in `Debug`), so I recommend a separate
  prompt for `cargo update -p rustls` (needs `e2e-request: required` as a dependency update).

## Out-of-scope findings

- `/workspaces/clawonshape/docs/interface/onshape-mcp.md:77-83` (§2.3 table): the rows list
  `onshape_api_schema` before `onshape_api_call`, but the snapshot and the server order are
  `…, onshape_api_call, onshape_api_schema`. The contract says the snapshot order is
  authoritative, so this is only an editorial inconsistency in the table. Not fixed.
- `Cargo.lock:151` `rustls 0.23.45`: RUSTSEC-2026-0346..0349 (see *Checks*). Not fixed.
