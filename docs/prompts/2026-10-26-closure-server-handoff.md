---
target-repo: edge-robot/onshape-mcp
branch: feat/5-goal-grant
worktree: /home/kaos/Projects/01_pib/02_sw/onshape-mcp (container: /workspaces/onshape-mcp)
issue: edge-robot/onshape-mcp#5, closure edge-robot/clawonshape#23
architecture-ref: clawonshape/docs/interface/onshape-mcp.md#9-versioning-and-compatibility
contract-version: 1.2.0-draft
foreign-repo-commit: the clawonshape HEAD of feat/21-work-context-selection at issue time (read-only mount)
base-commit: the commit that adds this prompt
status: done   # accepted 2026-10-26 (report ec121b7); #6 fixed 1226c6b; tokio advisory → #10; handoff links retargeted to the default branch
---
# Closure K3: server handoff on feat/5-goal-grant

## Goal

The project is closing; the decision record is edge-robot/clawonshape#23 and the closure ADR
"ADR-2026-10-26" in `clawonshape/docs/ADR.md`. Secure the server state of this fork:

- prove that the branch is green;
- fix the known advisory only if the fix is trivial;
- write an as-built handoff that a successor can pick up without chat context.

**No new features and no behaviour changes.** The deployed image is built from `cc67472`.

## Context

- Branch `feat/5-goal-grant` contains `claw-onshape/external-token`, the goal grant and the durable
  dispatch journal (#5), the healthcheck subcommand (#4), and the adopted fork spec `spec.md` (#8).
- Interface contract: `clawonshape/docs/interface/onshape-mcp.md` (read-only mount), `1.2.0-draft`.
- Open server issues:
  - #6: `cargo deny` advisory RUSTSEC-2026-0382 in rustls-webpki 0.103.15;
  - #7: `private_json` O_NOFOLLOW is x86_64-only and the path canonical check is lax;
  - #9: selected-context reads, parked.

## Binding constraints and prohibitions

- No change to tool behaviour, the admission rules, contract fixtures under
  `crates/onshape-mcp-core/tests/contract/`, the Channel A snapshot, `spec.md` or the `docs/src/project/**`
  pointers.
- #6: only a lockfile-level patch bump of `rustls-webpki` that `cargo deny check advisories` then
  accepts. Stop if the fix requires a minor or major version change of a direct dependency, a new
  `[advisories] ignore` or any code change; report that instead.
- #7: do not fix; describe the scope precisely in the handoff.
- Do not touch `Containerfile`, `.devcontainer/`, `scripts/dev.sh` or `.claude/`. No new
  dependencies.
- This branch is fork-only. Nothing goes to `altendky/onshape-mcp`.
- Work only on `feat/5-goal-grant`. No branches, worktrees or stashes. Never push.

## Steps

1. Verify branch and HEAD. Run, and record:
   - `cargo fmt --check`;
   - `cargo clippy --workspace --all-targets -- -D warnings`;
   - `cargo test --workspace`;
   - `cargo deny check`;
   - the contract tests.
2. #6, if trivial per the rule above: `cargo update -p rustls-webpki --precise <patched>`, then rerun
   all checks. Commit separately with `Refs: edge-robot/onshape-mcp#6`.
3. Write `docs/handoff-2026-10-26.md` (English). Keep it to about 150 lines, and give source
   paths, no prose duplication of the contract. It covers:
   - What the fork adds to upstream: modes, tools, admission, external-token handoff, goal grant,
     journal, healthcheck. Link to `spec.md` and to the contract sections.
   - Accepted versus implemented-only, from the existing reports in `docs/prompts/*.report.md` and
     the matrix in the contract §9. Live goal acceptance (G1–G7) is pending at the time of writing.
   - Build: how the deployed image `cc67472-goal-grant-curl` was produced (curl wrapper for the
     legacy `HealthCmd`) and how a successor rebuilds it reproducibly.
   - Open issues #6, #7 and #9 with exact scope, and the upstream-delta risk: commits behind
     `altendky/main`.
   - The Onshape REST quota note: every upstream API call counts against the plan's annual cap; the
     server has no call accounting. Name the code location where a per-request counter would go,
     but do not implement it.
4. Commit the handoff.

## Acceptance criteria and tests

| # | Criterion | Evidence |
|---|---|---|
| S1 | fmt, clippy, test, deny and contract tests pass (deny: pass, or #6 documented as blocked) | checks table |
| S2 | No behaviour change: `git diff --stat cc67472 -- crates` shows only `Cargo.lock` if #6 was fixed | diff stat |
| S3 | Handoff complete per step 3, with links that resolve on GitHub | file |

## Branch and worktree rules

Commit on `feat/5-goal-grant` only. Each commit has:

- author `Albert Kos <kickban3000@gmail.com>`;
- the trailers `Refs: edge-robot/onshape-mcp#5` (or `#6` for the advisory),
  `Prompt: docs/prompts/2026-10-26-closure-server-handoff.md`, `Agent: rust-mcp-dev` and
  `Co-Authored-By`.

Never push.

## Expected report / handoff format

Write `docs/prompts/2026-10-26-closure-server-handoff.report.md` with frontmatter `status` and
`e2e-request: none`. Include:

- the checks table;
- S1–S3;
- the #6 outcome;
- changed files;
- the commit list.
