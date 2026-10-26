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

#[tokio::test]
async fn embedding_admission_separates_every_space_component_and_owner_project() {
    use lambda_memory::integrations::runtime_embeddings::operation;
    let (_, store, item) = fixture().await;
    let space = vector().space;
    let owner = &item.revision.owner;
    let project = &item.revision.project;
    let revision = &item.revision.id;
    let original = operation(owner, project, revision, &space);
    let mut variants = Vec::new();
    let mut dimensions = space.clone();
    dimensions.dimensions = 3;
    variants.push(dimensions);
    let mut provider = space.clone();
    provider.provider = "other".into();
    variants.push(provider);
    let mut model = space.clone();
    model.model = "other".into();
    variants.push(model);
    let mut index = space.clone();
    index.revision = "other".into();
    variants.push(index);
    store.admit_runtime_embedding(&item, &space).await.unwrap();
    assert_eq!(
        store
            .admit_runtime_embedding(&item, &space)
            .await
            .unwrap_err(),
        RuntimeError::UncertainAdmission
    );
    for variant in variants {
        assert_ne!(original, operation(owner, project, revision, &variant));
        store
            .admit_runtime_embedding(&item, &variant)
            .await
            .unwrap();
    }
    assert_ne!(
        original,
        operation(&OwnerId::new("other").unwrap(), project, revision, &space)
    );
    assert_ne!(
        original,
        operation(owner, &ProjectId::new("other").unwrap(), revision, &space)
    );
    assert_ne!(
        original,
        operation(owner, project, &RevisionId::new("other").unwrap(), &space)
    );
    assert!(
        store
            .embedding_recovery_plan(&OwnerId::new("other").unwrap(), project, &space, 16)
            .await
            .unwrap()
            .items
            .is_empty()
    );
    assert!(
        store
            .embedding_recovery_plan(owner, &ProjectId::new("other").unwrap(), &space, 16)
            .await
            .unwrap()
            .items
            .is_empty()
    );
}

#[tokio::test]
async fn orphan_embedding_recovery_preserves_lineage_and_rejects_stale_replay_after_reopen() {
    let (path, store, item) = fixture().await;
    let space = vector().space;
    let owner = &item.revision.owner;
    let project = &item.revision.project;
    let actor = ActorId::new("operator").unwrap();
    store.admit_runtime_embedding(&item, &space).await.unwrap();
    let plan = store
        .embedding_recovery_plan(owner, project, &space, 16)
        .await
        .unwrap();
    assert_eq!(plan.items.len(), 1);
    assert_eq!(plan.items[0].previous_operations.len(), 1);
    drop(store);
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    let store = EmbeddedRuntimeStore::new(EmbeddedCaptureStore::open(&path).await.unwrap());
    let recovered = store
        .admit_embedding_recovery(owner, &actor, project, &plan, 200)
        .await
        .unwrap();
    assert_eq!(recovered.len(), 1);
    let next = store
        .embedding_recovery_plan(owner, project, &space, 16)
        .await
        .unwrap();
    assert_eq!(next.items[0].previous_operations.len(), 2);
    assert!(
        next.items[0]
            .previous_operations
            .contains(&plan.items[0].previous_operations[0])
    );
    drop(store);
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    let store = EmbeddedRuntimeStore::new(EmbeddedCaptureStore::open(&path).await.unwrap());
    assert_eq!(
        store
            .admit_embedding_recovery(owner, &actor, project, &plan, 201)
            .await
            .unwrap_err(),
        RuntimeError::UncertainAdmission
    );
    assert_eq!(
        store
            .admit_runtime_embedding(&item, &space)
            .await
            .unwrap_err(),
        RuntimeError::UncertainAdmission
    );
    let mut changed = next.clone();
    changed.items[0].snapshot = "0".repeat(64);
    assert_eq!(
        store
            .admit_embedding_recovery(owner, &actor, project, &changed, 202)
            .await
            .unwrap_err(),
        RuntimeError::Conflict
    );
    assert!(matches!(
        store
            .admit_embedding_recovery(&OwnerId::new("other").unwrap(), &actor, project, &next, 202)
            .await,
        Err(RuntimeError::Missing)
    ));
    assert!(matches!(
        store
            .admit_embedding_recovery(owner, &actor, &ProjectId::new("other").unwrap(), &next, 202)
            .await,
        Err(RuntimeError::Unauthorized)
    ));
    store
        .save_runtime_embedding(&recovered[0].0, &vector())
        .await
        .unwrap();
    assert!(
        store
            .embedding_recovery_plan(owner, project, &space, 16)
            .await
            .unwrap()
            .items
            .is_empty()
    );
    assert_eq!(
        store
            .admit_embedding_recovery(owner, &actor, project, &next, 203)
            .await
            .unwrap_err(),
        RuntimeError::Conflict
    );
}

