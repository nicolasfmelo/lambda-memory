---
name: lambda-memory-runtime
description: Use when setting up, connecting to or diagnosing the local Lambda Memory service, its MCP bridge and the OMP hooks extension; project binding, idle processing and blocked sources.
---

# Lambda Memory runtime

## How it fits together

- `lambda-memory.service`: systemd user service (Node) that starts one native worker per project and shares it between sessions.
- `~/.local/bin/lambda-memory --serve-mcp`: the bridge that MCP clients launch. It connects to the service over a private Unix socket and authenticates for the client. Never point a client at the native binary (`lambda-memory-native`).
- Configuration: `~/.config/lambda-memory/runtime.json` (or `LAMBDA_MEMORY_CONFIG`) and, in `openrouter` mode, a provider file. Both must be mode 600. Never print the token or API key. The provider file may also set `LAMBDA_MEMORY_ROUTER_BASE_URL` (https, or http on loopback with a port) to use another endpoint instead of `https://openrouter.ai`; the base URL is not sent to clients.
- Data: `~/.local/share/lambda-memory/projects`, one store per project.
- Modes: `openrouter` (Haiku, Gemini embeddings at 1536 dimensions by default, Luna; billable, no spending cap) or `local` (offline, no model calls). Local is never used as a fallback after provider errors.

Loading a skill does not start the service or mount tools. Installation steps are in the repository README.

## Connect a client

Claude Code:

```bash
claude mcp add -s user lambda-memory -- ~/.local/bin/lambda-memory --serve-mcp
```

OMP: merge into `~/.omp/agent/mcp.json`, keeping other entries, and check `/mcp list` shows `lambda-memory` connected:

```json
{ "mcpServers": { "lambda-memory": { "type": "stdio", "command": "lambda-memory", "args": ["--serve-mcp"], "timeout": 660000 } } }
```

Don't set `cwd` and don't put tokens in the client config. Tool names: `mcp__lambda-memory__memory_*` (Claude Code), `mcp__lambda_memory_memory_*` (OMP).

Optional OMP hooks: `~/.omp/agent/extensions/lambda-memory-local.ts` re-exports the installed `extensions/lambda-memory-local.mjs`. It recalls before each main-agent turn (25-second limit), injects the project playbook, records action episodes after tool calls, and adds `/lambda-memory <text>`. It does not register tools. Never also load `extensions/lambda-memory.mjs` in the same session. Reopen OMP after updating the extension; `/mcp reload` is enough for config changes.

## Playbook and agent hooks

`memory_playbook` (read-only, argument `project`) returns the project's playbook: a short, cited list of the commands, known error fixes and often-edited files that recorded actions show. The result has `project`, `playbook`, `revision`, `generated_at_ms` and `episodes`; `playbook` is `null` until one exists. It calls no model and does not postpone dreaming. The playbook is not part of `memory_recall`.

The idle worker builds it in `openrouter` mode from action episodes (`kind: "action"`) that the OMP extension and the Claude Code hooks record. If the model output is invalid it is dropped, the previous version stays and `memory_activity` shows `playbook_invalid_output`. Details are in the "Procedural playbook" section of `docs/reference.md`.

The OMP extension injects the playbook once per session and again after a context compaction. Claude Code needs three hook entries: merge this into `~/.claude/settings.json`, keeping your other settings.

```json
{
  "hooks": {
    "SessionStart": [
      { "hooks": [ { "type": "command", "command": "node \"$HOME/.local/share/lambda-memory/runtime/runtime/agent-hook.mjs\" claude-code session-start" } ] }
    ],
    "PostToolUse": [
      { "matcher": "*", "hooks": [ { "type": "command", "command": "node \"$HOME/.local/share/lambda-memory/runtime/runtime/agent-hook.mjs\" claude-code post-tool-use" } ] }
    ],
    "PostToolUseFailure": [
      { "matcher": "*", "hooks": [ { "type": "command", "command": "node \"$HOME/.local/share/lambda-memory/runtime/runtime/agent-hook.mjs\" claude-code post-tool-use" } ] }
    ]
  }
}
```

`session-start` (3 s limit) injects the playbook; `post-tool-use` (2 s limit) records one action episode. Failed tool calls arrive through `PostToolUseFailure`: register it next to `PostToolUse` with the same command. A hook never blocks Claude Code: any failure ends with exit code 0 and no output. Copy `agent-hook.mjs`, `hook-adapters/`, `claude-code-hook.mjs` (kept so older settings keep working), `import-actions.mjs`, `action-episode.mjs` and `transcripts.mjs` next to the other installed runtime files. Claude Code has no automatic recall; that stays OMP-only.

Codex uses the same core: `agent-hook.mjs codex session-start` on `SessionStart` and `agent-hook.mjs codex post-tool-use` on `PostToolUse` with matcher `^(Bash|apply_patch)$`, both with `"timeout": 5`, in `~/.codex/hooks.json`. The user must trust new hooks with `/hooks` in Codex. Codex reports no outcome, so its episodes are `unknown` and count as unverified uses in the playbook.

Conversation capture is opt-in: with `"captureConversation": true` in `runtime.json`, `UserPromptSubmit` and `Stop` hooks (`agent-hook.mjs <claude-code|codex> user-prompt-submit|stop`, `"async": true`) record each sanitized prompt and final reply as an `exchange` source for the curator. Turn it on only when the user asks: the text goes to OpenRouter.

