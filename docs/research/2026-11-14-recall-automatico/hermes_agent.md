# Hermes Agent (Nous Research): memory recall and context injection

**What was read, and when.** The `NousResearch/hermes-agent` source was cloned at commit `f578714e0ee122179ec28fecb9af989583b71baf`, committed 2026-11-15 01:58 +0100. The nearest tag is `rc.8-v0.23.2`, 726 commits back; `pyproject.toml` only carries the placeholder `version = "0.0.0"`. The docs in `website/docs/` were read at their last change, `f28ee0d4` (2026-11-14). The live docs site matches the repo docs: the plugin-guide hook table was spot-checked. The Honcho plugin, which now lives in the `plastic-labs/honcho` repo, was read at `bc500af8e62cf5860ffb79dcc0966fa53f1b385e` (2026-11-09).

**Permalinks may break.** One issue author says the hermes-agent "repo history is rewritten on release" ([#85135](https://github.com/NousResearch/hermes-agent/issues/85135)). If so, the commit permalinks below can stop resolving. File and line references are still accurate for the commit named above.

**How claims are labelled.** **[code]** means verified in source. **[docs]** means official docs. **[marketing]** means README or tagline claims. **[issue]** means a GitHub issue or PR. Anything else is marked as inference.

Link bases used below:
- `HA` = https://github.com/NousResearch/hermes-agent/blob/f578714e0ee122179ec28fecb9af989583b71baf
- `HON` = https://github.com/plastic-labs/honcho/blob/bc500af8e62cf5860ffb79dcc0966fa53f1b385e/hermes-plugin-honcho

---

## Q1. When is memory injected: once per session (frozen snapshot), every turn, every N turns, or on demand? Why?

### Takeaway
Hermes has three channels:
1. **Built-in MEMORY.md/USER.md.** Rendered **once at session start** into the system prompt's "volatile" tier as a **frozen snapshot**. It is never changed mid-session, except when compression rebuilds the prompt or an operator explicitly asks for a refresh. The stated reason is prompt-cache preservation.
2. **External provider recall.** Injected **once per user turn**, appended to the current user message inside a `<memory-context>` fence. The exact bytes are then persisted, so later requests replay them unchanged and stay cache-stable.
3. **Session search and full skill bodies.** **On demand only**, through model-invoked tools (`session_search`, `skill_view`). Only a compact skills index is always present.

### Cited Findings

**Built-in memory (frozen snapshot)**
- [docs] MEMORY.md (2,200-char limit, about 800 tokens) and USER.md (1,375 chars, about 500 tokens) live in `~/.hermes/memories/`. They "are injected into the system prompt as a frozen snapshot at session start." — [Memory docs](https://hermes-agent.nousresearch.com/docs/user-guide/features/memory)
- [docs] Stated rationale: "The system prompt injection is captured once at session start and never changes mid-session. This is intentional — it preserves the LLM's prefix cache for performance. When the agent adds/removes memory entries during a session, the changes are persisted to disk immediately but won't appear in the system prompt until the next session starts. Tool responses always show the live state." — [Memory docs](https://hermes-agent.nousresearch.com/docs/user-guide/features/memory)
- [docs] Rendered format: a `MEMORY (your personal notes) [67% — 1,474/2,200 chars]` header between `═` rules, followed by entries separated by `§`. The usage percentage is shown "so the agent knows capacity". — [Memory docs](https://hermes-agent.nousresearch.com/docs/user-guide/features/memory)
- [docs] The `memory` tool has no `read` action, because "memory content is automatically injected into the system prompt at session start." — [Memory docs](https://hermes-agent.nousresearch.com/docs/user-guide/features/memory)
- [code] `MemoryStore._system_prompt_snapshot` "is frozen at load time (prefix-cache stable)". `load_from_disk()` captures it. `format_for_system_prompt()` returns the "Frozen load-time snapshot (NOT live state — mid-session writes don't touch" it. — [HA/tools/memory_tool_store.py#L179-L258, #L655-L658](https://github.com/NousResearch/hermes-agent/blob/f578714e0ee122179ec28fecb9af989583b71baf/tools/memory_tool_store.py#L655)
- [code] Threat-scan hits are replaced by a `[BLOCKED: …]` placeholder in the snapshot only. — [HA/tools/memory_tool_store.py#L223-L226](https://github.com/NousResearch/hermes-agent/blob/f578714e0ee122179ec28fecb9af989583b71baf/tools/memory_tool_store.py#L223)

**System prompt layout**
- [code] The system prompt is built in three ordered cache tiers:
  - `stable`: identity, guidance, pinned skills, coding brief.
  - `context`: caller system_message, project context files such as AGENTS.md or CLAUDE.md, workspace snapshot.
  - `volatile`: in order, `[skills_prompt, *_memory_parts(agent)]`, which is the skills index, then the MEMORY snapshot, the USER snapshot and the external provider's `system_prompt_block()`. Plugin sections, the profile line, the timestamp line and runtime environment hints follow.
  - The docstring says it is "Never re-rendered mid-session"; `build_system_prompt` is "cached on `agent._cached_system_prompt` and only rebuilt after compression."
  - Sources: [HA/agent/system_prompt.py#L751-L826](https://github.com/NousResearch/hermes-agent/blob/f578714e0ee122179ec28fecb9af989583b71baf/agent/system_prompt.py#L751), [#L535-L562 `_memory_parts`](https://github.com/NousResearch/hermes-agent/blob/f578714e0ee122179ec28fecb9af989583b71baf/agent/system_prompt.py#L535)
- [code] Comment on the volatile tier: "most likely to differ on a rebuild; kept last so the stable prefix stays reusable … Skills are runtime-mutable, so the index leads the volatile band." — [HA/agent/system_prompt.py#L797-L800](https://github.com/NousResearch/hermes-agent/blob/f578714e0ee122179ec28fecb9af989583b71baf/agent/system_prompt.py#L797)
- [docs, partly stale] The prompt-assembly page agrees that memory and profile snapshots sit in the volatile tier and are "not injected as ad-hoc mid-turn overlays". Its illustrative "Layer" list is in a different order from the code: Honcho static block = layer 3, MEMORY = 5, USER = 6, skills index = 7. The code puts the skills index *before* memory. — [Prompt assembly docs](https://hermes-agent.nousresearch.com/docs/developer-guide/prompt-assembly), contradicted by [HA/agent/system_prompt.py#L800](https://github.com/NousResearch/hermes-agent/blob/f578714e0ee122179ec28fecb9af989583b71baf/agent/system_prompt.py#L800)

**When the snapshot is rebuilt**
- [code] Compression calls `invalidate_system_prompt()`. It clears the cached prompt and "reloads memory from disk so the rebuilt prompt captures any writes from this session". Compression is therefore the only automatic mid-session path by which new MEMORY.md entries reach the prompt. — [HA/agent/system_prompt.py#L829-L848](https://github.com/NousResearch/hermes-agent/blob/f578714e0ee122179ec28fecb9af989583b71baf/agent/system_prompt.py#L829)
- [code] A *continuing* session, such as a resumed gateway chat, "replays its stored system prompt verbatim so the provider prefix cache stays warm". As a result, SOUL.md, skills and project-context edits "never reach it before `/new`". The operator command `hermes sessions refresh-prompts` records an epoch. The next turn then rebuilds the prompt once: "One prefix-cache break per request … Nothing watches files and nothing changes mid-turn." — [HA/agent/prompt_refresh.py#L1-L13](https://github.com/NousResearch/hermes-agent/blob/f578714e0ee122179ec28fecb9af989583b71baf/agent/prompt_refresh.py#L1)
- [docs] "Memory Needs Session Boundaries" explains why boundaries matter. On messaging platforms one chat is "one continuous session" that can run for weeks. There, "the learning loop of forget → recall from memory → search past sessions almost never gets to fire. Fresh memory entries also stay invisible to the running session because of the frozen snapshot." The advice is to run `/new` at natural boundaries. — [Memory docs](https://hermes-agent.nousresearch.com/docs/user-guide/features/memory)

**External provider recall (per user turn)**
- [code] `_memory_turn_start_and_prefetch()` first calls `on_turn_start` on every provider. It then calls `prefetch_all(query)` **once per user turn, "before the tool loop"**, skipping "trivial prompts with no semantic signal". — [HA/agent/turn_context.py#L1060-L1090](https://github.com/NousResearch/hermes-agent/blob/f578714e0ee122179ec28fecb9af989583b71baf/agent/turn_context.py#L1060)
- [code] The result is held in `ext_prefetch_cache: str = ""  # external-memory prefetch, reused across iterations`. It is not re-fetched for each tool-loop API call. — [HA/agent/turn_context.py#L563](https://github.com/NousResearch/hermes-agent/blob/f578714e0ee122179ec28fecb9af989583b71baf/agent/turn_context.py#L563)
- [docs vs code] The plugin guide's hook table says `prefetch` is called "Before each API call" ([Memory provider plugin docs](https://hermes-agent.nousresearch.com/docs/developer-guide/memory-provider-plugin)). The code calls it once per user turn and reuses the result across the tool-loop iterations of that turn ([HA/agent/turn_context.py#L563, #L1367](https://github.com/NousResearch/hermes-agent/blob/f578714e0ee122179ec28fecb9af989583b71baf/agent/turn_context.py#L1367)).

**Skills (progressive disclosure)**
- [docs] Skills "follow a progressive disclosure pattern": Level 0 `skills_list()` returns name, description and category (about 3k tokens); Level 1 `skill_view(name)` returns full content; Level 2 `skill_view(name, path)` returns a reference file. — [Skills docs](https://hermes-agent.nousresearch.com/docs/user-guide/features/skills)
- [code] The skills index leads the volatile tier. Pinned or auto-loaded skills are "per-agent constants (resolved once)", so they sit in the stable prefix (`_auto_load_parts`). — [HA/agent/system_prompt.py#L776-L800](https://github.com/NousResearch/hermes-agent/blob/f578714e0ee122179ec28fecb9af989583b71baf/agent/system_prompt.py#L776)
- [docs] A skill can be shown only when certain toolsets or tools are, or are not, available (`requires_toolsets`, `fallback_for_toolsets`, and so on). — [Skills docs](https://hermes-agent.nousresearch.com/docs/user-guide/features/skills)
- [issue] Open PR #95387 (2026-08-26), "implicit skill prefetch". The motivation: the model reads the index, then has to call `skill_view()` at the cost of a round trip, and "On long conversations the model sometimes skips the `skill_view()` call entirely even when the user explicitly named the skill." It proposes loading a skill's SKILL.md into the turn's prefetch cache when the user prompt mentions its name at a word boundary, ported from OpenAI Codex implicit invocation. — [PR #95387](https://github.com/NousResearch/hermes-agent/pull/95387)
- [docs] Memory docs suggest a skill rather than a memory entry for recurring-task facts, because a skill "loads only when relevant and does not compete for the 2,200-character budget." — [Memory docs](https://hermes-agent.nousresearch.com/docs/user-guide/features/memory)

### Inferences
- Hermes deliberately splits recall by volatility:
  - always-on, tiny, curated facts live in the cached system prompt and are frozen per session;
  - query-dependent recall rides on the user-message channel each turn;
  - deep history and procedures are pull-only, through tools.

  This maps naturally onto Claude Code: SessionStart injection for the frozen snapshot, UserPromptSubmit `additionalContext` for per-turn recall, and MCP tools for pull.
- Because the per-turn recall is persisted on the user row (see Q3), it costs prefix-cache tokens on every later request for as long as that row stays in context. This is why Hermes cares about deduplication inside the block (Q4).

### Gaps
- This shallow clone could not tell me whether older versions put the external provider block, or per-turn recall, in the system prompt. Issue #12081 says recall was "injected into the system prompt of the following turn", but that is the issue author's description, not verified code (see Q3).

---

## Q2. How is session search (SQLite/FTS5) triggered, and what does it return?

### Takeaway
`session_search` is **purely model-invoked**. Nothing calls it automatically on each turn. A system-prompt guidance line nudges its use, and compaction summaries embed recovery hints that point to it. It returns **actual stored messages**, not LLM summaries, through an FTS5 search deduped by session lineage. The top hit is fully hydrated; the other hits are compact. The README still claims "LLM summarization", which conflicts with the current docs and code.

### Cited Findings
- [code] Module docstring: "Single-shape tool; the mode is inferred from the args: DISCOVERY (`query`; FTS5 deduped by lineage, adaptive detail hydrates only the top result), SCROLL (`session_id` + `around_message_id`; ±window around the anchor), READ (`session_id` alone; whole session or head/tail), BROWSE (no args). No LLM calls — every shape returns actual DB messages." — [HA/tools/session_search_tool.py#L1-L9](https://github.com/NousResearch/hermes-agent/blob/f578714e0ee122179ec28fecb9af989583b71baf/tools/session_search_tool.py#L1)
- [code] Constants:
  - Hidden sources: `kanban`, `subagent`, `tool`.
  - `cron` is searchable but demoted, to avoid "recall blindness" where cron vocabulary dominates BM25 ([#19434](https://github.com/NousResearch/hermes-agent/issues/19434)).
  - `_READ_MAX_CONTENT = 2000` characters per message in the read shape. It was added after one read returned 74K characters, raising a request from about 50K to about 89K tokens (#114344).
  - `_DISCOVER_SCAN_LIMIT = 300` FTS rows scanned before lineage dedup.
  - `_EXCLUDE_SESSION_IDS_CAP = 20`.
  - Source: [HA/tools/session_search_tool.py#L22-L48](https://github.com/NousResearch/hermes-agent/blob/f578714e0ee122179ec28fecb9af989583b71baf/tools/session_search_tool.py#L22)
- [code] Default `limit` is 3 for discovery and 10 for browse, clamped to 1–10. — [HA/tools/session_search_tool.py#L739](https://github.com/NousResearch/hermes-agent/blob/f578714e0ee122179ec28fecb9af989583b71baf/tools/session_search_tool.py#L739)
- [docs] What a discovery result carries:
  - `session_id`, `title`, `when`, `source`;
  - `snippet`, the FTS5-highlighted excerpt;
  - `detail`;
  - `bookend_start` / `bookend_end`, the first and last 3 user+assistant messages, for full results only;
  - `messages`, ±5 around the match for full results, or only the anchor for compact results;
  - `match_message_id`, `messages_before`, `messages_after`.

  Other details:
  - `detail="full"` hydrates every result.
  - `role_filter` defaults to `user,assistant`, because "tool output is usually noise".
  - `sort=newest|oldest` sits on top of relevance.
  - FTS5 syntax supports AND by default, phrases, OR/NOT and prefix matching.
  - Typical times are "tens of milliseconds" for discovery and "1–2ms per scroll call".

  Source: [Sessions docs](https://hermes-agent.nousresearch.com/docs/user-guide/sessions)
- [docs] "When It's Used: The agent is prompted to use session search automatically." The guidance quoted is: "When the user references something from a past conversation or you suspect relevant prior context exists, use session_search to recall it before asking them to repeat themselves." Typical triggers listed: "we did this before", "remember when", "last time". — [Sessions docs](https://hermes-agent.nousresearch.com/docs/user-guide/sessions)
- [code] The guidance constant `SESSION_SEARCH_GUIDANCE` is added to the system prompt only when the tool is available. — [HA/agent/prompt_builder.py#L240-L243](https://github.com/NousResearch/hermes-agent/blob/f578714e0ee122179ec28fecb9af989583b71baf/agent/prompt_builder.py#L240), [HA/agent/system_prompt.py#L300](https://github.com/NousResearch/hermes-agent/blob/f578714e0ee122179ec28fecb9af989583b71baf/agent/system_prompt.py#L300)
- [code] The tool description tells the model it "Searches conversation history ONLY — when the user gave a direct source (URL, file, contact, live system), inspect that first; never conclude 'not found' from history alone." — [HA/tools/session_search_tool.py#L816-L831](https://github.com/NousResearch/hermes-agent/blob/f578714e0ee122179ec28fecb9af989583b71baf/tools/session_search_tool.py#L816)
- [code] Compaction leaves recovery hints in the transcript: `Recover with session_search(query=..., session_id='…')`. — [HA/agent/context_compressor_tail.py#L48](https://github.com/NousResearch/hermes-agent/blob/f578714e0ee122179ec28fecb9af989583b71baf/agent/context_compressor_tail.py#L48), [HA/agent/context_compressor.py#L1000](https://github.com/NousResearch/hermes-agent/blob/f578714e0ee122179ec28fecb9af989583b71baf/agent/context_compressor.py#L1000)
- [code] No automatic per-turn call of session_search was found in the agent loop. The only programmatic caller found is an inline executor that runs when the model invokes the tool (`agent/inline_tool_executors.py:127`). — [HA/agent/inline_tool_executors.py#L127](https://github.com/NousResearch/hermes-agent/blob/f578714e0ee122179ec28fecb9af989583b71baf/agent/inline_tool_executors.py#L127)
- [docs] Comparison table: memory is "Instant (in system prompt)" with a "Token cost in every prompt" of about 1,300 tokens. Session search is "~20ms FTS5 query, ~1ms scroll", "Free — no LLM calls", and "On-demand". — [Memory docs](https://hermes-agent.nousresearch.com/docs/user-guide/features/memory)
- [marketing, conflicting] The README advertises "FTS5 session search with LLM summarization for cross-session recall." ([README](https://github.com/NousResearch/hermes-agent/blob/f578714e0ee122179ec28fecb9af989583b71baf/README.md)). The docs say the opposite: "no LLM summarization, no truncation" ([Memory docs](https://hermes-agent.nousresearch.com/docs/user-guide/features/memory)), and so does the code ("No LLM calls").
- [issue] #46955 (open, 2026-06-16): "agents still habitually reach for `session_search` … before reading the memory context that is already injected". The proposal is a prompt-side instruction to check injected memory blocks first. — [Issue #46955](https://github.com/NousResearch/hermes-agent/issues/46955)
- [issue] #29901 (closed 2026-05-21) asked to let an external memory provider serve as the session_search backend instead of local FTS5. #46955 calls it "COMPLETED". — [Issue #29901](https://github.com/NousResearch/hermes-agent/issues/29901)

### Inferences
- The README's "LLM summarization" claim most likely describes an earlier session_search implementation that was later replaced by raw-message views. That history was not verified (see Gaps).

### Gaps
- How #29901 was implemented could not be confirmed: I found no provider-as-backend hook in `session_search_tool.py` while scanning it. The point is open.
- The commit that removed LLM summarization from session_search could not be found, because the clone is shallow and history is reportedly rewritten on release.

---

## Q3. External memory providers: is there a per-turn prefetch hook? Synchronous or background? Where is recall placed, and how is it framed?

### Takeaway
Yes, providers get two hooks, and the core is a hybrid:
- **`prefetch(query)`** runs **synchronously on the turn's critical path**. It runs once, before the first LLM call of a user turn, and the join is bounded by a hard-coded **8 s** timeout.
- **`queue_prefetch(query)`** fires in the **background after each completed turn**, to pre-warm the next turn.

The ABC asks providers to make `prefetch()` "fast — recall in the background and return cached results". As a result, providers in their default async mode (RetainDB; Honcho unless `recallSync`) inject at turn N recall that was computed from turn N−1's message. ByteRover, and Honcho with `recallSync: true`, recall on the current query synchronously instead.

Recall is **appended to the current user message**, never the system prompt, inside `<memory-context>…</memory-context>` with a "[System note: … NOT new user input …]" header. The composed bytes are stamped into an `api_content` sidecar and persisted, so later requests replay them exactly. Before mid-2026 the design was purely async with a one-turn lag; issue #12081 asked for synchronous injection and is closed.

### Cited Findings

**Hook contract**
- [code] The ABC (`agent/memory_provider.py`):
  - `system_prompt_block()`: "STATIC system-prompt text … Recalled context goes through prefetch(), not here."
  - `prefetch()`: "Formatted recall context for the upcoming turn … Must be fast — recall in the background and return cached results."
  - `queue_prefetch()`: "Queue a background recall after each turn; prefetch() consumes it next turn."
  - Source: [HA/agent/memory_provider.py#L140-L151](https://github.com/NousResearch/hermes-agent/blob/f578714e0ee122179ec28fecb9af989583b71baf/agent/memory_provider.py#L140)
- [docs] Lifecycle hooks: `prefetch` "Before each API call / Return recalled context"; `queue_prefetch` "After each turn / Pre-warm for next turn"; `sync_turn` "After each completed turn"; `on_session_end`; `on_pre_compress`; `on_memory_write`. — [Memory provider plugin docs](https://hermes-agent.nousresearch.com/docs/developer-guide/memory-provider-plugin)
- [docs] User guide: "Prefetches relevant memories before each turn (background, non-blocking)". — [Memory providers docs](https://hermes-agent.nousresearch.com/docs/user-guide/features/memory-providers). Code nuance: the core's `prefetch_all` blocks the turn until the provider returns or 8 s elapse. It is "non-blocking" only when the provider serves cached results ([HA/agent/memory_manager.py#L374-L424](https://github.com/NousResearch/hermes-agent/blob/f578714e0ee122179ec28fecb9af989583b71baf/agent/memory_manager.py#L374)).

**Synchronous prefetch in the core**
- [code] `_EXTERNAL_PREFETCH_TIMEOUT_S = 8.0`. `_prefetch_provider` runs the provider's `prefetch` in a context-bound daemon thread and calls `thread.join(timeout)`. On timeout it logs that the call keeps running in the background and returns `""` for this turn. — [HA/agent/memory_manager.py#L42, #L374-L424](https://github.com/NousResearch/hermes-agent/blob/f578714e0ee122179ec28fecb9af989583b71baf/agent/memory_manager.py#L374)
- [code] The timeout is not configurable from config: `agent_init` builds `_MemoryManager()` with no argument, even though the constructor accepts `external_prefetch_timeout`. — [HA/agent/agent_init.py#L1215](https://github.com/NousResearch/hermes-agent/blob/f578714e0ee122179ec28fecb9af989583b71baf/agent/agent_init.py#L1215), [HA/agent/memory_manager.py#L213-L222](https://github.com/NousResearch/hermes-agent/blob/f578714e0ee122179ec28fecb9af989583b71baf/agent/memory_manager.py#L213)

**Background pre-warm after the turn**
- [code] After the turn, `_sync_external_memory_for_turn` calls `sync_all(user, assistant, messages=…)`. Then, unless the user text is trivial, it calls `queue_prefetch_all(user_text)`. Both are skipped for interrupted turns: "a prefetch keyed on the interrupted turn would fire against stale context. See #15218." — [HA/run_agent.py#L902-L933](https://github.com/NousResearch/hermes-agent/blob/f578714e0ee122179ec28fecb9af989583b71baf/run_agent.py#L902)
- [code] `queue_prefetch_all` and `sync_all` run on a lazily created single-worker `DaemonThreadPoolExecutor` (`mem-sync`) that serializes writes. They are "Never inline: a provider's `sync_turn` may block for minutes, which kept `run_conversation` open after the user saw the response." — [HA/agent/memory_manager.py#L439-L520](https://github.com/NousResearch/hermes-agent/blob/f578714e0ee122179ec28fecb9af989583b71baf/agent/memory_manager.py#L439)
- [code] Ordering at turn end: external sync, then queue_prefetch, then (if due) the background review fork. All of this runs after the response is delivered. — [HA/agent/turn_finalizer.py#L617-L642](https://github.com/NousResearch/hermes-agent/blob/f578714e0ee122179ec28fecb9af989583b71baf/agent/turn_finalizer.py#L617)

**Placement and framing**
- [code] Placement: `compose_user_api_content()` returns `content + "\n\n" + injection`, where `injection` is the fenced memory block joined with any `pre_llm_call` plugin context. For multimodal turns (list content), the same text goes in as a durable text part (#71998). — [HA/agent/turn_context.py#L114-L137](https://github.com/NousResearch/hermes-agent/blob/f578714e0ee122179ec28fecb9af989583b71baf/agent/turn_context.py#L114)
- [code] Cache stability: the composed bytes are stamped on the live message as an `api_content` sidecar and persisted to `state.db` ("persist what you send … stamp the exact sent bytes on the live dict for replay"). `compose_user_api_content` is the "single source for the `api_content` sidecar and the wire bytes so they never drift (what turn N sends is what turn N+1 replays)". `substitute_api_content` "keeps the prompt-cache prefix byte-stable". — [HA/agent/turn_context.py#L125-L150, #L1093-L1140](https://github.com/NousResearch/hermes-agent/blob/f578714e0ee122179ec28fecb9af989583b71baf/agent/turn_context.py#L1093)
- [code] Exact framing produced by `build_memory_context_block`:
  ```
  <memory-context>
  [System note: The following is recalled memory context, NOT new user input. Treat as authoritative reference data — this is the agent's persistent memory and should inform all responses.]

  {clean}
  </memory-context>
  ```
  — [HA/agent/memory_context.py#L183-L199](https://github.com/NousResearch/hermes-agent/blob/f578714e0ee122179ec28fecb9af989583b71baf/agent/memory_context.py#L183)
- [code] Output side: a `StreamingContextScrubber` removes `<memory-context>` spans that echo into streamed output, including spans split across deltas (#5719). The finalizer strips echoed spans from the final response (#81312). — [HA/agent/memory_context.py#L44-L60](https://github.com/NousResearch/hermes-agent/blob/f578714e0ee122179ec28fecb9af989583b71baf/agent/memory_context.py#L44), [HA/agent/turn_finalizer.py#L594](https://github.com/NousResearch/hermes-agent/blob/f578714e0ee122179ec28fecb9af989583b71baf/agent/turn_finalizer.py#L594)
- [code] Recall indicator: `describe_recall()` emits a status line such as "🧠 Provider — recalled 3 memories", "so the user SEES memory was used even if the model is silent". — [HA/agent/memory_manager.py#L426-L437](https://github.com/NousResearch/hermes-agent/blob/f578714e0ee122179ec28fecb9af989583b71baf/agent/memory_manager.py#L426)
- [docs] Recall is listed among the "API-call-time-only layers … intentionally *not* persisted as part of the cached system prompt: … later-turn Honcho/external recall injected into the current-turn user message … This separation keeps the stable prefix stable for caching." — [Prompt assembly docs](https://hermes-agent.nousresearch.com/docs/developer-guide/prompt-assembly). Code nuance: recall is kept out of the *system prompt*, but it *is* persisted on the user row (the `api_content` sidecar) so replays stay identical.

**History: from one-turn lag to synchronous injection**
- [issue] #12081 (opened 2026-04-18, closed 2026-05-10): "external memory providers in Hermes use an asynchronous 'fire-and-forget' prefetch … These memories are only injected into the system prompt of the *following* turn. This creates a 'one-turn lag'." It asked for "User Message -> Synchronous Memory Prefetch -> Inject Context … -> LLM Generation". #46955 later calls it "COMPLETED". — [Issue #12081](https://github.com/NousResearch/hermes-agent/issues/12081), [Issue #46955](https://github.com/NousResearch/hermes-agent/issues/46955)

**Per-provider behaviour**
- [code] **RetainDB** (bundled):
  - `queue_prefetch` fires three background threads at the end of the turn: a context overlay; a "dialectic" `ask_user`, whose reasoning level is "low" when the query is under 120 characters, "medium" under 400, otherwise "high"; and an agent self-model fetch.
  - `prefetch()` "Consume[s] prefetched results" and clears the cache.
  - So turn N injects recall keyed on turn N−1's message.
  - Source: [HA/plugins/memory/retaindb/__init__.py#L357-L404](https://github.com/NousResearch/hermes-agent/blob/f578714e0ee122179ec28fecb9af989583b71baf/plugins/memory/retaindb/__init__.py#L357)
- [code] **ByteRover** (bundled): `prefetch` runs `brv query` "synchronously (blocks up to _QUERY_TIMEOUT) so context is ready before the first LLM call". `queue_prefetch` is a no-op. Output is prefixed `## ByteRover Context`. — [HA/plugins/memory/byterover/__init__.py#L212-L221](https://github.com/NousResearch/hermes-agent/blob/f578714e0ee122179ec28fecb9af989583b71baf/plugins/memory/byterover/__init__.py#L212)
- [code] **Honcho** (plugin catalog, maintained by Plastic Labs):
  - `system_prompt_block()` is a "Static mode header + tool instructions (prompt-cache friendly). Live context … is injected via prefetch()."
  - `prefetch()` returns the base context layer, "refreshed on context_cadence", plus the dialectic supplement, "refreshed on dialectic_cadence". On later turns it "Consume[s] only results that are already ready; later turns never wait."
  - `queue_prefetch()` fires the context and dialectic refreshes in background threads, each gated by its own cadence.
  - Source: [HON/__init__.py#L489-L493, #L601-L646, #L761-L794](https://github.com/plastic-labs/honcho/blob/bc500af8e62cf5860ffb79dcc0966fa53f1b385e/hermes-plugin-honcho/__init__.py#L601)
- [code] Honcho's one-turn lag is explicit in its code: "in a long-lived process, turn N injects the refresh queue_prefetch() fired at the end of N-1". A resumed process recalls on N−1's message to emulate that. The turn-1 waits run in `on_turn_start` "rather than in prefetch(), which the memory manager abandons after a few seconds, so the waits can be configured longer than that." — [HON/__init__.py#L670-L685](https://github.com/plastic-labs/honcho/blob/bc500af8e62cf5860ffb79dcc0966fa53f1b385e/hermes-plugin-honcho/__init__.py#L670)
- [docs] Honcho README:
  - "Context is injected into the **user message** at API-call time (not the system prompt) to preserve prompt caching. Only a static mode header goes in the system prompt. The injected block is wrapped in `<memory-context>` fences."
  - Base layer contents: session summary, user representation, user peer card, AI self-representation, AI identity card.
  - Dialectic layer: multi-pass `.chat()`.
  - Source: [HON/README.md](https://github.com/plastic-labs/honcho/blob/bc500af8e62cf5860ffb79dcc0966fa53f1b385e/hermes-plugin-honcho/README.md)
- [docs] Honcho `recallSync` (default `false`):
  - Base and dialectic retrievals that are due "use the current user request before inference", within `timeout`/`requestTimeout` (5 s when unset).
  - "Timeouts, errors, busy workers and cadence gaps omit recall instead of reusing another query's context … No previous-query cache or generic user-representation/card fallback is substituted."
  - "The default is `false`, preserving background recall and cache reuse across turns."
  - Source: [HON/README.md](https://github.com/plastic-labs/honcho/blob/bc500af8e62cf5860ffb79dcc0966fa53f1b385e/hermes-plugin-honcho/README.md), [HON/recall_sync.py#L1-L20](https://github.com/plastic-labs/honcho/blob/bc500af8e62cf5860ffb79dcc0966fa53f1b385e/hermes-plugin-honcho/recall_sync.py#L1)
- [docs] Honcho first-turn waits: `firstTurnBaseWait` 3.0 s ("The wait runs before Hermes' prefetch timeout starts, so values above it hold. `0` disables … context surfaces on later turns") and `firstTurnDialecticWait` 2.0 s. — [HON/README.md](https://github.com/plastic-labs/honcho/blob/bc500af8e62cf5860ffb79dcc0966fa53f1b385e/hermes-plugin-honcho/README.md)
- [docs, inconsistent] The Hermes provider docs describe Honcho's `dialecticMaxChars` (600) as "Max chars of dialectic result injected into system prompt". The Honcho README says the injection goes into the user message, not the system prompt. — [Memory providers docs](https://hermes-agent.nousresearch.com/docs/user-guide/features/memory-providers) vs [HON/README.md](https://github.com/plastic-labs/honcho/blob/bc500af8e62cf5860ffb79dcc0966fa53f1b385e/hermes-plugin-honcho/README.md)
- [docs] Other catalog providers:
  - Hindsight: `auto_recall: true` ("Automatically recall memories before each turn"), `recall_budget: mid` (low/mid/high), `memory_mode: hybrid|context|tools`.
  - Supermemory: `auto_recall: true`, `max_recall_results: 10`, `profile_frequency: 50` ("Include profile facts on first turn and every N turns"), `api_timeout: 5.0`, and "Automatic context fencing — strips recalled memories from captured turns to prevent recursive memory pollution".
  - Source: [Memory providers docs](https://hermes-agent.nousresearch.com/docs/user-guide/features/memory-providers)

**Known issues**
- [issue] #8893 (open, 2026-04-13): appending prefetch to the current user message "can contaminate the active user turn: even though the block says 'NOT new user input', it is still delivered inside the user-message channel, which can cause the model to drift toward recalled memory instead of responding to the user's actual question." The reporter proposes tools-only access by default, or a separate channel. — [Issue #8893](https://github.com/NousResearch/hermes-agent/issues/8893)

### Inferences
- Hermes's core is synchronous per turn, but recall freshness is the provider's choice. Default async providers trade *relevance to the current query* (a one-turn lag) for *zero added latency and cache reuse*. Opt-in synchronous modes trade latency, capped at about 5–8 s, for current-query relevance.
- The current framing tells the model the block is "authoritative reference data … should inform all responses". That is stronger than the "informational background data" wording quoted in #8893. The framing wording therefore appears to have changed after April 2026. Inferred from comparing the issue text with current code.
- For a Claude Code design: a UserPromptSubmit hook that recalls synchronously on the current prompt corresponds to Hermes's ByteRover / Honcho-`recallSync` mode. Prewarming in a Stop hook and consuming the result at the next UserPromptSubmit corresponds to RetainDB / default Honcho.

### Gaps
- No PR is linked from #12081's timeline, so the exact change that closed it, and its date in a release, could not be confirmed.
- The Mem0 plugin's automatic recall behaviour (cadence, top-k) is not described in the Hermes docs sections I read. Its upstream source (`mem0ai/mem0/integrations/hermes-plugin-mem0`) was not inspected.

---

## Q4. Is there deduplication, recency/novelty gating, a token budget, or "only when relevant" filtering?

### Takeaway
The core applies:
1. A **trivial-prompt gate**: a regex that matches greetings, acknowledgements and slash commands. It skips both prefetch and queue_prefetch.
2. Skipping on **interrupted turns** and on **non-primary contexts** (subagents, cron).
3. **Within-block exact-bullet dedupe** in the composed recall.
4. An opt-in **size spill**, plus a 100K-character safety ceiling.
5. A **stuck-call admission gate** for slow backends.

The core has **no cross-turn novelty gating and no relevance threshold**; both are left to providers (Honcho `contextTokens`, cadences, empty-streak backoff; Supermemory `max_recall_results`; Hindsight `recall_budget`). Built-in memory is bounded by hard character limits and rejects exact duplicates.

### Cited Findings
- [code] `TRIVIAL_PROMPT_RE` matches, anchored, `yes|no|ok|okay|sure|thanks|thank you|y|n|yep|nope|yeah|nah|hi|hey|hello|yo|sup|continue|go ahead|do it|proceed|got it|cool|nice|great|done|next|lgtm|k`, followed only by punctuation or whitespace. `is_trivial_prompt` also returns true for empty input and anything starting with `/`. Rationale: "skipping recall saves a round-trip and keeps stale context from derailing one-word replies". — [HA/agent/memory_provider.py#L66-L81](https://github.com/NousResearch/hermes-agent/blob/f578714e0ee122179ec28fecb9af989583b71baf/agent/memory_provider.py#L66)
- [code] The trivial gate is applied both before `prefetch_all` ([HA/agent/turn_context.py#L1081-L1082](https://github.com/NousResearch/hermes-agent/blob/f578714e0ee122179ec28fecb9af989583b71baf/agent/turn_context.py#L1081)) and before `queue_prefetch_all` ([HA/run_agent.py#L929-L931](https://github.com/NousResearch/hermes-agent/blob/f578714e0ee122179ec28fecb9af989583b71baf/run_agent.py#L930)). Honcho shares the same classifier "so the two classifiers can never drift apart" ([HON/__init__.py#L796-L797](https://github.com/plastic-labs/honcho/blob/bc500af8e62cf5860ffb79dcc0966fa53f1b385e/hermes-plugin-honcho/__init__.py#L796)).
- [code] Query hygiene before a provider sees the query:
  - `/skill` scaffolding is stripped, leaving only the user's instruction (`extract_user_instruction_from_skill_message`).
  - Secrets are redacted (`_redact_for_provider`).
  - The query comes from the latest real user message, skipping scaffolding rows, `/steer` markers and gateway timestamps.
  - Sources: [HA/agent/memory_manager.py#L356-L372](https://github.com/NousResearch/hermes-agent/blob/f578714e0ee122179ec28fecb9af989583b71baf/agent/memory_manager.py#L356), [HA/agent/turn_context.py#L1016-L1058](https://github.com/NousResearch/hermes-agent/blob/f578714e0ee122179ec28fecb9af989583b71baf/agent/turn_context.py#L1016)
- [code] Dedup inside a block: `_drop_repeated_recall_lines` removes a self-contained markdown bullet only when an earlier line of the *same section* of the *same block* states it byte-for-byte. Bullets with indented continuation lines are never dropped. The rationale is about cost: "the composed block is stamped into the user row's `api_content` sidecar and replayed verbatim on every later request for as long as that row is in context, so each duplicate is paid once per turn, forever." — [HA/agent/memory_context.py#L137-L180](https://github.com/NousResearch/hermes-agent/blob/f578714e0ee122179ec28fecb9af989583b71baf/agent/memory_context.py#L137)
- [code] `sanitize_context` strips pre-wrapped `<memory-context>` tags returned by a provider, and logs a warning when it does. — [HA/agent/memory_context.py#L15-L23, #L183-L190](https://github.com/NousResearch/hermes-agent/blob/f578714e0ee122179ec28fecb9af989583b71baf/agent/memory_context.py#L183)
- [docs] Size: external `prefetch()` results are returned "in full by default", up to a safety ceiling of 10× `hooks.output_spill.max_chars` (at least 100,000 characters). With `memory.prefetch_spill_enabled: true` (default `false`), results over `hooks.output_spill.max_chars` (default 10,000) are written to a spill file. They are replaced by the first and last 500 characters plus a path, so "the relevance-ranked middle is no longer immediately visible". — [Memory provider plugin docs](https://hermes-agent.nousresearch.com/docs/developer-guide/memory-provider-plugin), [Memory docs](https://hermes-agent.nousresearch.com/docs/user-guide/features/memory)
- [code] Stuck-backend gating: `_StuckCallGate` with `_MAX_STUCK_CALLS = 4`, `_PROBE_INTERVAL_S = 60.0`, `_MAX_INFLIGHT_CALLS = 16` and `_BACKGROUND_CALL_STALL_S = 30.0`. While the gate is closed, later turns skip a provider whose earlier calls are still hung. — [HA/agent/memory_manager.py#L42-L58](https://github.com/NousResearch/hermes-agent/blob/f578714e0ee122179ec28fecb9af989583b71baf/agent/memory_manager.py#L42)
- [docs] Contexts where the host does not run prefetch: for `agent_context="subagent"` the host keeps "no `prefetch()`/`queue_prefetch()`, no `sync_turn()`, `on_turn_start()`, `on_session_end()` or `on_delegation()`". — [Memory provider plugin docs](https://hermes-agent.nousresearch.com/docs/developer-guide/memory-provider-plugin)
- [docs] Honcho budgets and gates:
  - `contextTokens`: "Token budget for auto-injected context per turn", default `null` (uncapped). In code it truncates at about 4 characters per token, at a word boundary.
  - `contextCadence` default 1; `dialecticCadence` default 2 (recommended 1–5); `dialecticDepth` 1–3; `injectionFrequency` `every-turn` or `first-turn`.
  - The dialectic cadence widens with an "empty-streak backoff".
  - Sources: [Memory providers docs](https://hermes-agent.nousresearch.com/docs/user-guide/features/memory-providers), [HON/__init__.py#L748-L794](https://github.com/plastic-labs/honcho/blob/bc500af8e62cf5860ffb79dcc0966fa53f1b385e/hermes-plugin-honcho/__init__.py#L748)
- [docs] Honcho `queryRewrite` (opt-in) uses the shared auxiliary task `memory_query_rewrite` to turn the latest message into one retrieval question, at the cost of one extra auxiliary LLM call per dialectic cycle. — [HON/README.md](https://github.com/plastic-labs/honcho/blob/bc500af8e62cf5860ffb79dcc0966fa53f1b385e/hermes-plugin-honcho/README.md). Core helper: "Provider-agnostic: any memory provider can pass `rewrite_memory_query` … configured under `auxiliary.memory_query_rewrite`" ([HA/plugins/memory/query_rewrite.py#L1-L5](https://github.com/NousResearch/hermes-agent/blob/f578714e0ee122179ec28fecb9af989583b71baf/plugins/memory/query_rewrite.py#L1)).
- [docs] Built-in memory:
  - Hard limits of 2,200 and 1,375 characters. "Memory does **not** auto-compact": an overflowing write returns an error with `current_entries`, and the agent must consolidate in one batch.
  - Exact duplicate entries are rejected ("no duplicate added").
  - Entries are scanned for prompt-injection and exfiltration patterns and invisible Unicode.
  - Source: [Memory docs](https://hermes-agent.nousresearch.com/docs/user-guide/features/memory)
- [code] The config comment puts the budget at "~800 tokens at 2.75 chars/token" (memory) and "~500 tokens" (user). — [HA/hermes_cli/config_defaults.py#L1261-L1276](https://github.com/NousResearch/hermes-agent/blob/f578714e0ee122179ec28fecb9af989583b71baf/hermes_cli/config_defaults.py#L1261)

### Inferences
- The core does not check whether the same recalled fact was already injected on a previous turn. Because each turn's block is persisted on its own user row, repeated recall of the same fact across turns accumulates in context. This is inferred from reading `prefetch_all`, `build_memory_context_block` and the dedupe docstring, which is explicitly scoped to "this same block". I found no cross-turn ledger.

### Gaps
- No core relevance-score threshold exists. Whether individual catalog providers (Mem0, Hindsight, Supermemory) apply score cutoffs was not verified in their source.

---

## Q5. How do memory writes happen: model tool, background review or nudge every N turns, end-of-session flush? Any nudge constants?

### Takeaway
There are three write paths:
1. **Foreground, model-directed `memory` tool calls**: add, patch and remove, batched per target.
2. A **background self-improvement review fork**. It is spawned after delivery when either counter trips: **10 user turns** since the last memory tool use (`memory.nudge_interval`, default 10), or **10 tool-calling iterations** since the last `skill_manage` call (`skills.creation_nudge_interval`, default 10).
3. **External providers**: `sync_turn` after every completed turn (in the background), `on_session_end` at real session boundaries, `on_pre_compress` before compaction, and `on_memory_write` mirroring.

I found no built-in end-of-session "flush" that writes MEMORY.md.

### Cited Findings

**Foreground: the `memory` tool**
- [code] The `memory` tool schema:
  - "Memory is injected into every future turn, so keep entries compact."
  - "WHEN: only for facts that apply to EVERY session regardless of task".
  - Task learnings "belong in the task's skill via skill_manage".
  - SKIP "task progress, completed-work logs, temporary TODO state (use session_search for those)".
  - Source: [HA/tools/memory_tool.py#L373-L395](https://github.com/NousResearch/hermes-agent/blob/f578714e0ee122179ec28fecb9af989583b71baf/tools/memory_tool.py#L373)
- [docs, inconsistent] The memory docs list "Completed work: 'Migrated database …' → save to `memory`" under "Save These (Proactively)". This contradicts the tool schema's "SKIP … completed-work logs". — [Memory docs](https://hermes-agent.nousresearch.com/docs/user-guide/features/memory) vs [HA/tools/memory_tool.py#L392-L394](https://github.com/NousResearch/hermes-agent/blob/f578714e0ee122179ec28fecb9af989583b71baf/tools/memory_tool.py#L392)
- [code] System-prompt memory guidance:
  - "Skills come first … Memory is the narrow exception for facts that apply to EVERY session".
  - "Write entries as declarative facts, not instructions to yourself: 'User prefers concise responses' ✓ — 'Always respond concisely' ✗ (imperative phrasing gets re-read as a directive in later sessions …)".
  - "A fact stale within a week belongs in session history."
  - Source: [HA/agent/prompt_builder.py#L195-L234](https://github.com/NousResearch/hermes-agent/blob/f578714e0ee122179ec28fecb9af989583b71baf/agent/prompt_builder.py#L195)
- [docs] Memory "only persists when the model *calls the `memory` tool*; a sentence like 'I've added that to my memory' is just text". Small local models (under about 30B) often claim saves they never made. — [Memory docs](https://hermes-agent.nousresearch.com/docs/user-guide/features/memory)
- [docs] There is no longer a `replace` action ("models used it as a find-and-swap, which wiped the other facts"); `patch` replaces a span instead. — [Memory docs](https://hermes-agent.nousresearch.com/docs/user-guide/features/memory)

**Background review and nudge counters**
- [code] Defaults:
  - `agent._memory_nudge_interval = int(mem_config.get("nudge_interval", 10))`.
  - `agent._skill_nudge_interval = int(... "creation_nudge_interval", 10)`.
  - Sources: [HA/agent/agent_init.py#L1193](https://github.com/NousResearch/hermes-agent/blob/f578714e0ee122179ec28fecb9af989583b71baf/agent/agent_init.py#L1193), [#L1235-L1237](https://github.com/NousResearch/hermes-agent/blob/f578714e0ee122179ec28fecb9af989583b71baf/agent/agent_init.py#L1235)
  - Config comment: `"nudge_interval": 10,  # Periodic built-in memory review; 0 when an external provider auto-extracts.` ([HA/hermes_cli/config_defaults.py#L1271-L1272](https://github.com/NousResearch/hermes-agent/blob/f578714e0ee122179ec28fecb9af989583b71baf/hermes_cli/config_defaults.py#L1271))
- [code] Counter mechanics:
  - `_tick_memory_nudge` increments `_turns_since_memory` once per user turn and fires when it reaches the interval, then resets it ([HA/agent/turn_context.py#L820-L829](https://github.com/NousResearch/hermes-agent/blob/f578714e0ee122179ec28fecb9af989583b71baf/agent/turn_context.py#L820)).
  - Any `memory` tool call resets `_turns_since_memory = 0`; any `skill_manage` call resets `_iters_since_skill = 0` ([HA/agent/tool_executor.py#L745-L748](https://github.com/NousResearch/hermes-agent/blob/f578714e0ee122179ec28fecb9af989583b71baf/agent/tool_executor.py#L745)).
  - On resume, the counter is rehydrated as `prior_user_turns % interval` ([HA/agent/turn_context.py#L812-L817](https://github.com/NousResearch/hermes-agent/blob/f578714e0ee122179ec28fecb9af989583b71baf/agent/turn_context.py#L812)).
- [code] The skill review fires when `_iters_since_skill >= _skill_nudge_interval` and `skill_manage` is available. `post_llm_call` plugins receive read-only `review_signals` so they can follow the same cadence. — [HA/agent/review_signals.py#L1-L40](https://github.com/NousResearch/hermes-agent/blob/f578714e0ee122179ec28fecb9af989583b71baf/agent/review_signals.py#L1)
- [code] What the review is: "After every turn `AIAgent.run_conversation` may spawn a daemon thread that replays the conversation snapshot in a forked `AIAgent` and asks 'should any skill/memory be saved or updated?'. Writes go straight to the memory + skill stores; the main conversation and prompt cache are never touched. The fork inherits the parent's live runtime … so it hits the same prefix cache, and runs under a dispatch-side tool whitelist." — [HA/agent/background_review.py#L1-L6](https://github.com/NousResearch/hermes-agent/blob/f578714e0ee122179ec28fecb9af989583b71baf/agent/background_review.py#L1)
- [code] Memory review prompt: "Review the conversation above and consider saving to memory if appropriate … If nothing is worth saving, just say 'Nothing to save.' and stop." It is preceded by a routing block that forbids inferring user traits from topic or tone and forbids turning one-off requests into standing preferences. — [HA/agent/background_review.py#L450-L472](https://github.com/NousResearch/hermes-agent/blob/f578714e0ee122179ec28fecb9af989583b71baf/agent/background_review.py#L467)
- [code] When the review runs: after delivery, only if the response is non-empty and the turn was not interrupted. It is suppressed for cron, because "the fork costs ~30K tokens / event with no human-in-the-loop benefit". — [HA/agent/turn_finalizer.py#L617-L642](https://github.com/NousResearch/hermes-agent/blob/f578714e0ee122179ec28fecb9af989583b71baf/agent/turn_finalizer.py#L617)
- [docs] Review configuration under `auxiliary.background_review`:
  - `enabled` (default true);
  - `provider`/`model` (default `auto` = main model, with a full warm-cache replay; a different model gets a "digest" of recent turns verbatim plus a summary of older ones, "~3–5×" cheaper);
  - `max_input_tokens` (default 75% of the review model's window, capped at 600,000; 120,000 fallback);
  - `extra_tools` (default `[]`);
  - `defer: auto|never` and `defer_max_age_s: 1800` for managed local GPUs.

  Related settings: `/refine` triggers a review manually. `memory.write_approval` (default false) stages writes for `/memory pending`. `display.memory_notifications: off|on|verbose` controls the chat notice. — [Memory docs](https://hermes-agent.nousresearch.com/docs/user-guide/features/memory)
- [docs] A same-model review "always inherits the parent's reasoning effort … Reasoning settings, the system prompt, the full conversation snapshot, and tool definitions stay byte-identical to the parent at fork birth so the review can reuse its prompt-cache prefix." — [Memory docs](https://hermes-agent.nousresearch.com/docs/user-guide/features/memory)

**External-provider write hooks**
- [docs] `sync_turn()` "MUST be non-blocking" and should use `spawn_context_thread`. `on_session_end` covers "Conversation ends / Final extraction/flush". `on_pre_compress` covers "Before context compression / Save insights before discard"; its return value is "forwarded into the summary prompt". `on_memory_write` mirrors built-in writes. An opt-in, fail-closed checkpoint API (v2, `compression.checkpoint_required`) exists for archiving providers. — [Memory provider plugin docs](https://hermes-agent.nousresearch.com/docs/developer-guide/memory-provider-plugin)
- [code] `on_session_end` "fires only at real session boundaries, never per-turn" ([HA/agent/memory_provider.py#L197-L198](https://github.com/NousResearch/hermes-agent/blob/f578714e0ee122179ec28fecb9af989583b71baf/agent/memory_provider.py#L197)). `commit_memory_session` calls it on session-id rotation (`/new`, compression) ([HA/run_agent.py#L895-L900](https://github.com/NousResearch/hermes-agent/blob/f578714e0ee122179ec28fecb9af989583b71baf/run_agent.py#L895)). A serialized `commit_session_boundary_async` keeps extraction before rebinding; inline extraction used to block `/new` (#16454) ([HA/agent/memory_manager.py#L660-L670](https://github.com/NousResearch/hermes-agent/blob/f578714e0ee122179ec28fecb9af989583b71baf/agent/memory_manager.py#L660)).
- [docs] Honcho `writeFrequency`: `async` (background thread, the default), `turn` (sync), `session` (batch on end), or an integer N. — [Memory providers docs](https://hermes-agent.nousresearch.com/docs/user-guide/features/memory-providers)

### Inferences
- "Nudge" in current Hermes means a counter-triggered **background fork review**, not a reminder message injected into the main conversation. Whether earlier versions injected visible nudge messages could not be verified from this snapshot.
- The design defers durable writes off the critical path in every case: the background fork, the background provider sync, and the post-delivery review.

### Gaps
- I found no built-in "flush memories before compression or session end" routine for MEMORY.md/USER.md in the current code (searched for `flush_memories`, `memory_flush`; no hits). Whether one existed in earlier versions is unverified.

---

## Q6. Documented rationale and tradeoffs, plus known issues and discussions

### Takeaway
The dominant stated rationale everywhere is **prompt-cache prefix stability**:
- frozen system-prompt snapshot;
- recall in the user-message channel, with bytes persisted for exact replay;
- a review fork that reuses the parent's warm prefix;
- verbatim replay of the stored prompt on resume.

**Latency** is second: background `queue_prefetch`, background `sync_turn`, the 8 s cap, the trivial-prompt skip, and the post-delivery review. The open issues cluster around four areas:
- the cost of that design: one-turn lag (historically), the 8 s cap being too short for cold backends, and late results being discarded;
- contamination of the user channel;
- agents ignoring injected recall;
- recall evidence being lost across compaction.

### Cited Findings

**Stated rationale**
- [docs] Frozen snapshot "preserves the LLM's prefix cache for performance". — [Memory docs](https://hermes-agent.nousresearch.com/docs/user-guide/features/memory)
- [docs] "This separation keeps the stable prefix stable for caching." (API-call-time layers, including external recall in the user message.) — [Prompt assembly docs](https://hermes-agent.nousresearch.com/docs/developer-guide/prompt-assembly)
- [docs] Honcho: user-message injection "to preserve prompt caching". `recallSync` is off by default to preserve "background recall and cache reuse across turns". — [HON/README.md](https://github.com/plastic-labs/honcho/blob/bc500af8e62cf5860ffb79dcc0966fa53f1b385e/hermes-plugin-honcho/README.md)
- [docs] The review on the main model is "already warm in the prompt cache, so it's cheap cache reads". — [Memory docs](https://hermes-agent.nousresearch.com/docs/user-guide/features/memory)

**Prefetch timeout and slow backends**
- [issue] #85135 (open, 2026-08-13): "a *cold* dialectic call … reliably takes 9–11s in our self-hosted setup … The prefetch is then cancelled at 8s, so the memory supplement silently never reaches the prompt." It proposes `memory.external_prefetch_timeout_s`. — [Issue #85135](https://github.com/NousResearch/hermes-agent/issues/85135)
- [issue] #104405 (open, 2026-09-06, reported around v0.21.0): a timed-out prefetch "**discards** its result" and parks the provider. It asks that the timeout skip "this turn only" and that "a late successful result for the same query" be delivered on the next turn. It references open PR #98045. Related open PRs: #87028 ("honor external prefetch timeout") and #141443 ("make external memory prefetch timeout env-configurable"). — [Issue #104405](https://github.com/NousResearch/hermes-agent/issues/104405), [PR #141443](https://github.com/NousResearch/hermes-agent/pull/141443), [PR #87028](https://github.com/NousResearch/hermes-agent/pull/87028)
- [issue] #134392 (open, 2026-10-07): a timed-out external prefetch leaks one thread per MemoryManager instance. — [Issue #134392](https://github.com/NousResearch/hermes-agent/issues/134392)

**Channel contamination and ignored recall**
- [issue] #8893 (open): user-channel contamination and answer drift. — [Issue #8893](https://github.com/NousResearch/hermes-agent/issues/8893)
- [issue] #46955 (open): agents call `session_search` instead of reading injected recall. The request is for a prompt-side instruction. — [Issue #46955](https://github.com/NousResearch/hermes-agent/issues/46955)

**Recall lost across compaction**
- [issue] #144449 (open, 2026-10-25): "Hermes replays a user row's `api_content` to the model, but `ContextCompressor._serialize_records_for_summary()` reads only `content`". A recalled fact that exists only in the prefetch sidecar is therefore invisible to the summarizer and can be lost after compaction. — [Issue #144449](https://github.com/NousResearch/hermes-agent/issues/144449)

**Wasted prefetches and session boundaries**
- [issue] #152517 (open, 2026-11-10): cron agents queue a post-turn prefetch that no next turn ever uses. Fix PR #152531 is open. — [Issue #152517](https://github.com/NousResearch/hermes-agent/issues/152517), [PR #152531](https://github.com/NousResearch/hermes-agent/pull/152531)
- [docs] Tradeoff on long sessions: "If you never reset, a chat can run for weeks as a single session … the learning loop … almost never gets to fire". The advice is `/new` at boundaries. — [Memory docs](https://hermes-agent.nousresearch.com/docs/user-guide/features/memory)

**Older proposals**
- [issue] #625 (open, 2026-03-07) proposes "Structured Temporal Memory with Confidence-Gated Facts … Debounced Queue & Token-Aware Injection". It is an early design proposal with no evidence it was implemented. — [Issue #625](https://github.com/NousResearch/hermes-agent/issues/625)

### Inferences
- Hermes accepts **staleness** on two fronts to get cache hits and low latency: the built-in snapshot lags by a session (or until compaction), and async provider recall lags by a turn. Users who want freshness must start `/new`, run `refresh-prompts`, or enable synchronous provider modes.
- For a Claude Code hook design, the lessons are:
  - keep a small curated snapshot static per session (SessionStart);
  - make per-prompt recall cheap, gate trivial prompts, and deduplicate;
  - carry the block in the user-turn channel;
  - give it an explicit "not user input" fence;
  - scrub echoes from the output;
  - bound recall latency, and decide explicitly between current-query synchronous recall and previous-turn prewarming;
  - watch for compaction dropping injected evidence.

### Gaps
- No public design doc or blog post giving measured cache-hit or latency numbers for these choices was found in the repo docs. The only figures are the anecdotal 9–11 s cold Honcho dialectic (#85135) and the "~3–5×" cheaper digest review.

---

## Q7. Config keys, constants, file paths and code locations

### Takeaway
All locations below are verified at commit `f578714e0ee122179ec28fecb9af989583b71baf` (2026-11-15) unless marked [docs].

### Cited Findings

**Files and paths**
- [docs] Built-in store: `~/.hermes/memories/MEMORY.md` and `USER.md`, or `~/.hermes/profiles/<name>/memories/` per profile. Session DB: `~/.hermes/state.db` (SQLite + FTS5). Staged skill writes: `~/.hermes/pending/skills/<id>.json`. — [Memory docs](https://hermes-agent.nousresearch.com/docs/user-guide/features/memory)

**`memory.*` config defaults** — [HA/hermes_cli/config_defaults.py#L1261-L1276](https://github.com/NousResearch/hermes-agent/blob/f578714e0ee122179ec28fecb9af989583b71baf/hermes_cli/config_defaults.py#L1261)

| Key | Default | Notes |
|---|---|---|
| `memory_enabled` | `True` | |
| `user_profile_enabled` | `True` | |
| `prefetch_spill_enabled` | `False` | |
| `write_approval` | `False` | |
| `memory_char_limit` | `2200` | |
| `user_char_limit` | `1375` | |
| `nudge_interval` | `10` | |
| `provider` | `""` | Bundled: `retaindb`, `byterover`. Catalog: `honcho`, `hindsight`, `supermemory`, `mem0`, `openviking`, `holographic`. |

**Other config keys** ([docs] [Memory docs](https://hermes-agent.nousresearch.com/docs/user-guide/features/memory))
- `skills.creation_nudge_interval` (default 10)
- `skills.write_approval`
- `auxiliary.background_review.{enabled, provider, model, reasoning_effort, max_input_tokens, extra_tools, defer, defer_max_age_s}`
- `display.memory_notifications`
- `hooks.output_spill.{enabled, max_chars}`
- `compression.checkpoint_required`
- `auxiliary.memory_query_rewrite`

**Core constants** (`agent/memory_manager.py`) — [HA/agent/memory_manager.py#L39-L58](https://github.com/NousResearch/hermes-agent/blob/f578714e0ee122179ec28fecb9af989583b71baf/agent/memory_manager.py#L39)
- `_EXTERNAL_PREFETCH_TIMEOUT_S = 8.0`
- `_SYNC_DRAIN_TIMEOUT_S = 5.0`
- `_BACKGROUND_CALL_STALL_S = 30.0`
- `_MAX_STUCK_CALLS = 4`
- `_PROBE_INTERVAL_S = 60.0`
- `_MAX_INFLIGHT_CALLS = 16`

**Code locations**
- [code] Turn hooks: `_memory_turn_start_and_prefetch` at [HA/agent/turn_context.py#L1060](https://github.com/NousResearch/hermes-agent/blob/f578714e0ee122179ec28fecb9af989583b71baf/agent/turn_context.py#L1060), called at #L1367. Injection composition at #L114-L137. Turn end: `_sync_external_memory_for_turn` at [HA/run_agent.py#L902](https://github.com/NousResearch/hermes-agent/blob/f578714e0ee122179ec28fecb9af989583b71baf/run_agent.py#L902), with ordering in `_mirror_turn_to_memory` at [HA/agent/turn_finalizer.py#L617](https://github.com/NousResearch/hermes-agent/blob/f578714e0ee122179ec28fecb9af989583b71baf/agent/turn_finalizer.py#L617).
- [code] Fence and dedupe: [HA/agent/memory_context.py#L137, #L183](https://github.com/NousResearch/hermes-agent/blob/f578714e0ee122179ec28fecb9af989583b71baf/agent/memory_context.py#L183). Provider ABC and trivial gate: [HA/agent/memory_provider.py#L66-L151](https://github.com/NousResearch/hermes-agent/blob/f578714e0ee122179ec28fecb9af989583b71baf/agent/memory_provider.py#L66).
- [code] System prompt tiers: [HA/agent/system_prompt.py#L751-L848](https://github.com/NousResearch/hermes-agent/blob/f578714e0ee122179ec28fecb9af989583b71baf/agent/system_prompt.py#L751). Frozen snapshot: [HA/tools/memory_tool_store.py#L179-L258, #L655](https://github.com/NousResearch/hermes-agent/blob/f578714e0ee122179ec28fecb9af989583b71baf/tools/memory_tool_store.py#L179). Prompt refresh on resume: [HA/agent/prompt_refresh.py](https://github.com/NousResearch/hermes-agent/blob/f578714e0ee122179ec28fecb9af989583b71baf/agent/prompt_refresh.py).
- [code] Review fork: [HA/agent/background_review.py](https://github.com/NousResearch/hermes-agent/blob/f578714e0ee122179ec28fecb9af989583b71baf/agent/background_review.py) (prompts at #L467, #L532, #L605; spawn at #L1429). Counters: [HA/agent/turn_context.py#L820](https://github.com/NousResearch/hermes-agent/blob/f578714e0ee122179ec28fecb9af989583b71baf/agent/turn_context.py#L820) and [HA/agent/tool_executor.py#L745](https://github.com/NousResearch/hermes-agent/blob/f578714e0ee122179ec28fecb9af989583b71baf/agent/tool_executor.py#L745).
- [code] Session search: [HA/tools/session_search_tool.py](https://github.com/NousResearch/hermes-agent/blob/f578714e0ee122179ec28fecb9af989583b71baf/tools/session_search_tool.py) (schema at #L816). FTS lives in `hermes_state_fts.py` / `hermes_state_search.py` at the repo root; I did not inspect those in depth.

**Provider discovery and rules** ([docs] [Memory provider plugin docs](https://hermes-agent.nousresearch.com/docs/developer-guide/memory-provider-plugin))
- Precedence, first match wins: `plugins/memory/<name>/` (bundled, "Closed to new providers"), then `$HERMES_HOME/plugins/<name>/`, then `./.hermes/plugins/<name>/` (needs `HERMES_ENABLE_PROJECT_PLUGINS=1`), then the `hermes_agent.memory_providers` entry point.
- "Only **one** external memory provider can be active at a time."

**Honcho plugin**
- [code] Defaults set in the constructor: `_injection_frequency = "every-turn"`, `_context_cadence = 1`, `_dialectic_cadence = 1` ("backwards-compat fallback; wizard writes 2 on new configs"), `_dialectic_depth = 1`, `_reasoning_level_cap = "high"`. — [HON/__init__.py#L140-L180](https://github.com/plastic-labs/honcho/blob/bc500af8e62cf5860ffb79dcc0966fa53f1b385e/hermes-plugin-honcho/__init__.py#L140)
- [docs] Config lives in `$HERMES_HOME/honcho.json`, then `~/.hermes/honcho.json`, then `~/.honcho/config.json`. Relevant keys:
  - `recallMode` (`hybrid`)
  - `contextTokens` (null)
  - `contextCadence` (1)
  - `dialecticCadence` (2)
  - `dialecticDepth` (1)
  - `dialecticMaxChars` (600)
  - `writeFrequency` (`async`)
  - `recallSync` (false)
  - `injectionFrequency` (`every-turn`)
  - `queryRewrite` (false)
  - `firstTurnBaseWait` (3.0)
  - `firstTurnDialecticWait` (2.0)

  The opt-in injection audit log is written to `~/.honcho/injection.log`. — [Memory providers docs](https://hermes-agent.nousresearch.com/docs/user-guide/features/memory-providers), [HON/README.md](https://github.com/plastic-labs/honcho/blob/bc500af8e62cf5860ffb79dcc0966fa53f1b385e/hermes-plugin-honcho/README.md)

**What changed over time**
- [docs] Honcho, Mem0, Hindsight, Supermemory, Holographic and OpenViking moved out of the core tree into the plugin catalog, with automatic migration. Only RetainDB and ByteRover are still bundled. — [Memory providers docs](https://hermes-agent.nousresearch.com/docs/user-guide/features/memory-providers)
- [issue] One-turn-lag async recall was replaced by a synchronous per-turn `prefetch_all` (#12081, closed 2026-05-10). — [Issue #12081](https://github.com/NousResearch/hermes-agent/issues/12081)
- [docs] The `replace` memory action was removed in favour of `patch`. — [Memory docs](https://hermes-agent.nousresearch.com/docs/user-guide/features/memory)
- [marketing vs docs] The README's session_search "LLM summarization" claim conflicts with the current "no LLM calls" (see Q2).

### Inferences
- None beyond those already stated.

### Gaps
- The shallow clone (depth 200) and the reported history rewrites prevented dating most behaviour changes precisely. Version labels seen: rc tags for v0.23.2; "v0.21.0 / label 2026.8.31" mentioned in #104405; "0.17.0" in #50765.
