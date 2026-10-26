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

fn source_event(id: &str, kind: WorkEventKind, origin: &str, at: u64) -> CaptureEvent {
    CaptureEvent {
        kind,
        origin: origin.into(),
        content: format!("content of {id}"),
        ..event(id, at)
    }
}
async fn publish(
    store: &EmbeddedRuntimeStore,
    revision: &str,
    memory: &str,
    sources: &[CaptureEvent],
    watermark: u64,
) {
    for source in sources {
        capture(store, source).await;
    }
    let snapshot = JobSnapshot {
        id: JobId::new(format!("job-{revision}")).unwrap(),
        owner: sources[0].owner.clone(),
        project: sources[0].project.clone(),
        watermark_ms: watermark,
        sources: sources.to_vec(),
    };
    let item = Revision {
        id: RevisionId::new(revision).unwrap(),
        memory: MemoryId::new(memory).unwrap(),
        owner: sources[0].owner.clone(),
        project: sources[0].project.clone(),
        kind: KnowledgeKind::Knowledge,
        status: RevisionStatus::PendingReview,
        content: format!("memory {revision}"),
        prerequisites: vec![],
        sources: sources.iter().map(|s| s.source.clone()).collect(),
        predecessor: None,
        model: "fixture".into(),
        prompt_version: "v1".into(),
    };
    assert!(store.commit_batch(snapshot, vec![item]).await.unwrap());
    store
        .review(
            &sources[0].owner,
            &sources[0].actor,
            &ReviewRequest {
                project: sources[0].project.clone(),
                revision: RevisionId::new(revision).unwrap(),
                action: ReviewAction::Promote,
                expected_active: None,
                conflict_with: None,
            },
            watermark + 1,
        )
        .await
        .unwrap();
}
async fn reopen(path: &str) -> EmbeddedRuntimeStore {
    for _ in 0..20 {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        if let Ok(capture) = EmbeddedCaptureStore::open(path).await {
            return EmbeddedRuntimeStore::new(capture);
        }
    }
    panic!("datastore lock did not release after close");
}
async fn operator(path: &str, args: &[&str]) -> serde_json::Value {
    // The datastore lock is released shortly after the owning store is dropped; opening a
    // locked store fails before anything is read or written, so retrying is safe.
    for _ in 0..30 {
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_lambda-memory"))
            .args(args)
            .env("LAMBDA_MEMORY_DATA_DIR", path)
            .env("LAMBDA_MEMORY_TOKEN", "t".repeat(40))
            .env("LAMBDA_MEMORY_OWNER", "owner")
            .env("LAMBDA_MEMORY_ACTOR", "actor")
            .env("LAMBDA_MEMORY_PROJECT", "project")
            .output()
            .unwrap();
        if output.status.success() {
            return serde_json::from_slice(&output.stdout).unwrap();
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("storage unavailable"), "{stderr}");
    }
    panic!("datastore lock did not release");
}
async fn recalled(store: &EmbeddedRuntimeStore) -> Vec<String> {
    store
        .recall(
            &OwnerId::new("owner").unwrap(),
            &[ProjectId::new("project").unwrap()],
            &["memory".into()],
            8,
        )
        .await
        .unwrap()
        .into_iter()
        .map(|hit| hit.item.revision.id.to_string())
        .collect()
}
async fn states(store: &EmbeddedRuntimeStore, state: LifecycleState) -> Vec<String> {
    store
        .list(
            &OwnerId::new("owner").unwrap(),
            &ProjectId::new("project").unwrap(),
            &[state],
            32,
        )
        .await
        .unwrap()
        .into_iter()
        .map(|item| item.revision.id.to_string())
        .collect()
}

