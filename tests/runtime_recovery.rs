#![cfg(feature = "memory-runtime")]
use lambda_memory::domain_models::{
    ActorId, JobId, MemoryId, OwnerId, ProjectId, RevisionId, SourceId,
};
use lambda_memory::domain_models::{capture::*, consolidation::*, runtime::*};
use lambda_memory::integrations::database::embedded_capture::{
    EmbeddedCaptureStore, EmbeddedRuntimeStore,
};
use lambda_memory::services::runtime_service::MemoryRuntime;
use std::sync::atomic::{AtomicUsize, Ordering};
static NEXT: AtomicUsize = AtomicUsize::new(0);

fn event(id: &str, at: u64) -> CaptureEvent {
    CaptureEvent {
        source: SourceId::new(id).unwrap(),
        owner: OwnerId::new("owner").unwrap(),
        actor: ActorId::new("actor").unwrap(),
        project: ProjectId::new("project").unwrap(),
        kind: WorkEventKind::Error,
        outcome: WorkOutcome::Failed,
        origin: "fixture://recovery".into(),
        content: "The previous operation failed; do not treat it as a success.".into(),
        recorded_at_ms: at,
    }
}
async fn store() -> (String, EmbeddedRuntimeStore) {
    let path = format!(
        "{}/lambda-recovery-{}-{}",
        std::env::temp_dir().display(),
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    );
    let store = EmbeddedRuntimeStore::new(EmbeddedCaptureStore::open(&path).await.unwrap());
    (path, store)
}
async fn capture(store: &EmbeddedRuntimeStore, event: &CaptureEvent) {
    store
        .capture(event.clone(), &RuntimeLimits::default())
        .await
        .unwrap();
}
async fn block(store: &EmbeddedRuntimeStore, events: &[CaptureEvent]) -> JobId {
    let operation = JobId::new("dream-job:original").unwrap();
    store
        .admit_runtime_operation(
            &events[0].owner,
            &events[0].project,
            operation.as_str(),
            events,
        )
        .await
        .unwrap();
    operation
}

#[tokio::test]
async fn oldest_blocked_sources_do_not_starve_scoped_later_batches() {
    let (_, store) = store().await;
    let blocked = (0..4)
        .map(|i| event(&format!("blocked:{i}"), 100 + i))
        .collect::<Vec<_>>();
    let free = (0..3)
        .map(|i| event(&format!("free:{i}"), 200 + i))
        .collect::<Vec<_>>();
    for source in blocked.iter().chain(&free) {
        capture(&store, source).await;
    }
    block(&store, &blocked).await;
    let mut other_owner = free[0].clone();
    other_owner.owner = OwnerId::new("other-owner").unwrap();
    capture(&store, &other_owner).await;
    block(&store, &[other_owner]).await;
    let mut other_project = event("other-project", 50);
    other_project.project = ProjectId::new("other-project").unwrap();
    capture(&store, &other_project).await;
    block(&store, &[other_project]).await;
    let owner = &blocked[0].owner;
    let project = &blocked[0].project;
    assert_eq!(
        store
            .pending_sources(owner, project, 2, true)
            .await
            .unwrap(),
        free[..2]
    );
    assert_eq!(
        store
            .pending_sources(owner, project, 16, false)
            .await
            .unwrap(),
        blocked.iter().chain(&free).cloned().collect::<Vec<_>>()
    );
    assert_eq!(
        store
            .blocked_dream_sources(owner, project, 2)
            .await
            .unwrap()
            .sources,
        blocked[..2]
    );
}

#[tokio::test]
async fn explicit_recovery_preserves_old_admission_and_rejects_reusing_approval_after_restart() {
    let (path, store) = store().await;
    let source = event("blocked", 100);
    capture(&store, &source).await;
    let original = block(&store, std::slice::from_ref(&source)).await;
    let plan = store
        .dream_recovery_batch(
            &source.owner,
            &source.project,
            std::slice::from_ref(&source.source),
        )
        .await
        .unwrap();
    let recovery = JobId::new("dream-job:recovery:approved").unwrap();
    store
        .admit_runtime_recovery_operation(
            &source.owner,
            &source.actor,
            &source.project,
            recovery.as_str(),
            &plan.sources,
            &plan.previous_operations,
            200,
        )
        .await
        .unwrap();
    let next = store
        .dream_recovery_batch(
            &source.owner,
            &source.project,
            std::slice::from_ref(&source.source),
        )
        .await
        .unwrap();
    assert_eq!(next.sources, vec![source.clone()]);
    assert_eq!(next.previous_operations, vec![original, recovery]);
    assert_eq!(
        store
            .admit_runtime_operation(
                &source.owner,
                &source.project,
                "dream-job:automatic",
                &plan.sources
            )
            .await
            .unwrap_err(),
        RuntimeError::UncertainAdmission
    );
    drop(store);
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    let reopened = EmbeddedRuntimeStore::new(EmbeddedCaptureStore::open(&path).await.unwrap());
    assert_eq!(
        reopened
            .admit_runtime_recovery_operation(
                &source.owner,
                &source.actor,
                &source.project,
                "dream-job:recovery:stale",
                &plan.sources,
                &plan.previous_operations,
                300
            )
            .await
            .unwrap_err(),
        RuntimeError::UncertainAdmission
    );
    assert_eq!(
        reopened
            .pending_sources(&source.owner, &source.project, 8, true)
            .await
            .unwrap(),
        Vec::<CaptureEvent>::new()
    );
}

