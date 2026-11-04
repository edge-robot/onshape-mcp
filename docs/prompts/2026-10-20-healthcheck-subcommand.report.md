---
prompt: docs/prompts/2026-10-20-healthcheck-subcommand.md
branch: claw-onshape/external-token
commits: ba46bd0..e4b0191
status: partial
contract-version: 1.0.0-draft
e2e-request: required
---
# Report: `onshape-mcp healthcheck` subcommand and SIGTERM shutdown (#4)

## Summary

- **Subcommand `onshape-mcp healthcheck [--url <URL>] [--ready] [--timeout-ms <N>]`.**
  It uses a blocking `std::net::TcpStream` and sends one HTTP/1.1 `GET`. It reads only
  the status line (at most 1024 bytes). It runs before any tokio runtime is built and
  reads no config, token or approval file.
  - Exit codes: `0` only on HTTP 200, `1` otherwise, `2` on invalid arguments.
  - Only loopback hosts are accepted. `localhost` maps to `127.0.0.1` and then `::1`
    without DNS or `/etc/hosts`.
  - No new crates and no `Cargo.toml` or `deny.toml` change.
- **Graceful shutdown.** Both HTTP transports (external-token and OAuth) now go through
  `serve_until_shutdown` in `onshape-mcp-io`.
  - It reacts to SIGTERM and SIGINT.
  - It cancels the MCP sessions, lets in-flight requests drain for at most 3 s and
    returns `Ok`.
  - `main` then shuts the runtime down with `shutdown_timeout(1 s)`.
  - The process exits 0, measured about 20 ms after the signal.
- **Not done: the `Containerfile` `HEALTHCHECK` (step 3, third commit).** `Containerfile`
  is mounted read-only in the devcontainer (`OSError: [Errno 30] Read-only file system`).
  My agent rules also assign it to the architect. I did not work around this. The
  ready-to-apply diff is under *Open points and risks*. This is why the status is
  `partial`.

## Changed files

`git diff --stat acceacd..e4b0191` (code commits; the report commit follows `e4b0191`):

| File | Purpose |
|---|---|
| `crates/onshape-mcp/src/healthcheck.rs` (+507, new) | Argument validation, loopback URL parser, std-net probe, status-line parser, 12 unit tests |
| `crates/onshape-mcp/src/main.rs` (+26/−2) | `Command::Healthcheck`. `main` is now a plain fn: it dispatches the healthcheck before the runtime exists, then builds the multi-thread runtime explicitly (same as `#[tokio::main]`) and bounds the runtime shutdown to 1 s |
| `crates/onshape-mcp/tests/healthcheck.rs` (+239, new) | 12 binary integration tests against local port-0 listeners |
| `crates/onshape-mcp/tests/shutdown.rs` (+222, new) | 5 binary tests: SIGTERM/SIGINT for external-token and OAuth `http`, stalled in-flight request, healthcheck against the live server |
| `crates/onshape-mcp-io/src/lib.rs` (+204/−8) | `SHUTDOWN_DRAIN`, `shutdown_signal`, `serve_until_shutdown`, `serve_with_shutdown`; OAuth `run_http` uses them; 3 unit tests |
| `crates/onshape-mcp-io/src/external_token.rs` (+1/−6) | External-token `run_http` uses `serve_until_shutdown` |
| `docs/prompts/2026-10-20-healthcheck-subcommand.md` | The prompt, committed unchanged |
| `docs/prompts/2026-10-20-healthcheck-subcommand.report.md` | This report |

## Checks

| Command | Baseline (`acceacd`) | After (`e4b0191`) |
|---|---|---|
| `cargo fmt --all --check` | pass | pass |
| `cargo clippy --workspace --all-targets --all-features -- -D warnings` | pass | pass |
| `cargo nextest run --workspace --all-features` | pass, **637** tests | pass, **669** tests (+32) |
| `cargo nextest run -p onshape-mcp-core --test contract` | pass (13) | pass (13); `tools/list` snapshot unchanged |
| `cargo deny check` | `advisories ok, bans ok, licenses ok, sources ok` | same |
| `mdbook build docs` | – | pass (`docs/src/` not touched) |
| `python scripts/apply-openapi-overlays.py --check` | not run: OpenAPI and overlays not touched | – |
| `cargo llvm-cov nextest --workspace --all-features --text` | – | see below |

The 32 new tests:

