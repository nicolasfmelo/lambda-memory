---
name: lambda-memory-timeline
description: Use when reading the automatic decision timeline or manually correcting, recording or retracting a decision outside captured work.
---

# Lambda Memory decision timeline

## Preconditions

Use `memory_timeline` and `memory_timeline_write`; if missing, see the [runtime skill](../lambda-memory-runtime/SKILL.md). Pass the authorized project. Never open another native process on an occupied store.

Published decision positions create adopted entries automatically. Supersede marks the old position superseded and opens a new one; retirement retracts its adopted entry. Auto IDs are `auto:<memory>:<revision>`, tagged `origin-auto`. Manual writes cannot create new `auto:` IDs, strip the tag, and detach edited auto entries. Use manual writes for corrections or decisions outside captured work, not routine transcription of uncovered previews.

Cited sources must already be stored for this owner/project; capture first (see [capture](../lambda-memory-capture/SKILL.md)). Linked revisions/memories must exist in the project. Writing is local, without model calls. Do not copy secrets/private details. Timeline records are curated, not verified truth, and are not served by `memory_recall`.

## Read first

`memory_timeline` is read-only: no operation record or idle postponement. Optional arguments:

- `limit`: 1–32, default 32;
- `statuses`: 1–5 distinct values from `proposed`, `adopted`, `superseded`, `reverted`, `retracted`;
- `order`: `asc` by default, or `desc`; sorted by `(occurred_at_ms, entry)`;
- `entry` or `cursor` (at most 4096 bytes).

```json
{"name":"memory_timeline","arguments":{"project":"demo-project","limit":32}}
```

The first page (no cursor/entry) includes `uncovered_decisions`: decision sources not cited by any entry's latest version, even if retracted. At most 16, newest first, with `source`, `origin`, `recorded_at_ms`, `outcome`, `preview` (≤512 bytes), `preview_truncated`; `uncovered_truncated` means more exist. This is not a write queue: automatic publication handles captured positions. `memory_node` can show original text; do not invent missing evidence.

Continue `next_cursor` with the same project/statuses/order. Wrong-filter cursors return `isError: true`, malformed ones `-32602`. `truncated` means the 1 MiB response budget cut the page; continue the cursor.

`entry` returns ascending version history and cannot combine with statuses/order. Unknown entries return an empty page. Read the head before editing:

```json
{"name":"memory_timeline","arguments":{"project":"demo-project","entry":"database-choice"}}
```

## Write

1. Choose a stable, non-blank, control-free entry ID, at most 512 UTF-8 bytes. Reuse it for edits, not new decisions.
2. `occurred_at_ms` is decision time; server-assigned `recorded_at_ms` is recording time. Decision time must be between 946684800000 and now plus 86400000.
3. `title`: non-empty/control-free, ≤200 bytes. `summary`: non-blank, within the configured content cap. `status`: one of the five values above.
4. `sources`: required, 1–32 distinct stored IDs. Optional `revisions`/`memories`: 0–32 distinct existing IDs each. `tags`: 0–16 distinct strings matching `^[a-z0-9][a-z0-9._-]{0,63}$`. Keys are strict; owner/actor are never arguments.
5. Omit `expected_version` on first write; expect `write: "recorded"`, `version: 1`.

```json
{"name":"memory_timeline_write","arguments":{"project":"demo-project","entry":"database-choice","title":"Use Postgres","summary":"Chose Postgres for the project database.","occurred_at_ms":1760600000000,"status":"adopted","sources":["session-demo/decision-1"],"memories":["database-choice"],"tags":["database"]}}
```

To edit, resend all fields and the head's `expected_version`. Omitting it on an existing entry conflicts unless the request exactly repeats the head:

```json
{"name":"memory_timeline_write","arguments":{"project":"demo-project","entry":"database-choice","title":"Use Postgres","summary":"Chose Postgres for the project database; managed hosting confirmed.","occurred_at_ms":1760600000000,"status":"adopted","sources":["session-demo/decision-1"],"memories":["database-choice"],"tags":["database"],"expected_version":1}}
```

Edits append versions. Check returned entry, version and status.

## Supersede and retract

Write a new entry with `supersedes` naming the old entry (bare ID, not itself):

```json
{"name":"memory_timeline_write","arguments":{"project":"demo-project","entry":"database-choice-sqlite","title":"Use SQLite","summary":"Replaced Postgres with SQLite for the project database.","occurred_at_ms":1760700000000,"status":"adopted","sources":["session-demo/decision-2"],"supersedes":"database-choice"}}
```

Then version the old entry as superseded, repeating its fields:

```json
{"name":"memory_timeline_write","arguments":{"project":"demo-project","entry":"database-choice","title":"Use Postgres","summary":"Chose Postgres for the project database.","occurred_at_ms":1760600000000,"status":"superseded","sources":["session-demo/decision-1"],"memories":["database-choice"],"tags":["database"],"expected_version":2}}
```

Use `reverted` for an undone decision without replacement. Use `retracted` for an incorrect/accidental record. Neither deletes history or source coverage. Only purge/retention of cited sources or cascaded revisions deletes whole entries (see [purge](../lambda-memory-purge/SKILL.md)). Surviving `supersedes`/`memories` links may dangle. Later versions can resend unchanged dangling links, but new links must exist. Prefer new IDs after purge: reusing an ID re-targets dangling supersedes links.

## Failure and reporting

Malformed arguments (unknown/missing keys, types, bad status, limit outside 1–32, expected_version below 1, entry with statuses/order) return JSON-RPC `-32602`, nothing written. Domain errors use `isError: true`: text/time/link/tag bounds, unknown sources/revisions/memories/supersedes and version Conflict.

On Conflict, re-read and reconcile; use the new head version only if the edit is still right. Never guess a version, force or delete/recreate. Identical-head replay returns `write: "already_recorded"`.

On timeout/missing result, re-read before any retry. If the head has your content, report recorded; otherwise replay the same request, not a new decision. `diagnostics_warning.code: "final_record_unavailable"` preserves the known result; report bookkeeping failure without repeating to repair it.

Report exact IDs, versions and statuses. The UI shows these entries with an `auto` badge for `origin-auto`. Timeline is for the user/agent to read, not activation approval or recall content.