#[tokio::test]
async fn recovery_refuses_unmarked_or_changed_originals_and_scope_widening() {
    let (_, store) = store().await;
    let source = event("blocked", 100);
    let free = event("free", 101);
    capture(&store, &source).await;
    capture(&store, &free).await;
    let previous = vec![block(&store, std::slice::from_ref(&source)).await];
    assert_eq!(
        store
            .admit_runtime_recovery_operation(
                &source.owner,
                &source.actor,
                &source.project,
                "dream-job:recovery:unmarked",
                &[source.clone(), free.clone()],
                &previous,
                200
            )
            .await
            .unwrap_err(),
        RuntimeError::UncertainAdmission
    );
    assert_eq!(
        store
            .dream_recovery_batch(
                &source.owner,
                &source.project,
                std::slice::from_ref(&free.source)
            )
            .await
            .unwrap_err(),
        RuntimeError::Conflict
    );
    let mut changed = source.clone();
    changed.outcome = WorkOutcome::Succeeded;
    assert_eq!(
        store
            .admit_runtime_recovery_operation(
                &source.owner,
                &source.actor,
                &source.project,
                "dream-job:recovery:changed",
                &[changed],
                &previous,
                200
            )
            .await
            .unwrap_err(),
        RuntimeError::InvalidInput("immutable original mismatch")
    );
    assert_eq!(
        store
            .admit_runtime_recovery_operation(
                &source.owner,
                &source.actor,
                &ProjectId::new("foreign").unwrap(),
                "dream-job:recovery:foreign",
                std::slice::from_ref(&source),
                &previous,
                200
            )
            .await
            .unwrap_err(),
        RuntimeError::Unauthorized
    );
    assert_eq!(
        store
            .pending_sources(&source.owner, &source.project, 8, false)
            .await
            .unwrap(),
        vec![source, free]
    );
}

#[tokio::test]
async fn recovered_or_purged_recaptured_sources_cannot_reuse_old_authorization() {
    let (_, store) = store().await;
    let source = event("blocked", 100);
    capture(&store, &source).await;
    let previous = vec![block(&store, std::slice::from_ref(&source)).await];
    let snapshot = JobSnapshot {
        id: JobId::new("completed-job").unwrap(),
        owner: source.owner.clone(),
        project: source.project.clone(),
        watermark_ms: 100,
        sources: vec![source.clone()],
    };
    let revision = Revision {
        id: RevisionId::new("completed-revision").unwrap(),
        memory: MemoryId::new("completed-memory").unwrap(),
        owner: source.owner.clone(),
        project: source.project.clone(),
        kind: KnowledgeKind::Knowledge,
        status: RevisionStatus::PendingReview,
        content: source.content.clone(),
        prerequisites: vec![],
        sources: vec![source.source.clone()],
        predecessor: None,
        model: "fixture".into(),
        prompt_version: "v1".into(),
    };
    assert!(store.commit_batch(snapshot, vec![revision]).await.unwrap());
    assert_eq!(
        store
            .admit_runtime_recovery_operation(
                &source.owner,
                &source.actor,
                &source.project,
                "dream-job:recovery:consumed",
                std::slice::from_ref(&source),
                &previous,
                200
            )
            .await
            .unwrap_err(),
        RuntimeError::Conflict
    );
    store
        .purge(
            &source.owner,
            &source.project,
            std::slice::from_ref(&source.source),
        )
        .await
        .unwrap();
    let mut fresh = source.clone();
    fresh.recorded_at_ms = 300;
    capture(&store, &fresh).await;
    assert_eq!(
        store
            .admit_runtime_recovery_operation(
                &source.owner,
                &source.actor,
                &source.project,
                "dream-job:recovery:stale-snapshot",
                std::slice::from_ref(&source),
                &previous,
                400
            )
            .await
            .unwrap_err(),
        RuntimeError::InvalidInput("immutable original mismatch")
    );
    assert_eq!(
        store
            .admit_runtime_recovery_operation(
                &fresh.owner,
                &fresh.actor,
                &fresh.project,
                "dream-job:recovery:stale-approval",
                std::slice::from_ref(&fresh),
                &previous,
                400
            )
            .await
            .unwrap_err(),
        RuntimeError::UncertainAdmission
    );
}

#[tokio::test]
async fn explicit_paid_recovery_cannot_fall_back_to_local_extraction() {
    let (_, store) = store().await;
    let source = event("blocked", 100);
    capture(&store, &source).await;
    let previous = vec![block(&store, std::slice::from_ref(&source)).await];
    let runtime = MemoryRuntime::new(
        store.clone(),
        RuntimeScope {
            owner: source.owner.clone(),
            actor: source.actor.clone(),
            projects: vec![source.project.clone()],
        },
        RuntimeLimits::default(),
    )
    .unwrap();
    assert_eq!(
        runtime
            .recover_dream_sources(
                &source.project,
                std::slice::from_ref(&source.source),
                &previous
            )
            .await
            .result
            .unwrap_err(),
        RuntimeError::InvalidInput("explicit paid recovery bounds")
    );
    assert_eq!(
        store
            .pending_sources(&source.owner, &source.project, 8, false)
            .await
            .unwrap(),
        vec![source]
    );
}