| Location | Tests |
|---|---|
| `healthcheck::tests` (unit) | `defaults_probe_liveness_on_8080`, `ready_switches_default_path`, `non_loopback_hosts_are_rejected`, `non_http_schemes_are_rejected`, `malformed_urls_are_rejected`, `bad_ports_are_rejected`, `loopback_hosts_are_accepted`, `timeout_is_capped_at_ten_seconds`, `zero_timeout_is_rejected`, `invalid_arguments_exit_two`, `status_lines_parse_strictly`, `failure_lines_name_only_the_class` |
| `tests/healthcheck.rs` | `http_200_exits_zero_silently_without_config` (with `--config /nonexistent/config.toml`), `http_503_exits_one_with_single_status_line` (body and header markers absent from stderr), `non_200_success_codes_are_unhealthy` (204), `garbage_response_exits_one`, `closed_port_exits_one`, `silent_listener_times_out_within_budget` (300 ms; asserts ≥ 300 ms and < 3 s), `ready_path_is_requested_verbatim`, `ready_flag_conflicts_with_url`, `non_loopback_url_exits_two_without_connecting`, `zero_timeout_exits_two`, `closed_without_status_line_exits_one`, `overlong_status_line_exits_one` |
| `onshape-mcp-io` `tests` (unit) | `shutdown_lets_in_flight_request_finish_within_drain`, `shutdown_cancels_stuck_request_after_drain_deadline`, `serve_runs_until_signal` |
| `tests/shutdown.rs` | `sigterm_stops_server_with_exit_zero`, `sigint_stops_server_with_exit_zero`, `sigterm_stops_oauth_http_server_with_exit_zero`, `sigterm_with_stalled_in_flight_request_stops_within_budget`, `healthcheck_probes_live_external_token_server` |

**Failing first.** The shutdown tests were written before the fix and run against the
unfixed code:
- `sigterm_stops_server_with_exit_zero` and `sigterm_with_stalled_in_flight_request_stops_within_budget`
  failed with `TERM: exit signal: 15 (SIGTERM)`.
- `sigint_stops_server_with_exit_zero` passed, because Ctrl+C was already handled.

The healthcheck tests were written together with the new subcommand, so there is no
earlier failing run. The mutation checks below show that they catch regressions.

**Coverage** (`cargo llvm-cov`). All new branches are covered except these:
- `ArgError::{Port, Malformed}` `Display` arms: only the variants are asserted, not
  their text.
- `remaining()` → `Timeout` when the deadline is already zero before a socket call.
- `io_failure` → `ConnectionFailed` (no reliable local trigger).
- The multi-chunk path of the status-line reader. The kernel delivers the short test
  responses in one read.
- The two "signal handler could not be installed" fallbacks in `shutdown_signal`.

## Acceptance criteria

| Criterion | Result | Evidence |
|---|---|---|
| fmt, clippy `-D warnings`, tests and `cargo deny check` are green; the count is the baseline plus the new tests | **met** | 637 → 669 = 637 + 32 new tests |
| Mutation: accept a non-loopback host → a test fails | **met** | `_ => return Err(NonLoopbackHost)` → `_ => vec![127.0.0.1]`: 3 failures (`non_loopback_hosts_are_rejected`, `invalid_arguments_exit_two`, `non_loopback_url_exits_two_without_connecting`) |
| Mutation: treat 503 as success → a test fails | **met** | `Some(200)` → `Some(200 \| 503)`: 2 failures (`http_503_exits_one_with_single_status_line`, `healthcheck_probes_live_external_token_server`) |
| Mutation: remove the shutdown handler → a test fails | **met** | (a) SIGTERM arm replaced by `pending()`: 4 failures (all SIGTERM tests). (b) `shutdown_signal()` replaced by `pending()`: 5 failures (all of `tests/shutdown.rs`). (c) Additionally, the drain deadline removed: `shutdown_cancels_stuck_request_after_drain_deadline` fails |
| `tools/list` unchanged, `mcp-tools.json` contract test green | **met** | `contract` 13/13 pass; no snapshot or fixture touched |
| No new `#[allow]` without justification | **met** | One new attribute: `#[allow(clippy::expect_used)]` on the test-only `mod tests` in `crates/onshape-mcp/src/healthcheck.rs`. Reason: same convention as `main.rs`'s test module, test code only. The new `tests/*.rs` files use `Result`-returning tests and need no `#[allow]` |
| `git show --stat` of every commit lists only allowed paths | **met** | `ba46bd0`, `e4b0191`: `crates/onshape-mcp/**`. `8178fb7`: `crates/onshape-mcp-io/src/{lib,external_token}.rs` (shutdown wiring and its tests only), `crates/onshape-mcp/tests/shutdown.rs`. The report commit: `docs/prompts/**` |
| Step 3: three commits, one of them for the Containerfile `HEALTHCHECK` | **partial** | The subcommand and shutdown commits exist. The `Containerfile` is read-only in the container, so there is no commit (see *Open points*) |
| No `unwrap`/`expect`/`panic` outside tests, no `unsafe` | **met** | New invariant allows: none |

