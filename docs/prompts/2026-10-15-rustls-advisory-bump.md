---
target-repo: edge-robot/onshape-mcp
branch: claw-onshape/external-token
worktree: /home/kaos/Projects/01_pib/02_sw/onshape-mcp (container: /workspaces/onshape-mcp)
issue: edge-robot/onshape-mcp#3
architecture-ref: clawonshape/docs/interface/onshape-mcp.md#8-security-invariants
contract-version: 0.2.0-draft
foreign-repo-commit: f49b76d05c448ded91f05b8498aa4b7633e99dab
status: done
---
# Bump rustls past RUSTSEC-2026-0346..0349 (lockfile only)

## Goal

Make `cargo deny check` pass again on `claw-onshape/external-token`. To do that, update
`rustls` in `Cargo.lock` from `0.23.45` to the newest compatible `0.23.x` (≥ `0.23.46`).
Do not change any `Cargo.toml`.

## Context

- Issue: https://github.com/edge-robot/onshape-mcp/issues/3
- Found in the G1 report `docs/prompts/2026-10-15-g1-external-token-tools-contract.report.md`
  (*Checks*, *Out-of-scope findings*).
- Advisories against `rustls 0.23.45` (`Cargo.lock`), all with solution "Upgrade to >=0.23.46":
  - RUSTSEC-2026-0346: `Debug` impls for client session values expose TLS secrets
  - RUSTSEC-2026-0347: buffered application data sent to outer server after ECH rejection
  - RUSTSEC-2026-0348: Brotli certificate decompression can allocate up to 1 GiB before authentication
  - RUSTSEC-2026-0349: unauthenticated ServerHello can downgrade later connections to classical key exchange
- `rustls` is a transitive dependency, mainly via `reqwest` (`Cargo.toml:40`, feature
  `rustls`). The server uses it for TLS to `cad.onshape.com` with the operator's token
  (contract §8, security invariants).
- Toolchain: MSRV `rust-version = 1.89`, toolchain 1.99.

## Binding constraints and prohibitions

- **Lockfile only.** No change to any `Cargo.toml`, no new crates, no feature changes.
- Run exactly `cargo update -p rustls`. This command is on your *ask* list, and the user
  will approve it in your pane. Do not run a plain `cargo update`, and do not run
  `cargo update` for other packages.
- If `cargo update -p rustls` also changes other packages, list every changed package
  with old → new version. Stop with `status: blocked` if any of them is a direct
  dependency or a major or minor bump of a security-relevant crate (`ring`,
  `aws-lc-rs`, `rustls-*`, `webpki*`, `hyper*`, `reqwest`).
- **MSRV:** read `rust-version` of the new `rustls` from the registry source
  (`~/.cargo/registry/src/*/rustls-<new>/Cargo.toml`, or `cargo metadata`). If it is
  greater than `1.89`, stop with `status: blocked`. Raising the MSRV needs an ADR.
- Do not add `ignore` entries to `deny.toml` and do not weaken it in any other way.
- No code changes. If the new rustls needs code changes, stop and report.
- No network except the crates.io registry for the update itself and the RustSec
  advisory database for `cargo deny`. Never call the real Onshape API.

## Steps

1. Preconditions (stop and report on any mismatch):
   - `git branch --show-current` → `claw-onshape/external-token`
   - `git rev-parse HEAD~1` → `5932f7dff2dbc71e65dbd46a7ffc9d480cd860f4` (HEAD is the commit that issued this prompt)
   - `git status --short` is empty
   - `head -1 crates/onshape-mcp-core/tests/contract/VERSION` → `0.2.0-draft`
   - `git -C /workspaces/clawonshape rev-parse HEAD` → `f49b76d05c448ded91f05b8498aa4b7633e99dab`
2. Record the baseline: run `cargo deny check advisories` and keep the advisory IDs.
3. Run `cargo update -p rustls`, then `git diff Cargo.lock`. Record each changed package
   with old → new version.
4. Check the MSRV of the new rustls (see constraints).
5. Run all mandatory checks from your agent definition, in full: fmt, clippy, nextest
   `--all-features`, and `cargo deny check` (all sections, not only advisories).
6. Commit `Cargo.lock` alone, with the message
   `fix: 🔒 bump rustls to <version> for RUSTSEC-2026-0346..0349` and these trailers:
   ```
   Refs: edge-robot/onshape-mcp#3
   Prompt: docs/prompts/2026-10-15-rustls-advisory-bump.md
   Agent: rust-mcp-dev
   Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
   ```

## Acceptance criteria and tests

| # | Criterion | Evidence |
|---|---|---|
| R1 | `rustls` in `Cargo.lock` is ≥ `0.23.46` and in `0.23.x` | `git diff Cargo.lock` excerpt |
| R2 | Only `Cargo.lock` changed; every changed package is listed | `git diff --stat HEAD~1` against the prompt commit, package table |
| R3 | `cargo deny check` passes in all sections | exact output summary line |
| R4 | fmt, clippy `-D warnings` and nextest `--all-features` pass, with test counts | command → result |
| R5 | MSRV of the new rustls is ≤ 1.89 | `rust-version` value and its source |
| R6 | Contract tests still pass (`-p onshape-mcp-core --test contract`) | nextest output |

## Branch and worktree rules

- Work and commit only on `claw-onshape/external-token` in `/workspaces/onshape-mcp`.
  Do not create branches or worktrees, do not switch branches, and never push.
- The worktree is returned when the report is committed and `git status --short` is
  clean. The architect reviews and pushes.

## Expected report / handoff format

Write `docs/prompts/2026-10-15-rustls-advisory-bump.report.md` in the format from your
agent definition (*Report*), with `contract-version: 0.2.0-draft`.

- `e2e-request: required`, because this is a dependency update. The live check for the
  architect: the rebuilt `clawonshape-mcp` image performs an allowed external-token
  read (§2.4) against `cad.onshape.com` successfully. The architect bundles this with
  the G1 live E2E.
- Put the before and after `cargo deny` advisory lists under *Checks*.
