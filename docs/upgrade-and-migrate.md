# Upgrading λambda memory and migrating old memories

**Audience: coding agents** (Claude Code, Codex, OMP, any agent with a shell) that a person asked to install or upgrade λambda memory on their machine. Follow the steps in order. Every step says what to check before you move on.

Two rules apply throughout:

- **Ask before paid or disruptive steps.** Migration makes billable OpenRouter calls with no spending cap, and the service must be stopped while it runs. Show the person the plan summary and get an explicit yes before you set `accept_charges` to `true`. Do not approve on their behalf, and do not retry a failed paid step on your own.
- **Never print secrets.** `~/.config/lambda-memory/runtime.json` holds a token and `openrouter.env` holds the OpenRouter key. Check that the files exist and are mode 0600, but never `cat` them, echo them or paste them into chat.

For a fresh install with no earlier version, use the [README install section](../README.md#install) and stop after step 6 below. Nothing needs migrating.

## What changes in 0.4.0

- Knowledge becomes a **memory tree** per project, with four fixed roots (Decisions, Preferences, Architecture, Open items). A curator merges new evidence into existing nodes and discards what is not durable. Older versions stored one flat memory per source.
- Recall descends the tree and returns paths. `memory_node` reads a node with its original sources.
- Hooks are harness-agnostic: `runtime/agent-hook.mjs <harness> <mode>` serves Claude Code and Codex. `claude-code-hook.mjs` keeps working as a shim.
- Opt-in conversation capture turns each prompt and final reply into knowledge for the curator.

Projects with **no** active flat memories switch to the tree for free. Projects **with** flat memories keep them in recall until you migrate them. Migration curates the sources behind those memories into the tree once, then retires the flat memories from recall. Original sources are never deleted.

## 1. Check the current installation

```bash
systemctl --user status lambda-memory.service --no-pager | head -5
ls -l ~/.config/lambda-memory/runtime.json ~/.config/lambda-memory/openrouter.env   # both must be -rw-------
ls ~/.local/share/lambda-memory/bin/lambda-memory-native ~/.local/share/lambda-memory/runtime/runtime/
```

If any of these is missing, this is not an upgrade. Do a fresh install from the README.

## 2. Build the new version

From a checkout of the release you are installing (for example `git checkout v0.4.0`):

```bash
cargo build --release --locked --features memory-runtime,router-http
# or with Docker:
# docker build --build-arg LAMBDA_FEATURES=memory-runtime,router-http -t lambda-memory:runtime .
# docker create --name lm lambda-memory:runtime && docker cp lm:/usr/local/bin/lambda-memory target/release/lambda-memory && docker rm lm
```

Check that the build succeeded and that `target/release/lambda-memory` exists. Building does not change the installed service.

## 3. Stop the service and back up

Tell the person that their agent sessions will lose memory access until step 5, then:

```bash
systemctl --user stop lambda-memory.service
systemctl --user show lambda-memory.service --property=ActiveState --value   # must print inactive
APP=~/.local/share/lambda-memory
B=$APP/backups/pre-0.4.0-$(date +%Y%m%d-%H%M%S)
mkdir -p -m 700 "$B"
cp -a $APP/projects "$B/" && cp -a $APP/bin/lambda-memory-native "$B/" && cp -a $APP/runtime "$B/"
chmod -R go-rwx "$B"
```

Keep the backup until the person has used the new version for a while. **Do not restore it after a paid migration step has run**, because restoring would erase the record of calls already billed.

## 4. Install the binary and the JavaScript bundle

```bash
install -m 755 target/release/lambda-memory $APP/bin/lambda-memory-native
cp runtime/*.mjs $APP/runtime/runtime/
mkdir -p $APP/runtime/runtime/hook-adapters && cp runtime/hook-adapters/*.mjs $APP/runtime/runtime/hook-adapters/
cp extensions/*.mjs $APP/runtime/extensions/
cp -r runtime/web/. $APP/runtime/runtime/web/   # skip this if the person customized the web UI; diff first
```

The configuration files, the systemd unit and the `~/.local/bin/lambda-memory` bridge do not change.

## 5. Start, verify, and plan the migration

Start the service and check one project:

```bash
systemctl --user start lambda-memory.service
systemctl --user status lambda-memory.service --no-pager | head -3
```

From inside a project directory, call the `memory_status` MCP tool. Its result now has a `tree` block: `mode` is `tree` for projects that had no flat memories, and `flat` for projects waiting for migration.

Plan the migration. Planning makes **no model calls**, but it needs the service stopped:

```bash
systemctl --user stop lambda-memory.service
cd $APP/runtime
node runtime/migrate-all-trees.mjs plan > /tmp/lm-tree-plan.json
node runtime/migrate-all-trees.mjs summary < /tmp/lm-tree-plan.json
```

The summary lists:

- the projects to curate, with their batch and source counts;
- the projects switched to tree mode at no cost;
- the skipped registry entries, with a reason: `missing_cwd` (the directory is gone), `binding_changed` (the directory now resolves to another project identity, for example after adding a Git remote; the old store is left alone), `excluded` or `not_selected`.

Each batch is one paid Haiku call of up to four sources. Refresh, split and node embeddings add some more. As a rough guide, budget about one to three US cents per batch, and say clearly that this is an estimate.

Look for accidental scopes, such as sessions started in `/tmp` or the home directory, and offer to leave them out. To narrow the plan, regenerate it:

```bash
node runtime/migrate-all-trees.mjs plan --exclude path:/tmp --exclude path:/home/YOU > /tmp/lm-tree-plan.json
node runtime/migrate-all-trees.mjs plan --only repo:github.com/owner/project > /tmp/lm-tree-plan.json   # one project, e.g. as a first test
```

**Show the person the summary and ask whether to proceed.** If they decline, restart the service (`systemctl --user start lambda-memory.service`) and stop here. Their flat memories keep working.

## 6. Apply the approved plan

Only after an explicit yes, set the consent flag. Do not change anything else in the plan, because the native command checks it exactly. Then apply:

```bash
python3 - <<'EOF'
import json; p = json.load(open('/tmp/lm-tree-plan.json')); p['accept_charges'] = True
json.dump(p, open('/tmp/lm-tree-plan-approved.json', 'w'))
EOF
chmod 600 /tmp/lm-tree-plan-approved.json
node runtime/migrate-all-trees.mjs summary < /tmp/lm-tree-plan-approved.json   # must say "Consent: GIVEN"
node runtime/migrate-all-trees.mjs apply < /tmp/lm-tree-plan-approved.json > /tmp/lm-tree-apply.json
systemctl --user start lambda-memory.service
```

Projects run cheapest first. `apply` stops at the first project that fails and prints `{"outcomes": [...], "skipped": [...]}`. Projects before the failure are migrated. The failed project stays in flat mode with its partial tree hidden from recall, and the rest are untouched. Large projects take a while, roughly ten seconds per batch.

**If a project fails:** do not rerun automatically. Start the service and read the project's diagnostics with the `memory_activity` MCP tool, looking at the `curation`, `tree_refresh` and `tree_split` phases and their `reason`. `provider_unavailable` or `provider_rejected` points at the provider (network, credit, rate limit). `provider_invalid_output` or a `tree_*` reason (`tree_unknown_slot`, `tree_missing_operation`, `tree_too_many_operations`, `tree_invalid_text`, `tree_secret_in_text`, `tree_bounds`) means the curator's answer was unusable. Report what you found to the person. An already billed request is never replayed, and a rerun after a fix continues with the sources not yet linked. Ask before rerunning, because a rerun makes new paid calls for the remaining batches. To resume, run `apply` again with the same approved plan: projects already in tree mode are reported as `already_migrated` at no cost, and sources a project already handled, discarded ones included, are not curated again.

## 7. Verify

With the service running, from inside a migrated project:

- `memory_status` shows `tree.mode: "tree"` and `nodes_active` greater than 0.
- `memory_recall` with a question about a known decision returns hits with `node` and `path`.
- `memory_node` with `{"project": …, "node": <a hit's node>, "sources": true}` returns the original messages behind that node.

## 8. Hooks (optional)

**Claude Code.** Existing `~/.claude/settings.json` entries that call `claude-code-hook.mjs` keep working. New entries should call `agent-hook.mjs claude-code session-start|post-tool-use` (see the [reference](reference.md#procedural-playbook)).

**Codex.** Merge the `SessionStart` and `PostToolUse` entries from the [reference](reference.md#procedural-playbook) into `~/.codex/hooks.json`, keeping the existing entries. Then ask the person to run `/hooks` in Codex and trust them. Codex silently skips hooks that have not been trusted.

**Conversation capture** is off by default. It sends the sanitized text of the person's prompts and the agent's replies to OpenRouter during curation. Enable it only if the person asks: add `"captureConversation": true` to `runtime.json`, keeping every other key (edit the JSON without printing it), then add the `UserPromptSubmit` and `Stop` hooks shown in the reference.

## Rolling back

Before any paid migration step, you can return to the old version: stop the service, put back `bin/lambda-memory-native` and `runtime/` from the backup, and start it. **After** a paid step, roll back the code only and keep the store. The new tables are ignored by older code paths, but restoring the old `projects/` would drop the record of billed calls.