**Exact CLI help** (`onshape-mcp healthcheck --help`):
```
Probe a local `/health` endpoint; exits 0 on HTTP 200.

For container healthchecks in images without a shell or curl. Exits 1 on any other status, a connection error or a timeout, and 2 on invalid arguments. Only loopback `http://` URLs are accepted. Reads no config, token or approval file and prints nothing on success.

Usage: onshape-mcp healthcheck [OPTIONS]

Options:
      --url <URL>
          URL to probe. Must be `http://` on 127.0.0.1, `[::1]` or localhost.
          
          Default: `http://127.0.0.1:8080/health`.

      --ready
          Probe `/health/ready` instead of `/health` on the default address

      --timeout-ms <TIMEOUT_MS>
          Total timeout in milliseconds (connect, write and read); capped at 10000
          
          [default: 3000]

  -h, --help
          Print help (see a summary with '-h')
```
In `onshape-mcp --help` the subcommand appears as
`healthcheck  Probe a local `/health` endpoint; exits 0 on HTTP 200`.

**Exact stderr lines.** Success prints nothing on stdout or stderr. Every probe failure
prints exactly one line:

| Situation | Exit | stderr |
|---|---|---|
| Non-200 status | 1 | `healthcheck: unhealthy status <code>` (for example `… 503`, `… 204`) |
| Nothing listening | 1 | `healthcheck: connection refused` |
| Other connect or I/O error | 1 | `healthcheck: connection failed` |
| Deadline exceeded | 1 | `healthcheck: timeout` |
| Not an HTTP/1.x status line, EOF, or line > 1024 B | 1 | `healthcheck: invalid response` |
| Rejected `--url` or `--timeout-ms 0` | 2 | `healthcheck: invalid argument: <reason>`, where `<reason>` is one of `url scheme must be http`, `url host must be loopback (127.0.0.1, [::1] or localhost)`, `url port is invalid`, `url is malformed`, `timeout-ms must be greater than 0` |
| clap parse errors (`--ready` together with `--url`, a non-numeric `--timeout-ms`, an unknown flag) | 2 | clap's standard multi-line usage error, for example `error: the argument '--ready' cannot be used with '--url <URL>'` |

**Measured shutdown time.** Measured with `tests/shutdown.rs --no-capture`, polling every
20 ms:
- SIGTERM, external-token: exit 0 after 20.1 ms.
- SIGINT: exit 0 after 20.1 ms.
- SIGTERM, OAuth: exit 0 after 20.1 ms.
- SIGTERM with a stalled `POST /mcp` body in flight: exit 0 after 20.2 ms.

The worst case is bounded by `SHUTDOWN_DRAIN` (3 s) plus the runtime `shutdown_timeout`
(1 s), so below 5 s. The drain deadline itself is proven in-process by
`shutdown_cancels_stuck_request_after_drain_deadline`, which uses a 200 ms drain.

**Commits.** Every commit carries these trailers:
`Refs: edge-robot/onshape-mcp#4`, `Prompt: docs/prompts/2026-10-20-healthcheck-subcommand.md`,
`Agent: rust-mcp-dev`, `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.

| SHA | Subject |
|---|---|
| `ba46bd0` | `feat: ✨ onshape-mcp healthcheck subcommand` |
| `8178fb7` | `fix: 🐛 graceful shutdown of the http server on SIGTERM` |
| `e4b0191` | `test: 🧪 healthcheck EOF and overlong status line` |
| report commit | `docs: 📖 report for healthcheck-subcommand prompt` (includes the unchanged prompt) |

## Contract deviations and change requests

None. No tool, field, error code, file format or snapshot changed.

Contract §1 already announces `onshape-mcp healthcheck` as a planned, minor,
server-first change. Once Clawake switches its `HealthCmd`, the architect may want to
update the §1 "Healthcheck" row from "Planned" to the shipped command line. That is a
contract edit for the architect, not for me.

## Live E2E request

**Preconditions:**
- A runtime image built from this branch with the `HEALTHCHECK` diff below applied
  (`podman build --target runtime`).
- The usual `[external_token]` config. No config change is needed.

**Verify:**
1. `onshape-mcp healthcheck`, run inside the running container
   (`podman exec <ctr> /usr/local/bin/onshape-mcp healthcheck; echo $?`), returns `0`
   with no output.
   - Note: `podman run --rm <image> healthcheck` starts a *new* container that has no
     server listening on its own loopback. Expect exit `1` and
     `healthcheck: connection refused` there, which is correct behaviour.
2. `… healthcheck --ready` returns `0` once channel B holds a valid token. Without a
   token it returns `1` with `healthcheck: unhealthy status 503`.
3. With Clawake `HealthCmd=/usr/local/bin/onshape-mcp healthcheck` (clawake#39), the
   unit stays `healthy` and the restart counter stays flat for more than 5 minutes.
4. `podman stop -t 2 <ctr>` completes without falling back to SIGKILL. Expect no
   `StopSignal`/SIGKILL warning and container exit code 0.
5. Regression: one read and one approved mutation still behave as in the 2026-10-20
   approval-v2 E2E. Shutdown wiring only; admission is unchanged.

## Open points and risks

1. **`Containerfile` `HEALTHCHECK` is not committed** (read-only mount, architect-owned).
   Proposed diff for the `runtime` stage, with the base image unchanged:
   ```diff
    EXPOSE 8080
   -# No HEALTHCHECK: distroless has no curl. Clawake keeps the current image until the
   -# planned `onshape-mcp healthcheck` subcommand exists (contract §1).
   +# Distroless has no shell or curl, so the binary probes itself: `onshape-mcp healthcheck`
   +# GETs http://127.0.0.1:8080/health (add --ready for /health/ready) and exits 0 only on
   +# HTTP 200. It reads no config, token or approval file (contract §1, edge-robot/onshape-mcp#4).
   +# Clawake's quadlet HealthCmd overrides this; see edge-robot/clawake#39.
   +HEALTHCHECK CMD ["/usr/local/bin/onshape-mcp","healthcheck"]
    ENTRYPOINT ["/usr/local/bin/onshape-mcp"]
   ```
   Note: podman ignores `HEALTHCHECK` for OCI-format images. Building with
   `--format docker` keeps it. Clawake's quadlet `HealthCmd` is authoritative anyway.
2. **In-flight mutation at shutdown.** A mutation that is still dispatched to Onshape
   (timeout 30 s, contract §7) when SIGTERM arrives is dropped after at most about 4 s.
   - The client then sees a transport error, so the outcome is uncertain.
   - There is no server-side retry. After a restart the consumed record is gone, but the
     revision CAS (§2.5 step 6) still blocks a second application, as the contract
     documents.
   - The plugin must treat a dropped connection during a mutation as `UNKNOWN`. That is
     already the contract's intent, but worth checking in E2E step 5.
3. **Behaviour change for the signal-install failure.** Previously `let _ = ctrl_c().await`
   resolved immediately if the handler could not be installed, which would shut the
   server down at once. Now a failed installation never resolves.
4. **`CHANGELOG.md` not updated.** It is outside the prompt's allowed paths (§7 scope).
   Suggested entry under *Unreleased / Added*: "`onshape-mcp healthcheck` subcommand;
   HTTP transports shut down gracefully on SIGTERM". `docs/src/` has no CLI reference,
   so I added none.
5. **`--ready` with the default URL** is covered only by a unit test
   (`ready_switches_default_path`), because a test cannot own port 8080. The integration
   test probes `/health/ready` through `--url`.
6. **`--ready` with `--url`** is rejected as conflicting (exit 2) instead of rewriting the
   path. The prompt only defines `--ready` as switching the *default* path.
7. **Timeouts above 10000 ms** are clamped silently. `0` is rejected with exit 2 because
   a zero socket timeout is invalid.

## Out-of-scope findings

- `/workspaces/clawonshape/docs/interface/onshape-mcp.md:48` (§1 "Healthcheck") still
  reads "Planned". It should be updated once this ships (architect).
- The stdio transport (`onshape_mcp_io::run`, `crates/onshape-mcp-io/src/lib.rs` ~L1818)
  is not part of this change. It ends on stdin EOF. SIGTERM uses the default disposition,
  which terminates the process when it is not PID 1. Not touched.
