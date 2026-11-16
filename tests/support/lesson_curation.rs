use super::*;
use crate::domain_models::memory_tree::LessonStats;
use crate::domain_models::runtime_diagnostics::ActivityOperation;
use serde_json::{Value, json};

const A: &str = "0123456789abcdef";
const B: &str = "fedcba9876543210";
const C: &str = "aaaaaaaaaaaaaaaa";
fn lesson_json(kind: &str) -> String {
    json!({"v":1,"kind":kind,"scope":"project","title":"Run targeted tests","applies_when":"Changing storage","guidance":"1. Run targeted tests.\n2. Check the result.","task":"Fix storage","task_outcome":"succeeded","evidence":[]}).to_string()
}
async fn source(
    runtime: &MemoryRuntime,
    id: &str,
    kind: WorkEventKind,
    lesson_kind: &str,
) -> CaptureEvent {
    let event = CaptureEvent {
        owner: runtime.scope.owner.clone(),
        actor: runtime.scope.actor.clone(),
        project: runtime.scope.projects[0].clone(),
        source: SourceId::new(id).unwrap(),
        origin: "fixture://lesson".into(),
        recorded_at_ms: now_ms().unwrap(),
        kind: kind.clone(),
        outcome: WorkOutcome::Unknown,
        content: if kind == WorkEventKind::Lesson {
            lesson_json(lesson_kind)
        } else {
            "Run targeted tests for storage".into()
        },
    };
    runtime
        .store
        .capture(event.clone(), &runtime.limits)
        .await
        .unwrap();
    event
}
fn op(action: &str, target: &str) -> Value {
    json!({"op":action,"node":if action == "create" { Value::Null } else { json!(target) },"parent":if action == "create" { json!(target) } else { Value::Null },"title":"Run targeted tests","content":"Changing storage\n1. Run targeted tests.\n2. Check the result."})
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
async fn nodes(runtime: &MemoryRuntime) -> Vec<crate::domain_models::memory_tree::TreeNode> {
    runtime
        .store
        .tree_nodes(&runtime.scope.owner, &runtime.scope.projects[0])
        .await
        .unwrap()
}
async fn stats(runtime: &MemoryRuntime, node: &MemoryId) -> LessonStats {
    runtime
        .node(&runtime.scope.projects[0], node, false, false, None)
        .await
        .unwrap()
        .lesson
        .unwrap()
}
async fn seed(runtime: &MemoryRuntime, fake: &Fake) -> MemoryId {
    let event = source(
        runtime,
        &format!("lesson/{A}/1"),
        WorkEventKind::Lesson,
        "strategy",
    )
    .await;
    curate(
        runtime,
        fake,
        vec![event],
        vec![vec![op("create", "lessons")]],
    )
    .await;
    nodes(runtime).await[0].memory.clone()
}

#[tokio::test]
async fn lesson_source_creates_under_lessons_and_counts_session() {
    let fake = Fake::start();
    let runtime = setup(&fake).await;
    let node = seed(&runtime, &fake).await;
    let view = runtime
        .node(&runtime.scope.projects[0], &node, true, true, None)
        .await
        .unwrap();
    assert_eq!(view.node.root, TreeRoot::Lessons);
    assert_eq!(view.path, ["Lessons", "Run targeted tests"]);
    assert_eq!(
        view.lesson.unwrap(),
        LessonStats {
            sessions: vec![A.into()],
            strategies: 1,
            pitfalls: 0
        }
    );
    assert_eq!(
        runtime
            .brief(&runtime.scope.projects[0])
            .await
            .unwrap()
            .roots[0]
            .root,
        TreeRoot::Lessons
    );
    assert_eq!(
        view.history[0].item.revision.prompt_version,
        "tree-curation-v6"
    );
}

#[tokio::test]
async fn lesson_merge_reinforces_with_distinct_sessions() {
    for (action, session, expected) in [
        ("merge", A, 1),
        ("merge", B, 2),
        ("supersede", A, 1),
        ("supersede", B, 2),
    ] {
        let fake = Fake::start();
        let runtime = setup(&fake).await;
        let node = seed(&runtime, &fake).await;
        let event = source(
            &runtime,
            &format!("lesson/{session}/2"),
            WorkEventKind::Lesson,
            "strategy",
        )
        .await;
        curate(&runtime, &fake, vec![event], vec![vec![op(action, "n0")]]).await;
        assert_eq!(nodes(&runtime).await.len(), 1);
        let stats = stats(&runtime, &node).await;
        assert_eq!(stats.sessions.len(), expected);
        assert_eq!(stats.strategies, 2);
    }
}

#[tokio::test]
async fn lesson_kinds_are_counted() {
    let fake = Fake::start();
    let runtime = setup(&fake).await;
    let node = seed(&runtime, &fake).await;
    let event = source(
        &runtime,
        &format!("lesson/{B}/1"),
        WorkEventKind::Lesson,
        "pitfall",
    )
    .await;
    // A curator merge does not alter the immutable source's lesson kind.
    curate(&runtime, &fake, vec![event], vec![vec![op("merge", "n0")]]).await;
    let stats = stats(&runtime, &node).await;
    assert_eq!(
        (stats.sessions.len(), stats.strategies, stats.pitfalls),
        (2, 1, 1)
    );
}

#[tokio::test]
async fn lesson_with_no_valid_operation_is_created_under_lessons() {
    for operations in [
        vec![],
        vec![op("create", "decisions")],
        vec![op("create", "missing")],
    ] {
        let fake = Fake::start();
        let runtime = setup(&fake).await;
        let event = source(
            &runtime,
            &format!("lesson/{A}/1"),
            WorkEventKind::Lesson,
            "strategy",
        )
        .await;
        curate(&runtime, &fake, vec![event], vec![operations]).await;
        let nodes = nodes(&runtime).await;
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].root, TreeRoot::Lessons);
        assert_eq!(nodes[0].title, "Run targeted tests");
        assert_eq!(
            nodes[0].content,
            "Changing storage\n1. Run targeted tests.\n2. Check the result."
        );
    }
}