#[tokio::test]
async fn reject_tool_metadata_dry_run_then_apply() {
    let (path, store) = store().await;
    let legacy = |id: &str, at| source_event(id, WorkEventKind::Result, "omp://tool_result", at);
    for (i, id) in ["t1", "t2", "t3"].into_iter().enumerate() {
        publish(
            &store,
            &format!("rev-{id}"),
            &format!("mem-{id}"),
            &[legacy(id, 10 + i as u64)],
            100 + i as u64 * 10,
        )
        .await;
    }
    publish(
        &store,
        "rev-mixed",
        "mem-mixed",
        &[
            legacy("m1", 20),
            source_event("m2", WorkEventKind::Result, "fixture://other", 21),
        ],
        200,
    )
    .await;
    let decision = source_event("d1", WorkEventKind::Decision, "fixture://decision", 30);
    publish(
        &store,
        "rev-decision",
        "mem-decision",
        std::slice::from_ref(&decision),
        300,
    )
    .await;
    let owner = OwnerId::new("owner").unwrap();
    let project = ProjectId::new("project").unwrap();
    let found = store
        .tool_metadata_revisions(&owner, &project, 32)
        .await
        .unwrap();
    assert_eq!(
        found.iter().map(|id| id.as_str()).collect::<Vec<_>>(),
        ["rev-t1", "rev-t2", "rev-t3"]
    );
    drop(store);

    let dry = operator(&path, &["--reject-tool-metadata"]).await;
    assert_eq!(dry["project"], "project");
    assert_eq!(dry["count"], 3);
    assert_eq!(dry["samples"].as_array().unwrap().len(), 3);
    assert_eq!(dry["samples"][0]["revision"], "rev-t1");
    assert_eq!(dry["samples"][0]["content"], "memory rev-t1");
    let store = reopen(&path).await;
    assert_eq!(states(&store, LifecycleState::Active).await.len(), 5);
    assert_eq!(recalled(&store).await.len(), 5);
    assert!(states(&store, LifecycleState::Rejected).await.is_empty());
    drop(store);

    assert_eq!(
        operator(&path, &["--reject-tool-metadata", "--apply"]).await,
        serde_json::json!({"rejected": 3})
    );
    let store = reopen(&path).await;
    let mut active = states(&store, LifecycleState::Active).await;
    active.sort();
    assert_eq!(active, ["rev-decision", "rev-mixed"]);
    // Recall no longer returns the retired revisions.
    let mut hits = recalled(&store).await;
    hits.sort();
    assert_eq!(hits, ["rev-decision", "rev-mixed"]);
    // The lineage read (with audit) still decodes the operator audit entry as a rejection.
    let page = store
        .list_page(
            &OwnerId::new("owner").unwrap(),
            &ListQuery {
                project: ProjectId::new("project").unwrap(),
                states: vec![LifecycleState::Rejected],
                limit: 8,
                memory: Some(MemoryId::new("mem-t1").unwrap()),
                after: None,
            },
            true,
        )
        .await
        .unwrap();
    assert_eq!(page.audit.last().unwrap().action, ReviewAction::Reject);
    assert_eq!(page.audit.last().unwrap().to, LifecycleState::Rejected);
    let mut rejected = states(&store, LifecycleState::Rejected).await;
    rejected.sort();
    assert_eq!(rejected, ["rev-t1", "rev-t2", "rev-t3"]);
    // Sources are kept: re-capturing the identical original is an idempotent no-op.
    assert_eq!(
        store
            .capture(legacy("t1", 10), &RuntimeLimits::default())
            .await
            .unwrap()
            .status,
        CaptureStatus::AlreadySavedPending
    );
    assert!(
        store
            .tool_metadata_revisions(&owner, &project, 32)
            .await
            .unwrap()
            .is_empty()
    );
    drop(store);
    assert_eq!(
        operator(&path, &["--reject-tool-metadata"]).await["count"],
        0
    );
}

