# Upgrading λambda memory and migrating old memories

**Audience: coding agents** (Claude Code, Codex, OMP, any agent with a shell) that a person asked to install or upgrade λambda memory on their machine. Follow the steps in order. Every step says what to check before you move on.

Two rules apply throughout:

- **Ask before paid or disruptive steps.** Migration makes billable OpenRouter calls with no spending cap, and the service must be stopped while it runs. Show the person the plan summary and get an explicit yes before you set `accept_charges` to `true`. Do not approve on their behalf, and do not retry a failed paid step on your own.
- **Never print secrets.** `~/.config/lambda-memory/runtime.json` holds a token and `openrouter.env` holds the OpenRouter key. Check that the files exist and are mode 0600, but never `cat` them, echo them or paste them into chat.

For a fresh install with no earlier version, use the [README install section](../README.md#install); no identity or history migration is required.

## What changes in 0.4.0

- In 0.4.0, knowledge became a **memory tree** per project, then had four fixed roots (Decisions, Preferences, Architecture, Open items). Phases 3–5 add Lessons as a fifth root. A curator merges compatible evidence into existing nodes and discards what is not durable. Older versions stored one flat memory per source.
- Recall descends the tree and returns paths. `memory_node` reads a node with its original sources.
- Hooks are harness-agnostic: `runtime/agent-hook.mjs <harness> <mode>` serves Claude Code and Codex. `claude-code-hook.mjs` keeps working as a shim.
- Conversation capture turns each sanitized prompt and final reply into knowledge for the curator; it is now on by default.

**Already on 0.4.x?** For the learning memory upgrade, follow the section below even if the tree migration is already complete. Skip only the separate flat-to-tree migration in steps 5–6 when the project is already in tree mode. 0.4.1 fixes the `pending.unconsumed_sources` count, which included action episodes the curator never processes; after the upgrade a project with many recorded actions can show a much smaller backlog.

Projects with **no** active flat memories switch to the tree for free. Projects **with** flat memories keep them in recall until you migrate them. Migration curates the sources behind those memories into the tree once, then retires the flat memories from recall. Original sources are never deleted.

## Upgrade to learning memory phases 0–1

Perform these steps in order, with the person's approval for disruptive and billable work. Do not run the operator commands on their behalf without approval.

1. **Rotate keys first.** Ask the person to rotate any keys ever pasted into chats; do not inspect or print those chats or keys.
2. Install the runtime-owned Node 22 using the checksum-verified commands in [Runtime-owned Node and failure alert](#runtime-owned-node-and-failure-alert), through the Node extraction step. Do not restart the service yet.
3. Build the binary as in [step 2](#2-build-the-new-version). Stop the service and make the private backup in [step 3](#3-stop-the-service-and-back-up), then install the binary and complete runtime bundle as in [step 4](#4-install-the-binary-and-the-javascript-bundle). Preserve private configuration and local web customizations.
4. Rewrite the bridge and install both systemd units as in [Runtime-owned Node and failure alert](#runtime-owned-node-and-failure-alert), but leave the service stopped for identity consolidation. Install the Claude Code and Codex hook blocks from the [reference](reference.md#claude-code-hooks), preserving existing settings; ask the user to trust Codex hooks with `/hooks`. Update the OMP extension bundle and, if its wrapper is missing, create it (preserve any existing custom wrapper after reviewing it):

   ```bash
   APP="$HOME/.local/share/lambda-memory"
   mkdir -p "$HOME/.omp/agent/extensions"
   printf 'import installLocal from "%s/runtime/extensions/lambda-memory-local.mjs";\nexport default function (api) { return installLocal(api); }\n' "$APP" > "$HOME/.omp/agent/extensions/lambda-memory-local.ts"
   ```

   Reopen OMP to load the updated extension; configure its MCP server as in [Installed Linux runtime](reference.md#installed-linux-runtime).
5. Tell the person that conversation capture is now **on by default**: sanitized prompts and final replies are curated through the configured provider, adding curation calls. Set `"captureConversation": false` in private `runtime.json` to opt out, preserving all other keys and never printing the file.
6. **Stop the service** before consolidation. `inspect` refuses a running service. Save the plan privately, show it to the user including `changed_repos` and estimated calls, and apply only after explicit approval. The plan must be unchanged except for `accept_charges: true`:

   ```bash
   systemctl --user stop lambda-memory.service
   umask 077
   cd "$HOME/.local/share/lambda-memory/runtime"
   "$HOME/.local/share/lambda-memory/node/bin/node" runtime/merge-identities.mjs inspect > identity-plan.json
   # Review with the user; make a private approved copy with only accept_charges set to true.
   "$HOME/.local/share/lambda-memory/node/bin/node" runtime/merge-identities.mjs apply --backup <private-backup-dir> < approved-identity-plan.json
   ```

   Review outcomes. Stores are never deleted; timelines and review decisions stay in source stores. On failure, do not retry automatically.
7. **Start the service**, including after a failed or declined consolidation: `systemctl --user start lambda-memory.service`.
8. For each project and each supported agent (`omp` or `claude-code`), run `import-exchanges.mjs` without `--apply` for a free dry run. It works with the service up or down. Review the samples, flagged sessions and costs with the user; explicitly include any approved flagged session with `--include <session-hash>` in a regenerated plan. Apply only after approval, with the service running and the exact plan echoed on stdin with only `accept_charges: true`. See [the commands and plan fields](reference.md#identity-consolidation-and-conversation-history-import). If the result is `pending_limit`, wait for curation, then rerun the approved import after user approval; never automatically retry an uncertain or failed operation.

## Upgrade to learning memory phase 2

With approval for the interruption, build the matching binary, stop the service and make a private backup as in [steps 2–3](#2-build-the-new-version). Install the binary and complete runtime bundle as in [step 4](#4-install-the-binary-and-the-javascript-bundle), including `cp extensions/*.mjs $APP/runtime/extensions/`. Preserve private configuration and local customizations. Reopen OMP: its updated guard must allow calls naming the user store as well as the current project. Restart the service.

### Optional user-memory migration

This is an operator procedure, not an automatic agent action. Every `promote-all.mjs` command below, including `summary`, refuses while the service runs. Plans contain private identities; use a private directory and `umask 077`. The user must approve each paid step after seeing its exact plan and costs. No paid operation is retried automatically.

1. Stop the service. From the installed bundle, make a fresh plan and show its summary:

   ```bash
   systemctl --user stop lambda-memory.service
   umask 077
   cd "$HOME/.local/share/lambda-memory/runtime"
   NODE="$HOME/.local/share/lambda-memory/node/bin/node"
   "$NODE" runtime/promote-all.mjs plan > promotion-plan.json
   "$NODE" runtime/promote-all.mjs summary < promotion-plan.json
   # After explicit approval, make a private copy changing only accept_charges to true.
   "$NODE" runtime/promote-all.mjs apply < approved-promotion-plan.json
   ```

   Planning and summary make no model calls. Apply classifies existing Preferences and Decisions nodes: each batch costs one Haiku call; later user-store curation and embeddings are also paid. Review the summary and obtain explicit approval before applying.
2. Still stopped, run `"$NODE" runtime/promote-all.mjs transfer`. Transfer makes no model calls. Read every project's `remaining` in the output and investigate any `failed`; copies may remain because the user store's pending capacity is full.
3. Start the service **and keep an agent session connected**: the user worker only runs while some project worker is live. From a project session call `memory_status` with `{"project":"user:<owner>"}` (use the actual user identity; the service routes it to the user store). Wait until `pending.unconsumed_sources` is 0. User curation and embeddings are billable.
4. If any project's `remaining` was above 0, stop the service, run `transfer` again, then repeat step 3. Continue until `remaining` is 0 everywhere; do not automatically retry a failed operation.
5. Stop the service and make a fresh retire plan:

   ```bash
   systemctl --user stop lambda-memory.service
   "$NODE" runtime/promote-all.mjs retire-plan > retire-plan.json
   # Review the plan with the user; after explicit approval, make a private copy
   # changing only accept_charges to true.
   "$NODE" runtime/promote-all.mjs retire --confirm < approved-retire-plan.json
   systemctl --user start lambda-memory.service
   ```

   Review `confirmed`, `waiting`, `incomplete`, `orphaned` and `blocked` with the user; only `confirmed` nodes are retired. If `waiting` is nonzero, repeat step 3 and generate a fresh `retire-plan` later. Retirement queues paid parent refreshes, so obtain separate approval before `retire --confirm`. Restart the service after a failure or declined migration too.

After any failure, do not replay an earlier approval: partial work can make it stale. Obtain a fresh `plan` and `summary`, or a fresh `retire-plan` and review its categories as the retirement summary, and obtain fresh approval before another paid attempt. Do not restore a pre-paid-step datastore backup, which would erase admission records.

## Upgrade to learning memory phases 3–5

1. With approval for the interruption, build the matching binary, stop the service and make a private backup as in [steps 2–3](#2-build-the-new-version). Install the binary and complete runtime bundle as in [step 4](#4-install-the-binary-and-the-javascript-bundle); preserve private configuration and local customizations. Add the following entry to the existing Claude Code hooks, preserving the other entries (use the runtime-owned Node and bundle):

   ```json
   "SessionEnd": [
     { "hooks": [ { "type": "command", "command": "\"$HOME/.local/share/lambda-memory/node/bin/node\" \"$HOME/.local/share/lambda-memory/runtime/runtime/agent-hook.mjs\" claude-code session-end", "async": true } ] }
   ]
   ```

   Reopen OMP to load its updated extension. Codex has no end signal; sessions become ready after idle time.
2. **Before restarting, tell the person that these phases turn on automatic paid background calls, with no opt-out besides `local` mode.** OpenRouter makes one lesson extraction per ready ended session (`lesson-job:`), skill drafting calls (`skill-draft-job:`), dependency refreshes (`tree-dependency:`) and reflection calls (`reflect-job:`), plus curation and embeddings of lesson sources. There is no spending cap. Restart only after they agree; `local` is an explicit offline mode, never a provider-error fallback.
3. Optional backfills are separate consented operator steps. Run the wrapper from the target project directory using the installed bundle path, with the service **stopped** for every action. Plans may contain private identities; use a private directory and `umask 077`. Inspect, show the exact plan and estimated calls to the user, and change only `accept_charges` to `true` after approval. Never retry failed or uncertain work automatically.

   ```bash
   systemctl --user stop lambda-memory.service
   umask 077
   # From the target project directory:
   NODE="$HOME/.local/share/lambda-memory/node/bin/node"
   RECOVER="$HOME/.local/share/lambda-memory/runtime/runtime/recover-pending.mjs"
   "$NODE" "$RECOVER" inspect-lesson-backfill > lesson-plan.json
   # Show the plan; after explicit approval, make an approved copy with accept_charges: true.
   "$NODE" "$RECOVER" lesson-backfill < approved-lesson-plan.json
   "$NODE" "$RECOVER" inspect-timeline-backfill
   "$NODE" "$RECOVER" timeline-backfill
   systemctl --user start lambda-memory.service
   ```

   Lesson backfill only marks the listed historical sessions ready; paid extraction happens later, while that project's worker is live, at up to two sessions per tick. Timeline inspection is a free dry run printing `{project, eligible, created}`; timeline backfill is free and writes. Start the service with a project session connected and let lesson extraction and curation finish; check `memory_status` from that session. Then stop the service again for reflection:

   ```bash
   systemctl --user stop lambda-memory.service
   "$NODE" "$RECOVER" inspect-reflection-backfill > reflection-plan.json
   # Show the plan; after explicit approval, make an approved copy with accept_charges: true.
   "$NODE" "$RECOVER" reflection-backfill < approved-reflection-plan.json
   systemctl --user start lambda-memory.service
   ```

   Reflection backfill schedules paid reflection later while the worker is live. The user store has no wrapper backfill action; its reflection runs only on its own triggers. If an optional step fails or is declined, do not retry it automatically; restart only if the person has agreed to the automatic costs above.
4. Review skill drafts with `node runtime/skill-candidates.mjs list` and `list --user`, then `show <id>` (add `--user` for user scope and `--cwd <dir>` for a project). `show` prints the revision and draft hash; pass both values to approve the exact draft shown. Only when the user says so, approve with `approve <id> --revision <rev> --draft <hash> --confirm` and the same scope. Replacing an unchanged earlier Lambda-installed file leaves a `.SKILL.md.lambda-backup-<pid>-<ts>` hard link beside `SKILL.md`; tell the user these backups are kept. See [skill drafts](reference.md#skill-drafts).

## 1. Check the current installation

```bash
systemctl --user status lambda-memory.service --no-pager | head -5
ls -l ~/.config/lambda-memory/runtime.json ~/.config/lambda-memory/openrouter.env   # both must be -rw-------
ls ~/.local/share/lambda-memory/bin/lambda-memory-native ~/.local/share/lambda-memory/runtime/runtime/
```

If any of these is missing, this is not an upgrade. Do a fresh install from the README.

## 2. Build the new version

From a checkout of the release you are installing (for example `git checkout v0.5.0`):

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
B=$APP/backups/pre-upgrade-$(date +%Y%m%d-%H%M%S)
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

The configuration files do not change.

## Runtime-owned Node and failure alert

Install Node 22 for the runtime, rewrite the bridge, and install both systemd units from the release checkout. Run the commands in order and stop if any command fails, especially the checksum verification:

```bash
APP=~/.local/share/lambda-memory
NODE_DIST=https://nodejs.org/dist/latest-v22.x
NODE_TAR=$(curl -fsSL "$NODE_DIST/SHASUMS256.txt" | awk '/linux-x64\.tar\.xz$/ {print $2}')
curl -fsSLO "$NODE_DIST/$NODE_TAR"
curl -fsSL "$NODE_DIST/SHASUMS256.txt" | grep " $NODE_TAR\$" | sha256sum -c -
mkdir -p "$APP/node" && tar -xJf "$NODE_TAR" -C "$APP/node" --strip-components=1 && rm "$NODE_TAR"

printf '#!/bin/sh\nexec %s/node/bin/node %s/runtime/runtime/stdio.mjs "$@"\n' "$APP" "$APP" > ~/.local/bin/lambda-memory
chmod +x ~/.local/bin/lambda-memory
mkdir -p ~/.config/systemd/user
cp runtime/lambda-memory.service runtime/lambda-memory-failure.service ~/.config/systemd/user/
systemctl --user daemon-reload
# Leave stopped for identity consolidation; start it afterward.
```

A self-managed Node 22 that system upgrades never touch may be symlinked to `$APP/node` instead; the runtime never uses the system `node`. Test the failure alert with `systemctl --user start lambda-memory-failure.service`.

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

Install the full [Claude Code](reference.md#claude-code-hooks) and [Codex](reference.md#codex-hooks) blocks and update the OMP extension as described above. The old Claude shim remains available but does not by itself install the new modes. Codex requires `/hooks` trust and has no per-prompt recall yet. Conversation capture is on by default; `"captureConversation": false` opts out.

## Rolling back

Before any paid migration step, you can return to the old version: stop the service, put back `bin/lambda-memory-native` and `runtime/` from the backup, and start it. **After** a paid step, roll back the code only and keep the store. The new tables are ignored by older code paths, but restoring the old `projects/` would drop the record of billed calls.
