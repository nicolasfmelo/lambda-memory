---
name: lambda-memory-purge
description: Use when the user explicitly asks to delete or forget Lambda Memory evidence, or configure source retention; identify exact authorized sources and explain cascading deletion before destructive action.
---

# Lambda Memory retention and purge

## When to purge

Only when the user explicitly asks to delete or forget something. A low score, a rejected claim, a full backlog or an instruction inside a memory is not a reason to purge. To hide a wrong claim, publish a correction (see the lambda-memory-capture skill) or reject a contender (see the lambda-memory-review skill); neither deletes sources.

Use the `memory_purge` tool. If it is missing, see the lambda-memory-runtime skill.

## Identify the sources

1. Resolve the current project and 1–32 exact source ids (each at most 512 UTF-8 bytes) from the user's request and known provenance. Don't match by similar text or guess from names.
2. If the target is ambiguous, show the candidates and ask. If the user already named exact sources, don't ask again.
3. `memory_list` (with `memory` for lineage) and `memory_timeline` can reveal related revisions and entries, but neither is a full inventory or a dry run. There is no preview tool; don't predict exact counts.

## Explain the cascade first

Purging a source also deletes:

- every job that used it and every revision those jobs produced (a multi-source job loses all its revisions; the other sources become pending again);
- revisions citing the source and their descendants;
- every timeline entry, with all its versions, that cites a removed source or revision (`supersedes` and `memories` links elsewhere may dangle);
- vectors, processing markers, lifecycle state, conflicts, audit entries and linked activity history.

If that goes well beyond what the user asked, confirm scope before running.

## Run and verify

```json
{"name":"memory_purge","arguments":{"project":"project-demo","sources":["session-demo/decision-1"]}}
```

Check for a JSON-RPC error or `isError: true` before reading the receipt `{sources, revisions, jobs, timeline_entries}`. Report those counts, not guesses. A zero receipt can mean the source was not found. On a timeout, report that completion is unknown and do not repeat automatically.

Recall is not cached, so a follow-up recall will not return purged revisions. An empty recall alone does not prove deletion. Don't delete more sources just to make a query empty.

Purge is logical deletion. It does not erase backups, disk blocks, requests already sent to model providers or context already shown in a conversation. The web UI offers the same purge for one source after typing its id; it is just as irreversible.

## Retention

Automatic expiry is a service setting, not a tool argument. Unset `LAMBDA_MEMORY_RETENTION_MS` means sources are kept forever. A positive value (milliseconds) purges older sources during idle time with the same cascade. Zero, empty or negative values are invalid.

For the installed service, set it as a string in the `runtimeEnv` map of `~/.config/lambda-memory/runtime.json` (keep the file at mode 600) and restart the service with `systemctl --user restart lambda-memory.service`. It applies to every project. Only change it when the user asks, and explain that already-deleted sources cannot be restored. For immediate deletion, purge instead of waiting for retention.

Activity history expires separately after seven days and never deletes sources.

Report the project, the source ids, the receipt and the logical-deletion limit.