#[tokio::test]
async fn legacy_unknown_embedding_admission_fails_closed_only_in_its_space() {
    use lambda_memory::integrations::runtime_embeddings::legacy_operation;
    let (_, store, item) = fixture().await;
    let owner = &item.revision.owner;
    let project = &item.revision.project;
    let space = vector().space;
    let legacy = legacy_operation(&item.revision.id, &space);
    store
        .admit_runtime_operation(owner, project, &legacy, &item.sources)
        .await
        .unwrap();
    assert_eq!(
        store
            .admit_runtime_embedding(&item, &space)
            .await
            .unwrap_err(),
        RuntimeError::UncertainAdmission
    );
    let mut dimension_unknown = space.clone();
    dimension_unknown.dimensions = 3;
    assert_eq!(
        store
            .admit_runtime_embedding(&item, &dimension_unknown)
            .await
            .unwrap_err(),
        RuntimeError::UncertainAdmission
    );
    let plan = store
        .embedding_recovery_plan(owner, project, &space, 16)
        .await
        .unwrap();
    assert_eq!(plan.items[0].previous_operations[0].as_str(), legacy);
    let mut new_space = space.clone();
    new_space.revision = "new-preparation".into();
    store
        .admit_runtime_embedding(&item, &new_space)
        .await
        .unwrap();
    promote(&store, &item).await;
    let active_plan = store
        .embedding_recovery_plan(owner, project, &space, 16)
        .await
        .unwrap();
    store
        .admit_embedding_recovery(
            owner,
            &ActorId::new("operator").unwrap(),
            project,
            &active_plan,
            201,
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn recovery_consent_cannot_apply_to_recaptured_originals() {
    let (_, store, item) = fixture().await;
    let space = vector().space;
    let owner = &item.revision.owner;
    let project = &item.revision.project;
    let actor = ActorId::new("operator").unwrap();
    store.admit_runtime_embedding(&item, &space).await.unwrap();
    let plan = store
        .embedding_recovery_plan(owner, project, &space, 16)
        .await
        .unwrap();
    store
        .purge(owner, project, &item.revision.sources)
        .await
        .unwrap();
    let mut fresh = item.sources[0].clone();
    fresh.recorded_at_ms = 200;
    store
        .capture(fresh, &RuntimeLimits::default())
        .await
        .unwrap();
    let fresh = store
        .propose(
            owner,
            &actor,
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
    store.admit_runtime_embedding(&fresh, &space).await.unwrap();
    assert_eq!(
        store
            .admit_embedding_recovery(owner, &actor, project, &plan, 201)
            .await
            .unwrap_err(),
        RuntimeError::Conflict
    );
}

#[tokio::test]
async fn dimensions_separate_vectors_and_diagnostics_track_recovery_admissions() {
    use lambda_memory::domain_models::{
        WorkerInstanceId, runtime_diagnostics::ActivityReadContext,
    };
    let (_, store, item) = fixture().await;
    let owner = &item.revision.owner;
    let project = &item.revision.project;
    let embedding = vector();
    let mut alternate = embedding.clone();
    alternate.space.dimensions = 3;
    alternate.values = vec![1.0, 0.0, 0.0];
    let mut context = ActivityReadContext {
        instance: WorkerInstanceId::new("worker").unwrap(),
        active_entries: vec![],
        active_paid_operations: vec![],
    };
    store
        .admit_runtime_embedding(&item, &embedding.space)
        .await
        .unwrap();
    assert_eq!(
        store
            .activity_snapshot(owner, project, Some(&embedding.space), &context, 200)
            .await
            .unwrap()
            .pending
            .blocked_revisions,
        1
    );
    assert_eq!(
        store
            .activity_snapshot(owner, project, Some(&alternate.space), &context, 200)
            .await
            .unwrap()
            .pending
            .blocked_revisions,
        0
    );
    let plan = store
        .embedding_recovery_plan(owner, project, &embedding.space, 16)
        .await
        .unwrap();
    let recovery = store
        .admit_embedding_recovery(
            owner,
            &ActorId::new("operator").unwrap(),
            project,
            &plan,
            201,
        )
        .await
        .unwrap();
    assert_eq!(
        store
            .activity_snapshot(owner, project, Some(&embedding.space), &context, 202)
            .await
            .unwrap()
            .pending
            .blocked_revisions,
        1
    );
    context.active_paid_operations.push(recovery[0].1.clone());
    assert_eq!(
        store
            .activity_snapshot(owner, project, Some(&embedding.space), &context, 202)
            .await
            .unwrap()
            .pending
            .blocked_revisions,
        0
    );
    store
        .save_runtime_embedding(&item, &embedding)
        .await
        .unwrap();
    assert!(
        !store
            .has_runtime_embedding(&item, &alternate.space)
            .await
            .unwrap()
    );
    promote(&store, &item).await;
    assert!(
        store
            .runtime_vector_hits(owner, project, &alternate, 8)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        store
            .unindexed_active(owner, project, &alternate.space, 8)
            .await
            .unwrap()
            .len(),
        1
    );
    store
        .save_runtime_embedding(&item, &alternate)
        .await
        .unwrap();
    assert_eq!(
        store
            .runtime_vector_hits(owner, project, &alternate, 8)
            .await
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        store
            .runtime_vector_hits(owner, project, &embedding, 8)
            .await
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn invalid_member_rolls_back_the_entire_embedding_recovery_admission_set() {
    let (_, store, first) = fixture().await;
    let owner = &first.revision.owner;
    let project = &first.revision.project;
    let actor = ActorId::new("operator").unwrap();
    let second = store
        .propose(
            owner,
            &actor,
            RevisionProposal {
                project: project.clone(),
                memory: MemoryId::new("other-memory").unwrap(),
                revision: RevisionId::new("revision-two").unwrap(),
                sources: first.revision.sources.clone(),
                content: "Keep immutable storage evidence.".into(),
                predecessor: None,
            },
            101,
        )
        .await
        .unwrap();
    let space = vector().space;
    store.admit_runtime_embedding(&first, &space).await.unwrap();
    store
        .admit_runtime_embedding(&second, &space)
        .await
        .unwrap();
    let plan = store
        .embedding_recovery_plan(owner, project, &space, 16)
        .await
        .unwrap();
    assert_eq!(plan.items.len(), 2);
    let mut stale = plan.clone();
    stale.items[1].snapshot = "0".repeat(64);
    assert_eq!(
        store
            .admit_embedding_recovery(owner, &actor, project, &stale, 200)
            .await
            .unwrap_err(),
        RuntimeError::Conflict
    );
    assert_eq!(
        store
            .embedding_recovery_plan(owner, project, &space, 16)
            .await
            .unwrap(),
        plan
    );
    let mut duplicate = plan.clone();
    duplicate.items[1] = duplicate.items[0].clone();
    assert!(matches!(
        store
            .admit_embedding_recovery(owner, &actor, project, &duplicate, 200)
            .await,
        Err(RuntimeError::InvalidInput(_))
    ));
    assert_eq!(
        store
            .admit_embedding_recovery(owner, &actor, project, &plan, 201)
            .await
            .unwrap()
            .len(),
        2
    );
}

#[tokio::test]
async fn playbook_is_never_indexed_nor_recalled() {
    use lambda_memory::domain_models::procedural::PLAYBOOK_MEMORY;
    let (_, store, item) = fixture().await;
    let owner = &item.revision.owner;
    let project = &item.revision.project;
    let actor = ActorId::new("actor").unwrap();
    let embedding = vector();
    // One ordinary active revision with a current-space vector.
    promote(&store, &item).await;
    store
        .save_runtime_embedding(&item, &embedding)
        .await
        .unwrap();
    // An active playbook revision whose text contains the query term and which has no vector.
    let playbook = store
        .propose(
            owner,
            &actor,
            RevisionProposal {
                project: project.clone(),
                memory: MemoryId::new(PLAYBOOK_MEMORY).unwrap(),
                revision: RevisionId::new("playbook-1").unwrap(),
                sources: item.revision.sources.clone(),
                content: "- Run the SQLite migration with cargo test [F1]".into(),
                predecessor: None,
            },
            102,
        )
        .await
        .unwrap();
    promote(&store, &playbook).await;
    assert!(store.has_active_runtime(owner, project).await.unwrap());
    // The playbook never blocks recall: it is not part of the unindexed active set.
    assert!(
        store
            .unindexed_active(owner, project, &embedding.space, 16)
            .await
            .unwrap()
            .is_empty()
    );
    // Lexical recall skips it even though it matches the term.
    let hits = store
        .recall(
            owner,
            std::slice::from_ref(project),
            &["sqlite".to_string()],
            8,
        )
        .await
        .unwrap();
    assert_eq!(
        hits.iter()
            .map(|h| h.item.revision.id.as_str())
            .collect::<Vec<_>>(),
        vec!["revision"]
    );
    // Even with a stray vector, vector recall skips it.
    store
        .save_runtime_embedding(&playbook, &embedding)
        .await
        .unwrap();
    let hits = store
        .runtime_vector_hits(owner, project, &embedding, 8)
        .await
        .unwrap();
    assert_eq!(
        hits.iter()
            .map(|h| h.item.revision.id.as_str())
            .collect::<Vec<_>>(),
        vec!["revision"]
    );
    // It is still readable as the active playbook.
    let active = store
        .active_playbook(owner, project)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(active.revision.id.as_str(), "playbook-1");
    assert!(active.revision.content.contains("SQLite"));
    assert!(
        store
            .active_playbook(owner, &ProjectId::new("other").unwrap())
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn playbook_alone_is_not_an_active_runtime() {
    use lambda_memory::domain_models::procedural::PLAYBOOK_MEMORY;
    let (_, store, item) = fixture().await;
    let owner = &item.revision.owner;
    let project = &item.revision.project;
    let playbook = store
        .propose(
            owner,
            &ActorId::new("actor").unwrap(),
            RevisionProposal {
                project: project.clone(),
                memory: MemoryId::new(PLAYBOOK_MEMORY).unwrap(),
                revision: RevisionId::new("playbook-1").unwrap(),
                sources: item.revision.sources.clone(),
                content: "- Run tests [F1]".into(),
                predecessor: None,
            },
            102,
        )
        .await
        .unwrap();
    promote(&store, &playbook).await;
    assert!(!store.has_active_runtime(owner, project).await.unwrap());
    assert!(
        store
            .active_playbook(owner, project)
            .await
            .unwrap()
            .is_some()
    );
}
