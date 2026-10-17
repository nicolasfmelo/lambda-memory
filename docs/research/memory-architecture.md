# Memory architecture research

> Background research that informed the design; not a description of current behaviour.

Research date: October 10, 2026. This note synthesizes read-only research on existing memory systems. It is not a benchmark reproduction or a product comparison. Source-reported behaviour and measurements are distinguished from design proposals. For how λambda memory works today, see the [reference](../reference.md) and [architecture](../architecture.md).

## Recommendation

Use an evidence-preserving service with a small accountable pipeline:

**Capture → immutable episodes → versioned knowledge → candidate retrieval → selection → bounded context.**

Borrow mechanisms, not an entire agent runtime. Rust, SurrealDB, OpenRouter for planning and generation, Luna Decisions for candidate selection and MCP delivery were already chosen. Capture of relevant work events (attempts, results, errors and decisions) with their origin is in scope; indiscriminate log collection and session handoffs are not.

Early synthetic checks on small fixed datasets (10 to 28 cases) found every expected memory among the retrieval candidates, while the final selection step produced most but not all of the expected contexts (for example 26 of 28). They were sanity checks of the pipeline, not quality benchmarks.

## A vocabulary that separates evidence from context

The following semantics are a proposed common vocabulary across systems, not a finalized table design.

| Memory class | What it represents | What should make it usable |
|---|---|---|
| Episodic | Events, attempts, observations and outcomes | Exact evidence, source identity, actor, workstream/project and recorded time |
| Semantic | Claims, decisions, preferences and project knowledge | Supporting episodes, authority, scope, validity and current lifecycle status |
| Procedural | A method that can be reused | Prerequisites, steps, failure conditions, validation and applicable environment |
| Working context | A small derived view for the current task | Relevance, diversity, token bound and traceable source links; never independent truth |

Source authority, extraction confidence, task relevance and lifecycle status answer different questions. An authoritative receipt may be irrelevant to the present task. A plausible extraction may still be pending review. A once-correct instruction may be superseded. A recalled passage should not become more authoritative just because a model ranked it highly.

A failed attempt belongs in episodic history with its outcome. It should not become a recommended procedure unless later evidence supports the repaired method and its prerequisites. Similarly, an assistant saying “deployment completed” is not a verified completion without an actual deployment receipt. These distinctions prevent the system from consolidating an intention or hallucinated outcome into durable advice. Project-specific facts should retain origin and applicability instead of becoming global defaults through repeated summarization.

## Pitfalls to avoid in a lexical baseline

A bounded lexical recall with term expansion, lifecycle filters (expired, inactive, superseded) and a small token budget is a reasonable starting point, and a lifecycle vocabulary with provenance, review metadata, supersession and expiry is worth keeping. Several implementation gaps should not be replicated: a non-unique dedup key plus SELECT/CREATE is not concurrency-safe; a user scope still constrained by a project key is not automatic cross-project memory; multi-write lifecycle changes need atomicity; free-text provenance is weaker than structured sources; visibility is not ACL. Bounded Jaccard similarity for review/supersession is a lexical heuristic, not generative consolidation.

## Mechanisms to borrow from the requested systems

### OptMem: recoverable originals and progressive detail

