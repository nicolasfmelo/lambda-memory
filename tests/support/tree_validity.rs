use super::*;
mod timeline {
    include!("timeline_auto.rs");
}
use crate::domain_models::memory_tree::{TreeNode, TreeQueue};
use crate::domain_models::runtime_diagnostics::ActivityOperation;
use crate::engines::tree_policy::{valid_from_date, validity_note};
use serde_json::{Value, json};

const JAN_1: u64 = 1_735_689_600_000; // 2025-01-01 UTC
const DAY: u64 = 86_400_000;

async fn evidence(runtime: &MemoryRuntime, id: &str, at: u64) -> CaptureEvent {
    let event = CaptureEvent {
        owner: runtime.scope.owner.clone(),
        actor: runtime.scope.actor.clone(),
        project: runtime.scope.projects[0].clone(),
        source: SourceId::new(id).unwrap(),
        origin: "fixture://validity".into(),
        recorded_at_ms: at,
        kind: WorkEventKind::Decision,
        outcome: WorkOutcome::Unknown,
        content: "SQLite durable storage position changed".into(),
    };
    runtime
        .store
        .capture(event.clone(), &runtime.limits)
        .await
        .unwrap();
    event
}
fn update(op: &str, title: &str, date: Option<&str>) -> Value {
    json!({"op":op,"node":"n0","parent":null,"title":title,"content":format!("SQLite {title}"),"valid_from":date})
}
async fn curate(
    runtime: &MemoryRuntime,
    fake: &Fake,
    events: Vec<CaptureEvent>,
    ops: Vec<Vec<Value>>,
) {
    let results: serde_json::Map<_, _> = ops
        .into_iter()
        .enumerate()
        .map(|(i, ops)| (format!("s{i}"), json!(ops)))
        .collect();
    fake.reply(200, &json!({"results":results}).to_string());
    let project = &runtime.scope.projects[0];
    let mut trace = runtime
        .begin_trace(
            std::slice::from_ref(project),
            ActivityOperation::Dream,
            ActivityPhase::Curation,
            true,
        )
        .await
        .unwrap();
    let result = runtime
        .curate_batch(project, events, LinkOperation::Create, &mut trace)
        .await;
    runtime.finish_trace(trace, result).await.result.unwrap();
}
async fn current(runtime: &MemoryRuntime) -> TreeNode {
    runtime
        .store
        .tree_nodes(&runtime.scope.owner, &runtime.scope.projects[0])
        .await
        .unwrap()
        .remove(0)
}
async fn seed(runtime: &MemoryRuntime, fake: &Fake) -> TreeNode {
    let event = evidence(runtime, "original", JAN_1).await;
    curate(runtime, fake, vec![event], vec![vec![json!({"op":"create","node":null,"parent":"decisions","title":"SQLite original","content":"SQLite original position","valid_from":null})]]).await;
    current(runtime).await
}
async fn history(
    runtime: &MemoryRuntime,
    node: &MemoryId,
) -> Vec<crate::domain_models::memory_tree::NodeHistory> {
    runtime
        .node(&runtime.scope.projects[0], node, true, false, None)
        .await
        .unwrap()
        .history
}

#[tokio::test]
async fn supersede_closes_predecessor_validity() {
    for stated in [Some("2025-01-02"), None] {
        let fake = Fake::start();
        let runtime = setup(&fake).await;
        let old = seed(&runtime, &fake).await;
        let a = evidence(&runtime, "change-a", JAN_1 + 3 * DAY + 100).await;
        let b = evidence(&runtime, "change-b", JAN_1 + 4 * DAY + 200).await;
        let expected = if stated.is_some() {
            JAN_1 + DAY
        } else {
            b.recorded_at_ms
        };
        curate(
            &runtime,
            &fake,
            vec![a, b],
            vec![
                vec![update("supersede", "changed", stated)],
                vec![update("supersede", "current", None)],
            ],
        )
        .await;
        let new = current(&runtime).await;
        assert_eq!(new.memory, old.memory);
        assert_eq!(new.valid_from_ms, Some(expected));
        let revisions = history(&runtime, &old.memory).await;
        assert_eq!(revisions.len(), 2);
        assert_eq!(
            revisions
                .iter()
                .find(|r| r.item.revision.id == old.revision)
                .unwrap()
                .valid_until_ms,
            Some(expected)
        );
        let active = revisions
            .iter()
            .find(|r| r.item.revision.id == new.revision)
            .unwrap();
        assert_eq!(active.item.revision.predecessor, Some(old.revision.clone()));
        assert_eq!(active.valid_until_ms, None);
        let links = runtime
            .store
            .node_sources(
                &runtime.scope.owner,
                &runtime.scope.projects[0],
                &old.memory,
                None,
                32,
            )
            .await
            .unwrap()
            .0;
        assert_eq!(
            links
                .iter()
                .filter(|(_, op, _, _)| *op == LinkOperation::Supersede)
                .count(),
            2
        );
    }
}

