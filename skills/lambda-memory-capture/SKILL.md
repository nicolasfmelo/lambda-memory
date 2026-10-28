---
name: lambda-memory-capture
description: Use when saving a decision or note in Lambda Memory, recording relevant work evidence, or publishing a source-backed revision or correction automatically.
---

# Lambda Memory capture and proposals

## Before you start

Use the `memory_capture` and `memory_propose` tools. In Claude Code they appear as `mcp__lambda-memory__memory_*`, in OMP as `mcp__lambda_memory_memory_*`. If they are missing, see the lambda-memory-runtime skill.

Every call must pass `project` exactly as shown in the tool description ("Current authorized project: …"). Owner and actor come from configuration and are never arguments.

Capture is local. Afterwards, idle processing (dreaming) turns sources into memories automatically. In `openrouter` mode that sends the captured text to models through OpenRouter, so never capture secrets, credentials or private data the user would not want sent. No human approval is needed per memory.

## Capture a source

1. Pick the smallest useful evidence. Do not paste whole logs, tool arguments or files.
2. Choose a `source` id that is stable for this event (for example `session-42/decision-1`). Ids are non-blank, have no control characters and are at most 512 UTF-8 bytes. Reusing an id with different content is a conflict; a correction is a new source.
3. `kind`: `attempt`, `result`, `error` or `decision` (`exchange` is reserved for the conversation-capture hooks). `outcome`: `unknown`, `succeeded` or `failed`. Use `unknown` for plans and decisions unless a result was observed. A failure stays a failure.
4. `origin` names where the evidence came from (at most 2048 bytes). `content` is at most the configured cap (default and maximum 8192 bytes). If it is too long, narrow it yourself; do not truncate silently.
5. Check the receipt: `saved_pending` or `already_saved_pending` for your source id. That confirms storage, not a published memory.

```json
{"name":"memory_capture","arguments":{"project":"project-demo","source":"session-demo/decision-1","kind":"decision","outcome":"unknown","origin":"user://explicit-decision","content":"Use Postgres for the project database."}}
```

With the OMP hooks extension, `/lambda-memory <text>` captures an explicit note. Ordinary tool calls are already recorded as small action episodes (`kind: "action"`, origin `omp://action` or `claude-code://action`), so do not capture them again under new ids and do not send `kind: "action"` yourself. Action episodes do not count toward the pending limit, never go through dreaming and only feed the project's playbook, which `memory_playbook` reads. The server rejects an episode that contains a likely secret.

In a project using the memory tree, a curator files each captured source into the tree: it merges it into the matching node, adds a new node, or skips it if nothing is worth keeping. Short, plain statements of a decision and its reason work best. You do not need to look for the right node yourself.

## Publish a claim right away

Use `memory_propose` when the user wants a specific claim available now instead of waiting for dreaming. Its 1–32 `sources` must already be captured in this project. Choose stable `memory` and `revision` ids.

```json
{"name":"memory_propose","arguments":{"project":"project-demo","memory":"database-choice","revision":"database-choice-r1","sources":["session-demo/decision-1"],"content":"Use Postgres for the project database."}}
```

For a correction, capture the new evidence, then propose a new revision of the same memory naming the current one as `predecessor`:

```json
{"name":"memory_capture","arguments":{"project":"project-demo","source":"session-demo/decision-2","kind":"decision","outcome":"unknown","origin":"user://explicit-correction","content":"Replace Postgres with SQLite for the project database."}}
```

```json
{"name":"memory_propose","arguments":{"project":"project-demo","memory":"database-choice","revision":"database-choice-r2","sources":["session-demo/decision-2"],"content":"Use SQLite for the project database.","predecessor":"database-choice-r1"}}
```

A successful proposal returns the revision as `active`; the predecessor becomes `superseded` and stays in history. Returned claims are always `provisional: true`.

## Errors

- JSON-RPC `-32602`: malformed arguments (unknown or missing keys, bad `kind`/`outcome`, ids over 512 bytes, 1–32 sources). Nothing was written.
- `isError: true`: content over the cap, origin over 2048 bytes, duplicate or unknown sources, or a conflicting replay.
- A timeout is indeterminate. Do not repeat the write under the same or a new id; check with `memory_status` or `memory_list` first.
- `diagnostics_warning.code: "final_record_unavailable"`: the operation result stands; only its history record is missing. Report it and do not repeat.

Report the source and revision ids and the confirmed state. Use `memory_status` to see pending or blocked processing. For optional rejection or conflicts, see the lambda-memory-review skill.
