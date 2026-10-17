# Changelog

All notable changes to this project are documented here. The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project uses [semantic versioning](https://semver.org/).

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
