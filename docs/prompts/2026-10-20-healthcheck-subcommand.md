---
target-repo: edge-robot/onshape-mcp
branch: claw-onshape/external-token
worktree: <workspace>/onshape-mcp (container: /workspaces/onshape-mcp)
issue: edge-robot/onshape-mcp#4
architecture-ref: clawonshape/docs/interface/onshape-mcp.md#1-topology-and-lifecycle
contract-version: 1.0.0-draft
foreign-repo-commit: b63a82b   # clawonshape, branch demo/gateway-native-read-20261014 (read-only mount)
base-commit: acceacd
status: done
---
# `onshape-mcp healthcheck` subcommand and SIGTERM shutdown (#4)

## Goal

Make the distroless runtime image (`Containerfile --target runtime`) health-checkable without a
shell or `curl`. Make it stop promptly on SIGTERM when it runs as PID 1.

**Background (live, 2026-10-20).**
- The Clawake quadlet runs `HealthCmd=curl --fail --silent http://127.0.0.1:8080/health` with
  `HealthOnFailure=kill`. In the distroless image this command can never succeed, so systemd
  killed the server about every 60 s (97 restarts).
- The stopgap is a Debian + `curl` wrapper image around the same binary.
- Separately, `podman stop -t 2` had to fall back to SIGKILL: the server ignored SIGTERM.

Read edge-robot/onshape-mcp#4 and edge-robot/clawake#39 for details.

## Context

- `crates/onshape-mcp/src/main.rs`: the clap `Cli` with `Command::{Auth, Http}`. Check
  whether config loading happens before the subcommand dispatch. `healthcheck` must not need
  a config file, credentials or network access beyond the probed URL.
- `crates/onshape-mcp-io/src/external_token.rs` ~L296: the routes `/health` (always 200)
  and `/health/ready`.
- `crates/onshape-mcp-io/src/oauth_server.rs`: the OAuth-mode `/health`.
- `Containerfile`: the runtime stage (distroless `static-debian13:nonroot`, `USER 1000:1000`)
  and the comment that announces this subcommand.
- Contract §1, topology and lifecycle, in the read-only mount
  `/workspaces/clawonshape/docs/interface/onshape-mcp.md`.

## Binding constraints and prohibitions

1. **CLI shape.**
   ```
   onshape-mcp healthcheck [--url <URL>] [--ready] [--timeout-ms <N>]
   ```
   - Default URL: `http://127.0.0.1:8080/health`. `--ready` switches the default path to
     `/health/ready`.
   - `--timeout-ms` defaults to 3000 and is capped at 10000.
   - Exit codes:
     - `0` on HTTP 200;
     - `1` on any other status, a connection error or a timeout;
     - `2` on invalid arguments (for example a non-`http://` URL or a non-loopback host).
2. **Behaviour.**
   - On success, print nothing.
   - On failure, print exactly one line to stderr: the status or error class only. Never
     print a response body, a header or an environment value.
   - Never read the config file, the token file or the approval file.
   - Do not start tokio's multi-threaded runtime if a lighter path exists. In any case, run
     no server and no background work.
3. **Dependencies.** Prefer a minimal HTTP/1.1 GET over `tokio::net::TcpStream` or
   `std::net::TcpStream` and parse only the status line. Use an HTTP client only if one is
   already a workspace dependency of the `onshape-mcp` crate's dependency graph. **No new
   external crates**, and no change to `deny.toml`.
4. **Loopback only.** The host must be `127.0.0.1`, `::1` or `localhost`. This keeps the
   command from becoming an SSRF probe.
5. **SIGTERM.** The `http` server installs SIGTERM and SIGINT handlers and shuts down
   gracefully (axum graceful shutdown or the equivalent) within 5 s:
   - in-flight requests finish or are cancelled;
   - the process exits 0.

   Do not change any admission, token or approval semantics. The consumed-approval record
   stays in memory, as documented in contract §2.5.
6. **Runtime image.** Add `HEALTHCHECK CMD ["/usr/local/bin/onshape-mcp","healthcheck"]` to
   the `runtime` stage and update the comment there. Do not change the base image.
7. **Scope.**
   - Allowed: `crates/onshape-mcp/`, `crates/onshape-mcp-io/` (only the shutdown wiring),
     `Containerfile`, `docs/` for the CLI reference if one exists, and tests.
   - Not allowed: the contract fixtures under `crates/onshape-mcp-core/tests/contract/`
     (read-only), `mcp-tools.json`, `.claude/`, `.devcontainer/`, `scripts/dev.sh`.
   - Never run `podman`, `systemctl` or a deployment.
8. **Commits** go to this fork branch only. Prompts and reports never go into branches that
   are destined for `altendky/onshape-mcp`.

## Steps

1. Check the preconditions:
   - the branch is `claw-onshape/external-token` and HEAD is `acceacd`;
   - the worktree is clean apart from this prompt.

   Record a baseline with `cargo fmt --check`, `cargo clippy --workspace --all-targets
   --all-features -D warnings`, `cargo nextest run --workspace` (or `cargo test`) and
   `cargo deny check`.
2. Write the failing tests first.
   - Unit tests for argument validation:
     - a non-loopback host gives exit 2;
     - the timeout cap holds.
   - Integration tests against a local listener bound to port 0 inside the test:
     - 200 gives exit 0 with no output;
     - 503 gives exit 1 with one stderr line;
     - a closed port gives exit 1;
     - a listener that never answers gives exit 1 within the timeout;
     - `--ready` hits `/health/ready`.
   - A shutdown test: start the `http` external-token server with a test config, send
     SIGTERM (or trigger the shutdown future), and assert that it exits within 5 s.
3. Implement. Use one commit per concern:
   - the subcommand;
   - the graceful shutdown;
   - the Containerfile `HEALTHCHECK`.

   Each commit carries `Refs: edge-robot/onshape-mcp#4`.
4. Run the checks and write the report.

## Acceptance criteria and tests

- [ ] fmt, clippy (`-D warnings`), the tests and `cargo deny check` are green. The test count
      is the baseline plus the new tests.
- [ ] Mutation checks, each of which must fail a test:
  - accept a non-loopback host;
  - treat 503 as success;
  - remove the shutdown handler.
- [ ] `tools/list` is unchanged, so the `mcp-tools.json` contract test is still green.
- [ ] There are no new `#[allow]` attributes without a justification in the report.
- [ ] `git show --stat` of every commit lists only allowed paths.

## Branch and worktree rules

- Commit only to `claw-onshape/external-token` in this checkout.
- No push, no rebase, and no other branches or worktrees.
- Commit this prompt unchanged together with your report. Leave the worktree clean.

## Expected report / handoff format

`docs/prompts/2026-10-20-healthcheck-subcommand.report.md` in the agent-definition format.
It contains:

- the checks table, baseline vs. after;
- the tests before and after;
- the mutation results;
- the exact CLI help text;
- the exact stderr lines;
- the measured shutdown time;
- the commit list with trailers.

Set `e2e-request: required`.

The architect will:
1. build the runtime image;
2. run `podman run --rm <image> healthcheck` against a running container;
3. switch the Clawake `HealthCmd` (edge-robot/clawake#39);
4. roll the image out. The rollout is held until the dashboard work (clawonshape#17) is done.
