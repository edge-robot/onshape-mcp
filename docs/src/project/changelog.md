# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed

- External-token mode (contract 1.0.0-draft, expand step): `[external_token.mutation]`
  requires `mutation_approval_file` at startup (`target_configuration_invalid`);
  approval file v2 with target claims is admitted next to deprecated v1; the
  `workspace_id`, `feature_id` and `expected_microversion` pins are optional; a
  replayed `proposalId` is rejected with `mutation_approval_consumed`; reads of the
  claimed Candidate workspace are admitted while a v2 approval is active
- Upgraded to rmcp 3.1 and MCP 2026-07-28, including stateless discovery,
  MRTR-aware server responses, result discriminators, and cache hints; public
  Rust resource and content types use `Resource` and `ContentBlock`

### Added

- External-token goal grants (contract 1.1.0-draft §12): optional
  `goal_grant_file` and `dispatch_journal_dir` (required with the grant file);
  goal admission in contract order with `mutation_authority_conflict` when a grant
  and an active approval coexist; durable, append-only, fsynced dispatch journal
  per goal (budget, duplicate and uncertain-outcome blocking survive restarts);
  STOP re-check before the single dispatch; Candidate reads while a grant exists;
  `GET /capabilities` and `GET /goals/{goalId}/journal-summary`. New error codes
  `goal_grant_unavailable`, `mutation_authority_conflict`, `goal_grant_invalid`,
  `goal_grant_stopped`, `goal_scope_denied`, `goal_budget_exhausted`,
  `goal_reconciliation_required`, `goal_dispatch_duplicate`,
  `dispatch_journal_unavailable`
- Initial project documentation structure
- Generic API tools (`onshape_api_search`, `onshape_api_explain`, `onshape_api_call`) powered by embedded Onshape OpenAPI specification
- OpenAPI spec parsing module (`openapi.rs`) with search, explain, and request building
- Vendored Onshape OpenAPI spec (`crates/onshape-mcp-io/onshape-openapi.json`) embedded at compile time
- Effects-as-data pattern for `onshape_api_call` (`ToolResult::OnshapeApiRequest`)
