# Changelog

All notable changes to this project are documented here. The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project uses [semantic versioning](https://semver.org/).

## Unreleased

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