#[tokio::test]
async fn merge_keeps_validity_open() {
    let fake = Fake::start();
    let runtime = setup(&fake).await;
    let original = seed(&runtime, &fake).await;
    let event = evidence(&runtime, "refine-unknown", JAN_1 + DAY).await;
    curate(
        &runtime,
        &fake,
        vec![event],
        vec![vec![update("merge", "refined", None)]],
    )
    .await;
    assert!(
        history(&runtime, &original.memory)
            .await
            .iter()
            .all(|r| r.valid_from_ms.is_none() && r.valid_until_ms.is_none())
    );
    let event = evidence(&runtime, "changed", JAN_1 + 2 * DAY).await;
    curate(
        &runtime,
        &fake,
        vec![event],
        vec![vec![update("supersede", "changed", Some("2025-01-02"))]],
    )
    .await;
    let changed = current(&runtime).await;
    let event = evidence(&runtime, "refine-known", JAN_1 + 3 * DAY).await;
    curate(
        &runtime,
        &fake,
        vec![event],
        vec![vec![update("merge", "refined again", None)]],
    )
    .await;
    assert_eq!(current(&runtime).await.valid_from_ms, Some(JAN_1 + DAY));
    let revisions = history(&runtime, &original.memory).await;
    assert_eq!(
        revisions
            .iter()
            .find(|r| r.item.revision.id == changed.revision)
            .unwrap()
            .valid_until_ms,
        None
    );
    assert_eq!(
        revisions
            .iter()
            .filter(|r| r.valid_until_ms.is_some())
            .count(),
        1
    );
}

#[tokio::test]
async fn supersede_with_invalid_date_falls_back_to_recorded_time() {
    for date in [
        "02-01-2025",
        "1999-12-31",
        "2025-01-10",
        "2025-01-01",
        "2025-02-30",
    ] {
        let fake = Fake::start();
        let runtime = setup(&fake).await;
        seed(&runtime, &fake).await;
        let event = evidence(&runtime, "first-change", JAN_1 + DAY).await;
        curate(
            &runtime,
            &fake,
            vec![event],
            vec![vec![update(
                "supersede",
                "first change",
                Some("2025-01-02"),
            )]],
        )
        .await;
        let at = JAN_1 + 3 * DAY + 123;
        let event = evidence(&runtime, "bad-date", at).await;
        curate(
            &runtime,
            &fake,
            vec![event],
            vec![vec![update("supersede", "current", Some(date))]],
        )
        .await;
        assert_eq!(current(&runtime).await.valid_from_ms, Some(at), "{date}");
    }
}

