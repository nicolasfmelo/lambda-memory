# Architecture and runtime contract

This page describes how λambda memory stores, processes and retrieves evidence. Settings, limits and operational commands are in the [reference](reference.md).

## Pipeline

```
capture → immutable source → dreaming (idle) → revision (processing)
        → embedding → published revision (active) → recall → bounded context
```

- A **source** is an immutable record of one work event: project, caller-chosen source id, kind (`attempt`, `result`, `error`, `decision`), outcome (`unknown`, `succeeded`, `failed`), origin and content.
- **Dreaming** is the background step that runs when a project has been idle (default 60 s). It takes up to 8 pending sources per batch and turns each into one short claim, a **revision** that cites its source.
- A revision starts as `processing`, is embedded and is then published as `active`. `active` means retrievable, not verified. Every returned claim carries `provisional: true`; the calling agent decides whether to use it.
- **Recall** searches the current project's active revisions and returns a small context in which every claim names its sources.

## Store

Each project has its own embedded SurrealDB store (SurrealKV engine, SurrealDB 3.3.3) inside one native process. There is no database server. The schema is in [`migrations/`](../migrations/) and is compiled into the binary.

- Sources are never edited. Capturing the same source id again with identical content is a replay; with different content it is a conflict.
- Revisions are immutable. A correction is a new revision of the same memory that names its predecessor; publication replaces the predecessor atomically, using it as a compare-and-swap guard. An unfinished successor never hides its active predecessor.
- Lifecycle state (`processing`, `active`, `rejected`, `conflicted`, `superseded`) is stored separately from revision content, with an audit trail per transition.
- Vectors are bound to owner, project, revision and the exact provider, model and dimensions. A different embedding space is never compared.

## Dreaming

- Ordinary tool calls (capture, recall, list, review, purge, timeline) postpone the next batch. `memory_status`, `memory_activity` and `memory_timeline` do not.
- In `openrouter` mode the batch goes to `anthropic/claude-haiku-5.5` with a strict JSON schema that has one required slot per source. The native adapter maps slots back to source ids and rejects missing, extra, empty, oversized or non-string claims. A malformed response is a failure; validation is never relaxed.
- In `local` mode the claim is the exact source text. No model is called.
- Consuming sources and writing revisions happen in one transaction. A restart after commit finishes embedding and publication without calling the model again.
- Before each paid batch the store records an admission marker per source. If the outcome of a call is uncertain (timeout, crash), those sources are **blocked**: they are not sent again automatically, so a call is never billed twice by accident. Blocked sources do not stop other pending sources. See [recovering blocked sources](reference.md#recovering-blocked-sources).
- A refusal before any billable work (network down, HTTP 401/402/403/429) releases the admission; the sources stay pending and the worker backs off. A new provider failure stops further provider work in that worker while the MCP tools keep serving.

## Recall

In `openrouter` mode:

1. Finish a bounded amount of pending preparation (at most four `processing` revisions and four missing embeddings per call). If more remains, recall returns an explicit "catch-up incomplete" error instead of partial results.
2. Haiku plans the query; `google/gemini-embedding-2` embeds it (128 dimensions).
3. Lexical and vector candidates are retrieved from the current project and merged with reciprocal-rank fusion (at most 8 candidates).
4. `openai/gpt-6-luna-decisions` (OpenRouter `POST /api/alpha/decisions`, typed `noul`, `choice` and `score` questions) keeps only relevant candidates.
5. Native code packs the selected claims and their sources into the context budget. A claim that does not fit is excerpted and the result reports `truncated: true`.

The result reports `mode: "openrouter_semantic"`. In `local` mode recall uses normalized substring terms only and reports `local_lexical`. Local mode is never used as a fallback for provider errors.

Recall is not cached. Scope, state and source timestamps are re-checked after every model call, so a purge or correction that lands mid-recall is respected.

## Provider boundary

- Models are pinned. `LAMBDA_PLANNER_MODEL`, `LAMBDA_EMBEDDING_MODEL` and `LAMBDA_SELECTOR_MODEL` may only repeat the pinned ids; any other value is rejected.
- The key is read from `OPEN_ROUTER_KEY`. It never appears in logs, errors, tool results or URLs. Provider errors are reported as a classified status, not the provider's response body.
- Redirects and proxy discovery are disabled; request and response sizes and timeouts are bounded.
- There is no spending cap and no automatic retry of a paid request.
- Model-generated queries never reach the database as SurrealQL; the planner returns typed terms that native code binds as parameters.

## MCP contract

- Transport: newline-delimited JSON-RPC 2.0 over stdio. Requests up to 64 KiB, responses up to 1 MiB, four queued input frames. Supported protocol versions: 2025-11-25, 2025-06-18 and 2024-11-05.
- Authentication: the native server requires `params._meta.lambdaMemoryToken` in `initialize` and closes the session on a mismatch. `notifications/initialized` must follow before any tool call. The bridge adds the token for its clients.
- Identity: owner and actor come from configuration, never from tool arguments. Every call names `project`; `memory_recall` takes `projects`. A call for a project that is not authorized fails.
- Arguments are strict: unknown keys, missing keys or wrong types return JSON-RPC `-32602` with the reason and nothing is written. Domain failures (size limits, unknown ids, version conflicts) return a tool result with `isError: true`.
- Results are a text content block containing JSON. Tools carry `readOnlyHint` and `destructiveHint` annotations so hosts can ask before writes and purges.

## OMP hooks

[`extensions/lambda-memory-local.mjs`](../extensions/lambda-memory-local.mjs) is the hooks adapter for the shared service. It does not register tools; the OMP MCP manager does that through the bridge.

- Before each main-agent turn it recalls for the current project and injects non-empty context as untrusted evidence. The whole step has a 25-second deadline (below OMP's 30-second handler limit); on expiry the turn proceeds without memory.
- After `edit`, `write`, `bash`, `eval` and `task` results, or a failed ordinary tool, it captures safe metadata only: never arguments, output or file contents.
- `/lambda-memory <text>` captures an explicit note.
- Subagents do not capture, and their memory tool calls are blocked. A call that names another project is blocked, as is every call after the session leaves its project.

[`extensions/lambda-memory.mjs`](../extensions/lambda-memory.mjs) is the underlying client and a standalone adapter that launches its own native process. Load one adapter per session, never both.

## Layers

The code follows the [MASA framework](https://www.masa-framework.org/): `domain_models` (types), `engines` (pure rules, no I/O), `integrations` (store, HTTP), `services` (orchestration) and `delivery` (MCP and CLI), with one-way dependencies. See [MASA.md](../MASA.md); `scripts/check_layers.py` checks the boundaries.

Background reading: [memory architecture research](research/memory-architecture.md).
