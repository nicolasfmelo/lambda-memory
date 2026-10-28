#![cfg(feature = "memory-runtime")]
use lambda_memory::domain_models::capture::{CaptureEvent, WorkEventKind, WorkOutcome};
use lambda_memory::domain_models::memory_tree::TreeMode;
use lambda_memory::domain_models::runtime::{
    ReviewAction, ReviewRequest, RevisionProposal, RuntimeLimits,
};
use lambda_memory::domain_models::{ActorId, MemoryId, OwnerId, ProjectId, RevisionId, SourceId};
use lambda_memory::integrations::database::embedded_capture::{
    EmbeddedCaptureStore, EmbeddedRuntimeStore,
};
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT: AtomicUsize = AtomicUsize::new(0);
async fn store() -> EmbeddedRuntimeStore {
    let path = format!(
        "/tmp/lambda-tree-store-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::SeqCst)
    );
    EmbeddedRuntimeStore::new(EmbeddedCaptureStore::open(&path).await.unwrap())
}

#[tokio::test]
async fn empty_project_defaults_to_tree_mode() {
    let store = store().await;
    let owner = OwnerId::new("owner").unwrap();
    let project = ProjectId::new("project").unwrap();
    assert_eq!(
        store.tree_mode(&owner, &project).await.unwrap(),
        TreeMode::Tree
    );
    store
        .set_tree_mode(&owner, &project, TreeMode::Flat, 10)
        .await
        .unwrap();
    assert_eq!(
        store.tree_mode(&owner, &project).await.unwrap(),
        TreeMode::Flat
    );
    store
        .set_tree_mode(&owner, &project, TreeMode::Tree, 20)
        .await
        .unwrap();
    assert_eq!(
        store.tree_mode(&owner, &project).await.unwrap(),
        TreeMode::Tree
    );
}

#[tokio::test]
async fn project_with_active_flat_memory_is_flat() {
    let store = store().await;
    let owner = OwnerId::new("owner").unwrap();
    let actor = ActorId::new("actor").unwrap();
    let project = ProjectId::new("project").unwrap();
    let source = SourceId::new("source").unwrap();
    let revision = RevisionId::new("revision").unwrap();
    store
        .capture(
            CaptureEvent {
                owner: owner.clone(),
                actor: actor.clone(),
                project: project.clone(),
                source: source.clone(),
                kind: WorkEventKind::Decision,
                outcome: WorkOutcome::Unknown,
                origin: "fixture://test".into(),
                recorded_at_ms: 1,
                content: "A decision.".into(),
            },
            &RuntimeLimits::default(),
        )
        .await
        .unwrap();
    store
        .propose(
            &owner,
            &actor,
            RevisionProposal {
                project: project.clone(),
                memory: MemoryId::new("flat").unwrap(),
                revision: revision.clone(),
                sources: vec![source],
                content: "A decision.".into(),
                predecessor: None,
            },
            2,
        )
        .await
        .unwrap();
    store
        .review(
            &owner,
            &actor,
            &ReviewRequest {
                project: project.clone(),
                revision,
                action: ReviewAction::Promote,
                expected_active: None,
                conflict_with: None,
            },
            3,
        )
        .await
        .unwrap();
    assert_eq!(
        store.tree_mode(&owner, &project).await.unwrap(),
        TreeMode::Flat
    );
    store
        .set_tree_mode(&owner, &project, TreeMode::Tree, 4)
        .await
        .unwrap();
    assert_eq!(
        store.tree_mode(&owner, &project).await.unwrap(),
        TreeMode::Tree
    );
}

use lambda_memory::domain_models::embeddings::{Embedding, EmbeddingSpace};
use lambda_memory::domain_models::memory_tree::{
    CurationCommit, LinkOperation, ParentRef, ParkReason, TreeQueue, TreeRoot,
};
use lambda_memory::domain_models::runtime::{RuntimeError, RuntimeRevision};

