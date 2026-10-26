# Onshape MCP product specification

This is the sole normative source of this repository's product requirements.
It defines intended generic MCP/CAD behavior and the restricted ClawOnshape
integration. It is not a runtime availability report or authorization to execute
CAD writes. Technical/development references describe their own contracts and
conventions without redefining product scope. Historical checklists prove only
the evidence recorded at the time.

## Purpose and boundaries

Let an MCP client discover, understand and execute Onshape API operations in a
structured way. Users describe CAD intent; their assistant resolves endpoints,
reads schemas, performs permitted calls and returns useful observations/artifacts.
The server helps people who know Onshape; it does not replace engineering judgment.
Generic API execution must not depend on pib, a plate parameter or a team role.

The server owns generic CAD execution and hard admission checks. ClawOnshape owns
product team policy, identity/confirmation verification, OAuth in external-token
mode, Candidate/goal orchestration, independent result verification and delivery.
Clawake owns declarative deployment, pins, configuration reconciliation, private
mounts and service lifecycle. The MCP server owns none of these product roles.
It does not implement a team dashboard, Discord/A2A dialogue, Bambu Studio,
print-agent orchestration, production approval or physical print start.

## Standalone use and distribution

Preserve upstream standalone modes alongside the optional restricted integration:

- Local stdio is the primary, recommended transport for compatible MCP clients.
  Support both a native binary and the npm/Node launcher for the Rust application.
- Preserve generic document/part creation, feature operations, FeatureScript
  work, screenshots and export workflows where the API and mode permit them.
  API discoverability is not a promise that every endpoint has been accepted.
- Standalone authentication supports API keys and direct OAuth using a user's
  Onshape application. Preserve login, token storage, refresh and file-change
  handling in their supported modes. Optional OAuth-proxy software is self-hosted
  and explicitly selected; no public proxy service is promised.
- Experimental self-hosted Streamable HTTP remains available, with authentication
  appropriate to that mode. No public hosted MCP endpoint is offered. Client
  interoperability requires explicit acceptance; do not promise universal support.
- Local file inputs/outputs, screenshots and exports remain useful standalone
  capabilities. HTTP disables local MCP file reads/writes; external-token mode
  has additional restrictions. Neither remotely accesses a client's filesystem.
- Support Linux x86_64/aarch64, macOS x86_64/aarch64 and Windows x86_64.
  Windows aarch64 is deferred. Abstract platform-specific behavior and compile
  for all required targets; additional platforms are not prohibited.
- Preserve binary/npm distribution and MIT OR Apache-2.0 licensing. Installation,
  configuration and authentication guidance remain in the [README](README.md).

The restricted ClawOnshape mode must not silently remove or weaken standalone
obligations, nor inherit standalone privileges merely because tools share code.
Detailed platform/toolchain conventions stay in
[development requirements](docs/src/project/requirements.md).

## Discovery, schemas and execution

Clients can search API operations, inspect an endpoint's parameters and response
shape, and resolve schema definitions needed to construct valid requests.
Discovery must expose enough information to invoke tools through a native MCP
client; names-only catalogs are not evidence of usable argument schemas.
Schema references and polymorphic types must remain understandable to callers.

Execution builds requests from the API description, validates arguments and
mode-specific constraints, uses only permitted credentials and returns results
or actionable errors. Discovery never expands execution authority. Generic calls
and convenience tools must pass the applicable admission boundary consistently.
Do not silently alter a payload to manufacture valid authorization or revision
protection. API catalog/provider version compatibility needs explicit acceptance.

