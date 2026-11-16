<p align="center">
  <img src="assets/banner.png" alt="λambda memory — source-bound memory, scoped recall, traceable context" width="100%">
</p>

<h1 align="center">λambda memory</h1>

<p align="center">
  <strong>Keep the evidence. Recall what belongs.</strong><br>
  Local, source-bound memory for coding agents (Claude Code, Codex, OMP, any MCP client).
</p>

<p align="center">
  <a href="#how-it-works">How it works</a> ·
  <a href="#models-and-cost">Models &amp; cost</a> ·
  <a href="#install">Install</a> ·
  <a href="#privacy">Privacy</a> ·
  <a href="#using-it">Using it</a> ·
  <a href="#repository-layout">Layout</a> ·
  <a href="docs/reference.md">Full reference</a>
</p>

<p align="center"><sub>Rust · embedded SurrealKV · OpenRouter · MCP over stdio</sub></p>

<p align="center">
  <a href="https://github.com/nicolasfmelo/lambda-memory/actions/workflows/ci.yml"><img src="https://github.com/nicolasfmelo/lambda-memory/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue.svg" alt="MIT license"></a>
</p>

> **Status:** early release (0.5.0). Single user, Linux with systemd only.

<p align="center">
  <a href="https://github.com/nicolasfmelo/lambda-memory/releases/download/v0.5.0/lambda-memory-v0.5.0.mp4"><img src="assets/lambda-memory.gif" alt="λambda memory v0.5.0: the session brief and gated recall, lessons and skill drafts, the user store, validity and the decision timeline, observed versus concluded memories, and the new memory map UI" width="100%"></a>
  <br>
  <sub><a href="https://github.com/nicolasfmelo/lambda-memory/releases/download/v0.5.0/lambda-memory-v0.5.0.mp4">▶ Watch it with music (2:02)</a> · learning memory and the memory map UI</sub>
</p>

---

## What it is

- **Your agent saves evidence**, not summaries: decisions, results, errors and attempts are stored as immutable *sources*, each with its origin.
- **Memories are curated into a tree.** While the agent is idle, a background step called *dreaming* files useful evidence under Decisions, Preferences, Architecture, Open items and Lessons. Compatible knowledge is merged; a changed position gets a new version. Every node links to its original messages and keeps its history.
- **Recall is scoped and traceable.** The agent gets a small context from the project, user store and eligible workspace children, with paths and source attribution. `memory_node` shows the original messages behind a node. The agent decides whether to use it.
- **It learns from sessions.** Exchanges and action episodes feed lessons. Reinforced strategies can become skill drafts for you to review and install. A separate user store keeps Preferences, Working rules and General lessons across projects.
- **You can curate it.** A local web page lets you browse memories, send corrections, review skill drafts, keep a decision timeline and delete a source. Observed evidence and inferred conclusions are labeled separately.