#[tokio::test]
async fn merge_and_supersede_on_one_node_make_one_supersede() {
    let fake = Fake::start();
    let runtime = setup(&fake).await;
    let old = seed(&runtime, &fake).await;
    let mut events = Vec::new();
    for i in 0..4 {
        events.push(evidence(&runtime, &format!("change-{i}"), JAN_1 + (i + 3) * DAY).await);
    }
    curate(
        &runtime,
        &fake,
        events.clone(),
        vec![
            vec![update("merge", "refinement", None)],
            vec![update("supersede", "changed", Some("2025-01-03"))],
            vec![update("supersede", "changed again", Some("2025-01-04"))],
            vec![update("merge", "last full text", None)],
        ],
    )
    .await;
    let new = current(&runtime).await;
    assert_eq!(new.title, "last full text");
    assert_eq!(new.content, "SQLite last full text");
    assert_eq!(new.valid_from_ms, Some(JAN_1 + 3 * DAY));
    assert_eq!(history(&runtime, &old.memory).await.len(), 2);
    let links = runtime
        .store
        .node_sources(
            &runtime.scope.owner,
            &runtime.scope.projects[0],
            &old.memory,
            None,
            32,
        )
        .await
        .unwrap()
        .0;
    let links: Vec<_> = links
        .iter()
        .filter(|(_, _, revision, _)| revision == &new.revision)
        .collect();
    assert_eq!(links.len(), 4);
    assert!(
        links
            .iter()
            .all(|(_, op, _, _)| *op == LinkOperation::Supersede)
    );
    assert!(events.iter().all(|e| {
        links
            .iter()
            .any(|(source, _, _, _)| source.source == e.source)
    }));
}

#[tokio::test]
async fn replaced_note_survives_a_later_merge() {
    let fake = Fake::start();
    let runtime = setup(&fake).await;
    let old = seed(&runtime, &fake).await;
    let event = evidence(&runtime, "supersede", JAN_1 + 2 * DAY).await;
    curate(
        &runtime,
        &fake,
        vec![event],
        vec![vec![update("supersede", "changed", Some("2025-01-02"))]],
    )
    .await;
    for i in 0..2 {
        let event = evidence(&runtime, &format!("merge-{i}"), JAN_1 + (i + 3) * DAY).await;
        curate(
            &runtime,
            &fake,
            vec![event],
            vec![vec![update("merge", "refined", None)]],
        )
        .await;
    }
    let node = current(&runtime).await;
    let item = runtime
        .store
        .runtime_revision(
            &runtime.scope.owner,
            &runtime.scope.projects[0],
            &node.revision,
        )
        .await
        .unwrap();
    let replaced = runtime.store.tree_replaced(&item).await.unwrap().unwrap();
    assert_eq!(replaced.title, old.title);
    assert_eq!(
        validity_note(node.valid_from_ms, Some(&replaced)),
        " (since 2025-01-02; replaced: SQLite original (until 2025-01-02))"
    );
    // A later supersede replaces the most recent position, not the original one.
    let event = evidence(&runtime, "second-supersede", JAN_1 + 6 * DAY).await;
    curate(
        &runtime,
        &fake,
        vec![event],
        vec![vec![update("supersede", "latest", Some("2025-01-06"))]],
    )
    .await;
    let latest = current(&runtime).await;
    let item = runtime
        .store
        .runtime_revision(
            &runtime.scope.owner,
            &runtime.scope.projects[0],
            &latest.revision,
        )
        .await
        .unwrap();
    assert_eq!(
        runtime
            .store
            .tree_replaced(&item)
            .await
            .unwrap()
            .unwrap()
            .title,
        "refined"
    );
}

