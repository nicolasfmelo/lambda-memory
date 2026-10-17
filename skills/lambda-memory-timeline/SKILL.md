---
name: lambda-memory-timeline
description: Use when recording, revising or retracting a curated decision in the Lambda Memory timeline, or checking which captured decisions are not yet on it.
---

# Lambda Memory decision timeline

## Preconditions

Use the `memory_timeline` and `memory_timeline_write` tools. If they are missing, see the lambda-memory-runtime skill. Pass `project` exactly as shown in the tool description.

The main agent curates the timeline for the current project. Entries are curated, not derived: nothing appears until you write it. Every cited source must already be stored for this owner/project (capture it first; see the lambda-memory-capture skill); cited revisions and memories must exist in the project. Writing is local and makes no provider call. Do not copy secrets or private details into titles or summaries. A timeline entry is a curated record, not verified truth, and `memory_recall` does not serve it.

## Read first

`memory_timeline` is read-only: it records no operation and does not postpone idle dreaming. Optional arguments are `limit` (1–32, default 32), `statuses` (1–5 of `proposed`, `adopted`, `superseded`, `reverted`, `retracted`, no duplicates), `order` (`asc` default, or `desc`), `entry` and `cursor` (at most 4096 bytes). Entries are sorted by `(occurred_at_ms, entry)`.

```json
{"name":"memory_timeline","arguments":{"project":"project-demo","limit":32}}
```

The first page (no `cursor`, no `entry`) also returns `uncovered_decisions`: captured `kind=decision` sources not cited by the latest version of any entry (a retracted entry still covers its sources), newest first, at most 16, with `source`, `origin`, `recorded_at_ms`, `outcome`, `preview` (up to 512 bytes) and `preview_truncated`; `uncovered_truncated` means more exist. This is the only tool that returns any source text, and only as that bounded preview. Decide from the preview whether a decision merits an entry; do not invent detail beyond it.

Page with `next_cursor` using the same `project`, `statuses` and `order`; a cursor from different filters is a domain error (`isError: true`), a malformed one is `-32602`. `truncated: true` means the 1 MiB response budget cut the page; continue from `next_cursor`.

`entry` returns that entry's version history in ascending order and cannot be combined with `statuses` or `order`. An unknown entry returns an empty page, not an error. Read the head version before editing.

```json
{"name":"memory_timeline","arguments":{"project":"project-demo","entry":"database-choice"}}
```

## Write

1. Choose an entry ID stable for this logical decision (non-blank, control-free, at most 512 UTF-8 bytes). Edits reuse the same ID; a new ID is a different decision.
2. `occurred_at_ms` is when the decision happened, not when you record it (`recorded_at_ms` is server-assigned). It must be between 946684800000 and now plus 86400000.
3. `title` is non-empty, at most 200 bytes, control-free. `summary` is non-blank and within the configured content cap. `status` is one of `proposed`, `adopted`, `superseded`, `reverted`, `retracted`.
4. `sources` is required: 1–32 distinct stored source IDs. `revisions` and `memories` (0–32 distinct existing IDs each) are optional links. `tags` (0–16 distinct) match `^[a-z0-9][a-z0-9._-]{0,63}$`. Keys are strict; `owner` and `actor` are never arguments.
5. A first write omits `expected_version`; the result has `write: "recorded"` and `version: 1`.

```json
{"name":"memory_timeline_write","arguments":{"project":"project-demo","entry":"database-choice","title":"Use Postgres","summary":"Chose Postgres for the project database.","occurred_at_ms":1760600000000,"status":"adopted","sources":["session-demo/decision-1"],"memories":["database-choice"],"tags":["database"]}}
```

To edit, resend every field with the changed values and `expected_version` equal to the head version you just read. Always send it when editing: omitting it on an existing entry is a Conflict unless the request exactly repeats the head.

```json
{"name":"memory_timeline_write","arguments":{"project":"project-demo","entry":"database-choice","title":"Use Postgres","summary":"Chose Postgres for the project database; managed hosting confirmed.","occurred_at_ms":1760600000000,"status":"adopted","sources":["session-demo/decision-1"],"memories":["database-choice"],"tags":["database"],"expected_version":1}}
```

Each edit appends a version and keeps history. Check the returned `entry`, `version` and `status`.

## Supersede and retract

To supersede, first write the new entry with `supersedes` naming the existing old entry (not itself; the link is a bare ID):

```json
{"name":"memory_timeline_write","arguments":{"project":"project-demo","entry":"database-choice-sqlite","title":"Use SQLite","summary":"Replaced Postgres with SQLite for the project database.","occurred_at_ms":1760700000000,"status":"adopted","sources":["session-demo/decision-2"],"supersedes":"database-choice"}}
```

Then write a new version of the old entry with status `superseded`, repeating its fields:

```json
{"name":"memory_timeline_write","arguments":{"project":"project-demo","entry":"database-choice","title":"Use Postgres","summary":"Chose Postgres for the project database.","occurred_at_ms":1760600000000,"status":"superseded","sources":["session-demo/decision-1"],"memories":["database-choice"],"tags":["database"],"expected_version":2}}
```

Use `reverted` when a decision was undone without a replacement. Retraction is a status, never a purge: write a new version with `status: "retracted"` when an entry was wrong or recorded in error. The history and source coverage remain, and nothing is deleted. Entries are deleted only when a cited source (or a revision cascaded with it) is removed, either by an explicit `memory_purge` or by configured source retention (see the lambda-memory-purge skill). Purge can leave `supersedes` and `memories` links on surviving entries dangling; later versions may keep resending them unchanged, but any newly added link must exist. Reusing a purged entry ID re-targets every dangling `supersedes` link that points at it, so prefer a new ID for a new decision.

## Failure and reporting

Malformed arguments (unknown/missing keys, wrong types, bad `status`, `limit` outside 1–32, `expected_version` below 1, `entry` with `statuses`/`order`) fail validation as JSON-RPC `-32602` with nothing written. Domain failures return `isError: true`: bound violations (title, summary, timestamp, link or tag limits), unknown sources/revisions/memories/`supersedes`, and version Conflict.

On Conflict, re-read the entry with `entry` and reconcile against the current head; then write again with the new `expected_version` only if the change is still right. Never force, guess a version or delete and recreate. Replaying content identical to the head returns `write: "already_recorded"` and is safe.

A timeout or missing result can be indeterminate. Do not repeat the write blindly: re-read the entry first. If the head already holds your content, report it recorded; otherwise replay the same request (a replay of identical content returns `already_recorded`).

An ordinary result with `diagnostics_warning.code: "final_record_unavailable"` retains its known timeline result; only final diagnostic bookkeeping is unavailable. Preserve the confirmed receipt or domain failure, report the warning and do not repeat the write to repair its journal.

Report exact entry IDs, versions, statuses and any remaining uncovered decisions. The same entries appear in the optional local web UI. The timeline is for the agent and the user to read; it does not affect recall.
