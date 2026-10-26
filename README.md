<p align="center">
  <img src="assets/banner.png" alt="λambda memory — source-bound memory, scoped recall, traceable context" width="100%">
</p>

<h1 align="center">λambda memory</h1>

<p align="center">
  <strong>Keep the evidence. Recall what belongs.</strong><br>
  Local, source-bound memory for coding agents (Claude Code, OMP, any MCP client).
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

> **Status:** early release (0.3.0). Single user, Linux with systemd only.

<p align="center">
  <a href="https://github.com/nicolasfmelo/lambda-memory/releases/download/v0.1.0/lambda-memory.mp4"><img src="assets/lambda-memory-poster.png" alt="λambda memory — watch the film" width="100%"></a>
  <br>
  <sub><a href="https://github.com/nicolasfmelo/lambda-memory/releases/download/v0.1.0/lambda-memory.mp4">▶ Watch the film (3:30)</a> · features, dreaming, recall and curation in the web UI</sub>
</p>

---

## What it is

- **Your agent saves evidence**, not summaries: decisions, results, errors and attempts are stored as immutable *sources*, each with its origin.
- **Memories are built from that evidence.** While the agent is idle, a background step we call *dreaming* turns new sources into short memories that cite them. Corrections create a new version; nothing is overwritten.
- **Recall is scoped and traceable.** The agent asks a question and gets a small context from the current project only, with the source of every claim. The agent decides whether to use it.
- **It learns how the project works.** Tool calls are recorded as small action episodes. While idle, they become a short playbook of the commands, known error fixes and often-edited files of the project, with every line citing its facts. OMP and Claude Code inject it at the start of a session, and agents can read it with `memory_playbook` ([details](docs/reference.md#procedural-playbook)).
- **You can curate it.** A local web page lets you browse memories, fix one, keep a decision timeline and delete a source.

Everything runs on your machine: one local service and one embedded database per project. The only network calls are the model calls to **OpenRouter** in `openrouter` mode; `local` mode makes none (see [Privacy](#privacy)).

## How it works

```
 agent ── memory_capture ──▶  sources (immutable, with origin)
                                   │  project idle ≥ 60 s
                                   ▼
                    dreaming: Haiku drafts one claim per source
                                   │
                    Gemini embeds it ─▶ memory published (active)

 agent ── memory_recall ───▶  Haiku plans the query · Gemini embeds it
                              lexical + vector search in this project
                              Luna keeps only what is relevant
                                   │
                                   ▼
                       small context, every claim with its sources
```

| Piece | What it does |
|---|---|
| Native binary (Rust) | Store, lifecycle rules, retrieval and the MCP tools. One process per project store. |
| Local service (Node) | Systemd user service. Starts one native worker per project and shares it across agent sessions. |
| `lambda-memory` bridge | What MCP clients launch. Connects to the service and handles authentication for them. |
| Web UI | Optional page on `127.0.0.1` for browsing and curating memories. |
| OMP extension | Optional hooks for [OMP](https://github.com/can1357/oh-my-pi): recall before each turn, playbook injection, capture after work. |

Two modes: **`openrouter`** (the normal one, uses the models below) and **`local`** (offline, exact-text extraction and lexical recall, no model calls, no cost). The install below starts in `local` mode.

## Models and cost

The `openrouter` mode uses three models. They are pinned: other IDs are rejected.

| Role | Model |
|---|---|
| Turns sources into memories; plans recall queries | `anthropic/claude-haiku-5.5` |
| Embeddings (1536 dimensions by default; configurable) | `google/gemini-embedding-2` |
| Picks the relevant memories at recall | `openai/gpt-6-luna-decisions` |

**Real usage.** One week of daily use by the author (October 2026), across all projects:

| Model | Requests | Tokens | Spend |
|---|---:|---:|---:|
| Claude Haiku 5.5 | 489 | 3.33 M | US$ 0.45 |
| GPT-6 Luna Decisions | 245 | 1.02 M | US$ 0.10 |
| Gemini Embedding 2 | 1,940 | 169 K | US$ 0.04 |
| **Total** | **2.7 K** | **4.5 M** | **US$ 0.59** |

That week included one day of catching up on an old backlog, so it is closer to an upper bound. It works out to **about US$ 2.50 per month**.

**Per operation**, derived from the same numbers:

| Operation | Model calls | Cost |
|---|---|---|
| Recall | 1 Haiku + 1 embedding + 1 Luna | ≈ US$ 0.0013 |
| Dreaming, per batch of up to 8 sources | 1 Haiku + 1 embedding per memory | ≈ US$ 0.001 |
| Capture, list, status, timeline, web UI | none | free |

| Usage | Per day | Per month |
|---|---|---|
| Light: 10 recalls, 20 captured events | ≈ US$ 0.02 | ≈ US$ 0.50 |
| Heavy: 50 recalls, 100 captured events | ≈ US$ 0.08 | ≈ US$ 2.40 |

Cost grows with text length. There is **no spending cap** in the resident service, so keep an eye on your OpenRouter dashboard.

## Install

### Prerequisites

- Linux with a systemd user session (`systemctl --user` works)
- Node.js 22 or newer
- git (a project's identity comes from its git remote)
- openssl (to generate the token)
- Docker, or Rust 1.88 or newer, to build the native binary
- `~/.local/bin` on your `PATH`
- For `openrouter` mode only: an [OpenRouter](https://openrouter.ai) API key

The snippets below are bash. If your shell is fish, run `bash` first. Run them from the root of a clone of this repository.

### 1. Build the binary

**With Docker.** This runs the test suite and then compiles an optimized binary. From scratch it takes about 10 minutes on a 16-core machine; add `--build-arg RUN_TESTS=0` to skip the tests.

```bash
docker build -t lambda-memory:runtime .
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

```bash
APP=~/.local/share/lambda-memory
mkdir -p $APP/bin $APP/runtime ~/.local/bin ~/.config/lambda-memory ~/.config/systemd/user
install -m 755 lambda-memory-native $APP/bin/
cp -r runtime extensions $APP/runtime/

# the bridge that MCP clients launch
printf '#!/bin/sh\nexec %s %s/runtime/runtime/stdio.mjs "$@"\n' "$(command -v node)" "$APP" > ~/.local/bin/lambda-memory
chmod +x ~/.local/bin/lambda-memory
```

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

Replace `sk-or-...` with your key. If the service is already running, restart it with `systemctl --user restart lambda-memory.service`. Finally, make sure both files are private:

```bash
chmod 600 ~/.config/lambda-memory/*
```

The service refuses to start if either file can be read by other users. All settings are listed in the [reference](docs/reference.md#configuration).

### 4. Start the service

The unit file assumes `/usr/bin/node`; this substitutes your actual Node path.

```bash
sed "s#/usr/bin/node#$(command -v node)#" runtime/lambda-memory.service > ~/.config/systemd/user/lambda-memory.service
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

Optionally, two Claude Code hooks inject the playbook when a session starts and record action episodes after tool calls. The `settings.json` snippet is in the [reference](docs/reference.md#claude-code).

**[OMP](https://github.com/can1357/oh-my-pi):** merge this entry into `~/.omp/agent/mcp.json`, then check `/mcp list`:

```json
{ "mcpServers": { "lambda-memory": { "type": "stdio", "command": "lambda-memory", "args": ["--serve-mcp"], "timeout": 660000 } } }
```

Optionally, add the hooks extension, which recalls before each turn, injects the playbook and records actions automatically:

```bash
mkdir -p ~/.omp/agent/extensions
cat > ~/.omp/agent/extensions/lambda-memory-local.ts <<EOF
import installLocal from '$HOME/.local/share/lambda-memory/runtime/extensions/lambda-memory-local.mjs';
export default function lambdaMemoryLocal(api) { return installLocal(api); }
EOF
```

Use either this hooks extension or nothing else that loads `extensions/lambda-memory.mjs` directly, never both.

Each agent session is bound to the project of its working directory (its git remote when there is one). Tool calls must name that `project`; the bridge adds it to every tool description so the agent knows the value.

### Verify

```bash
curl -s -o /dev/null -w '%{http_code}\n' http://127.0.0.1:7731/   # 200 if webPort is set
journalctl --user -u lambda-memory -n 50                          # service logs
```

Then, inside a project, ask your agent to run `memory_status` for this project. It should report the worker and an empty backlog.

### Update

Rebuild, repeat step 2 (it overwrites the installed files), then run `systemctl --user restart lambda-memory.service` and restart your agent sessions.

**Upgrading from 0.1.0:** recall now takes `budget_bytes` (setting `LAMBDA_MEMORY_CONTEXT_BYTES`) instead of `budget_tokens`, and embeddings move to a new 1536-dimension space. Update the native binary and the `runtime/` and `extensions/` files together. Existing memories stay intact but need a vector in the new space; see [embedding index preparation](docs/reference.md#embedding-index-preparation-and-recovery).

### Docker only, without installing

The image can serve MCP directly for one project, which is useful for testing or for a custom client:

```bash
TOKEN=$(openssl rand -hex 32)
docker run -i --rm -v lm-data:/var/lib/lambda-memory \
  -e LAMBDA_MEMORY_MODE=local -e LAMBDA_MEMORY_OWNER=me -e LAMBDA_MEMORY_ACTOR=me \
  -e LAMBDA_MEMORY_PROJECT=demo -e LAMBDA_MEMORY_TOKEN=$TOKEN \
  lambda-memory:runtime --serve-mcp
```

The client must send the token as `params._meta.lambdaMemoryToken` in its `initialize` request. Generic MCP clients can't do that, so they should use the bridge from the install above.

## Using it

### MCP tools

| Tool | Use it to |
|---|---|
| `memory_capture` | Save a decision, result, error, attempt or action episode as evidence |
| `memory_propose` | Publish a source-backed memory or a correction right away |
| `memory_recall` | Ask a question and get scoped, source-attributed context |
| `memory_list` | Browse memories by state, with lineage and history |
| `memory_review` | Reject, declare a conflict or resolve one (optional) |
| `memory_timeline_write` | Record, revise or retract a curated decision |
| `memory_timeline` | Read the timeline and decisions not yet on it |
| `memory_status` | See whether dreaming is running, what is pending or blocked |
| `memory_activity` | Read the last 7 days of operations |
| `memory_playbook` | Read the project's procedural playbook, built from recorded actions |
| `memory_purge` | Delete a source and everything derived from it |

### Web UI

With `webPort` set, open `http://127.0.0.1:<your webPort>` (7731 in the example config). You can browse memories by state, see their sources and versions, correct a memory, manage the decision timeline, and purge a source (you have to type its id). It only listens on loopback and needs no login.

The UI puts content first: memories and decisions read as full text, while ids, sources, lineage and history sit in collapsible sections. Each state says what it means (`active` is available for recall, not verified truth; `processing` is unfinished preparation, not an approval queue). Everything stays inside the open project. Themes: Auto, Light and Dark. [Screenshots](assets/screenshots/).

### Skills

[`skills/`](skills/) has six short guides that teach the agent when and how to use each tool: recall, capture, review, timeline, purge and runtime.


## Privacy

In `local` mode nothing leaves your machine.

In `openrouter` mode these go to [OpenRouter](https://openrouter.ai), which forwards them to the model providers:

- **Dreaming:** the text of captured sources goes to Claude Haiku 5.5, and the resulting memories go to Gemini Embedding 2.
- **Recall:** your question goes to Claude Haiku 5.5 and Gemini Embedding 2; the question and candidate memories go to GPT-6 Luna Decisions.
- **Corrections and proposals:** the memory text goes to Gemini Embedding 2.

OpenRouter's and the providers' own data policies apply. λambda memory does not request zero-data-retention routing.

All data stays under `~/.local/share/lambda-memory`, with configuration in `~/.config/lambda-memory`. Capture, list, status, timeline and the web UI make no model calls. Don't capture secrets: the agent decides what to capture, and captured text is sent to the models when dreaming runs.

## Troubleshooting

**The service exits or keeps restarting.** Run it in the foreground to see the error:

```bash
LAMBDA_MEMORY_CONFIG=~/.config/lambda-memory/runtime.json \
  node ~/.local/share/lambda-memory/runtime/runtime/local-service.mjs
```

Common causes:

- `private configuration required`: a config file is readable by other users. Run `chmod 600 ~/.config/lambda-memory/*`.
- `socket path bound`: `socketPath` is longer than 100 bytes.
- `provider key required` or `private provider environment required`: `openrouter` mode without a valid `providerEnvFile` or key.
- `status=203/EXEC` in `systemctl --user status`: the Node path in the unit is wrong. Repeat step 4.

**The MCP server is not connected.** Check `systemctl --user status lambda-memory.service`, then restart your agent (or run `/mcp reload` in OMP).

**Tool errors mention `project`.** Every call must pass the exact project shown in the tool description. A session started in one repository can't write to another; start the agent from the project's directory.

More in the [reference](docs/reference.md#diagnostics).

## Uninstall

```bash
systemctl --user disable --now lambda-memory.service
rm ~/.config/systemd/user/lambda-memory.service
systemctl --user daemon-reload
claude mcp remove lambda-memory -s user
rm -f ~/.omp/agent/extensions/lambda-memory-local.ts   # and remove the entry from ~/.omp/agent/mcp.json
rm -f ~/.local/bin/lambda-memory
rm -rf ~/.config/lambda-memory
rm -rf ~/.local/share/lambda-memory   # deletes all memories
```

Removing `~/.local/share/lambda-memory` deletes every stored source and memory for all projects. Back it up first if you might want them again.

## Repository layout

```
src/
  domain_models/   types: sources, revisions, timeline, diagnostics
  engines/         pure rules: validity, scope, ranking, context budget
  integrations/    embedded database, OpenRouter HTTP
  services/        capture, dreaming, recall, timeline orchestration
  delivery/        MCP server and CLI
migrations/        database schema (compiled into the binary)
runtime/           local service, bridge, project binding, systemd unit, Claude Code hook
runtime/web/       web UI (plain HTML/JS/CSS + self-hosted Fira Code, no build step)
extensions/        OMP extension (automatic recall and capture hooks)
skills/            agent usage guides
tests/             Rust and Node test suites
docs/              architecture, reference and background research
scripts/           architecture boundary check
```

The code follows the [MASA framework](https://www.masa-framework.org/): five layers with one-way dependencies. How it maps to Rust is in [MASA.md](MASA.md), and `scripts/check_layers.py` checks the boundaries.

## Development

```bash
cargo test --all-features          # Rust tests
node --test tests/*.test.mjs       # service, bridge and web UI tests
python3 scripts/check_layers.py    # architecture boundaries
cargo fmt --check                  # formatting
```

See [CONTRIBUTING.md](CONTRIBUTING.md). Security issues: [SECURITY.md](SECURITY.md).

## More

- [Reference](docs/reference.md): settings and limits, project binding, MCP contract, diagnostics, procedural playbook and Claude Code hooks, purge, recovery of blocked sources, web UI security model.
- [Architecture](docs/architecture.md): the pipeline, store, dreaming, recall and provider boundary.
- [Memory architecture research](docs/research/memory-architecture.md): background research behind the design.
- [Changelog](CHANGELOG.md)

## License

[MIT](LICENSE) © 2026 Nicolas Melo

<p align="center"><sub>Evidence before confidence.</sub></p>