async fn pending_update(
    runtime: &MemoryRuntime,
    old: &TreeNode,
    id: &str,
    supersede: Option<u64>,
) -> RuntimeRevision {
    let event = evidence(runtime, id, JAN_1 + 3 * DAY).await;
    let plan = CurationCommit {
        links: vec![],
        operation_id: id.into(),
        sources: vec![event.source.clone()],
        promotes: vec![],
        merges: vec![(
            old.memory.clone(),
            old.revision.clone(),
            id.into(),
            format!("SQLite {id}"),
            vec![event.source],
        )],
        supersedes: supersede
            .map(|date| (old.memory.clone(), date))
            .into_iter()
            .collect(),
        creates: vec![],
        parked: vec![],
        stale: vec![],
        split: vec![],
        operation: LinkOperation::Create,
    };
    runtime
        .store
        .commit_curation(
            &runtime.scope.owner,
            &runtime.scope.actor,
            &runtime.scope.projects[0],
            plan,
            now_ms().unwrap(),
        )
        .await
        .unwrap()
        .remove(0)
}
#[tokio::test]
async fn unpublished_supersede_never_closes_predecessor() {
    for reject in [false, true] {
        let fake = Fake::start();
        let runtime = setup(&fake).await;
        let old = seed(&runtime, &fake).await;
        let pending = pending_update(&runtime, &old, "pending", Some(JAN_1 + DAY)).await;
        assert_eq!(
            history(&runtime, &old.memory)
                .await
                .iter()
                .find(|r| r.item.revision.id == old.revision)
                .unwrap()
                .valid_until_ms,
            None
        );
        if reject {
            runtime
                .store
                .review(
                    &runtime.scope.owner,
                    &runtime.scope.actor,
                    &ReviewRequest {
                        project: runtime.scope.projects[0].clone(),
                        revision: pending.revision.id.clone(),
                        action: ReviewAction::Reject,
                        expected_active: None,
                        conflict_with: None,
                    },
                    now_ms().unwrap(),
                )
                .await
                .unwrap();
        } else {
            let winner = pending_update(&runtime, &old, "winner", None).await;
            let mut trace = runtime
                .begin_trace(
                    &runtime.scope.projects,
                    ActivityOperation::Dream,
                    ActivityPhase::Publication,
                    true,
                )
                .await
                .unwrap();
            runtime.prepare_revision(winner, &mut trace).await.unwrap();
            assert!(matches!(
                runtime.prepare_revision(pending.clone(), &mut trace).await,
                Err(RuntimeError::Conflict)
            ));
            runtime.finish_trace(trace, Ok(())).await.result.unwrap();
        }
        let revisions = history(&runtime, &old.memory).await;
        assert_eq!(
            revisions
                .iter()
                .find(|r| r.item.revision.id == old.revision)
                .unwrap()
                .valid_until_ms,
            None
        );
        assert_eq!(
            revisions
                .iter()
                .find(|r| r.item.revision.id == pending.revision.id)
                .unwrap()
                .item
                .state,
            LifecycleState::Rejected
        );
    }
}