#[tokio::test]
async fn tool_metadata_listing_skips_the_playbook_and_is_bounded() {
    let (_, store) = store().await;
    let legacy = source_event("p1", WorkEventKind::Result, "omp://tool_result", 10);
    publish(
        &store,
        "rev-playbook",
        "procedural:playbook",
        &[legacy],
        100,
    )
    .await;
    let owner = OwnerId::new("owner").unwrap();
    let project = ProjectId::new("project").unwrap();
    assert!(
        store
            .tool_metadata_revisions(&owner, &project, 32)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        store.tool_metadata_count(&owner, &project).await.unwrap(),
        0
    );
    for limit in [0, 33] {
        assert!(
            store
                .tool_metadata_revisions(&owner, &project, limit)
                .await
                .is_err()
        );
    }
}

#[tokio::test]
async fn retire_tool_metadata_skips_ineligible_revisions_and_review_still_refuses_active() {
    let (_, store) = store().await;
    let legacy = |id: &str, at| source_event(id, WorkEventKind::Result, "omp://tool_result", at);
    publish(&store, "rev-legacy", "mem-legacy", &[legacy("l1", 10)], 100).await;
    publish(
        &store,
        "rev-playbook",
        "procedural:playbook",
        &[legacy("p1", 11)],
        110,
    )
    .await;
    publish(
        &store,
        "rev-mixed",
        "mem-mixed",
        &[
            legacy("m1", 12),
            source_event("m2", WorkEventKind::Result, "fixture://other", 13),
        ],
        120,
    )
    .await;
    let decision = source_event("d1", WorkEventKind::Decision, "fixture://decision", 14);
    publish(&store, "rev-decision", "mem-decision", &[decision], 130).await;
    let owner = OwnerId::new("owner").unwrap();
    let actor = ActorId::new("operator").unwrap();
    let project = ProjectId::new("project").unwrap();
    let ids = |names: &[&str]| {
        names
            .iter()
            .map(|n| RevisionId::new(*n).unwrap())
            .collect::<Vec<_>>()
    };
    for bad in [vec![], ids(&["rev-legacy"; 33])] {
        assert_eq!(
            store
                .retire_tool_metadata(&owner, &actor, &project, &bad, 500)
                .await
                .unwrap_err(),
            RuntimeError::InvalidInput("retire batch bounds")
        );
    }
    // The public review contract is unchanged: an active revision cannot be rejected.
    let reject = |revision: &str| ReviewRequest {
        project: project.clone(),
        revision: RevisionId::new(revision).unwrap(),
        action: ReviewAction::Reject,
        expected_active: None,
        conflict_with: None,
    };
    assert_eq!(
        store
            .review(&owner, &actor, &reject("rev-legacy"), 400)
            .await
            .unwrap_err(),
        RuntimeError::InvalidInput("cannot reject active")
    );
    // Ineligible and unknown revisions are ignored even when listed.
    let retired = store
        .retire_tool_metadata(
            &owner,
            &actor,
            &project,
            &ids(&[
                "rev-playbook",
                "rev-mixed",
                "rev-decision",
                "rev-unknown",
                "rev-legacy",
            ]),
            500,
        )
        .await
        .unwrap();
    assert_eq!(retired, 1);
    let mut active = states(&store, LifecycleState::Active).await;
    active.sort();
    assert_eq!(active, ["rev-decision", "rev-mixed", "rev-playbook"]);
    assert_eq!(
        states(&store, LifecycleState::Rejected).await,
        ["rev-legacy"]
    );
    // A second call finds it no longer active and retires nothing.
    assert_eq!(
        store
            .retire_tool_metadata(&owner, &actor, &project, &ids(&["rev-legacy"]), 600)
            .await
            .unwrap(),
        0
    );
    // Another project's scope never retires this project's revisions.
    let foreign = ProjectId::new("foreign").unwrap();
    assert_eq!(
        store
            .retire_tool_metadata(&owner, &actor, &foreign, &ids(&["rev-playbook"]), 600)
            .await
            .unwrap(),
        0
    );
}
