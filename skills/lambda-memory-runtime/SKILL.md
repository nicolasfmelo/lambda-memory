---
name: lambda-memory-runtime
description: Use when setting up, connecting to or diagnosing the local Lambda Memory service, its MCP bridge and the OMP hooks extension; project binding, idle processing and blocked sources.
---

# Lambda Memory runtime

## How it fits together

- `lambda-memory.service`: systemd user service (Node) that starts one native worker per project and shares it between sessions.
- `~/.local/bin/lambda-memory --serve-mcp`: the bridge that MCP clients launch. It connects to the service over a private Unix socket and authenticates for the client. Never point a client at the native binary (`lambda-memory-native`).
- Configuration: `~/.config/lambda-memory/runtime.json` (or `LAMBDA_MEMORY_CONFIG`) and, in `openrouter` mode, a provider file. Both must be mode 600. Never print the token or API key.
- Data: `~/.local/share/lambda-memory/projects`, one store per project.
- Modes: `openrouter` (Haiku, Gemini embeddings, Luna; billable, no spending cap) or `local` (offline, no model calls). Local is never used as a fallback after provider errors.

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

Optional OMP hooks: `~/.omp/agent/extensions/lambda-memory-local.ts` re-exports the installed `extensions/lambda-memory-local.mjs`. It recalls before each main-agent turn (25-second limit), captures safe metadata after work, and adds `/lambda-memory <text>`. It does not register tools. Never also load `extensions/lambda-memory.mjs` in the same session. Reopen OMP after updating the extension; `/mcp reload` is enough for config changes.

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

Dreaming runs after the project has been idle for 60 s (`LAMBDA_MEMORY_IDLE_MS`), up to 8 sources per batch (`LAMBDA_MEMORY_DREAM_BATCH`), while at least one client keeps the worker alive. Ordinary tool calls postpone it; status and activity reads do not. Limits go in the `runtimeEnv` map of `runtime.json` as strings and need a service restart.

Sources whose paid call had an unknown outcome become blocked and are never resent automatically. Only if the user accepts a possible duplicate charge, follow "Recovering blocked sources" in `docs/reference.md` (stop the service, `node runtime/recover-pending.mjs inspect`, set consent, `recover`, start the service). Never purge evidence or restore an old backup to force a retry.

## Shutdown and safety

- Restart the service only for configuration changes or updates, not as a routine fix for recall.
- Closing the client ends the MCP session; admitted work finishes before the worker stops.
- Don't open the native store directly while the service runs, and don't test against real memories; use a throwaway project.
- For deletion or retention, see the lambda-memory-purge skill.