async fn predecessor_entry(
    runtime: &MemoryRuntime,
    old: &TreeNode,
) -> crate::domain_models::memory_tree::NodeHistory {
    let item = runtime
        .store
        .runtime_revision(
            &runtime.scope.owner,
            &runtime.scope.projects[0],
            &old.revision,
        )
        .await
        .unwrap();
    runtime.store.tree_history_entry(item).await.unwrap()
}
async fn review(
    runtime: &MemoryRuntime,
    revision: &RevisionId,
    action: ReviewAction,
    expected_active: Option<RevisionId>,
    conflict_with: Option<RevisionId>,
) -> RuntimeRevision {
    runtime
        .store
        .review(
            &runtime.scope.owner,
            &runtime.scope.actor,
            &ReviewRequest {
                project: runtime.scope.projects[0].clone(),
                revision: revision.clone(),
                action,
                expected_active,
                conflict_with,
            },
            now_ms().unwrap(),
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn retiring_pending_supersede_keeps_published_winners_closure() {
    let fake = Fake::start();
    let runtime = setup(&fake).await;
    let p = seed(&runtime, &fake).await;
    let a = pending_update(&runtime, &p, "A", Some(JAN_1 + DAY)).await;
    let b = pending_update(&runtime, &p, "B", Some(JAN_1 + 2 * DAY)).await;
    let before = predecessor_entry(&runtime, &p).await;
    assert_eq!((before.valid_until_ms, before.valid_until_by), (None, None));
    review(
        &runtime,
        &b.revision.id,
        ReviewAction::Promote,
        Some(p.revision.clone()),
        None,
    )
    .await;
    let closed = predecessor_entry(&runtime, &p).await;
    assert_eq!(
        (closed.valid_until_ms, closed.valid_until_by),
        (Some(JAN_1 + 2 * DAY), Some(b.revision.id.clone()))
    );
    assert!(
        runtime
            .store
            .retire_stale(
                &runtime.scope.owner,
                &runtime.scope.actor,
                &runtime.scope.projects[0],
                &a.revision.id,
                now_ms().unwrap()
            )
            .await
            .unwrap()
    );
    let closed = predecessor_entry(&runtime, &p).await;
    assert_eq!(
        (closed.valid_until_ms, closed.valid_until_by),
        (Some(JAN_1 + 2 * DAY), Some(b.revision.id.clone()))
    );
    assert_eq!(current(&runtime).await.revision, b.revision.id);
}

#[tokio::test]
async fn published_supersede_keeps_closure_after_conflict_rejection() {
    for explicit_reject in [false, true] {
        let fake = Fake::start();
        let runtime = setup(&fake).await;
        let p = seed(&runtime, &fake).await;
        let s = pending_update(&runtime, &p, "S", Some(JAN_1 + DAY)).await;
        let m = pending_update(&runtime, &p, "M", None).await;
        review(
            &runtime,
            &s.revision.id,
            ReviewAction::Promote,
            Some(p.revision.clone()),
            None,
        )
        .await;
        review(
            &runtime,
            &s.revision.id,
            ReviewAction::Conflict,
            None,
            Some(m.revision.id.clone()),
        )
        .await;
        let closed = predecessor_entry(&runtime, &p).await;
        assert_eq!(
            (closed.valid_until_ms, closed.valid_until_by),
            (Some(JAN_1 + DAY), Some(s.revision.id.clone()))
        );
        if explicit_reject {
            review(&runtime, &s.revision.id, ReviewAction::Reject, None, None).await;
        }
        review(&runtime, &m.revision.id, ReviewAction::Promote, None, None).await;
        let closed = predecessor_entry(&runtime, &p).await;
        assert_eq!(
            (closed.valid_until_ms, closed.valid_until_by),
            (Some(JAN_1 + DAY), Some(s.revision.id.clone()))
        );
        assert_eq!(current(&runtime).await.revision, m.revision.id);
        assert_eq!(
            runtime
                .store
                .runtime_revision(
                    &runtime.scope.owner,
                    &runtime.scope.projects[0],
                    &s.revision.id
                )
                .await
                .unwrap()
                .state,
            LifecycleState::Rejected
        );
        let replaced = runtime.store.tree_replaced(&m).await.unwrap().unwrap();
        assert_eq!(
            (replaced.title, replaced.valid_until_ms),
            (p.title, JAN_1 + DAY)
        );
    }
}

#[tokio::test]
async fn pending_supersede_rejected_by_conflict_resolution_never_closes_predecessor() {
    let fake = Fake::start();
    let runtime = setup(&fake).await;
    let p = seed(&runtime, &fake).await;
    let s = pending_update(&runtime, &p, "S", Some(JAN_1 + DAY)).await;
    review(
        &runtime,
        &s.revision.id,
        ReviewAction::Conflict,
        None,
        Some(p.revision.clone()),
    )
    .await;
    let before = predecessor_entry(&runtime, &p).await;
    assert_eq!((before.valid_until_ms, before.valid_until_by), (None, None));
    review(&runtime, &p.revision, ReviewAction::Promote, None, None).await;
    let after = predecessor_entry(&runtime, &p).await;
    assert_eq!((after.valid_until_ms, after.valid_until_by), (None, None));
    assert_eq!(current(&runtime).await.revision, p.revision);
    assert_eq!(
        runtime
            .store
            .runtime_revision(
                &runtime.scope.owner,
                &runtime.scope.projects[0],
                &s.revision.id
            )
            .await
            .unwrap()
            .state,
        LifecycleState::Rejected
    );
}

#[tokio::test]
async fn published_supersede_always_owns_the_closure() {
    for days in [1, 2, 3] {
        let fake = Fake::start();
        let runtime = setup(&fake).await;
        let p = seed(&runtime, &fake).await;
        let a = pending_update(&runtime, &p, "A", Some(JAN_1 + days * DAY)).await;
        let b = pending_update(&runtime, &p, "B", Some(JAN_1 + 2 * DAY)).await;
        review(
            &runtime,
            &b.revision.id,
            ReviewAction::Promote,
            Some(p.revision.clone()),
            None,
        )
        .await;
        review(
            &runtime,
            &a.revision.id,
            ReviewAction::Conflict,
            None,
            Some(b.revision.id.clone()),
        )
        .await;
        review(&runtime, &a.revision.id, ReviewAction::Promote, None, None).await;
        // Idempotent promotion writes the same closure again.
        review(&runtime, &a.revision.id, ReviewAction::Promote, None, None).await;
        let closed = predecessor_entry(&runtime, &p).await;
        assert_eq!(
            (closed.valid_until_ms, closed.valid_until_by),
            (Some(JAN_1 + days * DAY), Some(a.revision.id.clone()))
        );
        assert_eq!(current(&runtime).await.revision, a.revision.id);
        let replaced = runtime.store.tree_replaced(&a).await.unwrap().unwrap();
        assert_eq!(replaced.valid_until_ms, JAN_1 + days * DAY);
    }
}

#[tokio::test]
async fn supersede_fallback_is_clamped_and_marks_parent_stale_and_split() {
    let fake = Fake::start();
    let mut runtime = setup(&fake).await;
    let parent = seed(&runtime, &fake).await;
    let event = evidence(&runtime, "child", JAN_1).await;
    curate(&runtime, &fake, vec![event], vec![vec![json!({"op":"create","node":null,"parent":"n0","title":"SQLite child","content":"SQLite child position","valid_from":null})]]).await;
    let child = runtime
        .store
        .tree_children(
            &runtime.scope.owner,
            &runtime.scope.projects[0],
            &parent.memory,
        )
        .await
        .unwrap()
        .remove(0);
    runtime.limits.tree_split_sources = 1;
    // Find the child's slot from the exact same map ordering as curate_batch.
    let all = runtime
        .store
        .tree_nodes(&runtime.scope.owner, &runtime.scope.projects[0])
        .await
        .unwrap();
    let map = crate::engines::tree_policy::tree_map(&all, runtime.limits.tree_map_bytes);
    let slot = format!(
        "n{}",
        map.iter().position(|n| n.memory == child.memory).unwrap()
    );
    let event = evidence(&runtime, "child-change", JAN_1 + 2 * DAY).await;
    let mut op = update("supersede", "child changed", Some("2025-01-03"));
    op["node"] = json!(slot);
    curate(&runtime, &fake, vec![event], vec![vec![op]]).await;
    assert_eq!(
        runtime
            .store
            .tree_queue_peek(
                &runtime.scope.owner,
                &runtime.scope.projects[0],
                TreeQueue::Stale
            )
            .await
            .unwrap(),
        Some(parent.memory)
    );
    assert_eq!(
        runtime
            .store
            .tree_queue_peek(
                &runtime.scope.owner,
                &runtime.scope.projects[0],
                TreeQueue::Split
            )
            .await
            .unwrap(),
        Some(child.memory.clone())
    );
    let event = evidence(&runtime, "out-of-order", JAN_1 + DAY).await;
    let mut op = update("supersede", "late evidence", None);
    op["node"] = json!(slot);
    curate(&runtime, &fake, vec![event], vec![vec![op]]).await;
    let child = runtime
        .store
        .tree_nodes(&runtime.scope.owner, &runtime.scope.projects[0])
        .await
        .unwrap()
        .into_iter()
        .find(|n| n.memory == child.memory)
        .unwrap();
    assert_eq!(child.valid_from_ms, Some(JAN_1 + 2 * DAY));
}

#[test]
fn calendar_dates_are_strict_and_bounded() {
    assert_eq!(
        valid_from_date("2024-02-29", JAN_1, None),
        Some(1_709_164_800_000)
    );
    for date in [
        "2025-02-29",
        "2024-2-29",
        "2025-00-01",
        "2025-13-01",
        "2025-01-00",
        "２０２５-01-01",
    ] {
        assert_eq!(valid_from_date(date, JAN_1, None), None, "{date}");
    }
    assert_eq!(
        valid_from_date("2025-01-02", JAN_1, None),
        Some(JAN_1 + DAY)
    );
    assert_eq!(valid_from_date("2025-01-03", JAN_1, None), None);
}
