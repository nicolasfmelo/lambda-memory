# Reference

The [README](../README.md) covers installation. This page lists configuration, limits, the MCP contract, diagnostics and maintenance. How the pipeline works is in [architecture](architecture.md).

## Components

| Component | Path | Role |
|---|---|---|
| Native binary | `lambda-memory` (installed as `lambda-memory-native`) | Store, lifecycle, retrieval and MCP server. One process per project store. |
| Local service | [`runtime/local-service.mjs`](../runtime/local-service.mjs), unit [`runtime/lambda-memory.service`](../runtime/lambda-memory.service) | Systemd user service. Listens on a private Unix socket and starts one native worker per project. |
| Bridge | [`runtime/stdio.mjs`](../runtime/stdio.mjs), launched as `~/.local/bin/lambda-memory` | What MCP clients run. Connects to the service and authenticates for the client. |
| Web UI | [`runtime/web-service.mjs`](../runtime/web-service.mjs), [`runtime/web/`](../runtime/web/) | Optional loopback curation page served by the service. |
| OMP hooks | [`extensions/lambda-memory-local.mjs`](../extensions/lambda-memory-local.mjs) | Automatic recall, playbook injection and action capture for [OMP](https://github.com/can1357/oh-my-pi). |
| Claude Code hooks | [`runtime/claude-code-hook.mjs`](../runtime/claude-code-hook.mjs) | Playbook injection and action capture for Claude Code. |

The service imports `runtime/project-registry.mjs` and `runtime/web-service.mjs`, so copy the whole `runtime/` and `extensions/` directories when installing or updating.

### Native binary

Users do not launch the native binary from MCP clients; they launch the bridge. Run without arguments or with `--help` to print usage.

| Flag | Purpose |
|---|---|
| `--serve-mcp` | Serve MCP over stdio for one project (used by the service) |
| `--inspect-dream-recovery` | Print a recovery plan for blocked sources (see below) |
| `--recover-dream-sources` | Apply a consented recovery plan read from stdin |
| `--inspect-embedding-recovery` | Print a recovery plan for interrupted embedding calls |
| `--recover-embeddings` | Apply a consented embedding recovery plan read from stdin |
| `--inspect-embedding-index` | Print the current embedding space and batch limit |
| `--prepare-embedding-index` | Embed one consented batch into the current space (stdin) |
| `--reject-tool-metadata [--apply]` | List, then retire, legacy per-tool-result memories (see below) |
| `--show-provider-config` | Print the resolved mode and models without calling a provider |
| `--version` | Print the version |

Cargo features: `memory-runtime` (store, MCP, dreaming) and `router-http` (OpenRouter client). Release builds use both.

## Configuration

### `runtime.json`

Default path `~/.config/lambda-memory/runtime.json`, or `LAMBDA_MEMORY_CONFIG`. It must be a regular file owned by you and not readable by other users (`0600`), at most 16 KiB.

| Key | Required | Value |
|---|---|---|
| `nativeBin` | yes | Absolute path to the native binary (must be executable) |
| `bridgeBin` | yes | Absolute path to the bridge launcher (must be executable) |
| `dataDir` | yes | Absolute path; one store per project is created under it |
| `socketPath` | yes | Absolute path, at most 100 bytes. `$XDG_RUNTIME_DIR/lambda-memory/service.sock` is a good choice. |
| `owner`, `actor` | yes | Non-empty, at most 512 bytes. Owner scopes all data; actor is recorded as provenance. |
| `token` | yes | 32–1024 printable ASCII characters without spaces, e.g. `openssl rand -hex 32` |
| `mode` | no | `local` (default) or `openrouter` |
| `providerEnvFile` | with `openrouter` | Absolute path to the provider file |
| `webPort` | no | 1024–65535. Absent means no web UI. |
| `webLeaseMs` | no | 5000–3600000, default 120000. How long the UI keeps a project worker alive after its last call. |
| `runtimeEnv` | no | Map of the `LAMBDA_MEMORY_*` limits below, as strings |

Invalid values stop the service with a specific message (for example `socket path bound` or `private configuration required`).

### Provider file

Required in `openrouter` mode; same permission rules as `runtime.json`. Only these lines are accepted:

```bash
OPEN_ROUTER_KEY=sk-or-...
LAMBDA_PLANNER_MODEL=anthropic/claude-haiku-5.5
LAMBDA_EMBEDDING_MODEL=google/gemini-embedding-2
LAMBDA_SELECTOR_MODEL=openai/gpt-6-luna-decisions
```

The model lines are optional and may only repeat the pinned ids. The key is passed to native workers only; it is never sent to the bridge or MCP clients.

### Limits

Set through `runtimeEnv` (installed service) or the environment (native binary). Values outside the range are rejected at startup.

| Variable | Default | Range |
|---|---:|---|
| `LAMBDA_MEMORY_MAX_CONTENT_BYTES` | 8192 | 1–8192 UTF-8 bytes per source |
| `LAMBDA_MEMORY_MAX_PENDING` | 1024 | 1–100000 unprocessed sources per project |
| `LAMBDA_MEMORY_MAX_CANDIDATES` | 8 | 1–8 in `openrouter` mode, 1–32 in `local` |
| `LAMBDA_MEMORY_CONTEXT_BYTES` | 8192 | 1–32768. UTF-8 bytes including provenance, not provider tokens. The old `LAMBDA_MEMORY_CONTEXT_TOKENS` is rejected. |
| `LAMBDA_MEMORY_DREAM_BATCH` | 8 | 1–16 sources per batch |
| `LAMBDA_MEMORY_IDLE_MS` | 60000 | 1–86400000 |
| `LAMBDA_MEMORY_TICK_MS` | 1000 | 1–60000 |
| `LAMBDA_MEMORY_EMBEDDING_DIMENSIONS` | 1536 | 128–3072 (`openrouter` mode). Changing it starts a new embedding space; see below. |
| `LAMBDA_MEMORY_RETENTION_MS` | unset (keep forever) | Positive milliseconds. Older sources are purged during idle time, with the same cascade as `memory_purge`. |

### Native environment

When running the native binary directly (Docker or tests) it reads `LAMBDA_MEMORY_DATA_DIR`, `LAMBDA_MEMORY_OWNER`, `LAMBDA_MEMORY_ACTOR`, `LAMBDA_MEMORY_PROJECT`, `LAMBDA_MEMORY_TOKEN`, `LAMBDA_MEMORY_MODE` and, in `openrouter` mode, `OPEN_ROUTER_KEY` plus the model variables. `LAMBDA_MEMORY_ALLOWED_PROJECTS` (comma-separated, at most 16 projects in total) authorizes extra projects for `memory_recall`; the installed service does not accept it, so recall there is always single-project. [`.env.example`](../.env.example) lists the variables with placeholders.

## Project binding

Each MCP session is bound to the project of the client's working directory:

1. If the directory is inside a git checkout, the identity is the host and repository path of the `upstream` remote, else `origin`, for example `repo:github.com/you/app` (credentials are never included).
2. A checkout without a remote, or a directory outside git, uses `path:` plus its canonical path.

Worktrees and clones of the same repository share one project and one worker. The bridge appends `Current authorized project: <project>` to every tool description; tools must pass exactly that value.

Workers stay resident while clients are connected, which lets idle dreaming run. When the last client disconnects, admitted work finishes before the worker stops. The unit must not use `PrivateTmp`: the service resolves clients' real directories, including worktrees under `/tmp`.

## MCP tools

All tools take `project` (except `memory_recall`, which takes `projects`). Owner and actor are never arguments.

| Tool | Writes | Main arguments |
|---|---|---|
| `memory_capture` | yes | `source`, `kind` (including `action`), `outcome`, `origin`, `content`, optional `recorded_at_ms` (imports only) |
| `memory_propose` | yes | `memory`, `revision`, `sources` (1–32), `content`, optional `predecessor` |
| `memory_recall` | may prepare pending revisions | `task` (≤ 4000 bytes), `projects`, optional `budget_bytes` |
| `memory_list` | no | optional `states`, `memory`, `limit` (1–32), `cursor` |
| `memory_review` | yes | `revision`, `action` (`reject`, `conflict`, `promote`), `conflict_with`, `expected_active` |
| `memory_timeline_write` | yes | `entry`, `title`, `summary`, `occurred_at_ms`, `status`, `sources`, optional links, `expected_version` |
| `memory_timeline` | no | optional `limit`, `statuses`, `order`, `entry`, `cursor` |
| `memory_playbook` | no | — |
| `memory_status` | no | — |
| `memory_activity` | no | optional `operations`, `outcomes`, `limit`, `cursor` |
| `memory_purge` | yes, destructive | `sources` (1–32) |

Ids (source, memory, revision, entry) are non-blank, control-character-free and at most 512 UTF-8 bytes. Origins are at most 2048 bytes. The [skills](../skills/) describe each tool's arguments, examples and error handling.

`memory_recall` can return several independently supported memories from the project, strongest first, sharing the byte budget. An explicit `budget_bytes` may not exceed `LAMBDA_MEMORY_CONTEXT_BYTES`; `0` returns no context. Attribution inside `context` may be compact when origins are long; full ids stay in `hits` and `memory_list`, and `truncated: true` also flags compact provenance. The old `budget_tokens` argument is rejected, so upgrade the native binary and the JavaScript bundle together.

Selection is per candidate. The selector's "best choice" only sets the order (first, if that candidate is itself supported); its confidence no longer blocks the others. A candidate is selected when its own relevance, utility and direct-support ratings pass the thresholds. If the selector abstains (no best choice) or two candidates share an id, nothing is selected. Query planning and query embedding run at the same time.

The query planner accepts JSON wrapped in a markdown code fence and drops unusable terms, keeping the first eight valid, unique ones. If it returns no usable term or invalid JSON, the call counts as completed and billed but unusable (`provider_invalid_output`). Only in that case does recall go on without keyword search, using the vector candidates alone; `memory_activity` shows the failed `query_planning` stage with its reason. Any other planner error (offline, rejected, timeout, storage) still makes recall fail, and nothing is retried.

Errors: malformed arguments return JSON-RPC `-32602` with the reason and nothing is written. Domain failures return `isError: true`. A timeout is indeterminate: re-read state before repeating a write.

## Diagnostics

`memory_status` and `memory_activity` are read-only. They call no model, do not prepare evidence and do not postpone dreaming.

```json
{"name":"memory_status","arguments":{"project":"project-demo"}}
{"name":"memory_activity","arguments":{"project":"project-demo","operations":["dream"],"limit":20}}
```

| Status field | Meaning |
|---|---|
| `worker.instance`, `worker.started_at_ms` | Current native worker; a restart creates a new instance |
| `worker.dreaming` | State, phase, stop reason and idle timing |
| `pending.unconsumed_sources` | Sources not yet turned into revisions |
| `pending.processing_revisions` | Revisions waiting for embedding and publication |
| `pending.blocked_sources`, `pending.blocked_revisions` | Subsets blocked by an uncertain paid call; do not add them to the totals above |
| `pending.active_without_current_embedding` | Active revisions still missing an embedding (`openrouter` mode) |
| `lifecycle`, `review.unresolved_conflicts` | Counts per lifecycle state |
| `review.activation_requires_review` | Always `false`: publication is automatic |
| `last_success`, `last_failure`, `last_dream_success`, `last_dream_failure` | Latest retained outcomes |
| `diagnostics`, `history` | Recording health and the start of retained history |

`memory_activity` returns newest-first operation and stage entries with timestamps, durations and effect counts. It never returns memory text, queries, origins, credentials or provider error bodies. Filters: `operations` from `capture`, `propose`, `dream`, `list`, `recall`, `review`, `purge`; `outcomes` from `running`, `succeeded`, `failed`, `indeterminate`. Pages default to 20 entries (maximum 32); pass `next_cursor` back unchanged with the same filters.

History is kept for seven days per project, independent of source retention. Work interrupted by a crash or restart is reported as `indeterminate`. If an ordinary tool result carries `diagnostics_warning.code: "final_record_unavailable"`, the operation itself succeeded or failed as reported; only its history record is missing. Do not repeat the operation.

Service logs: `journalctl --user -u lambda-memory -n 50`.

## Procedural playbook

Besides facts, the memory keeps a short playbook of how work is usually done in a project: which commands to run, which errors have a known fix and which files change often. It is built from recorded actions, is separate from the recalled facts and never changes what `memory_recall` returns.

### Action episodes

`memory_capture` accepts `kind: "action"`. The `content` is a JSON object with exactly these fields, at most 1024 bytes in total:

| Field | Value |
|---|---|
| `v` | `1` |
| `tool` | 1–128 characters, starts with a letter; letters, digits and `_ . : -` |
| `intent` | At most 160 characters |
| `summary` | At most 200 characters |
| `error_head` | At most 200 characters, or `null` |
| `session` | 16 lowercase hex characters |
| `seq` | Non-negative integer |

The server rejects an episode that breaks these bounds, or whose `intent`, `summary` or `error_head` contains a likely secret (provider key prefixes, `Bearer` tokens, credentials in a URL). The clients clean episodes first: [`runtime/action-episode.mjs`](../runtime/action-episode.mjs) hides credentials and long token-like strings, and replaces the project and home paths with `.` and `~`.

An episode is an ordinary immutable source with two differences: it does not count toward `LAMBDA_MEMORY_MAX_PENDING`, so a busy session cannot fill the backlog, and it never enters dreaming, so no model call runs per action. Retention and purge apply as for any source.

`recorded_at_ms` (integer milliseconds) is accepted only for `kind: "action"` with an origin that starts with `import://`, and must be between 400 days ago and now. The history importer below uses it.

### Reading the playbook

`memory_playbook` is read-only. It takes `{project}` and returns `{project, playbook, revision, generated_at_ms, episodes}`. `playbook` is the active text, or `null` if none exists yet (`revision` and `generated_at_ms` are then `null` too). `episodes` counts the project's retained action episodes. It calls no model and does not postpone idle dreaming.

### How it is generated

Only in `openrouter` mode, and only while the worker is idle with no source waiting for derivation. A new playbook is made when at least 20 of the newest episodes are newer than the active playbook (or there is none), or when at least one new episode exists and the active playbook is 24 hours old or more.

1. A deterministic step reads the 5,000 most recent episodes and ranks up to 30 facts, `F1`…`F30`: at most 15 procedures (a command repeated successfully in at least two sessions; absolute paths are masked as `<path>`), 10 pitfalls (an error followed within five steps by a similar command that worked, seen in at least two sessions) and 5 hot files (edited successfully in at least two sessions). Recent activity counts more: full weight for 7 days, then falling to 0.25 at 90 days. Episodes with unknown outcome are ignored.
2. One Haiku call phrases the numbered facts, given the current playbook text. It never sees the raw episodes.
3. A validator accepts the text only if it is not empty, at most 1536 bytes, every non-empty line is a heading or a bullet ending in `[F#]` citations, and every citation names a known fact.
4. A valid text is published as the memory `procedural:playbook`. The newest episode behind each cited fact is its source. A new version replaces the previous one in a single step.

The playbook memory is excluded from `memory_recall` and from embedding. You can see it with `memory_playbook` and `memory_list`.

If the output is invalid (empty, too long, a line without citation, an unknown citation), it is not published and not retried. The previous version stays active, the billed call stays recorded and the dream operation ends as failed with reason `playbook_invalid_output`, phase `playbook`. A marker stops the same call from being repeated for the same set of episodes; the next attempt needs new episodes. If the provider refuses before any billable work (offline, 401, 402, 403, 429), the marker is released and the call can be tried again.

### OMP

The extension records one `omp://action` episode per tool result, replacing the old per-event `omp://tool_result` metadata. Memory tools are skipped. Successful shell, edit/write and task tools are kept; any other tool only when it fails. It reads the playbook once per session before the first turn, and again after `session_compact`, with a 2 s deadline. If that fails it warns once and the turn goes on. The playbook is injected as a hidden message marked as history, not as instructions. Subagents neither record nor receive it.

### Claude Code

Three hook entries in `~/.claude/settings.json` give Claude Code the same capture and injection. Automatic recall stays OMP-only. They run [`runtime/claude-code-hook.mjs`](../runtime/claude-code-hook.mjs) from the installed runtime:

```json
{
  "hooks": {
    "SessionStart": [
      { "hooks": [ { "type": "command", "command": "node \"$HOME/.local/share/lambda-memory/runtime/runtime/claude-code-hook.mjs\" session-start" } ] }
    ],
    "PostToolUse": [
      { "matcher": "*", "hooks": [ { "type": "command", "command": "node \"$HOME/.local/share/lambda-memory/runtime/runtime/claude-code-hook.mjs\" post-tool-use" } ] }
    ],
    "PostToolUseFailure": [
      { "matcher": "*", "hooks": [ { "type": "command", "command": "node \"$HOME/.local/share/lambda-memory/runtime/runtime/claude-code-hook.mjs\" post-tool-use" } ] }
    ]
  }
}
```

Merge these keys into the existing file and keep your other settings and hooks. Claude Code reports failed tool calls through a separate `PostToolUseFailure` event, so that entry runs the same `post-tool-use` command. Calls you interrupt are not recorded. `session-start` has a 3 s deadline: it reads the playbook and returns it as `additionalContext`. `post-tool-use` has a 2 s deadline: it records one `claude-code://action` episode (shell, edit/write and task tools; other tools only when they fail).

Copy `claude-code-hook.mjs`, `import-actions.mjs`, `action-episode.mjs` and `transcripts.mjs` into the installed `runtime/runtime/` directory, next to the other runtime files. The hook also imports the installed `extensions/lambda-memory.mjs`. It uses the installed private configuration and the session's project. A hook never blocks Claude Code: any failure, timeout or missing configuration ends with exit code 0 and no output.

### Importing history

[`runtime/import-actions.mjs`](../runtime/import-actions.mjs) rebuilds episodes from the existing OMP or Claude Code transcripts of one directory:

```bash
node runtime/import-actions.mjs --agent omp|claude-code --cwd <dir> [--since <ISO>] [--apply]
```

Without `--apply` it is a dry run: it saves nothing and prints counts (calls, episodes, skipped by reason) and ten sample episodes. Read the samples for private data before applying. With `--apply` it saves through the installed bridge, with origin `import://<agent>` and the transcript time as `recorded_at_ms`; the server rejects older calls and they are counted as `rejected`. Source ids come from the session and the call, so a rerun is safe: episodes that exist are reported as `already_saved`. The import stops after ten failures in a row.

## Retiring legacy tool metadata

Older versions saved one `omp://tool_result` source per tool call, and the memories derived from them are noise. To retire them, stop the service (the command needs the project's store, like the other operator commands), export the single-project operator environment and run:

```bash
lambda-memory-native --reject-tool-metadata           # dry run: count and up to five samples
lambda-memory-native --reject-tool-metadata --apply   # retire in batches of 32
```

It targets active memories whose sources are **all** `omp://tool_result`, and moves them to `rejected` in batches of 32, each with an audit entry `operator_retire_tool_metadata`. The sources are kept. There is no undo command, so back up the store first. No MCP tool can run it, and it needs no provider.

## Purge

`memory_purge` deletes, in one transaction:

- the selected sources;
- every job that used them and every revision those jobs produced (a job with several sources loses all its revisions; surviving sources become eligible for dreaming again);
- revisions citing a selected source, and their descendants;
- every timeline entry, with all versions, that cites a removed source or revision (`supersedes` and `memories` links on other entries may be left dangling);
- vectors, admission markers, lifecycle state, conflicts, audit entries and linked activity history.

The purge's own activity record contains only counts. Recall is not cached, so the next call cannot return purged evidence. This is logical deletion: it does not erase disk blocks, backups, requests already sent to providers or context already shown to an agent.

## Recovering blocked sources

A source is blocked when a paid dreaming call was admitted but its outcome is unknown, for example after a crash. Blocked sources are never sent again automatically, because the earlier call may already have been billed. `memory_status` shows them under `pending.blocked_sources`.

To retry them, accept a possible duplicate charge and run the helper from inside the project's checkout (or set `LAMBDA_MEMORY_CWD` to it). The helper refuses to run while the service is active.

```bash
systemctl --user stop lambda-memory.service
cp -a ~/.local/share/lambda-memory/projects ~/lambda-memory-backup   # optional private backup
node ~/.local/share/lambda-memory/runtime/runtime/recover-pending.mjs inspect > plan.json
# edit plan.json: set "accept_duplicate_charge": true, change nothing else
node ~/.local/share/lambda-memory/runtime/runtime/recover-pending.mjs recover < plan.json
systemctl --user start lambda-memory.service
```

`inspect` makes no model calls and prints the project, source ids and their current admissions. `recover` checks that the sources and admissions are unchanged, keeps the old markers, records the recovery and then dreams, embeds and publishes the sources. If the recovery attempt itself fails uncertainly, the plan is invalid and you need a new `inspect`. Do not restore the backup after a paid attempt: that would erase the new admission markers. The helper wraps the native `--inspect-dream-recovery` and `--recover-dream-sources` flags.

## Embedding index preparation and recovery

Gemini embeddings use separate query and document prefixes and default to 1536 dimensions (`LAMBDA_MEMORY_EMBEDDING_DIMENSIONS`, 128–3072). Together these define the embedding space `runtime-gemini-retrieval-v2`. A vector, and the marker of a paid embedding call, is identified by owner, project, revision, provider, model, index revision and dimensions, so a different dimension is a different space: old vectors are kept but never reused or reinterpreted, and old markers keep blocking in their own space.

After upgrading a populated store, existing active memories have no vector in the new space (`pending.active_without_current_embedding` in `memory_status`). Recall prepares only a small batch at a time, which is not a full migration. To prepare the index, accept the embedding charges, stop the service and keep a private backup:

```bash
systemctl --user stop lambda-memory.service
node ~/.local/share/lambda-memory/runtime/runtime/recover-pending.mjs inspect-index > index.json
# edit index.json: set "accept_charges": true and, if you want, a smaller "limit" (1-16)
node ~/.local/share/lambda-memory/runtime/runtime/recover-pending.mjs prepare-index < index.json
systemctl --user start lambda-memory.service
```

Each run embeds at most `limit` active revisions that lack the space and reports `revisions_indexed`. Repeat until a run reports zero.

If an embedding call was interrupted with an unknown outcome, it is never retried automatically. `inspect-embeddings` prints the revisions, their snapshot digests and current admissions with `accept_duplicate_charge: false`; after accepting a possible duplicate charge, set it to `true` and feed the unchanged plan to `recover-embeddings`. The recovery checks that sources and admissions are unchanged, keeps old markers and records who ran it and when. A stale or replayed plan is rejected, even after a restart. On any uncertain result, inspect again instead of repeating the command, and do not restore a backup taken before a paid attempt. The native flags are `--inspect-embedding-index`, `--prepare-embedding-index`, `--inspect-embedding-recovery` and `--recover-embeddings`.

## Web UI

Set `webPort` in `runtime.json` and restart the service, then open `http://127.0.0.1:<webPort>`. The UI shows memories by state with sources, versions and audit, the decision timeline, status and activity. It can correct a memory, reject or resolve revisions, edit the timeline and purge one source. It talks to the same project workers as the MCP tools.

Security model (single user, loopback):

- Listens on `127.0.0.1` only. Each connection's peer UID must match the service's UID (read from `/proc/net/tcp`); if that check is unavailable the UI does not start, while the socket service keeps running.
- `Host`, `Origin` and `Sec-Fetch-Site` must be same-origin. Every `/api/*` call needs the per-start CSRF token; restarting the service rotates it.
- POST bodies are JSON, at most 64 KiB. No CORS; strict Content-Security-Policy.
- The token, owner, actor and provider settings never appear in responses.
- The UI never calls `memory_recall`, `memory_capture` or `memory_propose` directly, and `/api/tool` rejects `memory_purge`. Purge goes through `/api/purge`, one source at a time, after you type its id. It is irreversible and cascades as described above.

The interface is reading-first: memories and decisions show their full text, while ids, sources, lineage, audit and other versions are in collapsible sections. Timeline cards show a short preview with a link to the full decision and all its versions. Each lifecycle and timeline state explains what it means (`active` is available for recall, not verified; `processing` is unfinished preparation, not an approval queue), and dialogs state their consequences before you confirm. Theme is Auto, Light or Dark (stored in `localStorage["lm-theme"]`). The layout works down to 390 px wide and respects reduced motion and forced colors.

The project registry behind the project list is limited to 256 entries and 256 KiB; the oldest entries are dropped first. Dropping an entry never deletes the project's data. An oversized registry from an older version is kept and reported as an error instead of being reset.

Any process running as your user can reach the page, so do not forward the port. A project without a live worker returns `409 needs_start` until you press **Start worker**, which may process pending sources (billable in `openrouter` mode). An open tab polls every 10 s and keeps the worker alive. Error codes: 400 rejected arguments, 429 more than four UI leases, 502 not confirmed (do not retry writes automatically), 503 pending limit reached (safe to retry).

## Skills

[`skills/`](../skills/) contains six guides for agents:

- [`lambda-memory-recall`](../skills/lambda-memory-recall/SKILL.md): scoped recall and how to treat the evidence.
- [`lambda-memory-capture`](../skills/lambda-memory-capture/SKILL.md): saving evidence and publishing corrections.
- [`lambda-memory-review`](../skills/lambda-memory-review/SKILL.md): optional rejection and conflict handling.
- [`lambda-memory-timeline`](../skills/lambda-memory-timeline/SKILL.md): the curated decision timeline.
- [`lambda-memory-purge`](../skills/lambda-memory-purge/SKILL.md): deletion and retention.
- [`lambda-memory-runtime`](../skills/lambda-memory-runtime/SKILL.md): setup, connection and diagnosis.

Claude Code loads them from `~/.claude/skills/`. In OMP, add the repository's absolute `skills/` path to `skills.customDirectories`, or copy the directories into `~/.omp/agent/managed-skills/`. Loading a skill does not start the service or mount tools.