async fn seed_map_nodes(
    runtime: &MemoryRuntime,
    root: TreeRoot,
    count: usize,
    linked_at: u64,
) -> Vec<MemoryId> {
    let event = source(
        runtime,
        &format!("seed-{}", root.as_str()),
        WorkEventKind::Decision,
        "",
    )
    .await;
    let project = &runtime.scope.projects[0];
    let items = runtime
        .store
        .commit_curation(
            &runtime.scope.owner,
            &runtime.scope.actor,
            project,
            CurationCommit {
                links: vec![],
                operation_id: format!("seed-{}", root.as_str()),
                sources: vec![event.source.clone()],
                promotes: vec![],
                merges: vec![],
                supersedes: vec![],
                creates: (0..count)
                    .map(|i| {
                        (
                            ParentRef::Root(root),
                            format!("Index {i}"),
                            "Index".into(),
                            vec![event.source.clone()],
                        )
                    })
                    .collect(),
                parked: vec![],
                stale: vec![],
                split: vec![],
                operation: LinkOperation::Create,
            },
            linked_at,
        )
        .await
        .unwrap();
    let mut memories = Vec::new();
    for item in items {
        runtime
            .store
            .review(
                &runtime.scope.owner,
                &runtime.scope.actor,
                &ReviewRequest {
                    project: project.clone(),
                    revision: item.revision.id,
                    action: ReviewAction::Promote,
                    expected_active: None,
                    conflict_with: None,
                },
                linked_at,
            )
            .await
            .unwrap();
        memories.push(item.revision.memory);
    }
    memories
}

#[tokio::test]
async fn lesson_fallback_at_root_capacity_parks_without_an_extra_child() {
    let fake = Fake::start();
    let runtime = setup(&fake).await;
    seed_map_nodes(&runtime, TreeRoot::Lessons, 64, now_ms().unwrap()).await;
    let event = source(
        &runtime,
        &format!("lesson/{A}/1"),
        WorkEventKind::Lesson,
        "strategy",
    )
    .await;
    let source_id = event.source.clone();
    curate(&runtime, &fake, vec![event], vec![vec![]]).await;
    assert_eq!(nodes(&runtime).await.len(), 64);
    let project = &runtime.scope.projects[0];
    let parked = runtime
        .store
        .tree_parked_cycles(&runtime.scope.owner, project)
        .await
        .unwrap();
    assert_eq!(parked.len(), 1);
    assert_eq!(parked[0].0.source, source_id);
    assert_eq!(
        parked[0].1,
        crate::domain_models::memory_tree::ParkReason::Split
    );
    assert_eq!(
        runtime
            .store
            .tree_queue_peek(
                &runtime.scope.owner,
                project,
                crate::domain_models::memory_tree::TreeQueue::Split
            )
            .await
            .unwrap(),
        Some(TreeRoot::Lessons.memory_id())
    );
}

