---
name: lambda-memory-runtime
description: Use when setting up, connecting to or diagnosing the local Lambda Memory service, its MCP bridge and the OMP hooks extension; project binding, idle processing and blocked sources.
---

# Lambda Memory runtime

## How it fits together

- `lambda-memory.service` is a systemd user service sharing one native worker per project/user store across sessions.
- `~/.local/bin/lambda-memory --serve-mcp` is the bridge MCP clients launch. It authenticates privately over a Unix socket. Never launch `lambda-memory-native` on the daemon's occupied store.
- Configuration is `~/.config/lambda-memory/runtime.json` or `LAMBDA_MEMORY_CONFIG`; OpenRouter uses a private provider file too. Files/socket are 0600 in private directories. Never print tokens, keys or initialization frames. Load approved configuration through `runtime/project-binding.mjs`.
- Default data root: `~/.local/share/lambda-memory/projects`.
- OpenRouter uses pinned `anthropic/claude-haiku-5.5`, `google/gemini-embedding-2` (1536 dimensions by default) and `openai/gpt-6-luna-decisions`. Recall reports `openrouter_semantic`. Explicit `local` mode makes no model calls, extracts exact text and reports `local_lexical`; it is never provider-error fallback.

Loading a skill does not start a service, mount tools, update binaries or change OMP configuration. Find the approved executable, store and project first. Inspect only named variable presence/non-secret settings; do not search unrelated credentials. Prefer mounted tools and inspect descriptors rather than assuming a running installation was upgraded.

## Connect a client

Claude Code:

```bash
claude mcp add -s user lambda-memory -- ~/.local/bin/lambda-memory --serve-mcp
```

OMP: merge into `~/.omp/agent/mcp.json`, preserving other entries:

```json
{ "mcpServers": { "lambda-memory": { "type": "stdio", "command": "lambda-memory", "args": ["--serve-mcp"], "timeout": 660000 } } }
```

Use the installed bridge on PATH or its absolute path, leave `cwd` unset, and put no credentials in client config. `/mcp list` must show connected. Names are `mcp__lambda-memory__memory_*` in Claude Code and `mcp__lambda_memory_memory_*` in OMP; native examples use `memory_*`. The bridge supplies missing token metadata privately, never overwrites an explicitly wrong token.

The generated `~/.omp/agent/extensions/lambda-memory-local.ts` wrapper calls the installed `extensions/lambda-memory-local.mjs` default export. It provides brief, gated recall, sanitized action/conversation capture and `/lambda-memory <text>`. The MCP manager registers tools; the extension does not duplicate them. Never load this and the direct `extensions/lambda-memory.mjs` adapter together. The direct loop has a private client, not general-purpose mounted tools.

Reopen OMP after replacing the extension; `/mcp reload` suffices for config-only changes after the new extension is loaded. Bridge/extension-only updates need no daemon restart. Native/daemon updates require authorized maintenance; a repository build never updates the installed bundle.

## Hooks, health and operator steps

`memory_playbook` and playbook generation are removed. The session brief replaces them. Old `procedural:playbook` rows stay excluded from recall. Actions and exchanges now feed lessons.

