use super::*;
use crate::domain_models::memory_tree::*;
use crate::domain_models::runtime_diagnostics::{
    ActivityOperation, ActivityReason, ActivityRequest,
};
use serde_json::{Value, json};

async fn event(runtime: &MemoryRuntime, id: &str) -> CaptureEvent {
    let event = CaptureEvent {
        owner: runtime.scope.owner.clone(),
        actor: runtime.scope.actor.clone(),
        project: runtime.scope.projects[0].clone(),
        source: SourceId::new(id).unwrap(),
        origin: "fixture://links".into(),
        recorded_at_ms: 100,
        kind: WorkEventKind::Decision,
        outcome: WorkOutcome::Unknown,
        content: format!("Own evidence {id}: use supported storage settings."),
    };
    runtime
        .store
        .capture(event.clone(), &runtime.limits)
        .await
        .unwrap();
    event
}
async fn publish(runtime: &MemoryRuntime, plan: CurationCommit) -> RuntimeRevision {
    let scope = &runtime.scope;
    let item = runtime
        .store
        .commit_curation(
            &scope.owner,
            &scope.actor,
            &scope.projects[0],
            plan,
            now_ms().unwrap(),
        )
        .await
        .unwrap()
        .remove(0);
    runtime
        .store
        .review(
            &scope.owner,
            &scope.actor,
            &ReviewRequest {
                project: scope.projects[0].clone(),
                revision: item.revision.id.clone(),
                action: ReviewAction::Promote,
                expected_active: item.revision.predecessor.clone(),
                conflict_with: None,
            },
            now_ms().unwrap(),
        )
        .await
        .unwrap();
    item
}
fn plan(id: &str, sources: Vec<SourceId>) -> CurationCommit {
    CurationCommit {
        links: vec![],
        operation_id: id.into(),
        sources,
        promotes: vec![],
        merges: vec![],
        supersedes: vec![],
        creates: vec![],
        parked: vec![],
        stale: vec![],
        split: vec![],
        operation: LinkOperation::Create,
    }
}
async fn seeded(fake: &Fake) -> (MemoryRuntime, TreeNode, TreeNode) {
    let runtime = setup(fake).await;
    let original = event(&runtime, "dependency").await;
    let mut commit = plan("seed", vec![original.source]);
    commit.creates.push((
        ParentRef::Root(TreeRoot::Architecture),
        "Dependency".into(),
        "Old dependency text".into(),
        commit.sources.clone(),
    ));
    let dependency = publish(&runtime, commit).await;
    // Exercise the actual decoder, validation and service assembly on create links.
    let own = event(&runtime, "own").await;
    fake.reply(200, &json!({"results":{"s0":[{"op":"create","node":null,"parent":"decisions","title":"Dependent","content":"Own supported statement","links":[{"to":"n0","relation":"depends_on"}]}]}}).to_string());
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
        .curate_batch(project, vec![own], LinkOperation::Create, &mut trace)
        .await;
    runtime.finish_trace(trace, result).await.result.unwrap();
    let nodes = runtime
        .store
        .tree_nodes(&runtime.scope.owner, project)
        .await
        .unwrap();
    let node = nodes
        .iter()
        .find(|node| node.memory != dependency.revision.memory)
        .unwrap()
        .clone();
    assert_eq!(
        runtime
            .store
            .tree_links(&runtime.scope.owner, project, &node.memory)
            .await
            .unwrap()
            .len(),
        1
    );
    let update = event(&runtime, "dependency-update").await;
    let mut commit = plan("update", vec![update.source]);
    commit.merges.push((
        dependency.revision.memory,
        dependency.revision.id,
        "Current dependency".into(),
        "Current dependency text, not the old text".into(),
        commit.sources.clone(),
    ));
    publish(&runtime, commit).await;
    let dependency = runtime
        .store
        .tree_nodes(&runtime.scope.owner, project)
        .await
        .unwrap()
        .into_iter()
        .find(|n| n.memory != node.memory)
        .unwrap();
    (runtime, node, dependency)
}
async fn tick(runtime: &MemoryRuntime) -> Result<Option<DreamReceipt>, RuntimeError> {
    let project = &runtime.scope.projects[0];
    let mut trace = runtime
        .begin_trace(
            std::slice::from_ref(project),
            ActivityOperation::Dream,
            ActivityPhase::TreeRefresh,
            true,
        )
        .await
        .unwrap();
    let result = runtime.tree_maintenance_tick(project, &mut trace).await;
    runtime.finish_trace(trace, result).await.result
}
fn key(runtime: &MemoryRuntime, node: &TreeNode, dependency: &TreeNode) -> String {
    format!(
        "tree-dependency:{}",
        stable_id(&[
            "tree-dependency-v1",
            runtime.scope.owner.as_str(),
            runtime.scope.projects[0].as_str(),
            node.revision.as_str(),
            dependency.revision.as_str()
        ])
    )
}
#[tokio::test]
async fn purging_dependency_link_evidence_drops_queued_refresh_without_a_call() {
    let fake = Fake::start();
    let (runtime, node, dependency) = seeded(&fake).await;
    // Keep both endpoints active after purging the link's sole source, "own".
    let retained = event(&runtime, "retained-own").await;
    let mut commit = plan("retain-dependent", vec![retained.source]);
    commit.merges.push((
        node.memory.clone(),
        node.revision.clone(),
        "Dependent".into(),
        "Retained own statement".into(),
        commit.sources.clone(),
    ));
    publish(&runtime, commit).await;
    let scope = &runtime.scope;
    assert_eq!(
        runtime
            .store
            .tree_dependency_peek(&scope.owner, &scope.projects[0])
            .await
            .unwrap(),
        Some((node.memory.clone(), dependency.memory.clone()))
    );
    runtime
        .store
        .purge(
            &scope.owner,
            &scope.projects[0],
            &[SourceId::new("own").unwrap()],
        )
        .await
        .unwrap();
    let nodes = runtime
        .store
        .tree_nodes(&scope.owner, &scope.projects[0])
        .await
        .unwrap();
    assert!(nodes.iter().any(|n| n.memory == node.memory));
    assert!(nodes.iter().any(|n| n.memory == dependency.memory));
    assert!(
        runtime
            .store
            .tree_links(&scope.owner, &scope.projects[0], &node.memory)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        runtime
            .store
            .tree_dependency_peek(&scope.owner, &scope.projects[0])
            .await
            .unwrap()
            .is_none()
    );
    let before = fake.chats.load(Ordering::SeqCst);
    assert!(tick(&runtime).await.unwrap().is_none());
    assert_eq!(fake.chats.load(Ordering::SeqCst), before);
    // Also defend against an orphan entry left by an earlier version of the worker.
    runtime
        .store
        .test_queue_dependency(
            &scope.owner,
            &scope.projects[0],
            &node.memory,
            &dependency.memory,
        )
        .await
        .unwrap();
    assert!(tick(&runtime).await.unwrap().is_some());
    assert!(
        runtime
            .store
            .tree_dependency_peek(&scope.owner, &scope.projects[0])
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(fake.chats.load(Ordering::SeqCst), before);
    let current = nodes.iter().find(|n| n.memory == node.memory).unwrap();
    let sources = runtime
        .store
        .tree_recent_sources(&scope.owner, &scope.projects[0], &node.memory)
        .await
        .unwrap();
    runtime
        .store
        .admit_runtime_operation(
            &scope.owner,
            &scope.projects[0],
            &key(&runtime, current, &dependency),
            &sources,
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn dependency_refresh_rewrites_from_own_sources() {
    let fake = Fake::start();
    let (runtime, node, dependency) = seeded(&fake).await;
    let scope = &runtime.scope;
    let sources = runtime
        .store
        .tree_recent_sources(&scope.owner, &scope.projects[0], &node.memory)
        .await
        .unwrap();
    fake.reply(
        200,
        r#"{"title":"Refreshed dependent","content":"Own supported rewritten statement"}"#,
    );
    let before = fake.chats.load(Ordering::SeqCst);
    assert!(tick(&runtime).await.unwrap().is_some());
    assert_eq!(fake.chats.load(Ordering::SeqCst), before + 1);
    assert!(
        runtime
            .store
            .tree_dependency_peek(&scope.owner, &scope.projects[0])
            .await
            .unwrap()
            .is_none()
    );
    let view = runtime
        .node(&scope.projects[0], &node.memory, true, true, None)
        .await
        .unwrap();
    assert_eq!(view.node.content, "Own supported rewritten statement");
    assert_ne!(view.node.revision, node.revision);
    assert!(
        view.sources
            .iter()
            .all(|s| s.event.source.as_str() == "own")
    );
    assert!(
        view.sources
            .iter()
            .any(|s| s.operation == LinkOperation::Rebuild)
    );
    let bodies = fake.chat_bodies.lock().unwrap().clone();
    let body: Value = serde_json::from_str(bodies.last().unwrap()).unwrap();
    assert_eq!(
        body["response_format"]["json_schema"]["name"],
        "runtime_tree_dependency_v1"
    );
    let input: Value =
        serde_json::from_str(body["messages"][1]["content"].as_str().unwrap()).unwrap();
    assert_eq!(input["dependency"]["content"], dependency.content);
    assert_eq!(input["node"]["content"], node.content);
    assert_eq!(input["sources"].as_array().unwrap().len(), 1);
    assert!(input.to_string().contains("Own evidence own"));
    assert!(!input["sources"].to_string().contains("dependency-update"));
    drop(bodies);
    // The persisted admission is the exact identity containing BOTH input revisions.
    assert_eq!(
        runtime
            .store
            .admit_runtime_operation(
                &scope.owner,
                &scope.projects[0],
                &key(&runtime, &node, &dependency),
                &sources
            )
            .await,
        Err(RuntimeError::UncertainAdmission)
    );
}
#[tokio::test]
async fn unusable_dependency_output_and_uncertain_admission_drop_only_the_entry() {
    for uncertain in [false, true] {
        let fake = Fake::start();
        let (runtime, node, dependency) = seeded(&fake).await;
        let scope = &runtime.scope;
        if uncertain {
            let sources = runtime
                .store
                .tree_recent_sources(&scope.owner, &scope.projects[0], &node.memory)
                .await
                .unwrap();
            runtime
                .store
                .admit_runtime_operation(
                    &scope.owner,
                    &scope.projects[0],
                    &key(&runtime, &node, &dependency),
                    &sources,
                )
                .await
                .unwrap();
        } else {
            fake.reply(200, r#"{"title":"","content":""}"#);
        }
        let before = fake.chats.load(Ordering::SeqCst);
        assert!(tick(&runtime).await.unwrap().is_some());
        assert_eq!(
            fake.chats.load(Ordering::SeqCst),
            before + usize::from(!uncertain)
        );
        assert!(
            runtime
                .store
                .tree_dependency_peek(&scope.owner, &scope.projects[0])
                .await
                .unwrap()
                .is_none()
        );
        let view = runtime
            .node(&scope.projects[0], &node.memory, false, false, None)
            .await
            .unwrap();
        assert_eq!(view.node.revision, node.revision);
        let activity = runtime
            .activity(ActivityRequest {
                project: scope.projects[0].clone(),
                limit: 32,
                operations: vec![],
                outcomes: vec![],
                cursor: None,
            })
            .await
            .unwrap();
        let reason = if uncertain {
            ActivityReason::UncertainAdmission
        } else {
            ActivityReason::ProviderInvalidOutput
        };
        assert!(
            activity
                .entries
                .iter()
                .any(|entry| entry.reason == Some(reason))
        );
    }
}
#[tokio::test]
async fn idle_tick_discovers_dependency_work() {
    let fake = Fake::start();
    let (runtime, node, _) = seeded(&fake).await;
    fake.reply(
        200,
        r#"{"title":"Idle refresh","content":"Own supported idle update"}"#,
    );
    let before = fake.chats.load(Ordering::SeqCst);
    runtime.dream_once().await.result.unwrap();
    assert_eq!(fake.chats.load(Ordering::SeqCst), before + 1);
    assert_eq!(
        runtime
            .node(&runtime.scope.projects[0], &node.memory, false, false, None)
            .await
            .unwrap()
            .node
            .title,
        "Idle refresh"
    );
}

#[tokio::test]
async fn rejected_dependency_call_releases_admission() {
    let fake = Fake::start();
    let (runtime, node, dependency) = seeded(&fake).await;
    fake.reply(402, "{}");
    assert_eq!(
        tick(&runtime).await.unwrap_err(),
        RuntimeError::ProviderRejected
    );
    let scope = &runtime.scope;
    assert!(
        runtime
            .store
            .tree_dependency_peek(&scope.owner, &scope.projects[0])
            .await
            .unwrap()
            .is_some()
    );
    let sources = runtime
        .store
        .tree_recent_sources(&scope.owner, &scope.projects[0], &node.memory)
        .await
        .unwrap();
    runtime
        .store
        .admit_runtime_operation(
            &scope.owner,
            &scope.projects[0],
            &key(&runtime, &node, &dependency),
            &sources,
        )
        .await
        .unwrap();
}
