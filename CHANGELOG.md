# Changelog

All notable changes to this project are documented here. The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project uses [semantic versioning](https://semver.org/).

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