fn ids() -> (OwnerId, ActorId, ProjectId) {
    (
        OwnerId::new("owner").unwrap(),
        ActorId::new("actor").unwrap(),
        ProjectId::new("project").unwrap(),
    )
}
fn plan(id: &str, sources: &[&str]) -> CurationCommit {
    CurationCommit {
        operation_id: id.into(),
        sources: sources.iter().map(|s| SourceId::new(*s).unwrap()).collect(),
        merges: vec![],
        creates: vec![],
        parked: vec![],
        stale: vec![],
        split: vec![],
        operation: LinkOperation::Create,
    }
}
async fn capture(store: &EmbeddedRuntimeStore, id: &str) {
    let (owner, actor, project) = ids();
    store
        .capture(
            CaptureEvent {
                owner,
                actor,
                project,
                source: SourceId::new(id).unwrap(),
                kind: WorkEventKind::Decision,
                outcome: WorkOutcome::Unknown,
                origin: "fixture://test".into(),
                recorded_at_ms: 1,
                content: format!("Source {id}"),
            },
            &RuntimeLimits::default(),
        )
        .await
        .unwrap();
}
async fn promote_tree(store: &EmbeddedRuntimeStore, item: &RuntimeRevision) {
    let (owner, actor, project) = ids();
    store
        .review(
            &owner,
            &actor,
            &ReviewRequest {
                project,
                revision: item.revision.id.clone(),
                action: ReviewAction::Promote,
                expected_active: item.revision.predecessor.clone(),
                conflict_with: None,
            },
            100,
        )
        .await
        .unwrap();
}
async fn create(
    store: &EmbeddedRuntimeStore,
    id: &str,
    parent: ParentRef,
    title: &str,
    content: &str,
) -> RuntimeRevision {
    capture(store, id).await;
    let (owner, actor, project) = ids();
    let mut p = plan(id, &[id]);
    p.creates
        .push((parent, title.into(), content.into(), p.sources.clone()));
    let item = store
        .commit_curation(&owner, &actor, &project, p, 10)
        .await
        .unwrap()
        .remove(0);
    promote_tree(store, &item).await;
    item
}