Everything runs on your machine: one local service and one embedded database per project, plus the user store. The only network calls are the model calls to **OpenRouter** in `openrouter` mode; `local` mode makes none (see [Privacy](#privacy)).

## How it works

```
 agent ── memory_capture ──▶  sources (immutable, with origin)
                                   │  project idle ≥ 60 s
                                   ▼
              dreaming: Haiku curates source batches into tree nodes
                                   │
              Gemini embeds the node ─▶ node published (active)

 agent ── memory_recall ───▶  Haiku plans the query · Gemini embeds it
                              scoped lexical + vector search
                              Luna keeps relevant nodes, then descends
                                   │
                                   ▼
                       small context, every claim with its sources
```

| Piece | What it does |
|---|---|
| Native binary (Rust) | Store, lifecycle rules, retrieval and MCP tools. One process per store. |
| Local service (Node) | Systemd user service. Shares project and user workers across agent sessions. |
| `lambda-memory` bridge | What MCP clients launch. Connects to the service and authenticates for them. |
| Web UI | Optional page on `127.0.0.1` for browsing and curating memories. |
| OMP extension | Optional hooks for [OMP](https://github.com/can1357/oh-my-pi): session brief, gated recall and automatic capture. |
| Agent hooks | `runtime/agent-hook.mjs` for Claude Code and Codex: session brief and capture. Claude Code also gets gated recall. |

The session brief replaces the removed `memory_playbook` tool. Two modes: **`openrouter`** (uses the models below) and **`local`** (offline, exact-text extraction and lexical recall, no model calls or cost). The install below starts in `local` mode.

## Models and cost

The `openrouter` mode uses three pinned models. Other IDs are rejected.

| Role | Model |
|---|---|
| Curates sources; plans queries; extracts lessons, drafts skills and reflects | `anthropic/claude-haiku-5.5` |
| Embeddings (1536 dimensions by default; configurable) | `google/gemini-embedding-2` |
| Picks relevant memories at recall | `openai/gpt-6-luna-decisions` |

**Historical usage.** One week of daily use by the author (October 2026), across all projects:

| Model | Requests | Tokens | Spend |
|---|---:|---:|---:|
| Claude Haiku 5.5 | 489 | 3.33 M | US$ 0.45 |
| GPT-6 Luna Decisions | 245 | 1.02 M | US$ 0.10 |
| Gemini Embedding 2 | 1,940 | 169 K | US$ 0.04 |
| **Total** | **2.7 K** | **4.5 M** | **US$ 0.59** |

That week included a day of catching up on an old backlog. At that rate it would be about US$ 2.50 per month. These are historical figures, not a current-runtime budget.

| Operation | Model calls | Cost |
|---|---|---|
| Recall | Haiku + embedding + up to 2 Luna selections per store | Historical estimate: ≈ US$ 0.0013 |
| Tree curation, per batch of up to 4 sources | 1 Haiku + node embeddings | Varies |
| Capture, list, status, timeline, web UI reads | none | free |

| Historical usage estimate | Per day | Per month |
|---|---|---|
| Light: 10 recalls, 20 captured events | ≈ US$ 0.02 | ≈ US$ 0.50 |
| Heavy: 50 recalls, 100 captured events | ≈ US$ 0.08 | ≈ US$ 2.40 |

These figures predate lessons, skill drafts, dependency refreshes, reflection and multi-store recall. They are **not estimates for 0.5.0**. OpenRouter mode automatically makes paid background calls for those jobs, plus lesson curation and embeddings. The only opt-out from these jobs is explicit `local` mode. Cost grows with text length. There is **no spending cap** in the resident service; watch your OpenRouter dashboard.

## Install

> **Coding agents: upgrading an existing installation?** Follow [docs/upgrade-and-migrate.md](docs/upgrade-and-migrate.md). It covers the 0.5.0 learning-memory upgrade, backups, optional history/user-memory backfills and migration of old flat memories into the tree. Show the user the cost summary and get an explicit yes before any paid step. For a fresh install, continue below.

### Prerequisites

- Linux with a systemd user session (`systemctl --user` works)
- Runtime-owned Node.js 22 (installed below, separate from system Node)
- git (a project's identity comes from its Git remote)
- openssl (to generate the token), curl, tar and sha256sum
- Docker, or Rust 1.88 or newer, to build the native binary
- `~/.local/bin` on your `PATH`
- For `openrouter` mode only: an [OpenRouter](https://openrouter.ai) API key

The snippets below are bash. If your shell is fish, run `bash` first. Run them from the root of a clone of this repository.

### 1. Build the binary

**With Docker.** This runs the test suite and then compiles an optimized binary. From scratch it takes about 10 minutes on a 16-core machine. The optional `--build-arg RUN_TESTS=0` skips build-time tests; leave it out for verification.

```bash
docker build --build-arg LAMBDA_FEATURES=memory-runtime,router-http -t lambda-memory:runtime .
docker create --name lm-extract lambda-memory:runtime
docker cp lm-extract:/usr/local/bin/lambda-memory ./lambda-memory-native
docker rm lm-extract
```

**With Rust installed** (about 5 minutes from scratch):

```bash
cargo build --release --locked --features memory-runtime,router-http
cp target/release/lambda-memory ./lambda-memory-native
```

### 2. Install the runtime

Run these commands in order and stop if any command fails, especially the checksum verification. The download below is for Linux x64.

```bash
APP=~/.local/share/lambda-memory
mkdir -p $APP/bin $APP/runtime ~/.local/bin ~/.config/lambda-memory ~/.config/systemd/user
install -m 755 lambda-memory-native $APP/bin/
cp -r runtime extensions $APP/runtime/

# install the runtime-owned Node 22
NODE_DIST=https://nodejs.org/dist/latest-v22.x
NODE_TAR=$(curl -fsSL "$NODE_DIST/SHASUMS256.txt" | awk '/linux-x64\.tar\.xz$/ {print $2}')
curl -fsSLO "$NODE_DIST/$NODE_TAR"
curl -fsSL "$NODE_DIST/SHASUMS256.txt" | grep " $NODE_TAR\$" | sha256sum -c -
mkdir -p "$APP/node" && tar -xJf "$NODE_TAR" -C "$APP/node" --strip-components=1 && rm "$NODE_TAR"

# the bridge MCP clients will launch
printf '#!/bin/sh\nexec %s/node/bin/node %s/runtime/runtime/stdio.mjs "$@"\n' "$APP" "$APP" > ~/.local/bin/lambda-memory
chmod +x ~/.local/bin/lambda-memory
```

A self-managed Node 22 that system upgrades never touch may be symlinked to `$APP/node` instead. The runtime never uses the system `node`.

### 3. Configure

Start in `local` mode: no API key, no cost, no network calls.

```bash
umask 077
cat > ~/.config/lambda-memory/runtime.json <<EOF
{
  "nativeBin": "$HOME/.local/share/lambda-memory/bin/lambda-memory-native",
  "bridgeBin": "$HOME/.local/bin/lambda-memory",
  "dataDir": "$HOME/.local/share/lambda-memory/projects",
  "socketPath": "$XDG_RUNTIME_DIR/lambda-memory/service.sock",
  "owner": "$USER",
  "actor": "$USER",
  "token": "$(openssl rand -hex 32)",
  "mode": "local",
  "webPort": 7731
}
EOF
```

`socketPath` must be at most 100 bytes; `$XDG_RUNTIME_DIR` (usually `/run/user/<uid>`) keeps it short. To run without the [web UI](#web-ui), remove the `webPort` line and the comma before it.

To switch to `openrouter` mode, create the provider file and point `runtime.json` at it:

```bash
umask 077
cat > ~/.config/lambda-memory/openrouter.env <<'EOF'
OPEN_ROUTER_KEY=sk-or-...
LAMBDA_PLANNER_MODEL=anthropic/claude-haiku-5.5
LAMBDA_EMBEDDING_MODEL=google/gemini-embedding-2
LAMBDA_SELECTOR_MODEL=openai/gpt-6-luna-decisions
EOF
sed -i "s#\"mode\": \"local\",#\"mode\": \"openrouter\",\n  \"providerEnvFile\": \"$HOME/.config/lambda-memory/openrouter.env\",#" \
  ~/.config/lambda-memory/runtime.json
```

Replace `sk-or-...` with your key. If the service is already running, restart it with `systemctl --user restart lambda-memory.service`. This enables automatic paid background jobs without a spending cap. Finally, make sure both files are private:

```bash
chmod 600 ~/.config/lambda-memory/*
```

The service refuses to start if either file can be read by other users. Settings are in the [reference](docs/reference.md#configuration).

### 4. Start the service

The service uses runtime-owned Node at `~/.local/share/lambda-memory/node/bin/node`. The main unit restarts on failure, limits starts to five per 300 seconds, and invokes the failure unit for a critical desktop notification.

```bash
cp runtime/lambda-memory.service runtime/lambda-memory-failure.service ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable --now lambda-memory.service
systemctl --user status lambda-memory.service --no-pager
```

### 5. Connect your agent

**Claude Code:**

```bash
claude mcp add -s user lambda-memory -- ~/.local/bin/lambda-memory --serve-mcp
mkdir -p ~/.claude/skills && cp -r skills/* ~/.claude/skills/   # optional usage guides
```

Optional [Claude Code hooks](docs/reference.md#claude-code-hooks) inject the session brief and gated recall, record actions and exchanges, and signal session end.

**Codex:** register the same bridge as an MCP server. Optional [Codex hooks](docs/reference.md#codex-hooks) inject the brief and record shell, `apply_patch` and conversation turns. Trust them with `/hooks`. Codex has no per-prompt recall yet.

**Conversation capture is on by default.** Installed Claude Code, Codex and OMP integrations record sanitized main-agent prompts and final replies. The configured provider curates them, adding calls. Set `"captureConversation": false` in `runtime.json` to opt out; that does not purge earlier sources. See [automatic recall and capture](docs/reference.md#automatic-recall-and-conversation-capture).

**[OMP](https://github.com/can1357/oh-my-pi):** merge into `~/.omp/agent/mcp.json`, then check `/mcp list`:

```json
{ "mcpServers": { "lambda-memory": { "type": "stdio", "command": "lambda-memory", "args": ["--serve-mcp"], "timeout": 660000 } } }
```

Optionally add the hooks extension for the session brief, gated recall and automatic action/conversation capture:

```bash
mkdir -p ~/.omp/agent/extensions
cat > ~/.omp/agent/extensions/lambda-memory-local.ts <<EOF
import installLocal from '$HOME/.local/share/lambda-memory/runtime/extensions/lambda-memory-local.mjs';
export default function lambdaMemoryLocal(api) { return installLocal(api); }
EOF
```

Never load this and `extensions/lambda-memory.mjs` directly in the same session. Reopen OMP after updating the extension.

Each agent session is bound to its project. The bridge adds the identifier to tool descriptions; calls must name it (or the user store where supported). See [project binding and ignored roots](docs/reference.md#project-binding).

### Verify

```bash
curl -s -o /dev/null -w '%{http_code}\n' http://127.0.0.1:7731/   # 200 if webPort is set
journalctl --user -u lambda-memory -n 50                          # service logs
```

Inside a project, ask your agent to run `memory_status`. It should report the worker and its backlog.

### Update

A repository build does not update the installed service. Upgrade the native binary and complete JavaScript bundle together, with a stopped-service private backup; follow the [upgrade guide](docs/upgrade-and-migrate.md), or ask your coding agent to.

0.5.0 adds learning memory, user/workspace recall and the memory map UI. Conversation capture defaults on. Explain automatic paid jobs before restarting in OpenRouter mode. Optional history, user-memory, lesson and reflection backfills require separate consent. Projects without active flat memories start in tree mode; existing flat projects keep working until a paid, approved migration.

From 0.1.0, recall changes from `budget_tokens` to `budget_bytes` (`LAMBDA_MEMORY_CONTEXT_BYTES`). Embeddings use a new 1536-dimension space by default; old memories stay intact but need new vectors. See [embedding index preparation](docs/reference.md#embedding-index-preparation-and-recovery).

### Docker only, without installing

The image can serve MCP directly for one project, useful for testing or a custom client:

```bash
TOKEN=$(openssl rand -hex 32)
docker run -i --rm -v lm-data:/var/lib/lambda-memory \
  -e LAMBDA_MEMORY_MODE=local -e LAMBDA_MEMORY_OWNER=me -e LAMBDA_MEMORY_ACTOR=me \
  -e LAMBDA_MEMORY_PROJECT=demo-project -e LAMBDA_MEMORY_TOKEN=$TOKEN \
  lambda-memory:runtime --serve-mcp
```

The client must send the token as `params._meta.lambdaMemoryToken` in `initialize`. Generic MCP clients should use the bridge, which handles authentication for them.

## Using it

### MCP tools

| Tool | Use it to |
|---|---|
| `memory_capture` | Save a decision, result, error, attempt or action episode as evidence |
| `memory_propose` | Publish a source-backed memory or correction right away |
| `memory_recall` | Ask a question and get scoped, source-attributed context |
| `memory_list` | Browse memories by state, with lineage and history |
| `memory_review` | Reject an unpublished contender, declare a conflict or resolve one (optional) |
| `memory_timeline_write` | Record, revise or retract a curated decision |
| `memory_timeline` | Read the timeline and decisions not yet on it |
| `memory_status` | See whether dreaming is running, what is pending or blocked |
| `memory_activity` | Read the last 7 days of operations |
| `memory_node` | Inspect a tree node, its history, links and sources |
| `memory_brief` | Read the session index and pending skill draft count |
| `memory_session_end` | Mark an existing session ended |
| `memory_purge` | Delete a source and everything derived from it |

Skill candidate operator tools and UI-only read tools are described in the [reference](docs/reference.md#resident-mcp-and-omp).

### Web UI

With `webPort` set, open `http://127.0.0.1:7731` (or your configured port). The loopback-only UI needs no login or build step; it uses one vendored Preact/htm module and self-hosted Fira Code. The memory map opens typed grids and a modal with Content, Sources, History and Links. Browse the project or switch to You (`@user`); review lifecycle queues, skill drafts, the decision timeline, and activity and diagnostics. `active` means available for recall, not verified truth; `processing` means unfinished preparation, not an approval queue.

Send correction on a tree memory captures evidence for later curation (billable in OpenRouter mode); flat memories have their own Correct action. Purge one source from the modal's Sources tab with typed confirmation. Skill installation requires confirmation of the exact paths; the CLI remains available. Opening the UI does not start a worker: Start worker is explicit and may be billable. Themes are Auto, Light and Dark.

The user identity (`user:<owner>`) appears as `@user` in the browser and never in API responses. Token, actor and provider values are never returned; owner is never returned as a field. Local paths are shown as they are on disk and may include your username. See the [security model, routes and install bundle](docs/reference.md#local-web-ui).

Screenshots: [map](assets/screenshots/map-1440-light.png), [decisions](assets/screenshots/decisions-1440-light.png), [lessons](assets/screenshots/lessons-1440-light.png), [memory modal](assets/screenshots/modal-content-1440-light.png), [skill confirmation](assets/screenshots/skill-confirmation-1440-light.png), [review](assets/screenshots/review-1440-light.png), [timeline](assets/screenshots/timeline-1440-light.png); [all light/dark and mobile captures](assets/screenshots/).

### Skills

[`skills/`](skills/) has six guides: recall, capture, review, timeline, purge and runtime. Loading a skill does not start the service or mount tools.

## Privacy

In `local` mode nothing leaves your machine.

In `openrouter` mode these go to [OpenRouter](https://openrouter.ai), which forwards them to model providers:

- **Curation and learning:** captured text, sanitized exchanges and action digests go to Haiku for curation, lessons, skill drafts, dependency refreshes and reflection. Curated nodes go to Gemini for embeddings.
- **Recall:** your question goes to Haiku and Gemini; the question and candidate memories go to Luna. Current-project recall can also query the user store and eligible workspace children.
- **Corrections and proposals:** source-backed proposals are embedded with Gemini. UI tree corrections are captured for later paid curation.

OpenRouter's and the providers' data policies apply. λambda memory does not request zero-data-retention routing.

Default data lives under `~/.local/share/lambda-memory`, configuration under `~/.config/lambda-memory`. Capture, list, status, timeline and UI reads make no model calls. Starting a worker or installing a correction can lead to paid processing. Never capture secrets; captured text can be sent to models when background work runs.

## Troubleshooting

**The service exits or keeps restarting.** With the service stopped, run it in the foreground to see the error:

```bash
LAMBDA_MEMORY_CONFIG=~/.config/lambda-memory/runtime.json \
  ~/.local/share/lambda-memory/node/bin/node ~/.local/share/lambda-memory/runtime/runtime/local-service.mjs
```

Common causes:

- `private configuration required`: a config file is readable by other users. Run `chmod 600 ~/.config/lambda-memory/*`.
- `socket path bound`: `socketPath` is longer than 100 bytes.
- `provider key required` or `private provider environment required`: OpenRouter mode lacks a valid provider file or key.
- `status=203/EXEC`: runtime-owned Node is missing or not executable. Check `~/.local/share/lambda-memory/node/bin/node` and repeat the verified install steps, then `systemctl --user reset-failed lambda-memory.service` before starting.

**The MCP server is not connected.** Check `systemctl --user status lambda-memory.service`, then restart your agent (or `/mcp reload` in OMP).

**Tool errors mention `project`.** Pass the exact project shown in the tool description. For a different project, start the agent there. User-store calls have the separate service routing described in the reference.

More in [read-only processing diagnostics](docs/reference.md#read-only-processing-diagnostics).

## Uninstall

```bash
systemctl --user disable --now lambda-memory.service
rm ~/.config/systemd/user/lambda-memory.service ~/.config/systemd/user/lambda-memory-failure.service
systemctl --user daemon-reload
claude mcp remove lambda-memory -s user
rm -f ~/.omp/agent/extensions/lambda-memory-local.ts   # also remove the entry from ~/.omp/agent/mcp.json
rm -f ~/.local/bin/lambda-memory
rm -rf ~/.config/lambda-memory
rm -rf ~/.local/share/lambda-memory   # deletes all memories
```

Remove the installed hooks from Claude Code and Codex settings too. Removing `~/.local/share/lambda-memory` deletes every source and memory for all projects and the user store. Back it up first if you might want them again.

## Repository layout

```
src/
  domain_models/   types: sources, revisions, timeline, diagnostics
  engines/         pure rules: validity, scope, ranking, context budget
  integrations/    embedded database, OpenRouter HTTP
  services/        capture, dreaming, recall, timeline orchestration
  delivery/        MCP server and CLI
migrations/        database schema (compiled into the binary)
runtime/           local service, bridge, project binding, systemd units, agent hooks
runtime/web/       web UI (no build step; vendored Preact/htm and self-hosted Fira Code)
extensions/        OMP extension (automatic recall and capture hooks)
skills/            agent usage guides
tests/             Rust and Node test suites
docs/              architecture, reference, upgrade guide and background research
scripts/           architecture boundary check
```

The code follows the [MASA framework](https://www.masa-framework.org/): five layers with one-way dependencies. [MASA.md](MASA.md) maps them to Rust; `scripts/check_layers.py` checks the boundaries.

## Development

```bash
cargo test --all-features          # Rust tests
node --test tests/*.test.mjs       # service, bridge and web UI tests
python3 scripts/check_layers.py    # architecture boundaries
cargo fmt --check                  # formatting
```

See [CONTRIBUTING.md](CONTRIBUTING.md). Security issues: [SECURITY.md](SECURITY.md).

## More

- [Reference](docs/reference.md): settings, limits, tools, learning memory, hooks, diagnostics, purge, recovery and web UI security.
- [Architecture](docs/architecture.md): pipeline, store, dreaming, recall and provider boundary.
- [Memory architecture research](docs/research/memory-architecture.md): background research.
- [Changelog](CHANGELOG.md)

## License

[MIT](LICENSE) © 2026 Nicolas Melo

<p align="center"><sub>Evidence before confidence.</sub></p>
