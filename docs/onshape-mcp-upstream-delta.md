# Fork delta and upstream boundaries

Base: altendky/onshape-mcp f78ddc48a445765f32fcf052d9d8e6d709497d56,
workspace 0.5.3-dev.0, MIT OR Apache-2.0. Fork branch:
`edge-robot/onshape-mcp:claw-onshape/external-token`. No new dependencies.

| Feature | Motivation / generic usefulness | Tests | Security / compatibility | Suggested PR boundary |
|---|---|---|---|---|
| Existing external-token mode | Integrate with an external OAuth owner without sharing refresh authority | missing/expiry/rotation, unsafe files, unknown refresh fields, read containment | optional config; no OAuth, refresh, persistence or token adoption; Linux private file contract | external credentials (existing fork commits) |
| Bounded feature modification | Allow externally authorized clients to replace existing features in assigned workspaces | parsing/default deny; wrong document/workspace/element/operation/feature; exclusion; mandatory CAS; body preservation; dispatch denial before credentials | optional mutation section; typed scope; exact route admission; Primary ID supplied by operator; read-only defaults preserved; v16 writes from vendored catalog | target/revision policy and external execution integration |
| Single-attempt HTTP constructor | Prevent write replay through redirects or transport retry | real local HTTP 307 test | additive API; existing upstream constructor unchanged | client retry/redirect control (can be separate) |
| Ambiguous write reporting | Give callers a safe reconciliation signal | UNKNOWN structured error, no retry; execution boundary tests | transport errors, 408, redirects and 5xx treated as UNKNOWN; no workflow ledger; no implicit retry | accompany bounded mutations or separate error contract |
| MCP schema contract test | Prove actual tools/list contains accepted structured schemas | binary stdio protocol tools/list, explain/schema, OAuth-tool refusal | no source mirror, no extra export tool; schemas already complete server-side; optional outputSchema is not invented | protocol regression test |
| Format/lint normalization | Make existing fork pass required Rust checks | fmt and strict Clippy | formats previous fork additions; boxes larger auth variant; moves imports before statements; no unrelated architecture refactor | retain with affected fork patch |

The patch has no project IDs, feature names, Claw-Onshape package dependency,
workflow engine or VC framework. Review target scope extensibility and Linux-only
external credential I/O with upstream before proposing PRs. Non-external upstream
OAuth retry behavior is unchanged; bounded mutation safety is claimed only for
external-token execution. See [security semantics](target-policy.md).