#[tokio::test]
async fn lesson_parking_ignores_dropped_merge_before_valid_expand() {
    let fake = Fake::start();
    let runtime = setup(&fake).await;
    let now = now_ms().unwrap();
    let lesson = seed_map_nodes(&runtime, TreeRoot::Lessons, 1, now)
        .await
        .remove(0);
    seed_map_nodes(&runtime, TreeRoot::Decisions, 1, now + 1).await;
    let map = crate::engines::tree_policy::tree_map(
        &nodes(&runtime).await,
        runtime.limits.tree_map_bytes,
    );
    assert_eq!(map[0].root, TreeRoot::Decisions);
    assert_eq!(map[1].root, TreeRoot::Lessons);
    let event = source(
        &runtime,
        &format!("lesson/{A}/1"),
        WorkEventKind::Lesson,
        "strategy",
    )
    .await;
    let terms = crate::engines::tree_policy::source_terms(&event.content);
    assert!(
        runtime
            .store
            .tree_lexical_candidates(&runtime.scope.owner, &runtime.scope.projects[0], &terms, 8)
            .await
            .unwrap()
            .is_empty()
    );
    curate(
        &runtime,
        &fake,
        vec![event],
        vec![vec![op("merge", "n0"), op("expand", "n1")]],
    )
    .await;
    let parked = runtime
        .store
        .tree_parked_cycles(&runtime.scope.owner, &runtime.scope.projects[0])
        .await
        .unwrap();
    assert_eq!(parked.len(), 1);
    assert_eq!(
        parked[0].1,
        crate::domain_models::memory_tree::ParkReason::Expand(lesson)
    );
    assert_eq!(nodes(&runtime).await.len(), 2);
}

