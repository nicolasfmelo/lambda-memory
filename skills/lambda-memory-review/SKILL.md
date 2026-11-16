---
name: lambda-memory-review
description: Use for optional Lambda Memory lifecycle inspection, rejection, explicit conflicts or guarded resolution; automatic publication never requires this review step.
---

# Lambda Memory optional lifecycle maintenance

## When to use

Normal capture, consolidation, indexing, publication and recall are automatic. Do not ask for approval per claim or call `memory_review` before recall. `active` means retrievable, not verified; the main agent decides use/ignore.

Use this skill only for authorized source-backed lifecycle maintenance. Use mounted tools or the [runtime skill](../lambda-memory-runtime/SKILL.md). Pass the exact authorized project; do not widen access or delete evidence as maintenance.

## Inspect

`memory_status` gives lifecycle/conflict counts and `review.activation_requires_review: false`. `processing` means unfinished preparation, not a review queue. `memory_activity` explains retained stages without models or state changes. In tree mode, `memory_node` shows originals; a source-backed correction can merge compatible evidence or `supersede` a changed position. Then list versions:

```json
{"name":"memory_list","arguments":{"project":"demo-project","states":["processing","active","conflicted"],"limit":32}}
```

- Without `states`, list returns active/processing. Request up to five of `processing`, `active`, `rejected`, `conflicted`, `superseded`; `limit` is 1–32.
- Response size may truncate the page. Continue `next_cursor` with the same `project`, `states` and `memory`. Paging uses `watermark_ms` and revision ID; items include `watermark_ms`. Wrong-filter cursors return `isError: true`, malformed cursors `-32602`. A non-null cursor or `truncated: true` means more may remain. An incomplete list never permits unconditional replacement.
- Add `memory` for lineage and `audit`, ordered by `at_ms`, with `action`, `from_state`, `to_state`, `expected_active`, `conflict_with`, `at_ms`. Actor identity is not projected.

```json
{"name":"memory_list","arguments":{"project":"demo-project","memory":"database-choice","states":["processing","active","rejected","conflicted","superseded"],"limit":32}}
```

Compare exact IDs, project, content, predecessor and sources against current evidence. Source metadata gives attribution, not truth; this tool does not expose original text separately. Inspect referenced files only with authorization. Failed evidence can support a failure report, not a successful procedure.

## Corrections

`memory_node` shows `basis`, `confidence`, `derived_from`. Concluded nodes are inferences, hidden from ordinary curation/maintenance. Reflection can link them to observations with a contradiction; when both are recalled, the observation wins. To fix a wrong conclusion, capture new observed evidence for later reflection to lower confidence/retire it, or explicitly purge sources behind its `derived_from`. `memory_review` cannot reject an active conclusion.

UI Review has Active, Processing, Conflicted, Rejected and Superseded tabs. Only Active hides tree nodes (`exclude_tree: true`). For tree summaries use **Send correction** in the node modal: it captures a `ui://correction` source for later curation, billable in OpenRouter mode. Never use flat Correct for a tree node.

For a supported flat correction, capture evidence and propose a new revision with the same memory ID and exact `predecessor` (see [capture](../lambda-memory-capture/SKILL.md)). Publication replaces it atomically without a separate promotion/approval step.

## Reject

Only processing/conflicted contenders can be rejected; already-rejected is a no-op. It deletes no source and prevents availability. Active rejection is invalid input; superseded rejection conflicts. To withdraw an active claim, publish a supported correction or declare a supported conflict, not rejection.

```json
{"name":"memory_review","arguments":{"project":"demo-project","revision":"database-choice-r3","action":"reject"}}
```

`expected_active` is only valid for `promote`. `conflict_with` is required for `conflict`, invalid otherwise. Never weaken guards, switch modes, remove evidence or reset paid markers to force a transition. Ignoring evidence needs no lifecycle change.

## Declare and resolve a conflict

Require a supported contradiction between distinct versions of the same memory/project/kind. Similar wording or different contexts is not enough. Rejected/superseded versions and a second unresolved pair are excluded.

Here r1 is active and r2 is a processing contender:

```json
{"name":"memory_review","arguments":{"project":"demo-project","revision":"database-choice-r1","action":"conflict","conflict_with":"database-choice-r2"}}
```

Both become conflicted and leave recall. The head is cleared, so resolving to r2 omits `expected_active` (expect no head, not unconditional replacement):

```json
{"name":"memory_review","arguments":{"project":"demo-project","revision":"database-choice-r2","action":"promote"}}
```

For processing siblings with a separate active predecessor, pass its exact `expected_active`. Resolution rejects the counterpart and supersedes the replaced head atomically. OpenRouter may embed with Gemini; never automatically retry uncertain work. Replaying an active transition retains its original guard.

## Verify and report

- Malformed arguments return JSON-RPC `-32602`; invalid transitions return `isError: true`.
- Check the exact revision/state, then relevant states or a targeted recall if useful. Empty recall alone is not transition proof.
- Report confirmed states and source basis; claims remain provisional. Never force publication to get a non-empty answer.
- Review is not deletion. For explicit removal use the [purge skill](../lambda-memory-purge/SKILL.md).
