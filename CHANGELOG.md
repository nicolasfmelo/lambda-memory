# Changelog

All notable changes to this project are documented here. The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project uses [semantic versioning](https://semver.org/).

## Unreleased

## 0.4.0 — 2026-10-28

### Added

- **Memory tree.** Each project's knowledge is a curated tree with four fixed roots (Decisions, Preferences, Architecture, Open items) and emergent contexts below them. One Haiku call per idle batch decides to discard, merge into an existing node (contradictions rewrite the current position and keep the earlier one in the lineage), create a child, or expand a node first. Raw sources are never embedded; only curated node summaries are. Refresh, split and rebuild keep the tree coherent. See "Memory tree" in the reference.
- Tree-mode recall descends from broad to specific nodes (at most two Luna calls) and returns each hit's `node` and `path`.
- `memory_node`, a read-only MCP tool that returns a node with its path, children, version history and the linked original sources.
- Operator migration of flat memories into the tree: `recover-pending.mjs inspect-tree|curate-existing` and `migrate-all-trees.mjs plan|summary|apply`, with an explicit charge consent. `plan` accepts `--only`/`--exclude`, orders projects cheapest first and skips registry entries whose directory is gone or now binds to another identity. A resumed `apply` skips projects already in tree mode and never curates a handled source twice.
- [docs/upgrade-and-migrate.md](docs/upgrade-and-migrate.md), step-by-step upgrade and migration instructions written for coding agents, linked from the README.
- `exchange` knowledge sources and opt-in conversation capture (`captureConversation: true`): Claude Code and Codex `UserPromptSubmit`/`Stop` hooks record each sanitized prompt and final reply for the curator.
- A harness-agnostic hook core (`runtime/agent-hook.mjs <harness> <mode>`) with adapters for Claude Code and Codex. Codex gets action capture and playbook injection. `claude-code-hook.mjs` remains as a shim.
- Tree bounds in `runtimeEnv`: `LAMBDA_MEMORY_TREE_MAX_CHILDREN`, `_TREE_MAX_DEPTH`, `_TREE_SPLIT_SOURCES`, `_TREE_MAP_BYTES` and `_CURATION_BATCH`.
- `LAMBDA_MEMORY_ROUTER_BASE_URL`, an optional base URL for the openrouter mode (default `https://openrouter.ai`). It must be `https://host[:port]`, or `http://` on a loopback literal with an explicit port, with no userinfo, path, query or fragment; an invalid value fails startup. The bearer stays `OPEN_ROUTER_KEY` and carries the token the endpoint expects. The daemon's provider env file accepts the variable; clients never receive it.

### Changed

- The playbook counts `unknown` outcomes (Codex hooks report no exit code) as unverified uses for procedures and hot files, never as successes or pitfalls. `apply_patch` counts as an edit.
- Flat 1:1 derivation applies only to projects that have not been migrated to the tree. Flat recall never returns tree nodes.
- `buildEpisode` takes normalized tool calls. Harness tool names live in `runtime/hook-adapters/`.
- Curator answers are handled defensively: unused fields are ignored, swapped node/parent targets are resolved, titles and summaries are fitted to their bounds, several merges into one node are combined, and a single unusable operation (empty or secret-bearing text, depth bound) is dropped and counted as `operations_dropped` instead of rejecting the batch. A completed but unusable answer is recorded with a specific reason (`provider_invalid_output`, `tree_*`) and no longer stops background work.

## 0.3.1 — 2026-10-26

### Fixed

- The Claude Code hook records tool failures, delivered by the `PostToolUseFailure` event; register it next to `PostToolUse`.

## 0.3.0 — 2026-10-26

### Added

- Procedural memory. `memory_capture` accepts `kind: "action"`: a small sanitized episode per tool call (tool, intent, summary, error head). Episodes never enter derivation and do not count toward the pending backlog.
- A per-project playbook, generated while idle from the most recent 5,000 episodes. Facts are aggregated deterministically, one Haiku call phrases them, and a validator requires every bullet to cite a fact. The playbook is stored as the fixed memory `procedural:playbook` and is excluded from recall and indexing.
- `memory_playbook`, a read-only MCP tool that returns the current playbook.
- The OMP extension injects the playbook once per session and again after a context compaction, and records action episodes (`omp://action`) instead of per-tool-result metadata.
- Claude Code hooks (`runtime/claude-code-hook.mjs`): `SessionStart` injects the playbook, `PostToolUse` records an action episode. A hook never blocks a turn.
- A one-time history importer (`runtime/import-actions.mjs`) that rebuilds episodes from OMP and Claude Code transcripts. It is a dry run by default and idempotent.
- Operator command `--reject-tool-metadata [--apply]`, which retires active memories derived only from old per-tool-result metadata. Sources are kept.

### Changed

- Recall selection is per candidate. The selector's best choice only orders the result; every candidate that passes the relevance, utility and direct-support thresholds is returned. An abstention still returns nothing.
- Query planning and query embedding run concurrently, about 0.8 s less per recall.
- The query planner accepts JSON wrapped in a markdown fence and keeps the first eight valid terms. If its output is still unusable, recall continues with vector search only and records `provider_invalid_output`. Other provider errors still fail closed, without retry.

### Fixed

- Action captures were refused with `BacklogFull` when the pending backlog was full.
- Capture latency grew with the size of the project. The backlog count and activity links now use indexes, and an action capture stays at a few milliseconds.
- Reading the playbook no longer waits behind the store's writer lock.

## 0.2.0 — 2026-10-21

### Changed

- **Breaking:** `memory_recall` takes `budget_bytes` instead of `budget_tokens`, and `LAMBDA_MEMORY_CONTEXT_BYTES` (default 8192, maximum 32768) replaces `LAMBDA_MEMORY_CONTEXT_TOKENS`. The old names are rejected. Update the native binary and the JavaScript bundle together.
- Recall can return several independently supported memories from the project, strongest first, instead of only one.
- Gemini embeddings default to 1536 dimensions (`LAMBDA_MEMORY_EMBEDDING_DIMENSIONS`, 128–3072) with separate query and document prefixes, in the new space `runtime-gemini-retrieval-v2`. Existing vectors are not reused; prepare the new index after upgrading.
- Reading-first web UI: full text first, technical details in collapsible sections, each state explained, Auto/Light/Dark themes.

### Added

- Embedding maintenance: inspect and prepare the index for the current space, and recover interrupted embedding calls with explicit consent (`recover-pending.mjs inspect-index|prepare-index|inspect-embeddings|recover-embeddings` and the matching native flags).

### Fixed

- A new revision whose predecessor was rejected no longer blocks recall.
- Embedding admission identity includes the dimensions, so changing them no longer collides with old markers.
- The OMP extension keeps its recall slot busy until the backend answers, so a late result is never injected into a later prompt.
- Pending captures are drained on shutdown or project change; dropped or refused captures produce a warning.
- The project registry stays within 256 entries and 256 KiB by dropping the oldest entries.

## 0.1.0 — 2026-10-17

Initial public release.

### Added

- Native Rust binary with an embedded SurrealDB (SurrealKV) store per project, serving MCP over stdio.
- Ten MCP tools: `memory_capture`, `memory_propose`, `memory_recall`, `memory_list`, `memory_review`, `memory_timeline_write`, `memory_timeline`, `memory_status`, `memory_activity` and `memory_purge`.
- Immutable sources, versioned revisions with lineage, automatic publication and optional conflict handling.
- Idle dreaming (default after 60 s, batches of 8) that turns sources into source-bound memories.
- `openrouter` mode with pinned models (Claude Haiku 5.5, Gemini Embedding 2 at 128 dimensions, GPT-6 Luna Decisions) and an offline `local` mode.
- Hybrid lexical and vector recall with source attribution and a bounded context budget.
- Node local service (systemd user unit) with one worker per project, and the `lambda-memory` stdio bridge for MCP clients.
- Project binding from the git remote.
- Optional loopback web UI for browsing and curating memories, the decision timeline and purge.
- OMP hooks extension for automatic recall and capture.
- Six agent skills: recall, capture, review, timeline, purge and runtime.
- Recovery helper for sources blocked by an uncertain paid call.