#[tokio::test]
async fn commit_creates_node_edge_links_and_consumes_sources() {
    let store = store().await;
    let node = create(
        &store,
        "s1",
        ParentRef::Root(TreeRoot::Decisions),
        "Database",
        "Use SQLite",
    )
    .await;
    let (o, _, p) = ids();
    assert_eq!(
        node.revision.memory.as_str(),
        "tree-node:8210a3802240c06d742dbb817d93630d1810ab9f41955ebcc54ddf86d5641b00"
    );
    let nodes = store.tree_nodes(&o, &p).await.unwrap();
    assert_eq!(nodes.len(), 1);
    assert_eq!(nodes[0].memory, node.revision.memory);
    assert_eq!(nodes[0].parent, Some(TreeRoot::Decisions.memory_id()));
    assert_eq!(nodes[0].direct_sources, 1);
    assert_eq!(store.tree_root_children(&o, &p).await.unwrap()[0].1, 1);
    assert_eq!(
        store
            .tree_children(&o, &p, &TreeRoot::Decisions.memory_id())
            .await
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        store
            .node_sources(&o, &p, &node.revision.memory, None, 32)
            .await
            .unwrap()
            .0
            .len(),
        1
    );
    assert!(
        store
            .eligible_sources(&o, &p, 32, false, None, None, false)
            .await
            .unwrap()
            .is_empty()
    );
}
#[tokio::test]
async fn discarded_sources_are_consumed_without_revision() {
    let store = store().await;
    capture(&store, "s").await;
    let (o, a, p) = ids();
    assert!(
        store
            .commit_curation(&o, &a, &p, plan("discard", &["s"]), 2)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        store
            .eligible_sources(&o, &p, 32, false, None, None, false)
            .await
            .unwrap()
            .is_empty()
    );
}
#[tokio::test]
async fn merge_with_stale_expected_revision_conflicts_and_changes_nothing() {
    let store = store().await;
    let node = create(
        &store,
        "s1",
        ParentRef::Root(TreeRoot::Decisions),
        "One",
        "First",
    )
    .await;
    capture(&store, "s2").await;
    let (o, a, p) = ids();
    let mut c = plan("bad", &["s2"]);
    c.merges.push((
        node.revision.memory.clone(),
        RevisionId::new("stale").unwrap(),
        "Changed".into(),
        "Changed".into(),
        c.sources.clone(),
    ));
    assert!(matches!(
        store.commit_curation(&o, &a, &p, c, 20).await,
        Err(RuntimeError::Conflict)
    ));
    assert_eq!(store.tree_nodes(&o, &p).await.unwrap()[0].title, "One");
    assert_eq!(
        store
            .eligible_sources(&o, &p, 32, false, None, None, false)
            .await
            .unwrap()
            .len(),
        1
    );
}
#[tokio::test]
async fn parked_source_stays_unconsumed_and_listed() {
    let store = store().await;
    capture(&store, "s").await;
    let (o, a, p) = ids();
    let mut c = plan("park", &["s"]);
    c.parked.push((c.sources[0].clone(), ParkReason::Split));
    store.commit_curation(&o, &a, &p, c, 2).await.unwrap();
    assert_eq!(
        store
            .eligible_sources(&o, &p, 32, false, None, None, false)
            .await
            .unwrap()
            .len(),
        1
    );
    assert_eq!(store.parked_sources(&o, &p).await.unwrap().len(), 1);
}
#[tokio::test]
async fn lexical_candidates_score_title_and_content() {
    let store = store().await;
    create(
        &store,
        "s1",
        ParentRef::Root(TreeRoot::Decisions),
        "SQLite storage",
        "keeps records",
    )
    .await;
    create(
        &store,
        "s2",
        ParentRef::Root(TreeRoot::Decisions),
        "Storage",
        "SQLite is used",
    )
    .await;
    let (o, _, p) = ids();
    let nodes = store
        .tree_lexical_candidates(&o, &p, &["sqlite".into()], 8)
        .await
        .unwrap();
    assert_eq!(nodes.len(), 2);
}
#[tokio::test]
async fn purge_only_source_queues_rebuild() {
    let store = store().await;
    let node = create(
        &store,
        "s1",
        ParentRef::Root(TreeRoot::Decisions),
        "One",
        "Content",
    )
    .await;
    let (o, _, p) = ids();
    store
        .purge(&o, &p, &[SourceId::new("s1").unwrap()])
        .await
        .unwrap();
    assert!(store.tree_nodes(&o, &p).await.unwrap().is_empty());
    assert_eq!(
        store
            .tree_queue_pop(&o, &p, TreeQueue::Rebuild)
            .await
            .unwrap(),
        Some(node.revision.memory)
    );
}
#[tokio::test]
async fn retire_node_reparents_children_and_marks_parent_stale() {
    let store = store().await;
    let root = TreeRoot::Decisions.memory_id();
    let parent = create(
        &store,
        "s1",
        ParentRef::Root(TreeRoot::Decisions),
        "Parent",
        "Content",
    )
    .await;
    let one = create(
        &store,
        "s2",
        ParentRef::Node(parent.revision.memory.clone()),
        "Child one",
        "Content",
    )
    .await;
    let two = create(
        &store,
        "s3",
        ParentRef::Node(parent.revision.memory.clone()),
        "Child two",
        "Content",
    )
    .await;
    let (o, a, p) = ids();
    store
        .purge(&o, &p, &[SourceId::new("s1").unwrap()])
        .await
        .unwrap();
    store
        .retire_node(&o, &a, &p, &parent.revision.memory, 50)
        .await
        .unwrap();
    let children = store.tree_children(&o, &p, &root).await.unwrap();
    assert_eq!(children.len(), 2);
    assert!(children.iter().any(|n| n.memory == one.revision.memory));
    assert!(children.iter().any(|n| n.memory == two.revision.memory));
    assert!(children.iter().all(|n| n.depth == 1));
    assert_eq!(
        store
            .tree_queue_pop(&o, &p, TreeQueue::Stale)
            .await
            .unwrap(),
        Some(root)
    );
}
#[tokio::test]
async fn tree_vector_hits_ignore_flat_revisions() {
    let store = store().await;
    let node = create(
        &store,
        "s1",
        ParentRef::Root(TreeRoot::Decisions),
        "Node",
        "Content",
    )
    .await;
    capture(&store, "s2").await;
    let (o, a, p) = ids();
    let flat = store
        .propose(
            &o,
            &a,
            RevisionProposal {
                project: p.clone(),
                memory: MemoryId::new("flat").unwrap(),
                revision: RevisionId::new("flat-rev").unwrap(),
                sources: vec![SourceId::new("s2").unwrap()],
                content: "Flat".into(),
                predecessor: None,
            },
            20,
        )
        .await
        .unwrap();
    promote_tree(&store, &flat).await;
    let emb = Embedding {
        space: EmbeddingSpace {
            provider: "fixture".into(),
            model: "fixture".into(),
            revision: "v1".into(),
            dimensions: 2,
        },
        values: vec![1.0, 0.0],
    };
    store.save_runtime_embedding(&node, &emb).await.unwrap();
    store.save_runtime_embedding(&flat, &emb).await.unwrap();
    let hits = store.tree_vector_hits(&o, &p, &emb, 8).await.unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].item.revision.memory, node.revision.memory);
}