Detailed tools, request formats and errors stay in
[tool reference](docs/src/project/mcp-tools.md) and the exact
[ClawOnshape execution interface contract](https://github.com/edge-robot/clawonshape/blob/feat/native-plugin-foundation/docs/interface/onshape-mcp.md),
including its fixtures, versioning and goal-authorization section (§12).
That contract governs the restricted integration, not all standalone transports.

## CAD reads and artifact capabilities

Required generic capabilities include document, workspace, element, part,
feature/evaluation and revision reads sufficient to identify the requested
model and explain observed state. Preserve explicit source revision identity;
mutable current state and an immutable revision must not be conflated.

Revision-bound rendering and export, including STL, are target capabilities.
Bind the result to exact document, revision, element and part selection. Retain
units, export/render parameters, file identity/integrity and useful dimension
metadata where available. Later CAD changes must not silently replace the source
of an approved artifact. Unsupported reads/formats produce a clear limitation.
A screenshot or successful export alone proves neither dimensions nor physical
fitness. ClawOnshape decides product approval and human delivery requirements.

Existing standalone screenshots/exports do not establish accepted revision-bound
render/export access in external-token mode. Extend the technical contract and
accept the constrained route before exposing such operations there; do not bypass
current read/file/tool restrictions to satisfy this target specification.

## Restricted ClawOnshape integration

External-token mode consumes access-token metadata supplied by ClawOnshape, the
single OAuth/refresh owner for this integration. The server must not initiate
login, refresh, merge legacy credentials or run a competing token lifecycle.
Reload and validate the restricted credential handoff on dispatch; absent,
expired, malformed or insecure credentials fail closed without leaking secrets.
Private transport membership is a trust boundary, not proof of team identity.

Default execution is bounded read-only. Candidate edits require explicit static
bounds plus valid external mutation authority. Before provider I/O reject wrong
model/element/workspace, protected Primary, denied operation/feature, missing
scope, expired/revoked authority and stale/missing revision conditions. Primary
exclusions use authoritative immutable IDs supplied by the control plane, not
workspace names. A token's write scope alone cannot authorize a mutation.

Initially permit only authorized existing-feature updates. Require the expected
source microversion and provider skew rejection on each update. Match the request
target and feature to the admitted authority; reject unsupported operations,
request injection and unrestricted generic-call escape routes. New operations
need an audited contract extension; automatic Primary promotion is not supported.

Support the exact-payload approval path during migration and the accepted bounded
goal-grant path without simultaneous competing authorities. ClawOnshape verifies
the human goal confirmation and supplies the grant. The Engineer can author
multiple different permitted updates under that grant in the same Candidate;
a different payload alone is not a new goal or a reason for human reapproval.
The server enforces scope, revision, state, expiry and dispatch budget; it does
not judge goal semantics, geometry preservation or production suitability.
STOP prevents further admission; an admitted in-flight request remains accounted
for. Resume after STOP/expiry requires renewed external confirmation/authority.
Missing support or inaccessible durable state blocks the goal path.

Use a durable private dispatch journal for goals so restarts do not reset budgets,
uncertainty or duplicate protection. Honor the accepted default 10 dispatches/two
hours and hard caps of 25/eight hours as specified in the interface. Do not
introduce transport retries, redirects or refresh-and-replay for restricted
writes. Persist uncertainty when an admitted write's outcome cannot be established;
block subsequent goal dispatches until control-plane reconciliation is recorded.
Never treat revision CAS alone as a durable attempt ledger. Reconciliation does
not prove correctness and must not blindly replay the uncertain request.

## Status and acceptance

Distinguish liveness, credential readiness, configured capabilities, effective
admission and successful provider execution. Status and errors identify missing
setup, expired credentials, denied policy, revision conflict, stop, exhausted
budget and unknown outcome without secret-bearing output. Available tools or
healthy processes do not prove the complete product journey works.

Acceptance must demonstrate discovery through schema-based invocation, supported
standalone authentication/transports/platforms, exact-target model/feature/revision
reads and useful artifacts in the applicable mode. Revision-bound render/STL and
their provenance require dedicated positive and negative acceptance before being
claimed available in the restricted integration.

For restricted writes demonstrate an authorized Candidate change and independent
consumer verification, plus rejection of wrong targets, Primary, wrong feature,
scope/revision mismatch, missing/expired/revoked authority and conflicting
approval sources. Demonstrate two permitted updates under one goal, budget/expiry,
STOP including an in-flight request, restart persistence, duplicate denial and
uncertain-outcome reconciliation. No implicit live CAD authorization follows.

Current code, historical reports and phase checkmarks do not establish current
runtime availability or complete acceptance. Consult the
[technical roadmap](docs/src/project/implementation.md),
[target-policy reference](docs/target-policy.md),
[goal workflow contract](https://github.com/edge-robot/clawonshape/blob/feat/native-plugin-foundation/docs/interface/goal-workflow.md) and
[accepted goal ADR](https://github.com/edge-robot/clawonshape/blob/feat/native-plugin-foundation/docs/ADR.md#adr-2026-10-21-goal-scoped-mutation-authorization)
for implementation context and unresolved integration questions. Product rollout
and full journey acceptance remain with ClawOnshape; generic executor acceptance
remains with this component.
