---
name: lambda-memory-capture
description: Use when saving a decision or note in Lambda Memory, recording relevant work evidence, or publishing a source-backed revision or correction automatically.
---

# Lambda Memory capture and proposals

## Before you start

Use `memory_capture` and `memory_propose` (`mcp__lambda-memory__memory_*` in Claude Code, `mcp__lambda_memory_memory_*` in OMP). If missing, see the [runtime skill](../lambda-memory-runtime/SKILL.md) for authenticated access. Never open a second native process on an occupied store; the installed bridge shares the worker safely.

Pass the authorized `project` from the tool description. Owner/actor come from configuration, never arguments. Stay within the approved capture/provider boundary; this does not authorize scanning private files or widening scope.

Capture stores evidence locally. Idle processing curates, embeds and publishes automatically, with no approval per memory. Tree curation can merge, create or discard it; short decisions and reasons work best. In OpenRouter mode the text goes to Haiku/Gemini and costs money, with no spending cap. Never capture secrets. A capture receipt confirms storage, not publication or truth. The main agent decides whether to use recalled evidence.

## Capture a source

Installed Claude Code, Codex and OMP conversation capture defaults on (`captureConversation` absent or `true`); `false` opts out. Sanitized main-agent prompts/replies become exchange sources and add provider curation calls. Use explicit notes for emphasis, not to duplicate every captured turn.

1. Pick the smallest useful evidence. Remove secrets/private details; do not paste whole logs, tool arguments, provider bodies or files.
2. Choose a stable event `source` ID, non-blank, control-free, at most 512 UTF-8 bytes. Reusing it with changed project, actor, origin, kind, outcome or content conflicts. Corrections use new sources.
3. `kind`: `attempt`, `result`, `error` or `decision`; `outcome`: `unknown`, `succeeded` or `failed`. Plans/decisions are normally unknown. A tool's successful exit does not prove unobserved downstream success; failure stays failure. Exchanges and lessons are produced by integrations, not manual lesson capture.
4. Give the real `origin` (at most 2048 bytes) and concise `content` within the configured cap (default/maximum 8192 UTF-8 bytes). Never imitate the UI's `ui://correction` origin. Narrow oversized evidence explicitly, not by silent truncation.
5. Check the matching source receipt: `saved_pending` or `already_saved_pending`. It is stored evidence, not yet active knowledge.

```json
{"name":"memory_capture","arguments":{"project":"demo-project","source":"session-demo/decision-1","kind":"decision","outcome":"unknown","origin":"user://explicit-decision","content":"Use Postgres for the project database."}}
```

For a general cross-project user preference or working rule, add `scope: "user"` with the current project. Never use it for project facts. Only the shared service supports this; direct native capture rejects `scope`. Project curation can also promote general user knowledge automatically.

OMP's `/lambda-memory <text>` captures an explicit note. Hooks already record sanitized action episodes (`kind: "action"`, origins such as `omp://action` and `claude-code://action`). Do not duplicate them under new IDs or send action JSON yourself. Actions do not enter ordinary curation or count toward pending capacity; their digest and exchanges feed automatic lessons. The server rejects secret-bearing episodes.

## Publish a claim right away

Use `memory_propose` for a specific source-backed claim/correction rather than waiting for idle consolidation. Supply 1–32 distinct sources already stored for this owner/project and stable memory/revision IDs. The server loads evidence; arguments cannot invent snapshots or identities.

```json
{"name":"memory_propose","arguments":{"project":"demo-project","memory":"database-choice","revision":"database-choice-r1","sources":["session-demo/decision-1"],"content":"Use Postgres for the project database."}}
```

For a correction, capture new evidence, keep the memory ID, and name the exact predecessor:

```json
{"name":"memory_capture","arguments":{"project":"demo-project","source":"session-demo/decision-2","kind":"decision","outcome":"unknown","origin":"user://explicit-correction","content":"Replace Postgres with SQLite for the project database."}}
```

```json
{"name":"memory_propose","arguments":{"project":"demo-project","memory":"database-choice","revision":"database-choice-r2","sources":["session-demo/decision-2"],"content":"Use SQLite for the project database.","predecessor":"database-choice-r1"}}
```

Check project, revision, sources and `active` state on successful preparation of a fresh proposal. A replay may return a later state. Indexing/publication are automatic, without review. Publication atomically supersedes the predecessor, keeping originals and lineage. Claims remain `provisional: true`. A root proposal creates knowledge, not a procedure; procedure corrections need successful evidence and must keep prerequisites.

## Errors and reporting

- JSON-RPC `-32602`: bad keys/types, kind/outcome, IDs over 512 bytes or source count outside 1–32. Nothing written.
- `isError: true`: policy/size failures, origins over 2048 bytes, duplicate/unknown sources or conflicting replay.
- Timeout, missing or mismatched receipt: persistence is unconfirmed. Do not automatically repeat under the same or a new ID. Inspect state first. Backlog rejection does not authorize eviction, raised limits or unrelated purge.
- `diagnostics_warning.code: "final_record_unavailable"`: the known result stands; only bookkeeping failed. Report it without repeating.

Report exact source/revision IDs and confirmed state. Availability still depends on recall relevance/scope/budget. `memory_status` and `memory_activity` inspect pending/blocked work and stages without models or idle postponement; their seven-day history is not an all-time inventory. Successful processing never relabels a failed original. The [review skill](../lambda-memory-review/SKILL.md) is for optional maintenance, not normal activation.