To build episodes from past sessions, run `node runtime/import-actions.mjs --agent omp|claude-code --cwd <dir> [--since <ISO>]`. It is a dry run until you add `--apply`, so read the sample episodes for private data first. A rerun is safe.

Operator only, with the service stopped and a backup: `lambda-memory-native --reject-tool-metadata` lists the active memories derived only from old `omp://tool_result` sources; add `--apply` to retire them in batches of 32. It has no undo; the sources are kept.

## Memory tree

Knowledge is a per-project tree with four roots (Decisions, Preferences, Architecture, Open items). While idle, one Haiku call per batch of up to four sources decides to discard, merge into a node, create a child or ask to see a node first. Only node summaries are embedded. Later idle steps refresh, split and rebuild nodes; each is one paid call and is never resent. `memory_status` has a `tree` block (`mode`, `nodes_active`, `stale`, `split_queue`, `parked_sources`); activity phases are `curation`, `tree_refresh`, `tree_split` and `tree_rebuild`, and an unusable answer shows as `provider_invalid_output` or a `tree_*` reason.

Projects with memories from 0.3 or earlier stay in `flat` mode until migrated. Migration is paid maintenance with the service stopped and a backup; never run it without the user's approval of the plan: `node runtime/migrate-all-trees.mjs plan > plan.json`, `summary < plan.json`, then, after approval, set `accept_charges` to `true` and `apply < plan.json`. `docs/upgrade-and-migrate.md` has the full procedure.

## Project binding

The session's project comes from its working directory: the `upstream` or `origin` git remote (for example `repo:github.com/you/app`), or `path:<directory>` without one. Worktrees of one repository share a project. The bridge appends "Current authorized project: …" to each tool description; every call must pass exactly that value (`project`, or `projects` for `memory_recall`). The OMP extension blocks calls naming another project, calls from subagents, and calls after the session leaves its project. To work on another project, start the agent there.

## Diagnose

1. Service running? `systemctl --user status lambda-memory.service --no-pager`; logs with `journalctl --user -u lambda-memory -n 50`.
2. If it keeps exiting, run it in the foreground to see the configuration error:

   ```bash
   LAMBDA_MEMORY_CONFIG=~/.config/lambda-memory/runtime.json node ~/.local/share/lambda-memory/runtime/runtime/local-service.mjs
   ```

   Typical messages: `private configuration required` (file not 600), `socket path bound` (socketPath over 100 bytes), `provider key required` (openrouter mode without key). `203/EXEC` in status means the Node path in the unit is wrong.
3. Client not connected: restart the client (or `/mcp reload` in OMP) after the service is up.
4. Processing state, without side effects:

```json
{"name":"memory_status","arguments":{"project":"project-demo"}}
{"name":"memory_activity","arguments":{"project":"project-demo","operations":["dream"],"limit":20}}
```

`memory_status` shows the worker, dreaming phase, pending and blocked counts, lifecycle counts and latest outcomes. Blocked counts are subsets of pending, not extra work. `memory_activity` lists recent operation stages (seven days, pages of up to 32 via `next_cursor`). Neither calls a model or postpones dreaming.

Error meanings: JSON-RPC `-32602` is a bad tool name or bad arguments, with the reason; `isError: true` is a domain failure; a timeout is indeterminate, so check state before repeating a write. A "catch-up incomplete" recall error means preparation is still running.

## Idle processing

Dreaming runs after the project has been idle for 60 s (`LAMBDA_MEMORY_IDLE_MS`), up to 4 sources per curation call in a tree project (`LAMBDA_MEMORY_CURATION_BATCH`; flat projects use `LAMBDA_MEMORY_DREAM_BATCH`, default 8), while at least one client keeps the worker alive. Ordinary tool calls postpone it; status and activity reads do not. Limits go in the `runtimeEnv` map of `runtime.json` as strings and need a service restart.

Sources whose paid call had an unknown outcome become blocked and are never resent automatically. Only if the user accepts a possible duplicate charge, follow "Recovering blocked sources" in `docs/reference.md` (stop the service, `node runtime/recover-pending.mjs inspect`, set consent, `recover`, start the service). Never purge evidence or restore an old backup to force a retry.

## Embedding maintenance

Embedding spaces are versioned (provider, model, index revision, dimensions; currently `runtime-gemini-retrieval-v2`, 1536 dimensions, set with `LAMBDA_MEMORY_EMBEDDING_DIMENSIONS`, 128–3072). Old vectors are never reinterpreted, so after an upgrade existing active memories have no vector in the new space until it is prepared. Recall's small catch-up is not a full migration.

Only during authorized maintenance with the service stopped, and with consent for the charges: `node runtime/recover-pending.mjs inspect-index` prints the project, space and a batch limit (up to 16) with `accept_charges: false`; `prepare-index` takes that JSON on stdin with consent set and embeds one batch. For interrupted embedding calls, `inspect-embeddings` prints the plan with `accept_duplicate_charge: false` and `recover-embeddings` runs it once consent is set. Never repeat an uncertain command automatically. Full procedure: "Embedding index preparation and recovery" in `docs/reference.md`.

## Shutdown and safety

- Restart the service only for configuration changes or updates, not as a routine fix for recall.
- Closing the client ends the MCP session; admitted work finishes before the worker stops.
- Don't open the native store directly while the service runs, and don't test against real memories; use a throwaway project.
- For deletion or retention, see the lambda-memory-purge skill.