#[tokio::test]
async fn merge_records_predecessor_and_purge_keeps_unrelated_successor() {
    let store = store().await;
    let original = create(
        &store,
        "s1",
        ParentRef::Root(TreeRoot::Architecture),
        "Before",
        "Old state",
    )
    .await;
    capture(&store, "s2").await;
    let (o, a, p) = ids();
    let mut c = plan("merge", &["s2"]);
    c.operation = LinkOperation::Merge;
    c.merges.push((
        original.revision.memory.clone(),
        original.revision.id.clone(),
        "After".into(),
        "New state".into(),
        c.sources.clone(),
    ));
    let merged = store
        .commit_curation(&o, &a, &p, c, 30)
        .await
        .unwrap()
        .remove(0);
    assert_eq!(merged.revision.predecessor, Some(original.revision.id));
    promote_tree(&store, &merged).await;
    assert_eq!(
        store
            .node_history(&o, &p, &merged.revision.memory, 32)
            .await
            .unwrap()
            .len(),
        2
    );
    store
        .purge(&o, &p, &[SourceId::new("s1").unwrap()])
        .await
        .unwrap();
    let nodes = store.tree_nodes(&o, &p).await.unwrap();
    assert_eq!(nodes.len(), 1);
    assert_eq!(nodes[0].title, "After");
    assert_eq!(nodes[0].direct_sources, 1);
    assert_eq!(
        store
            .tree_queue_pop(&o, &p, TreeQueue::Rebuild)
            .await
            .unwrap(),
        None
    );
}

#[tokio::test]
async fn migration_records_discarded_sources_as_handled() {
    let store = store().await;
    let (owner, actor, project) = ids();
    capture(&store, "kept").await;
    capture(&store, "dropped").await;
    let mut commit = plan("curation-job:migrate:one", &["kept", "dropped"]);
    commit.operation = LinkOperation::Migrate;
    commit.creates.push((
        ParentRef::Root(TreeRoot::Decisions),
        "Title".into(),
        "Content".into(),
        vec![SourceId::new("kept").unwrap()],
    ));
    store
        .commit_curation(&owner, &actor, &project, commit, 10)
        .await
        .unwrap();
    let mut handled: Vec<_> = store
        .tree_migrated_sources(&owner, &project)
        .await
        .unwrap()
        .into_iter()
        .map(|s| s.to_string())
        .collect();
    handled.sort();
    assert_eq!(handled, vec!["dropped".to_string(), "kept".to_string()]);
    // Migration never consumes: both originals are still unconsumed knowledge.
    assert_eq!(
        store
            .eligible_sources(&owner, &project, 8, false, None, None, true)
            .await
            .unwrap()
            .len(),
        2
    );
}
