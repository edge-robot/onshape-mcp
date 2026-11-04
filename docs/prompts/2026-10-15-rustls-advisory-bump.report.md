---
prompt: docs/prompts/2026-10-15-rustls-advisory-bump.md
branch: claw-onshape/external-token
commits: 3ba95e0..3ba95e0
status: done
contract-version: 0.2.0-draft
e2e-request: required
---
# Report: Bump rustls past RUSTSEC-2026-0346..0349 (lockfile only)

## Summary

`cargo update -p rustls` updated `rustls` in `Cargo.lock` from `0.23.45` to `0.23.46`.
No other package changed, and no `Cargo.toml` was touched. `cargo deny check` now passes
in all sections. fmt, clippy, nextest and the contract tests are green. The new rustls
declares `rust-version = "1.71"`, which is within the workspace MSRV `1.89`. No code
changes were needed.

## Changed files

`git diff --stat HEAD~1` at `3ba95e0` (against the prompt commit `e3cbef4`):

```
 Cargo.lock | 4 ++--
 1 file changed, 2 insertions(+), 2 deletions(-)
```

- `Cargo.lock`: `rustls` version and checksum bump.

Changed packages (complete list):

| Package | Old | New | Kind |
|---|---|---|---|
| `rustls` | 0.23.45 | 0.23.46 | patch; transitive (via `reqwest`, `hyper-rustls`, …) |

`cargo update` output: `Locking 1 package to highest compatible version` /
`Updating rustls v0.23.45 -> v0.23.46`.

`git diff Cargo.lock` excerpt:
```
 name = "rustls"
-version = "0.23.45"
+version = "0.23.46"
 source = "registry+https://github.com/rust-lang/crates.io-index"
-checksum = "0d41d731c7d2f962d1ccc364cec258de3c0e93b38c2fb3ba97ac74513048d634"
+checksum = "48e13bd8c0e9365c43cfa5c9e8f9ad49d3c8444926c9aac819e0e4dc503c8fdf"
```

## Checks

| Command | Result |
|---|---|
| `cargo deny check advisories` (before, baseline) | **fail**: `advisories FAILED` |
| `cargo update -p rustls` | ok: 1 package updated |
| `cargo fmt --all --check` | pass |
| `cargo clippy --all-targets --all-features -- -D warnings` | pass |
| `cargo nextest run --all-features` | pass: 616 run, 616 passed, 0 skipped |
| `cargo nextest run -p onshape-mcp-core --test contract` | pass: 4 run, 4 passed, 0 skipped |
| `cargo deny check` (after, all sections) | pass: `advisories ok, bans ok, licenses ok, sources ok` |
| `python scripts/apply-openapi-overlays.py --check` | not run: OpenAPI and overlays untouched |
| `mdbook build docs` | not run: `docs/src/` untouched |

Advisories before (`cargo deny check advisories`, all against `rustls 0.23.45`):
- RUSTSEC-2026-0346
- RUSTSEC-2026-0347
- RUSTSEC-2026-0348
- RUSTSEC-2026-0349

Advisories after: none (0 `error[...]` entries in `cargo deny check`).

The remaining `cargo deny` warnings are unchanged from before:
- 14 `warning[duplicate]`: `base64`, `block-buffer`, `cpufeatures`, `crypto-common`,
  `digest`, `getrandom`, `rand`, `rand_core`, `sha2`, `syn`, `thiserror`,
  `thiserror-impl`, `tower-http`, `windows-sys`.
- 2 `warning[license-not-encountered]`: `CDLA-Permissive-2.0`, `OpenSSL` in `deny.toml`.

These are warnings only and not failures. `deny.toml` is unchanged.

## Acceptance criteria

| # | Status | Evidence |
|---|---|---|
| R1 | met | `Cargo.lock`: `rustls` `0.23.45` → `0.23.46` (diff above) |
| R2 | met | `git diff --stat HEAD~1` shows only `Cargo.lock` (2+/2-). The only changed package is `rustls`, so no direct dependency and no `ring`/`aws-lc-rs`/`rustls-*`/`webpki*`/`hyper*`/`reqwest` change. |
| R3 | met | `advisories ok, bans ok, licenses ok, sources ok` (exit 0) |
| R4 | met | fmt pass. clippy `-D warnings` pass. nextest `--all-features`: 616/616 passed. |
| R5 | met | `rust-version = "1.71"`, from `/usr/local/cargo/registry/src/index.crates.io-1949cf8c6b5b557f/rustls-0.23.46/Cargo.toml:14`. `cargo metadata` agrees: `rustls 0.23.46 1.71`. 1.71 ≤ 1.89. |
| R6 | met | `-p onshape-mcp-core --test contract`: 4/4 passed |

## Contract deviations and change requests

None. No code, contract or fixture change.

## Live E2E request

Preconditions: rebuild the `clawonshape-mcp` image from `claw-onshape/external-token` at
`3ba95e0` or later, with the same config as the G1 E2E (`[external_token]`, Streamable HTTP).

- Run an allowed external-token read (§2.4), e.g.
  `onshape_api_call GET /documents/d/{d}/w/{w}/currentmicroversion` on the configured
  target.
- Expected: a successful TLS handshake to `cad.onshape.com` and a 2xx JSON response. There
  should be no `external_token_request_failed`.
- The architect bundles this check with the G1 live E2E.

## Open points and risks

- `rustls 0.23.46` is a patch release with security fixes. The local test suite does not
  exercise real TLS to `cad.onshape.com`, which needs network and credentials. So the
  live E2E above is the only proof of the TLS path.
- `CARGO_HOME` in the devcontainer is `/usr/local/cargo`, not `~/.cargo`. The prompt's
  hint path `~/.cargo/registry/src/...` does not exist there.

## Out-of-scope findings

- `cargo update` noted `17 unchanged dependencies behind latest`. I did not inspect or
  update them, as instructed.
