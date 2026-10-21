# Reference

The [README](../README.md) covers installation. This page lists configuration, limits, the MCP contract, diagnostics and maintenance. How the pipeline works is in [architecture](architecture.md).

## Components

| Component | Path | Role |
|---|---|---|
| Native binary | `lambda-memory` (installed as `lambda-memory-native`) | Store, lifecycle, retrieval and MCP server. One process per project store. |
| Local service | [`runtime/local-service.mjs`](../runtime/local-service.mjs), unit [`runtime/lambda-memory.service`](../runtime/lambda-memory.service) | Systemd user service. Listens on a private Unix socket and starts one native worker per project. |
| Bridge | [`runtime/stdio.mjs`](../runtime/stdio.mjs), launched as `~/.local/bin/lambda-memory` | What MCP clients run. Connects to the service and authenticates for the client. |
| Web UI | [`runtime/web-service.mjs`](../runtime/web-service.mjs), [`runtime/web/`](../runtime/web/) | Optional loopback curation page served by the service. |
| OMP hooks | [`extensions/lambda-memory-local.mjs`](../extensions/lambda-memory-local.mjs) | Automatic recall and capture for [OMP](https://github.com/can1357/oh-my-pi). |

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
| `memory_capture` | yes | `source`, `kind`, `outcome`, `origin`, `content` |
| `memory_propose` | yes | `memory`, `revision`, `sources` (1–32), `content`, optional `predecessor` |
| `memory_recall` | may prepare pending revisions | `task` (≤ 4000 bytes), `projects`, optional `budget_bytes` |
| `memory_list` | no | optional `states`, `memory`, `limit` (1–32), `cursor` |
| `memory_review` | yes | `revision`, `action` (`reject`, `conflict`, `promote`), `conflict_with`, `expected_active` |
| `memory_timeline_write` | yes | `entry`, `title`, `summary`, `occurred_at_ms`, `status`, `sources`, optional links, `expected_version` |
| `memory_timeline` | no | optional `limit`, `statuses`, `order`, `entry`, `cursor` |
| `memory_status` | no | — |
| `memory_activity` | no | optional `operations`, `outcomes`, `limit`, `cursor` |
| `memory_purge` | yes, destructive | `sources` (1–32) |

Ids (source, memory, revision, entry) are non-blank, control-character-free and at most 512 UTF-8 bytes. Origins are at most 2048 bytes. The [skills](../skills/) describe each tool's arguments, examples and error handling.

`memory_recall` can return several independently supported memories from the project, strongest first, sharing the byte budget. An explicit `budget_bytes` may not exceed `LAMBDA_MEMORY_CONTEXT_BYTES`; `0` returns no context. Attribution inside `context` may be compact when origins are long; full ids stay in `hits` and `memory_list`, and `truncated: true` also flags compact provenance. The old `budget_tokens` argument is rejected, so upgrade the native binary and the JavaScript bundle together.

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