At pinned commit [1fb164c](https://github.com/VictorTaelin/OptMem/blob/1fb164cf39028047781f72ac3bb1e5a691c1dcb0/README.md), OptMem combines a Python CLI, short append-only notes (280 bytes), a binary summary tree, a bounded overview, regex searches of original notes and expansion back into tree children. Summaries are rebuildable cache; the calling agent performs compression. The [implementation](https://github.com/VictorTaelin/OptMem/blob/1fb164cf39028047781f72ac3bb1e5a691c1dcb0/memo) is independently inspectable.

Borrow recoverable originals, progressive reading and separate storage/reading budgets. Adapt compression into an explicit, interruptible derived job rather than a startup prerequisite. A chronological tree is not a general semantic index. The author's 0.03-second million-memory claim concerns startup access, not answer quality or relevant-evidence recall. Do not impose OptMem's whole agent instruction ritual on a service-oriented MCP consumer.

### Hermes: stable briefs and procedures distinct from history

[Hermes memory documentation](https://hermes-agent.nousresearch.com/docs/user-guide/features/memory/) describes bounded curated `MEMORY.md` (2,200 characters) and `USER.md` (1,375 characters), injected as a stable session snapshot to preserve cache reuse. Raw history is searchable through SQLite/FTS5; writes are model-directed and overflow is explicit. [Skills](https://hermes-agent.nousresearch.com/docs/user-guide/features/skills/) provide progressive procedural loading and reviewable changes; see also the [repository](https://github.com/NousResearch/hermes-agent).

Borrow a tiny stable startup brief, original-history fallback, procedures with explicit loading, and confirmed write receipts. A brief is a derived view, not another authoritative truth store. Separate successful procedures from transcripts of failed attempts. Local profile isolation should not be assumed to provide multi-tenant authorization. Stable context can reduce repeated prompt churn, but a stale brief still needs invalidation when its inputs change.

### [ai-memory](https://github.com/akitaonrails/ai-memory): cross-harness continuity with evidence and versions

The pinned [architecture at 88f8804](https://github.com/akitaonrails/ai-memory/blob/88f88044560437fd988b4dea49a3c4d14227d6ea/docs/ARCHITECTURE.md) describes a Rust service spanning harnesses, lifecycle hooks, MCP and handoffs, with Git-backed Markdown wiki content and a derived SQLite index. Versioned content and lexical/entity/link retrieval can be augmented by a vector stream. Borrow bounded capture, source-event idempotency, revisions and explicit retrieval tests. Lambda can use SurrealDB as its authoritative store; editable Markdown need not become a second writable source of truth unless that editing workflow is explicitly wanted.

The pinned [local embedding documentation](https://github.com/akitaonrails/ai-memory/blob/88f88044560437fd988b4dea49a3c4d14227d6ea/docs/local-embeddings.md) reports MiniLM as the default from version 2.0, while older design prose differs. “Zero LLM calls” should therefore not be interpreted as “no embeddings,” “no local model,” or “no ingestion cost.” The [cross-project profile](https://github.com/akitaonrails/ai-memory/blob/88f88044560437fd988b4dea49a3c4d14227d6ea/docs/cross-project-profile.md) distinguishes explicit global facts, repeated observations, stack applicability, privacy/sharing and project overrides. Borrow evidence-backed promotion and applicability rather than automatically generalizing repeated local statements.

The [handoff design](https://github.com/akitaonrails/ai-memory/blob/88f88044560437fd988b4dea49a3c4d14227d6ea/docs/design-handoff-offer-not-claim.md) motivates offering a baton before accepting an exact ID: automatically claiming the newest handoff at arbitrary startup can consume another task's continuation. Handoffs are out of scope for Lambda. This is a factual comparison only: Lambda will not implement session/ownership transfer, baton offers, claims or acceptance. Code inspection during this research also reported compatibility authorization paths that can become open on unknown/error conditions; that is a warning to fail closed for protected Lambda scopes, not a claim that the entire product lacks authorization.

### Episodic-memory: search, then open the original

The pinned [obra/episodic-memory README](https://github.com/obra/episodic-memory/blob/7e06519357777badd7a115d2014a7ef845904310/README.md) describes searching and reading coding conversations with local embeddings and exact substring matching, rather than BM25. Its summaries aid display; they are not the searched content. Exclusion from an index may still leave material archived.

Borrow a two-step search/open-original flow and opaque source IDs with access checks. Clearly distinguish “not indexed,” “not retrievable,” and “purged.” Do not read native session archives merely because the files exist. Evidence access must be authorized at every source read, including when following an already-visible summary.

## Additional comparators with specific design value

| Comparator | Relevant mechanism | What Lambda should avoid importing wholesale |
|---|---|---|
| Letta/MemGPT | Bounded root context, deeper indexed reads, inspectable versions | A complete resident agent runtime merely to manage memory |
| Mem0 | Additive ingestion, processing completion and explicit current-truth lifecycle | Treating historical benchmark operations as today's universal API behavior |
| Zep/Graphiti | Episodes linked to facts, recorded/valid time, historical invalidation, hybrid retrieval | Automatically generated community/global graphs before a measured need |
| Hindsight | Distinguishing world facts, experiences, observations and beliefs | One LLM agent per memory category |
| SurrealDB Agent Memory | Provenance/traces, tiered retrieval and multiple temporal axes | Assuming it is just a feature of the database or adopting its full tier system by default |

[MemGPT](https://arxiv.org/abs/2310.08560) provides the bounded-context motivation; current [Letta MemFS](https://docs.letta.com/concepts/memfs), [shared memory](https://docs.letta.com/concepts/shared-memory) and [memory configuration](https://docs.letta.com/configuration/memory) describe Git-backed Markdown/root context, deeper reads, optional semantic search and background management. Use the inspectability and context-bound lessons, while preserving Lambda's independent service boundary.

The historical [Mem0 paper](https://arxiv.org/html/2504.19413v1) describes ADD/UPDATE/DELETE/NOOP operations. Current [add documentation](https://docs.mem0.ai/core-concepts/memory-operations/add), [algorithm discussion](https://mem0.ai/blog/mem0-the-token-efficient-memory-algorithm) and [graph-memory documentation](https://docs.mem0.ai/platform/features/graph-memory) require distinguishing additive capture, pending processing and completion. Expiration that hides search need not prevent direct reads. A receipt should distinguish saved evidence, pending derivation and completed indexing.

The [Zep paper](https://arxiv.org/html/2501.13956v1), [Graphiti](https://github.com/getzep/graphiti) and [namespacing documentation](https://help.getzep.com/graphiti/core-concepts/graph-namespacing) connect episodes to facts and distinguish recorded from valid time. Historical invalidation is more useful than destroying old claims. Namespace partitioning is not authorization. Managed Zep results cannot be assumed to transfer to a custom SurrealDB implementation.

[Hindsight](https://arxiv.org/html/2512.12818v1) separates facts, experiences, observations and beliefs in retain/recall/reflect workflows. The useful inference here is to mark evidence and inferred conclusions distinctly, without adding agents merely to mirror taxonomy names.

[SurrealDB Agent Memory](https://surrealdb.com/docs/agent-memory), formerly Spectron, is an application-tier product distinct from the database. Its [coherence/retrieval/tier architecture](https://surrealdb.com/docs/agent-memory/architecture/coherence-retrieval-and-tiers) and [tri-temporal model](https://surrealdb.com/docs/agent-memory/architecture/tri-temporal-model) discuss provenance, traces and known/valid/database time. Compare those mechanisms before duplicating them, but retain unknown validity rather than inventing dates. A later experiment should justify additional tiers or temporal axes beyond recorded/valid time.

The [repeated-consolidation paper](https://arxiv.org/abs/2605.12978) reports degradation in its ARC-AGI setting. That is a caution about lossy repeated transformations, not proof that all consolidation fails. Preserve originals and require regression gates before replacing an active derived revision.

## A constrained service proposal

The following is a design proposal, not a new implemented schema. Event, MemoryVersion, Relation and Run are logical record responsibilities; they do not necessarily require four physical tables.

| Record responsibility | Information worth retaining | Reason |
|---|---|---|
| Event | Sanitized evidence/source, actor, owner, session/workstream/project, recorded time, hash | Recover the actual attempt/observation and deduplicate replay |
| MemoryVersion | Kind/status/scope, evidence IDs, predecessor/successor, optional valid interval | Represent current knowledge without erasing historical claims |
| Relation | Typed endpoints, scope and supporting evidence | Connect relevant items without treating guessed links as facts |
| Run | Processing stage/status, input/output versions, candidates, usage, latency and errors | Distinguish persistence, pending work, retrieval and degraded outcomes |

Start entities as stable IDs and explicit aliases, not an automatic ontology. `recorded_at` records acquisition; optional `valid_from`/`valid_until` describe when a claim applies. Unknown validity should stay unknown. Do not equate “recently retrieved” with “currently true.”

Capture should authorize scope and remove secrets/exclusions before storage or external model input. Use idempotent source-event identity; persist evidence and job state, then return a receipt distinguishing evidence saved from derivation pending. Derived records should carry input IDs, extraction/prompt/model lineage and a lifecycle state. Any future dedup constraint must work atomically under concurrent replay.

Recall should authorize first, then offer deterministic ID/pinned/structured fast paths. When needed, a bounded typed plan can improve lexical/entity retrieval; vectors and bounded graph expansion are optional streams to evaluate separately. Fuse and deduplicate candidates, use at most one batched Luna call, then apply deterministic lifecycle/conflict/diversity/budget rules. Return excerpts, origin, sources and a run receipt. The selector cannot recover evidence absent from the candidate pool.

On planner/selector outage, a proposed degraded mode returns bounded local ranking with explicit status; it must not imply an enhanced path succeeded. The initial enhanced path should have at most one planner and one selector request, without a third answer-synthesis call. The main task agent remains the answer-producing consumer. This was only a proposal; the implemented runtime reports provider failures as explicit errors instead of degrading to local ranking.

[Luna Decisions model information](https://openrouter.ai/openai/gpt-6-luna-decisions) and the [OpenRouter Decisions guide](https://openrouter.ai/docs/guides/community/multimodal-decisions) describe named probabilistic questions. Independent question probabilities do not automatically constitute a coherent ranking. Calibrate thresholds and use deterministic candidate IDs; supersession/conflicts remain application rules. At the listed $0.10/M input and $0 output, 6,000 billed input tokens imply $0.0006 for selection alone; 10,000 such selections imply $6. Planner, ingestion, embeddings, main-agent context and other charges are excluded. Provider telemetry is not an SLA or our measured latency.

Origin, applicability and access should remain distinct in cross-project recall. Default to the current project plus explicitly global owner defaults. An additional project must be authorized, and a generalized promotion should create a separate version with evidence and applicable conditions. It must not weaken the source restrictions. Apply access before each retrieval stream, model request, cache hit and original-source read, not merely to the final response. Stable project IDs should survive paths/worktrees changing.

Accepted dreaming consolidation should be a versioned transformation: bounded comparable inputs, preserved originals, explicit lineage and staged contradictions. Similarity is not contradiction and is insufficient evidence for supersession. Forgetting/purging must invalidate derived summaries, indexes and caches; ordinary append-only operations do not remove the need for deliberate purge.

## Reading reported benchmark results responsibly

These are separate author-reported studies with different readers, tasks, exclusions, hardware and budgets. They must not be combined into a leaderboard or treated as Lambda predictions.

The pinned [ai-memory retrieval A/B](https://github.com/akitaonrails/ai-memory/blob/88f88044560437fd988b4dea49a3c4d14227d6ea/docs/benchmarks/retrieval-ab-r2.md) reports 470 scored LongMemEval-S questions, excluding 30 abstentions. FTS/entity/graph versus the same retrieval plus MiniLM changed Hit@5 from .666 to .815. Reported p50 was 5→94 ms and p95 40→162 ms on a 7950X3D. These are retrieval metrics, not answer accuracy. Token estimates used characters/4, not actual provider billing.

The historical [Mem0 evaluation](https://arxiv.org/html/2504.19413v1) reports LoCoMo accuracy of 66.88%, graph 68.44%, and full-context 72.90% with a GPT-4o-mini setting, excluding adversarial/unanswerable cases. The advertised 26% relative improvement uses 52.90% ChatGPT-memory as its reference, not full context. Reported p95 response latency was 1.440 versus 17.117 seconds; retrieval context was 1,764 versus 26,031 tokens. Neither comparison measures lifetime capture/consolidation cost.

The [2025 Zep study](https://arxiv.org/html/2501.13956v1) reports LongMemEval-S 71.2 versus 60.2 for full context with a GPT-4o reader/GPT-4o-mini graph setup: 11 percentage points, not 11% relative. Reported latency was 2.58 versus 28.9 seconds and context about 1.6k versus 115k tokens. Current [Zep research](https://www.getzep.com/research/) uses other models/budgets; this research noted headline/category arithmetic differences, another reason to avoid ranking from a headline.

[Letta's 2025 evaluation](https://www.letta.com/blog/benchmarking-ai-agent-memory/) reports 74% LoCoMo with GPT-4o-mini and parsed/embedded conversation plus semantic/text search. It should not be represented as a result achieved by plain grep alone. Its reader setup is another distinct experiment, not a control matching the studies above.

For database work, pin the Lambda runtime version before relying on examples. [SurrealDB overview](https://surrealdb.com/docs/what-is-surrealdb) and [index documentation](https://surrealdb.com/docs/reference/query-language/statements/define/indexes) should be checked against that version; current 3.x examples may not apply to older 2.x runtimes. No proposed schema or SurrealQL query was executed here.

## An evaluation plan that tests mechanisms independently

[LongMemEval](https://arxiv.org/html/2410.10813v2) and its [dataset/repository](https://github.com/xiaowu0162/LongMemEval), plus [LoCoMo](https://snap-research.github.io/locomo/), provide useful external task structures. They do not replace a small private-workflow fixture set with authorized, sanitized evidence and known expected sources.

Measure candidate evidence recall, selection on a fixed candidate pool, downstream answer accuracy, supported citations, stale answers, abstention and unauthorized disclosure separately. Also measure ingest/index delay, total stage/model cost, and P50/P95 end-to-end latency. A successful answer with an unsupported source is not a successful evidence result; a correct abstention should not be silently excluded.

Use cases that catch likely workflow failures:

- A failed approach is not recalled as a recommended procedure.
- A corrected preference supersedes a prior version with its evidence preserved.
- Identical entity names in different projects do not merge identity or scope.
- Private evidence stays private through derived artifacts, model input and source reads.
- An action that failed does not become “completed” in a summary.
- Deletion/purge invalidates derivatives and caches.
- Replaying capture produces one source event and an intelligible receipt.
- Procedures are recalled only when their prerequisites/applicability hold.
- Provider outage returns explicit degraded behavior rather than hidden fabrication.

Ablate lexical/entity baseline → embeddings → planner → selector → graph expansion → consolidation, holding the reader, task set and evidence budget fixed. Do not introduce every mechanism simultaneously and then attribute the aggregate change to one of them. First settle memory scope; later use the ablation results to decide which optional mechanisms earn their cost.

## Evidence notes and limitations

This research was completed on October 10, 2026 without independent reproductions or paid benchmarks. This synthesis preserves pinned sources where supplied; current product pages remain time-dependent. Source-reported claims are not warranties. The pinned OptMem README, Hermes memory documentation, SurrealDB Agent Memory documentation and the pinned A/B source were re-opened while preparing the report; every implementation was not re-audited and their tests were not reproduced.

The comparison is intended to support a short set of material scope decisions, not to justify a broad MVP. No source instructions were installed, native memory archives read, existing user memories updated, or framework services launched.
