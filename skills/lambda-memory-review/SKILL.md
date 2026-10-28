---
name: lambda-memory-review
description: Use for optional Lambda Memory lifecycle inspection, rejection, explicit conflicts or guarded resolution; automatic publication never requires this review step.
---

# Lambda Memory optional lifecycle maintenance

## When to use

Normal processing is automatic: captured sources become `active` memories without review. Don't ask the user to approve each memory and don't call `memory_review` before recall. `active` means retrievable, not verified.

Use this skill only to inspect lineage, reject an unsupported contender, or declare and resolve a contradiction. Use the `memory_list` and `memory_review` tools; if they are missing, see the lambda-memory-runtime skill. Pass `project` exactly as shown in the tool description.

## Inspect

`memory_status` gives lifecycle and conflict counts. `processing` means automatic preparation is unfinished; it is not a review queue. In a memory-tree project, `memory_node` shows a node with its versions and original sources. To fix a wrong summary, capture a source-backed correction: the curator merges it into that node. Then list versions:

```json
{"name":"memory_list","arguments":{"project":"project-demo","states":["processing","active","conflicted"],"limit":32}}
```

- Without `states` the list returns `active` and `processing`. Up to five of `processing`, `active`, `rejected`, `conflicted`, `superseded` may be requested; `limit` is 1–32.
- Continue with `next_cursor`, keeping the same `project`, `states` and `memory`. A cursor from other filters returns `isError: true`; a malformed one returns `-32602`. A non-null `next_cursor` or `truncated: true` means more may remain.
- Add `memory` to see one memory's lineage. Each item then carries `audit` entries (`action`, `from_state`, `to_state`, `expected_active`, `conflict_with`, `at_ms`).

```json
{"name":"memory_list","arguments":{"project":"project-demo","memory":"database-choice","states":["processing","active","rejected","conflicted","superseded"],"limit":32}}
```

Source ids show attribution, not truth. Don't invent missing proof.

## Corrections

To replace an active claim, capture the new evidence and propose a new revision with the same memory id and the current revision as `predecessor` (see the lambda-memory-capture skill). It publishes automatically; no promotion step is needed.

## Reject

Rejection applies only to a `processing` or `conflicted` revision. It keeps that contender from becoming available and deletes nothing. An `active` revision cannot be rejected (invalid input) and a `superseded` one returns a conflict. To withdraw an active claim, publish a correction or declare a conflict.

```json
{"name":"memory_review","arguments":{"project":"project-demo","revision":"database-choice-r3","action":"reject"}}
```

## Declare and resolve a conflict

A conflict needs a supported contradiction between two distinct revisions of the same memory, project and kind. Different contexts or similar wording are not enough.

```json
{"name":"memory_review","arguments":{"project":"project-demo","revision":"database-choice-r1","action":"conflict","conflict_with":"database-choice-r2"}}
```

Both revisions become `conflicted` and leave recall. To resolve, promote one. Here the active head was cleared by the conflict, so `expected_active` is omitted (meaning "expect no active head"):

```json
{"name":"memory_review","arguments":{"project":"project-demo","revision":"database-choice-r2","action":"promote"}}
```

If another revision is active, pass it as `expected_active`. Promotion rejects the counterpart and supersedes the replaced head atomically. `expected_active` is only valid with `promote`; `conflict_with` is required for `conflict` and invalid otherwise.

## Verify and report

- JSON-RPC `-32602` means malformed arguments; `isError: true` means an invalid transition.
- Confirm the returned revision and state. State is the evidence of the transition; an empty recall can also come from query or budget limits.
- Don't force a transition to get a non-empty answer. Review never deletes; for deletion see the lambda-memory-purge skill.
