use super::*;
mod review {
    include!("reflection_review.rs");
}
use crate as lm;
use crate::domain_models::{
    memory_tree::{Basis, TreeNode},
    reflection::*,
};
#[path = "skill_seed.rs"]
mod lesson_seed;
use crate::engines::reflection_policy::{self, POLICY};
use serde_json::{Value, json};
async fn seed(
    r: &MemoryRuntime,
    tag: &str,
    parent: ParentRef,
    merge: Option<&TreeNode>,
) -> TreeNode {
    let p = &r.scope.projects[0];
    let source = SourceId::new(tag).unwrap();
    r.store
        .capture(
            CaptureEvent {
                source: source.clone(),
                owner: r.scope.owner.clone(),
                actor: r.scope.actor.clone(),
                project: p.clone(),
                kind: WorkEventKind::Decision,
                outcome: WorkOutcome::Unknown,
                origin: "fixture://reflection".into(),
                recorded_at_ms: 100,
                content: format!("SQLite {tag}"),
            },
            &r.limits,
        )
        .await
        .unwrap();
    let mut plan = CurationCommit {
        links: vec![],
        operation_id: tag.into(),
        sources: vec![source.clone()],
        promotes: vec![],
        merges: vec![],
        supersedes: vec![],
        creates: vec![],
        parked: vec![],
        stale: vec![],
        split: vec![],
        operation: LinkOperation::Create,
    };
    if let Some(n) = merge {
        plan.merges.push((
            n.memory.clone(),
            n.revision.clone(),
            tag.into(),
            format!("SQLite {tag}"),
            vec![source],
        ));
    } else {
        plan.creates
            .push((parent, tag.into(), format!("SQLite {tag}"), vec![source]));
    }
    let item = r
        .store
        .commit_curation(&r.scope.owner, &r.scope.actor, p, plan, 200)
        .await
        .unwrap()
        .pop()
        .unwrap();
    r.store
        .review(
            &r.scope.owner,
            &r.scope.actor,
            &ReviewRequest {
                project: p.clone(),
                revision: item.revision.id.clone(),
                action: ReviewAction::Promote,
                expected_active: item.revision.predecessor,
                conflict_with: None,
            },
            300,
        )
        .await
        .unwrap();
    r.store
        .tree_nodes(&r.scope.owner, p)
        .await
        .unwrap()
        .into_iter()
        .find(|n| n.memory == item.revision.memory)
        .unwrap()
}
async fn due(r: &MemoryRuntime) -> ReflectionWork {
    let p = &r.scope.projects[0];
    r.store
        .reflection_work(&r.scope.owner, p, 400, POLICY)
        .await
        .unwrap()
        .unwrap()
}
async fn seeded(r: &MemoryRuntime) -> TreeNode {
    let mut b = seed(r, "branch", ParentRef::Root(TreeRoot::Preferences), None).await;
    seed(r, "child", ParentRef::Node(b.memory.clone()), None).await;
    for i in 0..4 {
        b = seed(r, &format!("rev-{i}"), ParentRef::Root(b.root), Some(&b)).await;
    }
    b
}
async fn reflect(
    r: &MemoryRuntime,
    work: ReflectionWork,
) -> Result<Option<DreamReceipt>, RuntimeError> {
    let p = &r.scope.projects[0];
    let mut trace = r
        .begin_trace(
            std::slice::from_ref(p),
            crate::domain_models::runtime_diagnostics::ActivityOperation::Dream,
            ActivityPhase::Derivation,
            true,
        )
        .await
        .unwrap();
    let result = r.reflection_tick(p, work, &mut trace).await;
    r.finish_trace(trace, result).await.result
}
fn empty() -> Value {
    json!({"conclusions":[],"confidence_updates":[],"contradictions":[]})
}
#[tokio::test]
async fn uncertain_admission_moves_baseline_and_keeps_background_running() {
    let fake = Fake::start();
    let r = setup(&fake).await;
    seeded(&r).await;
    let work = due(&r).await;
    let p = &r.scope.projects[0];
    let mut parts = vec![
        reflection_policy::PROMPT_VERSION,
        r.scope.owner.as_str(),
        p.as_str(),
        work.input.branch.as_str(),
    ];
    parts.extend(work.input.nodes.iter().map(|n| n.node.revision.as_str()));
    let op = format!("reflect-job:{}", stable_id(&parts));
    let events = r
        .store
        .tree_recent_sources(&r.scope.owner, p, &work.input.branch)
        .await
        .unwrap();
    r.store
        .admit_runtime_operation(&r.scope.owner, p, &op, &events)
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(3)).await;
    r.dream_once().await.result.unwrap();
    assert!(!r.background_stopped().unwrap());
    assert!(
        r.store
            .reflection_work(&r.scope.owner, p, 500, POLICY)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(fake.chats.load(Ordering::SeqCst), 0);
    fake.reply(200,r#"{"results":{"s0":[{"op":"discard","node":null,"parent":null,"title":null,"content":null}]}}"#);
    capture(&r, "later", WorkEventKind::Decision).await;
    r.dream_once().await.result.unwrap();
    assert_eq!(fake.chats.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn reflection_runs_on_user_store() {
    let fake = Fake::start();
    let mut r = setup(&fake).await;
    r.scope.projects[0] = ProjectId::new("user:owner").unwrap();
    seeded(&r).await;
    fake.reply(200, &empty().to_string());
    tokio::time::sleep(std::time::Duration::from_millis(3)).await;
    r.dream_once().await.result.unwrap();
    assert_eq!(fake.chats.load(Ordering::SeqCst), 1);
    let bodies = fake.chat_bodies.lock().unwrap().clone();
    let body: Value = serde_json::from_str(&bodies[0]).unwrap();
    assert_eq!(
        body["response_format"]["json_schema"]["name"],
        "runtime_reflection_v1"
    );
    assert!(!bodies[0].contains("maxItems"));
    assert!(
        r.store
            .reflection_work(&r.scope.owner, &r.scope.projects[0], 500, POLICY)
            .await
            .unwrap()
            .is_none()
    );
}
#[tokio::test]
async fn invalid_reflection_commits_nothing() {
    let fake = Fake::start();
    let r = setup(&fake).await;
    seeded(&r).await;
    let work = due(&r).await;
    let p = &r.scope.projects[0];
    let before = r.store.tree_nodes(&r.scope.owner, p).await.unwrap();
    fake.reply(200,&json!({"conclusions":[{"title":"Bad","content":"Bad inference","confidence":"high","derived_from":["n0"],"parent":"n0"}],"confidence_updates":[],"contradictions":[]}).to_string());
    reflect(&r, work).await.unwrap();
    assert_eq!(before, r.store.tree_nodes(&r.scope.owner, p).await.unwrap());
    assert!(
        r.store
            .reflection_work(&r.scope.owner, p, 500, POLICY)
            .await
            .unwrap()
            .is_none()
    );
}
async fn conclude(r: &MemoryRuntime, fake: &Fake) -> TreeNode {
    let work = due(r).await;
    let parent = work
        .input
        .nodes
        .iter()
        .position(|n| n.node.depth == 1)
        .unwrap();
    fake.reply(200,&json!({"conclusions":[{"title":"Private conclusion title","content":"Unique inferred memory content","confidence":"medium","derived_from":["n0","n1"],"parent":format!("n{parent}")}],"confidence_updates":[],"contradictions":[]}).to_string());
    reflect(r, work).await.unwrap();
    r.store
        .tree_nodes(&r.scope.owner, &r.scope.projects[0])
        .await
        .unwrap()
        .into_iter()
        .find(|n| n.basis == Basis::Concluded)
        .unwrap()
}
#[tokio::test]
async fn curator_never_sees_concluded_nodes() {
    let fake = Fake::start();
    let r = setup(&fake).await;
    seeded(&r).await;
    conclude(&r, &fake).await;
    fake.reply(200,r#"{"results":{"s0":[{"op":"discard","node":null,"parent":null,"title":null,"content":null}]}}"#);
    capture(&r, "curate-after", WorkEventKind::Decision).await;
    r.dream_once().await.result.unwrap();
    let bodies = fake.chat_bodies.lock().unwrap().clone();
    let body = bodies.last().unwrap();
    assert!(!body.contains("Private conclusion title"));
    assert!(!body.contains("Unique inferred memory content"));
}
#[tokio::test]
async fn refresh_ignores_concluded_children() {
    let fake = Fake::start();
    let r = setup(&fake).await;
    let branch = seeded(&r).await;
    conclude(&r, &fake).await;
    let p = &r.scope.projects[0];
    let plan = CurationCommit {
        links: vec![],
        operation_id: "mark-stale".into(),
        sources: vec![],
        promotes: vec![],
        merges: vec![],
        supersedes: vec![],
        creates: vec![],
        parked: vec![],
        stale: vec![branch.memory.clone()],
        split: vec![],
        operation: LinkOperation::Create,
    };
    r.store
        .commit_curation(&r.scope.owner, &r.scope.actor, p, plan, 500)
        .await
        .unwrap();
    fake.reply(
        200,
        r#"{"title":"SQLite branch","content":"SQLite observed summary"}"#,
    );
    let mut trace = r
        .begin_trace(
            std::slice::from_ref(p),
            crate::domain_models::runtime_diagnostics::ActivityOperation::Dream,
            ActivityPhase::TreeRefresh,
            true,
        )
        .await
        .unwrap();
    let result = r.tree_maintenance_tick(p, &mut trace).await;
    r.finish_trace(trace, result).await.result.unwrap();
    let bodies = fake.chat_bodies.lock().unwrap().clone();
    assert!(
        !bodies
            .last()
            .unwrap()
            .contains("Unique inferred memory content")
    );
}
#[tokio::test]
async fn first_sight_does_not_trigger_on_history() {
    let fake = Fake::start();
    let r = setup(&fake).await;
    seeded(&r).await;
    let p = &r.scope.projects[0];
    r.store
        .test_forget_reflection_state(&r.scope.owner, p)
        .await
        .unwrap();
    assert!(
        r.store
            .reflection_work(&r.scope.owner, p, 500, POLICY)
            .await
            .unwrap()
            .is_none()
    );
    let plan = r.inspect_reflection_backfill(p).await.unwrap();
    assert_eq!(plan.branches.len(), 1);
    r.reflection_backfill(p, &plan).await.unwrap();
    fake.reply(200, &empty().to_string());
    let work = due(&r).await;
    reflect(&r, work).await.unwrap();
    assert_eq!(fake.chats.load(Ordering::SeqCst), 1);
    assert!(
        r.store
            .reflection_work(&r.scope.owner, p, 600, POLICY)
            .await
            .unwrap()
            .is_none()
    );
}
#[tokio::test]
async fn first_sight_records_already_mature_lessons() {
    let fake = Fake::start();
    let r = setup(&fake).await;
    let p = &r.scope.projects[0];
    lesson_seed::reinforce(
        &r.store,
        &r.scope.owner,
        &r.scope.actor,
        p,
        "already-mature",
        None,
        0,
        3,
        "strategy",
    )
    .await;
    assert!(
        r.store
            .reflection_work(&r.scope.owner, p, 400, POLICY)
            .await
            .unwrap()
            .is_none()
    );
    r.store
        .test_forget_reflection_state(&r.scope.owner, p)
        .await
        .unwrap();
    assert!(
        r.store
            .reflection_work(&r.scope.owner, p, 500, POLICY)
            .await
            .unwrap()
            .is_none()
    );
}
#[tokio::test]
async fn reflection_triggers_after_five_revisions_and_on_lesson_threshold() {
    let fake = Fake::start();
    let r = setup(&fake).await;
    let p = &r.scope.projects[0];
    let node = lesson_seed::reinforce(
        &r.store,
        &r.scope.owner,
        &r.scope.actor,
        p,
        "mature",
        None,
        0,
        2,
        "strategy",
    )
    .await;
    assert!(
        r.store
            .reflection_work(&r.scope.owner, p, 400, POLICY)
            .await
            .unwrap()
            .is_none()
    );
    lesson_seed::reinforce(
        &r.store,
        &r.scope.owner,
        &r.scope.actor,
        p,
        "mature",
        Some(&node),
        2,
        1,
        "strategy",
    )
    .await;
    let work = due(&r).await;
    assert_eq!(work.mature_lessons, vec![node.memory]);
    fake.reply(200, &empty().to_string());
    reflect(&r, work).await.unwrap();
    assert!(
        r.store
            .reflection_work(&r.scope.owner, p, 500, POLICY)
            .await
            .unwrap()
            .is_none()
    );
}
#[tokio::test]
async fn reflection_commit_failure_moves_baseline_without_retry() {
    let fake = Fake::start();
    let r = setup(&fake).await;
    seeded(&r).await;
    let mut work = due(&r).await;
    work.input.nodes[0].node.title = "Snapshot no longer current".into();
    let before = r
        .store
        .tree_nodes(&r.scope.owner, &r.scope.projects[0])
        .await
        .unwrap();
    fake.reply(200, &empty().to_string());
    reflect(&r, work).await.unwrap();
    assert_eq!(
        before,
        r.store
            .tree_nodes(&r.scope.owner, &r.scope.projects[0])
            .await
            .unwrap()
    );
    assert!(
        r.store
            .reflection_work(&r.scope.owner, &r.scope.projects[0], 500, POLICY)
            .await
            .unwrap()
            .is_none()
    );
    assert!(!r.background_stopped().unwrap());
}
#[tokio::test]
async fn reflection_provider_unavailable_moves_baseline_and_stops_worker() {
    let fake = Fake::start();
    let r = setup(&fake).await;
    seeded(&r).await;
    fake.reply(0, "");
    tokio::time::sleep(std::time::Duration::from_millis(3)).await;
    assert_eq!(
        r.dream_once().await.result.unwrap_err(),
        RuntimeError::ProviderUnavailable
    );
    assert!(r.background_stopped().unwrap());
    assert!(
        r.store
            .reflection_work(&r.scope.owner, &r.scope.projects[0], 500, POLICY)
            .await
            .unwrap()
            .is_none()
    );
    r.dream_once().await.result.unwrap();
    assert_eq!(fake.chats.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn reflection_provider_rejection_releases_admission_and_moves_baseline() {
    let fake = Fake::start();
    let r = setup(&fake).await;
    seeded(&r).await;
    let work = due(&r).await;
    fake.reply(429, &empty().to_string());
    reflect(&r, work).await.unwrap();
    assert!(!r.background_stopped().unwrap());
    assert!(
        r.store
            .reflection_work(&r.scope.owner, &r.scope.projects[0], 500, POLICY)
            .await
            .unwrap()
            .is_none()
    );
}