Install the complete [Claude Code hooks](../../docs/reference.md#claude-code-hooks): session-start, async post-tool-use for success/failure, async user-prompt-submit, synchronous recall (timeout 5), async stop, pre-compact and async session-end. Session-start has 3 s, session-end 1.2 s, capture/compaction modes 2 s. OMP marks end after draining captures at shutdown; Codex relies on idle readiness. Install [Codex hooks](../../docs/reference.md#codex-hooks) for session-start, post-tool-use, user-prompt-submit and stop, then trust them with `/hooks`. Codex gets brief/capture, no per-prompt recall yet.

Use runtime-owned Node at `~/.local/share/lambda-memory/node/bin/node`, never system Node. Hooks fail open; session-start can report unavailable service. The main unit restarts after 3 s, limits starts to five per 300 s, and invokes `lambda-memory-failure.service` for a critical notification. Installation and checksum-verified Node steps are in the [README](../../README.md#2-install-the-runtime) and [upgrade guide](../../docs/upgrade-and-migrate.md#runtime-owned-node-and-failure-alert).

Operator steps need the user's approval, never automatic agent execution:

- Rotate any keys pasted into chats first.
- Stop the service for `node runtime/merge-identities.mjs inspect` and `apply --backup <dir>`. Save the plan privately, show costs and changed repositories, and apply the exact approved plan with only `accept_charges: true`. Sources/stores stay; timelines/review decisions remain in source stores. Restart even after failure.
- Conversation history: `node runtime/import-exchanges.mjs --agent omp|claude-code --cwd <dir> [--since <ISO>] [--include <session-hash>]` plans free with service up or down. Review flagged sessions/samples, then `--apply` with service running and exact approved plan on stdin. At pending_limit, wait for curation and obtain approval to rerun. Never automatically retry failed/uncertain work. See [phases 0–1](../../docs/upgrade-and-migrate.md#upgrade-to-learning-memory-phases-01) and [plan fields](../../docs/reference.md#identity-consolidation-and-conversation-history-import).
- Optional user-memory migration: stop for every `node runtime/promote-all.mjs` command (`plan`, `summary`, `apply`, `transfer`, `retire-plan`, `retire --confirm`). Apply uses the exact approved plan/charge consent; transfer is free, later user curation/embeddings paid. Run the service with a project session connected, wait for user pending sources to reach zero, and repeat stopped transfer/curation if any project reports remaining copies. Review a fresh retire plan and obtain separate consent: retirement queues paid parent refresh. After failure get a fresh plan/review/approval. Follow [phase 2](../../docs/upgrade-and-migrate.md#upgrade-to-learning-memory-phase-2).
- Phases 3–5 enable automatic paid lesson extraction, skill drafting, dependency refresh and reflection, plus lesson curation/embeddings, without a cap. Explain this **before restart**; the only opt-out is local mode. Optional stopped-service `recover-pending.mjs` actions are `inspect-lesson-backfill`/`lesson-backfill`, `inspect-timeline-backfill`/`timeline-backfill`, `inspect-reflection-backfill`/`reflection-backfill`. Lesson/reflection take approved plans on stdin with accept_charges true; timeline inspection/writing is free. Keep the project worker live for later extraction, finish lesson curation before scheduling reflection. Follow [phases 3–5](../../docs/upgrade-and-migrate.md#upgrade-to-learning-memory-phases-35).
- Skill review: `node runtime/skill-candidates.mjs list`, `list --user`, `show <id>`, then only on approval `approve <id> --revision <rev> --draft <hash> --confirm`. Use --user or --cwd <dir> for the same reviewed scope. Show prints both required values. Replacement keeps `.SKILL.md.lambda-backup-<pid>-<ts>` hard links; tell the user. Reject/export are available. See [skill drafts](../../docs/reference.md#skill-drafts).

Earlier `import-actions.mjs` and native `--reject-tool-metadata` are also consented operator steps; use the reference before applying. Never restore pre-paid-step data that would erase admission records.

## Memory tree

Projects have five fixed roots: Decisions, Preferences, Architecture, Open items, Lessons. User memory has Preferences, Working rules, General lessons. In tree mode one Haiku call per idle batch of up to four knowledge sources (`decision`, `exchange`, `lesson`, non-action attempt/result/error) can discard, merge, supersede, create, expand a node first, or promote general user knowledge. Only curated nodes, not raw sources, are embedded (1536 dimensions by default).

Later paid ticks refresh parents, split oversized nodes/full roots, rebuild after purge and refresh changed dependencies. Admission prefixes are `curation-job:`, `tree-refresh-job:`, `tree-split-job:`, `tree-rebuild-job:`, `tree-dependency:`. Activity uses `curation`, `tree_refresh`, `tree_split`, `tree_rebuild`. Lessons/drafts/reflection use `derivation` with `lesson-job:`, `skill-draft-job:`, `reflect-job:`. Status's tree block is `{mode, nodes_active, stale, split_queue, parked_sources}`. Uncertain calls are never automatically replayed.

Projects without active flat memories start tree mode. Flat migration is paid, approved, stopped-service maintenance with a private backup: `recover-pending.mjs inspect-tree` prints a free plan, `curate-existing` takes the approved copy with accept_charges true. `migrate-all-trees.mjs plan|summary|apply` handles registered projects with one combined plan, skips missing directories and stops at first failure. Failed migration keeps flat recall and hides the partial tree. Never retry automatically; follow the [upgrade guide](../../docs/upgrade-and-migrate.md#5-start-verify-and-plan-the-migration).

Conversation capture defaults on (absent/true); `"captureConversation": false` opts out. Installed Claude Code, Codex and OMP capture sanitized main-agent prompts/replies as exchanges. Tell the user this sends sanitized text to the configured OpenRouter curator and adds calls. The direct extension's explicit opt-in differs, as described below.

## Optional local web UI

`runtime.json.webPort` enables `http://127.0.0.1:<webPort>` with no login: integer 1024–65535, absent off. `webLeaseMs` is 5000–3600000, default 120000, controlling worker lifetime after the last UI call. Invalid settings fail closed. Install `runtime/local-service.mjs`, `runtime/project-registry.mjs`, `runtime/web-service.mjs`, `runtime/skill-install.mjs`, all `runtime/web/**`, remaining bundle and updated `extensions/lambda-memory.mjs` together; partial copies can break the daemon.

Single-user security: loopback only, peer UID checked through `/proc/net/tcp` (without it only the UI fails to start), same-origin Host/Origin/Sec-Fetch-Site, per-start `X-Lambda-Memory-CSRF` on all API calls, JSON POST bodies ≤64 KiB, no CORS, strict CSP including `font-src 'self'`. Fira Code is self-hosted. User identity appears as @user, never user:<owner> in API responses. Token/actor/provider values and owner fields are not returned; local paths remain exact and may include usernames. Same-user processes able to read the page can use it, including confirmed skill installation: do not expose the port. Restart rotates CSRF; reload tabs. Shutdown waits for in-flight calls.

Pages include the map, typed grids, tree modal (Content, Sources, History, Links), Skill drafts, Review, Timeline, Activity & diagnostics and You switch. Opening the UI never starts a worker. Start worker is explicit and may be billable. Skills require path confirmation or CLI approval with revision/hash; both refuse symlinked skill directories/SKILL.md below targets and retain replacement backups.

The UI is not your session's MCP client. It reads via daemon workers, never calls paid recall or raw capture/propose. `/api/correction` captures ui://correction evidence for paid tree curation. Flat `/api/correct` captures a ui://manual-curation decision and proposes against its predecessor; optional UUID nonce makes retries replay rather than duplicate. `/api/purge` deletes one typed-confirmed source; `/api/tool` rejects memory_purge. Non-live projects return 409 needs_start until `/api/start`; pending processing can be paid. Dashboard polling every 10 s keeps leases/workers live. Bad arguments return 400; unconfirmed failures 502 (never automatically retry writes); full pending capacity 503 (safe to retry); over four active UI leases 429. See [full UI reference](../../docs/reference.md#local-web-ui).

## Project binding

Claude Code/Codex hooks canonicalize cwd, ignore configured roots, then use the call's Git checkout. Otherwise they find the nearest CLAUDE.md, AGENTS.md or .lambda-memory at/below a usable containing session root, falling back to the root. Outside/without that root they search markers upward without the bound, otherwise use cwd. Git uses upstream then origin host/full path, credentials stripped; no remote uses canonical path. Symlinks resolve before containment. Installed OMP uses its session's Git-or-directory binding, not this marker resolver. Worktrees sharing identity share a worker.

The bridge annotates descriptors with the authorized project. Shared tools accept that project or the user store; user-store recall must name it alone. Current-project recall/brief also fan out to user and eligible workspace children, not arbitrary global search. OMP blocks subagents and every call after leaving the bound project. Reconnect after changing projects; never widen scope or stop another session's service merely to satisfy a skill.

## Build and launch when requested

For standalone native access export approved `LAMBDA_MEMORY_DATA_DIR`, `LAMBDA_MEMORY_OWNER`, `LAMBDA_MEMORY_ACTOR`, `LAMBDA_MEMORY_PROJECT`, `LAMBDA_MEMORY_TOKEN`. Token: private 32–1024 printable non-whitespace ASCII bytes through the approved secret mechanism, never pasted or logged. One native process per store. Native needs an explicit data path; Docker supplies one.

Build a provider-capable binary:

```bash
docker build --build-arg LAMBDA_FEATURES=memory-runtime,router-http -t lambda-memory:runtime .
```

For explicitly approved paid standalone MCP, with a dedicated operator-chosen volume and privately exported credentials/models:

```bash
docker run --rm -i --read-only --tmpfs /tmp:rw,nosuid,nodev \
  --cap-drop ALL --security-opt no-new-privileges \
  -v "${LAMBDA_MEMORY_VOLUME:?set a dedicated volume}:/var/lib/lambda-memory" \
  --env LAMBDA_MEMORY_OWNER --env LAMBDA_MEMORY_ACTOR \
  --env LAMBDA_MEMORY_PROJECT --env LAMBDA_MEMORY_TOKEN \
  --env LAMBDA_MEMORY_MODE=openrouter --env OPEN_ROUTER_KEY \
  --env LAMBDA_PLANNER_MODEL --env LAMBDA_EMBEDDING_MODEL --env LAMBDA_SELECTOR_MODEL \
  lambda-memory:runtime --serve-mcp
```

Use the container's `/var/lib/lambda-memory/store`, not accidentally forwarded host data paths. Forward approved limits/scope by variable name. The native MCP launch has no HTTP listener. Offline: add --network none, set LAMBDA_MEMORY_MODE=local, omit provider variables. Never change modes to conceal errors. Missing features/credentials or non-pinned models fail closed; a feature flag alone is not paid consent. Absent mode is offline.

Native `--serve-mcp` uses `LAMBDA_MEMORY_BIN` (otherwise lambda-memory on PATH). Authenticate initialize.params._meta.lambdaMemoryToken then notifications/initialized before calls. Generic clients need that capability unless using the bridge.

## One-shot access when tools are missing

Within the approved project/provider/data boundary, run this Node client from the repository root with a companion skill's `{name, arguments}` on stdin, not content in argv. For shared service set LAMBDA_MEMORY_CONFIG and, if different from the code directory, LAMBDA_MEMORY_CWD to the target. This safely coexists with OMP. Otherwise it uses exported native settings/LAMBDA_MEMORY_BIN and needs an unoccupied store. Normal capture/recall needs no redundant approval; deletion/scope changes keep separate consent.

```javascript
import { NativeMcpClient } from './extensions/lambda-memory.mjs';
import { loadConfig, resolveProjectBinding, clientEnvironment } from './runtime/project-binding.mjs';
let client;
try {
  let input = '';
  process.stdin.setEncoding('utf8');
  for await (const chunk of process.stdin) {
    input += chunk;
    if (Buffer.byteLength(input, 'utf8') > 65536) throw new Error('request bound');
  }
  const request = JSON.parse(input);
  const config = process.env.LAMBDA_MEMORY_CONFIG ? loadConfig() : null;
  const cwd = process.env.LAMBDA_MEMORY_CWD || process.cwd();
  const options = config ? { env: clientEnvironment(config, resolveProjectBinding(cwd)), cwd } : {};
  client = new NativeMcpClient(options);
  const result = await client.callTool(request.name, request.arguments);
  if (result?.isError) throw new Error('request rejected');
  const text = result.content.filter(part => part.type === 'text').map(part => part.text).join('\n');
  console.log(JSON.stringify(JSON.parse(text)));
} catch {
  console.error('Lambda Memory request failed; persistence is not confirmed.');
  process.exitCode = 2;
} finally {
  if (client) await client.dispose();
}
```

Use a temporary root-level .mjs file or node --input-type=module -e; inspect complete stdout, exit status and payload, then remove temporary scripts. Never dump arguments/environment to debug generic errors or automatically retry indeterminate mutations. OpenRouter clients allow 660 s per request and 630 s for admitted work at EOF; offline bounds are shorter. These are technical deadlines, not financial caps.

Automatic gated recall has a separate 3.5 s end-to-end deadline including readiness. It warns/proceeds without late context, never retries or overlaps admitted automatic recalls. Late readiness cannot admit work. Explicit tools/captures retain longer deadlines. Raising MCP timeout does not repair a handler watchdog; inspect provider/preparation stages and provider-free status latency instead of purging/resetting markers.

## Direct OMP loop opt-in

Only for a requested per-invocation binding, export exact LAMBDA_MEMORY_CWD and runtime variables:

```bash
omp -e /absolute/path/to/lambda-memory/extensions/lambda-memory.mjs
```

Replace the example path. It launches its own child, injects brief/gated recall, captures sanitized actions and supports /lambda-memory notes. Conversation capture requires its option `captureConversation: true`. Other cwd bindings/subagents are excluded. OMP can relocate HOME launches; verify actual cwd, never weaken binding. Do not load both adapters.

## Idle processing and limits

Defaults: 8192 content bytes, 1024 unconsumed curatable sources/project, eight candidates (OpenRouter maximum eight/project; offline maximum 32), 8192 context bytes, flat dream batch eight, idle 60000 ms, tick 1000 ms. Tree curation defaults to four sources. CONTEXT_BYTES is 1–32768; EMBEDDING_DIMENSIONS 128–3072, default 1536. Removed CONTEXT_TOKENS/budget_tokens are rejected; upgrade native and clients together. Direct native mode allows up to 16 explicitly authorized projects; shared service uses its bound project and scoped fan-out. Invalid numbers fail closed. Retention is indefinite unless positive LAMBDA_MEMORY_RETENTION_MS is configured.

Use explicit string limits in runtimeEnv, preserving secrets/0600 and restarting only with authorization. Identity/path/token/provider overrides are rejected; inherited shell settings do not silently change project policy. Provider files accept only OPEN_ROUTER_KEY, three pinned model variables and optional LAMBDA_MEMORY_ROUTER_BASE_URL (HTTPS or explicit-port loopback HTTP). The base/key never go to clients. See [settings and URL rules](../../docs/reference.md#configuration).

Flat OpenRouter dreaming derives one claim/source with strict required slots, preserves outcomes, embeds and publishes. Proposals prepare immediately. Recall does bounded preparation/backfill then Haiku planning, Gemini embeddings, native fusion and Luna selection. Unfinished catch-up is an error; processing is not human approval, active is not truth, all claims are provisional. Never request review as a normal recall gate. Malformed Haiku JSON/slots remain failure; never relax validation or replay uncertain work.

Ordinary foreground calls, even empty recall/list, postpone idle work. Status/activity do not prepare, backfill, clean history or autocapture outcomes. Admitted work can finish during activity only if sources remain live/unchanged. Already-admitted originals are excluded before batch limits so they cannot starve unrelated sources. Persisted outputs finish preparation without repeating derivation. New provider failure/uncertain admission stops provider background work but keeps MCP available. Storage/conflict/input maintenance errors back off up to 64 ticks. Provably uncharged refusals (offline/401/402/403/429) release admissions and back off; unknown embedding outcomes need explicit recovery.

Connected clients keep workers alive for dreaming. Last-client close waits for admitted work before release. A one-shot process has no idle worker after exit; use a resident connection or proposal. Do not enable PrivateTmp for the service: it must see actual client checkouts, including temporary ones. Restart only for authorized maintenance, not routine recall repair. Paid admissions prevent replay, not reserve money; there is no financial cap or spending ledger.

**Old ai-memory data:** preserved separately, not imported automatically or erased.

## Recovery and embedding maintenance

For unsettled sources, obtain explicit possible-duplicate-charge consent. Stop the shared service and retain a private backup. The helper accepts inactive/failed units, refuses running ones, and passes approved private configuration without printing credentials. Set LAMBDA_MEMORY_CWD if outside the target project; use the repository helper or installed runtime/runtime/recover-pending.mjs.

```bash
systemctl --user stop lambda-memory.service
node runtime/recover-pending.mjs inspect
# Save exact plan privately; change only accept_duplicate_charge after approval.
node runtime/recover-pending.mjs recover < approved-recovery.json
systemctl --user start lambda-memory.service
```

Inspect is free, consent false, exact project/source/admissions. Recovery checks immutable live originals and the exact admission set transactionally, retains old markers and appends dream-job:recovery: operator/time lineage. Success derives/indexes/publishes; new uncertainty requires fresh approval. Native flags: --inspect-dream-recovery/--recover-dream-sources with a single-project operator environment. Restart even after failure, reset-failed if needed, then inspect diagnostics without retrying. Never restore a pre-call datastore backup or purge/reset markers to force replay.

Embedding spaces include provider/model/index revision/dimensions and scoped revision identity; current profile is runtime-gemini-retrieval-v2 with asymmetric query/document prefixes. Never reinterpret vectors or change markers. Approved stopped-service inspect-index prints exact project/space, limit ≤16, accept_charges false; prepare-index reads the approved plan and embeds one active-revision batch. A separately approved zero-count result proves completion at that moment; recall catch-up is not whole-store migration.

inspect-embeddings prints immutable snapshot digests and prior admissions with accept_duplicate_charge false; recover-embeddings takes the unchanged consented plan, preserves old markers/lineage and rejects stale/replayed plans across restarts. Native flags: --inspect-embedding-index, --prepare-embedding-index, --inspect-embedding-recovery, --recover-embeddings. Never replay uncertain commands automatically; restart after maintenance and keep new admissions. See [recovery](../../docs/reference.md#recovering-blocked-pending-sources) and [embedding details](../../docs/reference.md#embedding-index-preparation-and-recovery).

## Diagnose and finish

1. Check service: `systemctl --user status lambda-memory.service --no-pager`; logs: `journalctl --user -u lambda-memory -n 50`.
2. With the service stopped, foreground startup can show config errors:

   ```bash
   LAMBDA_MEMORY_CONFIG=~/.config/lambda-memory/runtime.json ~/.local/share/lambda-memory/node/bin/node ~/.local/share/lambda-memory/runtime/runtime/local-service.mjs
   ```

   private configuration required means permissions; socket path bound means over 100 bytes; provider key required means missing OpenRouter key. 203/EXEC means the runtime-owned Node is missing/non-executable. Preserve secrets while diagnosing.
3. Confirm cwd, permissions, sole store ownership and authenticated initialized phase. Restart/reload clients after service readiness.
4. Inspect processing without side effects:

   ```json
   {"name":"memory_status","arguments":{"project":"demo-project"}}
   {"name":"memory_activity","arguments":{"project":"demo-project","operations":["dream"],"limit":20}}
   ```

Status gives actual worker/idle/dream phase, pending/blocked subsets, lifecycle/conflicts, latest outcomes, coverage and health. No latest outcome is not an all-time claim. Activity has operation/stage, timestamps/effect counts, no evidence/query/credential/provider bodies. Pages default 20, maximum 32, next_cursor with unchanged filters. History is seven days/project with no count cap, separate from retention; running trees survive and orphaned work becomes indeterminate, never fabricated success/retry authority. Coverage starts with instrumentation.

Diagnostic reads do not record operations or postpone idle work, though a cold bridge can start a worker. Read errors are explicit, not healthy zeros. final_record_unavailable preserves the known ordinary result; report bookkeeping failure without replay. Purge removes linked history; expiry never resets paid markers.

Unknown tool names/bad arguments return JSON-RPC -32602, not uncertain-write failure. Domain errors use isError. Timeout means unknown completion; do not repeat blindly. tools/list proves readiness only. If authorized, test synthetic capture/publication/recall/purge in a dedicated store without review, never real evidence. Outage means no memory, not fake success or local fallback. EOF ends the session and finishes bounded work; respect OMP's shutdown deadline and leave no orphan processes. Report tested commands, actual results and blockers. For authorized deletion/retention see the [purge skill](../lambda-memory-purge/SKILL.md).