#[tokio::test]
async fn promote_on_lesson_is_dropped() {
    let fake = Fake::start();
    let runtime = setup(&fake).await;
    let event = source(
        &runtime,
        &format!("lesson/{A}/1"),
        WorkEventKind::Lesson,
        "strategy",
    )
    .await;
    curate(&runtime, &fake, vec![event], vec![vec![op("promote", "")]]).await;
    assert_eq!(nodes(&runtime).await[0].root, TreeRoot::Lessons);
    assert!(
        runtime
            .forwards(&runtime.scope.projects[0], 32)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn expand_on_lesson_parks() {
    let fake = Fake::start();
    let runtime = setup(&fake).await;
    seed(&runtime, &fake).await;
    let event = source(
        &runtime,
        &format!("lesson/{B}/1"),
        WorkEventKind::Lesson,
        "strategy",
    )
    .await;
    curate(&runtime, &fake, vec![event], vec![vec![op("expand", "n0")]]).await;
    assert_eq!(nodes(&runtime).await.len(), 1);
    assert_eq!(
        runtime
            .store
            .tree_parked_cycles(&runtime.scope.owner, &runtime.scope.projects[0])
            .await
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn lesson_source_cannot_go_outside_lessons() {
    let fake = Fake::start();
    let runtime = setup(&fake).await;
    let event = source(
        &runtime,
        &format!("lesson/{A}/1"),
        WorkEventKind::Lesson,
        "strategy",
    )
    .await;
    curate(
        &runtime,
        &fake,
        vec![event],
        vec![vec![op("create", "decisions"), op("create", "lessons")]],
    )
    .await;
    assert_eq!(nodes(&runtime).await.len(), 1);
    assert_eq!(nodes(&runtime).await[0].root, TreeRoot::Lessons);
}

#[tokio::test]
async fn non_lesson_source_cannot_go_under_lessons() {
    let fake = Fake::start();
    let runtime = setup(&fake).await;
    let event = source(&runtime, "decision", WorkEventKind::Decision, "").await;
    curate(
        &runtime,
        &fake,
        vec![event],
        vec![vec![op("create", "lessons"), op("create", "decisions")]],
    )
    .await;
    assert_eq!(nodes(&runtime).await.len(), 1);
    assert_eq!(nodes(&runtime).await[0].root, TreeRoot::Decisions);
}

#[tokio::test]
async fn user_store_lesson_copy_goes_to_general_lessons() {
    let fake = Fake::start();
    let mut runtime = setup(&fake).await;
    runtime.scope.projects[0] = ProjectId::new("user:owner").unwrap();
    let first = source(
        &runtime,
        &format!("promote/aaaaaaaaaaaaaaaa/lesson/{A}/1"),
        WorkEventKind::Lesson,
        "strategy",
    )
    .await;
    let second = source(
        &runtime,
        &format!("promote/bbbbbbbbbbbbbbbb/lesson/{A}/2"),
        WorkEventKind::Lesson,
        "strategy",
    )
    .await;
    curate(
        &runtime,
        &fake,
        vec![first],
        vec![vec![op("create", "preferences")]],
    )
    .await;
    curate(&runtime, &fake, vec![second], vec![vec![op("merge", "n0")]]).await;
    let nodes = nodes(&runtime).await;
    assert_eq!(nodes.len(), 1);
    assert_eq!(nodes[0].root, TreeRoot::GeneralLessons);
    assert_eq!(stats(&runtime, &nodes[0].memory).await.sessions, [A]);
    // Hashed long-ID forwards cannot recover a session: each copy is its own key.
    let copy = "promote/aaaaaaaaaaaaaaaa/h-deadbeef";
    let event = source(&runtime, copy, WorkEventKind::Lesson, "pitfall").await;
    curate(&runtime, &fake, vec![event], vec![vec![op("merge", "n0")]]).await;
    assert_eq!(stats(&runtime, &nodes[0].memory).await.sessions, [A, copy]);
}

#[tokio::test]
async fn lesson_stats_follow_purge_and_split() {
    let fake = Fake::start();
    let runtime = setup(&fake).await;
    let node = seed(&runtime, &fake).await;
    for (session, n) in [(B, 1), (C, 1)] {
        let event = source(
            &runtime,
            &format!("lesson/{session}/{n}"),
            WorkEventKind::Lesson,
            "strategy",
        )
        .await;
        curate(&runtime, &fake, vec![event], vec![vec![op("merge", "n0")]]).await;
    }
    let (owner, project) = (&runtime.scope.owner, &runtime.scope.projects[0]);
    assert_eq!(stats(&runtime, &node).await.sessions.len(), 3);
    runtime
        .store
        .purge(
            owner,
            project,
            &[SourceId::new(format!("lesson/{B}/1")).unwrap()],
        )
        .await
        .unwrap();
    assert_eq!(
        runtime
            .store
            .lesson_stats(owner, project, &node)
            .await
            .unwrap()
            .sessions,
        [A, C]
    );
    // Rebuild restores the purged head from the surviving originals before splitting it.
    fake.reply(
        200,
        &json!({"title":"Run targeted tests","content":"Test storage"}).to_string(),
    );
    let mut trace = runtime
        .begin_trace(
            std::slice::from_ref(project),
            ActivityOperation::Dream,
            ActivityPhase::TreeRebuild,
            true,
        )
        .await
        .unwrap();
    let result = runtime.tree_maintenance_tick(project, &mut trace).await;
    runtime.finish_trace(trace, result).await.result.unwrap();
    assert_eq!(stats(&runtime, &node).await.sessions, [A, C]);
    let old = nodes(&runtime)
        .await
        .into_iter()
        .find(|n| n.memory == node)
        .unwrap();
    let ids: Vec<_> = [A, C]
        .map(|session| SourceId::new(format!("lesson/{session}/1")).unwrap())
        .into();
    let plan = MaintenanceCommit {
        operation_id: "lesson-split".into(),
        operation: LinkOperation::Split,
        rewrites: vec![(
            node.clone(),
            old.revision,
            "Tests".into(),
            "Test storage".into(),
            vec![ids[0].clone()],
        )],
        creates: ids
            .iter()
            .map(|id| {
                (
                    ParentRef::Node(node.clone()),
                    "Run targeted tests".into(),
                    "Test storage".into(),
                    vec![id.clone()],
                )
            })
            .collect(),
        move_sources: ids
            .iter()
            .enumerate()
            .map(|(i, id)| (id.clone(), node.clone(), i))
            .collect(),
        reparent: vec![],
        expected_reparent: vec![],
        stale: vec![],
        queue_item: None,
    };
    let items = runtime
        .store
        .commit_tree_maintenance(
            owner,
            &runtime.scope.actor,
            project,
            plan,
            now_ms().unwrap(),
        )
        .await
        .unwrap();
    let mut trace = runtime
        .begin_trace(
            std::slice::from_ref(project),
            ActivityOperation::Dream,
            ActivityPhase::TreeSplit,
            true,
        )
        .await
        .unwrap();
    for item in items {
        runtime.prepare_revision(item, &mut trace).await.unwrap();
    }
    runtime.finish_trace(trace, Ok(())).await.result.unwrap();
    let children = runtime
        .store
        .tree_children(owner, project, &node)
        .await
        .unwrap();
    assert_eq!(children.len(), 2);
    let mut sessions = Vec::new();
    for child in children {
        let stats = stats(&runtime, &child.memory).await;
        assert_eq!((stats.sessions.len(), stats.strategies), (1, 1));
        sessions.extend(stats.sessions);
    }
    sessions.sort();
    assert_eq!(sessions, [A, C]);
}
