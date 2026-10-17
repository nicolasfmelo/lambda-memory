#![cfg(feature = "memory-runtime")]
use lambda_memory::domain_models::{ActorId, MemoryId, OwnerId, ProjectId, RevisionId, SourceId};
use lambda_memory::domain_models::{capture::*, embeddings::*, runtime::*};
use lambda_memory::integrations::database::embedded_capture::{
    EmbeddedCaptureStore, EmbeddedRuntimeStore,
};
use std::sync::atomic::{AtomicUsize, Ordering};
static NEXT: AtomicUsize = AtomicUsize::new(0);

async fn fixture() -> (String, EmbeddedRuntimeStore, RuntimeRevision) {
    let path = format!(
        "{}/lambda-semantic-{}-{}",
        std::env::temp_dir().display(),
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    );
    let store = EmbeddedRuntimeStore::new(EmbeddedCaptureStore::open(&path).await.unwrap());
    let owner = OwnerId::new("owner").unwrap();
    let project = ProjectId::new("project").unwrap();
    let source = SourceId::new("source").unwrap();
    store
        .capture(
            CaptureEvent {
                source: source.clone(),
                owner: owner.clone(),
                actor: ActorId::new("actor").unwrap(),
                project: project.clone(),
                kind: WorkEventKind::Decision,
                outcome: WorkOutcome::Unknown,
                origin: "fixture://semantic".into(),
                content: "Use SQLite for durable storage.".into(),
                recorded_at_ms: 100,
            },
            &RuntimeLimits::default(),
        )
        .await
        .unwrap();
    let item = store
        .propose(
            &owner,
            &ActorId::new("actor").unwrap(),
            RevisionProposal {
                project,
                memory: MemoryId::new("memory").unwrap(),
                revision: RevisionId::new("revision").unwrap(),
                sources: vec![source],
                content: "Use SQLite for durable storage.".into(),
                predecessor: None,
            },
            100,
        )
        .await
        .unwrap();
    (path, store, item)
}
fn vector() -> Embedding {
    Embedding {
        space: EmbeddingSpace {
            provider: "fixture".into(),
            model: "embedding".into(),
            revision: "v1".into(),
            dimensions: 2,
        },
        values: vec![1.0, 0.0],
    }
}
async fn promote(store: &EmbeddedRuntimeStore, item: &RuntimeRevision) {
    store
        .review(
            &item.revision.owner,
            &ActorId::new("actor").unwrap(),
            &ReviewRequest {
                project: item.revision.project.clone(),
                revision: item.revision.id.clone(),
                action: ReviewAction::Promote,
                expected_active: None,
                conflict_with: None,
            },
            101,
        )
        .await
        .unwrap();
}
#[tokio::test]
async fn vectors_respect_review_scope_space_restart_and_purge() {
    let (path, store, item) = fixture().await;
    let embedding = vector();
    let owner = &item.revision.owner;
    let project = &item.revision.project;
    store
        .save_runtime_embedding(&item, &embedding)
        .await
        .unwrap();
    assert!(
        store
            .runtime_vector_hits(owner, project, &embedding, 8)
            .await
            .unwrap()
            .is_empty()
    );
    promote(&store, &item).await;
    assert_eq!(
        store
            .runtime_vector_hits(owner, project, &embedding, 8)
            .await
            .unwrap()[0]
            .item
            .revision
            .id,
        item.revision.id
    );
    assert!(
        store
            .runtime_vector_hits(&OwnerId::new("other").unwrap(), project, &embedding, 8)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        store
            .runtime_vector_hits(
                owner,
                &ProjectId::new("other-project").unwrap(),
                &embedding,
                8
            )
            .await
            .unwrap()
            .is_empty()
    );
    let mut wrong = embedding.clone();
    wrong.space.revision = "other-space".into();
    assert!(
        store
            .runtime_vector_hits(owner, project, &wrong, 8)
            .await
            .unwrap()
            .is_empty()
    );
    drop(store);
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    let reopened = EmbeddedRuntimeStore::new(EmbeddedCaptureStore::open(&path).await.unwrap());
    assert_eq!(
        reopened
            .runtime_vector_hits(owner, project, &embedding, 8)
            .await
            .unwrap()[0]
            .item
            .revision
            .content,
        item.revision.content
    );
    reopened
        .purge(owner, project, &item.revision.sources)
        .await
        .unwrap();
    assert!(
        reopened
            .runtime_vector_hits(owner, project, &embedding, 8)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(matches!(
        reopened.save_runtime_embedding(&item, &embedding).await,
        Err(RuntimeError::Missing)
    ));
    let mut fresh = item.sources[0].clone();
    fresh.recorded_at_ms = 200;
    reopened
        .capture(fresh, &RuntimeLimits::default())
        .await
        .unwrap();
    reopened
        .propose(
            owner,
            &ActorId::new("actor").unwrap(),
            RevisionProposal {
                project: project.clone(),
                memory: item.revision.memory.clone(),
                revision: item.revision.id.clone(),
                sources: item.revision.sources.clone(),
                content: item.revision.content.clone(),
                predecessor: None,
            },
            200,
        )
        .await
        .unwrap();
    assert!(matches!(
        reopened.save_runtime_embedding(&item, &embedding).await,
        Err(RuntimeError::Conflict)
    ));
}
#[tokio::test]
async fn legacy_active_revisions_are_reported_for_semantic_backfill() {
    let (_, store, item) = fixture().await;
    let embedding = vector();
    promote(&store, &item).await;
    let missing = store
        .unindexed_active(
            &item.revision.owner,
            &item.revision.project,
            &embedding.space,
            8,
        )
        .await
        .unwrap();
    assert_eq!(missing[0].revision.id, item.revision.id);
    store
        .save_runtime_embedding(&missing[0], &embedding)
        .await
        .unwrap();
    assert!(
        store
            .unindexed_active(
                &item.revision.owner,
                &item.revision.project,
                &embedding.space,
                8
            )
            .await
            .unwrap()
            .is_empty()
    );
}
#[tokio::test]
async fn paid_background_admission_survives_restart_and_rejects_overlapping_batches() {
    let (path, store, item) = fixture().await;
    store
        .admit_runtime_operation(
            &item.revision.owner,
            &item.revision.project,
            "dream-job:first",
            &item.sources,
        )
        .await
        .unwrap();
    drop(store);
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    let reopened = EmbeddedRuntimeStore::new(EmbeddedCaptureStore::open(&path).await.unwrap());
    assert_eq!(
        reopened
            .admit_runtime_operation(
                &item.revision.owner,
                &item.revision.project,
                "dream-job:first",
                &item.sources
            )
            .await
            .unwrap_err(),
        RuntimeError::UncertainAdmission
    );
    assert_eq!(
        reopened
            .admit_runtime_operation(
                &item.revision.owner,
                &item.revision.project,
                "dream-job:second",
                &item.sources
            )
            .await
            .unwrap_err(),
        RuntimeError::UncertainAdmission
    );
    reopened
        .purge(
            &item.revision.owner,
            &item.revision.project,
            &item.revision.sources,
        )
        .await
        .unwrap();
    assert_eq!(
        reopened
            .admit_runtime_operation(
                &item.revision.owner,
                &item.revision.project,
                "dream-job:first",
                &item.sources
            )
            .await
            .unwrap_err(),
        RuntimeError::Missing
    );
    let mut fresh = item.sources[0].clone();
    fresh.recorded_at_ms = 200;
    reopened
        .capture(fresh.clone(), &RuntimeLimits::default())
        .await
        .unwrap();
    reopened
        .admit_runtime_operation(
            &item.revision.owner,
            &item.revision.project,
            "dream-job:first",
            &[fresh],
        )
        .await
        .unwrap();
}
#[tokio::test]
async fn released_admission_can_be_admitted_again_without_touching_other_markers() {
    let (_path, store, item) = fixture().await;
    let (owner, project) = (&item.revision.owner, &item.revision.project);
    store
        .admit_runtime_operation(owner, project, "embedding:fixture", &item.sources)
        .await
        .unwrap();
    store
        .admit_runtime_operation(owner, project, "dream-job:kept", &item.sources)
        .await
        .unwrap();
    store
        .release_runtime_operation(owner, project, "embedding:fixture")
        .await
        .unwrap();
    store
        .admit_runtime_operation(owner, project, "embedding:fixture", &item.sources)
        .await
        .unwrap();
    assert_eq!(
        store
            .admit_runtime_operation(owner, project, "dream-job:other", &item.sources)
            .await
            .unwrap_err(),
        RuntimeError::UncertainAdmission
    );
    store
        .release_runtime_operation(owner, project, "dream-job:kept")
        .await
        .unwrap();
    store
        .admit_runtime_operation(owner, project, "dream-job:other", &item.sources)
        .await
        .unwrap();
}
