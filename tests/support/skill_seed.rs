use super::lm::domain_models::{
    ActorId, OwnerId, ProjectId, SourceId, capture::*, memory_tree::*, runtime::*,
};
use super::lm::integrations::database::embedded_capture::EmbeddedRuntimeStore;
use serde_json::json;
/// Add distinct lesson sessions, creating or reinforcing one real curated node.
#[allow(clippy::too_many_arguments)]
pub async fn reinforce(
    store: &EmbeddedRuntimeStore,
    owner: &OwnerId,
    actor: &ActorId,
    project: &ProjectId,
    tag: &str,
    memory: Option<&TreeNode>,
    start: usize,
    count: usize,
    kind: &str,
) -> TreeNode {
    let mut sources = Vec::new();
    for i in start..start + count {
        let source = SourceId::new(format!("lesson/{tag}-{i}/1")).unwrap();
        store.capture(CaptureEvent { owner:owner.clone(),actor:actor.clone(),project:project.clone(),source:source.clone(),kind:WorkEventKind::Lesson,outcome:WorkOutcome::Unknown,origin:format!("lesson://{tag}-{i}/1"),recorded_at_ms:100+i as u64,
            content:json!({"v":1,"kind":kind,"scope":if project.as_str().starts_with("user:") {"user"} else {"project"},"title":"Test storage","applies_when":"Changing storage","guidance":"1. Run targeted tests.\n2. Inspect the result.","task":"Verify","task_outcome":if kind=="strategy" {"succeeded"} else {"failed"},"evidence":[format!("evidence-{tag}-{i}")]}).to_string() },&RuntimeLimits::default()).await.unwrap();
        sources.push(source);
    }
    let mut commit = CurationCommit {
        links: vec![],
        operation_id: format!("seed-{tag}-{start}"),
        sources: sources.clone(),
        promotes: vec![],
        creates: vec![],
        merges: vec![],
        supersedes: vec![],
        parked: vec![],
        stale: vec![],
        split: vec![],
        operation: LinkOperation::Create,
    };
    let root = if project.as_str().starts_with("user:") {
        TreeRoot::GeneralLessons
    } else {
        TreeRoot::Lessons
    };
    if let Some(node) = memory {
        commit.merges.push((
            node.memory.clone(),
            node.revision.clone(),
            "Test storage".into(),
            "Changing storage\n1. Run targeted tests.\n2. Inspect the result.".into(),
            sources,
        ));
    } else {
        commit.creates.push((
            ParentRef::Root(root),
            "Test storage".into(),
            "Changing storage\n1. Run targeted tests.\n2. Inspect the result.".into(),
            sources,
        ));
    }
    let item = store
        .commit_curation(owner, actor, project, commit, 200 + start as u64)
        .await
        .unwrap()
        .pop()
        .unwrap();
    store
        .review(
            owner,
            actor,
            &ReviewRequest {
                project: project.clone(),
                revision: item.revision.id.clone(),
                action: ReviewAction::Promote,
                expected_active: item.revision.predecessor.clone(),
                conflict_with: None,
            },
            300 + start as u64,
        )
        .await
        .unwrap();
    store
        .tree_nodes(owner, project)
        .await
        .unwrap()
        .into_iter()
        .find(|n| n.memory == item.revision.memory)
        .unwrap()
}
#[allow(dead_code)] // Used by integration fixtures, not the provider-job fixtures.
pub fn candidate(node: &TreeNode) -> super::lm::domain_models::skills::SkillCandidate {
    use super::lm::domain_models::skills::*;
    SkillCandidate {
        candidate_id: node.memory.clone(),
        memory_id: node.memory.clone(),
        revision_id: node.revision.clone(),
        scope: "project",
        slug: "test-storage".into(),
        status: SkillStatus::Drafted,
        draft: Some("draft".into()),
        sessions_at_draft: 3,
        sessions_at_decision: None,
        created_at_ms: 400,
        decided_at_ms: None,
        install_paths: vec![],
        install_hashes: vec![],
        pitfall_memories: vec![],
        pitfall_revisions: vec![],
        pitfall_source_counts: vec![],
        installed_pitfall_memories: vec![],
    }
}
