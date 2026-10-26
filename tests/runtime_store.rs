#![cfg(feature = "memory-runtime")]
use lambda_memory::domain_models::*;
use lambda_memory::domain_models::{capture::*, consolidation::*, runtime::*};
use lambda_memory::integrations::capture_store::AsyncCaptureStore;
use lambda_memory::integrations::consolidation_store::AsyncConsolidationStore;
use lambda_memory::integrations::database::embedded_capture::{
    EmbeddedCaptureStore, EmbeddedKnowledgeStore, EmbeddedRuntimeStore,
};
use std::sync::atomic::{AtomicUsize, Ordering};
static NEXT: AtomicUsize = AtomicUsize::new(0);
fn path() -> String {
    format!(
        "{}/lambda-runtime-{}-{}",
        std::env::temp_dir().display(),
        std::process::id(),
        NEXT.fetch_add(1, Ordering::SeqCst)
    )
}
fn owner(id: &str) -> OwnerId {
    OwnerId::new(id).unwrap()
}
fn actor() -> ActorId {
    ActorId::new("actor").unwrap()
}
fn project(id: &str) -> ProjectId {
    ProjectId::new(id).unwrap()
}
fn source(id: &str) -> SourceId {
    SourceId::new(id).unwrap()
}
fn memory(id: &str) -> MemoryId {
    MemoryId::new(id).unwrap()
}
fn revision_id(id: &str) -> RevisionId {
    RevisionId::new(id).unwrap()
}
fn job(id: &str) -> JobId {
    JobId::new(id).unwrap()
}
fn limits(pending: usize) -> RuntimeLimits {
    RuntimeLimits {
        max_pending: pending,
        ..RuntimeLimits::default()
    }
}
fn event(
    owner_id: &str,
    project_id: &str,
    source_id: &str,
    at: u64,
    outcome: WorkOutcome,
    content: &str,
) -> CaptureEvent {
    CaptureEvent {
        source: source(source_id),
        owner: owner(owner_id),
        actor: actor(),
        project: project(project_id),
        origin: "fixture://runtime".into(),
        recorded_at_ms: at,
        kind: WorkEventKind::Result,
        outcome,
        content: content.into(),
    }
}
async fn open(path: &str) -> (EmbeddedCaptureStore, EmbeddedRuntimeStore) {
    let capture = EmbeddedCaptureStore::open(path).await.unwrap();
    (capture.clone(), EmbeddedRuntimeStore::new(capture))
}
fn knowledge(
    event: &CaptureEvent,
    id: &str,
    memory_id: &str,
    predecessor: Option<&str>,
) -> Revision {
    Revision {
        id: revision_id(id),
        memory: memory(memory_id),
        owner: event.owner.clone(),
        project: event.project.clone(),
        kind: KnowledgeKind::Knowledge,
        status: RevisionStatus::PendingReview,
        content: event.content.clone(),
        prerequisites: vec![],
        sources: vec![event.source.clone()],
        predecessor: predecessor.map(revision_id),
        model: "offline-fixture".into(),
        prompt_version: "fixture-v1".into(),
    }
}
fn snapshot(id: &str, sources: Vec<CaptureEvent>, watermark: u64) -> JobSnapshot {
    JobSnapshot {
        id: job(id),
        owner: sources[0].owner.clone(),
        project: sources[0].project.clone(),
        watermark_ms: watermark,
        sources,
    }
}
fn promote(project_id: &str, revision: &str, expected: Option<&str>) -> ReviewRequest {
    ReviewRequest {
        project: project(project_id),
        revision: revision_id(revision),
        action: ReviewAction::Promote,
        expected_active: expected.map(revision_id),
        conflict_with: None,
    }
}
fn reject(project_id: &str, revision: &str) -> ReviewRequest {
    ReviewRequest {
        project: project(project_id),
        revision: revision_id(revision),
        action: ReviewAction::Reject,
        expected_active: None,
        conflict_with: None,
    }
}
fn conflict(project_id: &str, revision: &str, other: &str) -> ReviewRequest {
    ReviewRequest {
        project: project(project_id),
        revision: revision_id(revision),
        action: ReviewAction::Conflict,
        expected_active: None,
        conflict_with: Some(revision_id(other)),
    }
}
fn proposal(
    source_event: &CaptureEvent,
    revision: &str,
    memory_id: &str,
    predecessor: Option<&str>,
    content: &str,
) -> RevisionProposal {
    RevisionProposal {
        project: source_event.project.clone(),
        memory: memory(memory_id),
        revision: revision_id(revision),
        sources: vec![source_event.source.clone()],
        content: content.into(),
        predecessor: predecessor.map(revision_id),
    }
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

#[tokio::test]
async fn pending_successor_does_not_hide_active_predecessor() {
    let (_db, store) = open(&path()).await;
    let first = event(
        "owner",
        "project",
        "s1",
        10,
        WorkOutcome::Succeeded,
        "Active database fact.",
    );
    let later = event(
        "owner",
        "project",
        "s2",
        20,
        WorkOutcome::Succeeded,
        "Successor database draft.",
    );
    store.capture(first.clone(), &limits(8)).await.unwrap();
    store.capture(later.clone(), &limits(8)).await.unwrap();
    assert!(
        store
            .commit_batch(
                snapshot("job-1", vec![first.clone()], 50),
                vec![knowledge(&first, "r1", "memory", None)]
            )
            .await
            .unwrap()
    );
    store
        .review(
            &owner("owner"),
            &actor(),
            &promote("project", "r1", None),
            100,
        )
        .await
        .unwrap();
    store
        .propose(
            &owner("owner"),
            &actor(),
            proposal(
                &later,
                "r2",
                "memory",
                Some("r1"),
                "Successor database draft.",
            ),
            200,
        )
        .await
        .unwrap();
    let active = store
        .list(
            &owner("owner"),
            &project("project"),
            &[LifecycleState::Active],
            8,
        )
        .await
        .unwrap();
    let pending = store
        .list(
            &owner("owner"),
            &project("project"),
            &[LifecycleState::Processing],
            8,
        )
        .await
        .unwrap();
    assert_eq!(
        active
            .iter()
            .map(|item| item.revision.id.as_str())
            .collect::<Vec<_>>(),
        vec!["r1"]
    );
    assert_eq!(
        pending
            .iter()
            .map(|item| item.revision.id.as_str())
            .collect::<Vec<_>>(),
        vec!["r2"]
    );
    let hits = store
        .recall(
            &owner("owner"),
            &[project("project")],
            &["database".into()],
            8,
        )
        .await
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].item.revision.id.as_str(), "r1");
    assert_eq!(hits[0].item.state, LifecycleState::Active);
    assert_eq!(hits[0].item.sources, vec![first]);
}

#[tokio::test]
async fn promotion_rejects_stale_cas_and_forks() {
    let (_db, store) = open(&path()).await;
    let first = event(
        "owner",
        "project",
        "s1",
        10,
        WorkOutcome::Succeeded,
        "Root database fact.",
    );
    let mid = event(
        "owner",
        "project",
        "s2",
        20,
        WorkOutcome::Succeeded,
        "Linear database successor.",
    );
    let fork = event(
        "owner",
        "project",
        "s3",
        30,
        WorkOutcome::Succeeded,
        "Forked database claim.",
    );
    store.capture(first.clone(), &limits(8)).await.unwrap();
    store.capture(mid.clone(), &limits(8)).await.unwrap();
    store.capture(fork.clone(), &limits(8)).await.unwrap();
    store
        .commit_batch(
            snapshot("job-1", vec![first.clone()], 50),
            vec![knowledge(&first, "r1", "memory", None)],
        )
        .await
        .unwrap();
    store
        .review(
            &owner("owner"),
            &actor(),
            &promote("project", "r1", None),
            80,
        )
        .await
        .unwrap();
    store
        .propose(
            &owner("owner"),
            &actor(),
            proposal(
                &mid,
                "r2",
                "memory",
                Some("r1"),
                "Linear database successor.",
            ),
            90,
        )
        .await
        .unwrap();
    store
        .propose(
            &owner("owner"),
            &actor(),
            proposal(&fork, "r3", "memory", Some("r2"), "Forked database claim."),
            100,
        )
        .await
        .unwrap();
    assert_eq!(
        store
            .review(
                &owner("owner"),
                &actor(),
                &promote("project", "r3", Some("r1")),
                110
            )
            .await
            .unwrap_err(),
        RuntimeError::Conflict
    );
    assert_eq!(
        store
            .review(
                &owner("owner"),
                &actor(),
                &promote("project", "r2", None),
                120
            )
            .await
            .unwrap_err(),
        RuntimeError::Conflict
    );
    let promoted = store
        .review(
            &owner("owner"),
            &actor(),
            &promote("project", "r2", Some("r1")),
            130,
        )
        .await
        .unwrap();
    assert_eq!(promoted.state, LifecycleState::Active);
    assert_eq!(promoted.reviewed_at_ms, Some(130));
    let replay = store
        .review(
            &owner("owner"),
            &actor(),
            &promote("project", "r2", Some("r1")),
            140,
        )
        .await
        .unwrap();
    assert_eq!(replay.state, LifecycleState::Active);
    assert_eq!(replay.reviewed_at_ms, Some(130));
    let listed = store
        .list(
            &owner("owner"),
            &project("project"),
            &[
                LifecycleState::Active,
                LifecycleState::Superseded,
                LifecycleState::Processing,
            ],
            8,
        )
        .await
        .unwrap();
    assert_eq!(
        listed
            .iter()
            .filter(|item| item.state == LifecycleState::Active)
            .map(|item| item.revision.id.as_str())
            .collect::<Vec<_>>(),
        vec!["r2"]
    );
    assert_eq!(
        listed
            .iter()
            .filter(|item| item.state == LifecycleState::Superseded)
            .map(|item| item.revision.id.as_str())
            .collect::<Vec<_>>(),
        vec!["r1"]
    );
    assert_eq!(
        listed
            .iter()
            .filter(|item| item.state == LifecycleState::Processing)
            .map(|item| item.revision.id.as_str())
            .collect::<Vec<_>>(),
        vec!["r3"]
    );
    assert_eq!(
        store
            .recall(
                &owner("owner"),
                &[project("project")],
                &["database".into()],
                8
            )
            .await
            .unwrap()[0]
            .item
            .revision
            .id
            .as_str(),
        "r2"
    );
}

#[tokio::test]
async fn conflict_blocks_both_and_resolves_with_stale_guard() {
    let (_db, store) = open(&path()).await;
    let first = event(
        "owner",
        "project",
        "s1",
        10,
        WorkOutcome::Succeeded,
        "Left database claim.",
    );
    let second = event(
        "owner",
        "project",
        "s2",
        20,
        WorkOutcome::Succeeded,
        "Right database claim.",
    );
    store.capture(first.clone(), &limits(8)).await.unwrap();
    store.capture(second.clone(), &limits(8)).await.unwrap();
    store
        .commit_batch(
            snapshot("job-1", vec![first.clone()], 50),
            vec![knowledge(&first, "r1", "memory", None)],
        )
        .await
        .unwrap();
    store
        .propose(
            &owner("owner"),
            &actor(),
            proposal(&second, "r2", "memory", Some("r1"), "Right database claim."),
            60,
        )
        .await
        .unwrap();
    let blocked = store
        .review(
            &owner("owner"),
            &actor(),
            &conflict("project", "r1", "r2"),
            70,
        )
        .await
        .unwrap();
    assert_eq!(blocked.state, LifecycleState::Conflicted);
    let replay = store
        .review(
            &owner("owner"),
            &actor(),
            &conflict("project", "r1", "r2"),
            71,
        )
        .await
        .unwrap();
    assert_eq!(replay.state, LifecycleState::Conflicted);
    let conflicted = store
        .list(
            &owner("owner"),
            &project("project"),
            &[LifecycleState::Conflicted],
            8,
        )
        .await
        .unwrap();
    assert_eq!(
        conflicted
            .iter()
            .map(|item| item.revision.id.as_str())
            .collect::<Vec<_>>(),
        vec!["r1", "r2"]
    );
    assert!(
        store
            .recall(
                &owner("owner"),
                &[project("project")],
                &["database".into()],
                8
            )
            .await
            .unwrap()
            .is_empty()
    );
    let resolved = store
        .review(
            &owner("owner"),
            &actor(),
            &promote("project", "r2", None),
            80,
        )
        .await
        .unwrap();
    assert_eq!(resolved.state, LifecycleState::Active);
    let counterpart = store
        .list(
            &owner("owner"),
            &project("project"),
            &[LifecycleState::Rejected],
            8,
        )
        .await
        .unwrap();
    assert_eq!(
        counterpart
            .iter()
            .map(|item| item.revision.id.as_str())
            .collect::<Vec<_>>(),
        vec!["r1"]
    );
    assert_eq!(
        store
            .review(
                &owner("owner"),
                &actor(),
                &promote("project", "r2", None),
                90
            )
            .await
            .unwrap()
            .state,
        LifecycleState::Active
    );
    assert_eq!(
        store
            .review(
                &owner("owner"),
                &actor(),
                &promote("project", "r1", None),
                100
            )
            .await
            .unwrap_err(),
        RuntimeError::Conflict
    );
    assert!(matches!(
        store
            .review(&owner("owner"), &actor(), &reject("project", "r2"), 110)
            .await,
        Err(RuntimeError::InvalidInput(_))
    ));
    assert_eq!(
        store
            .recall(
                &owner("owner"),
                &[project("project")],
                &["database".into()],
                8
            )
            .await
            .unwrap()[0]
            .item
            .revision
            .id
            .as_str(),
        "r2"
    );
}

#[tokio::test]
async fn process_reopen_preserves_active_and_legacy_pending() {
    let path = path();
    let (db, store) = open(&path).await;
    let first = event(
        "owner",
        "project",
        "s1",
        10,
        WorkOutcome::Failed,
        "Durable failed database evidence.",
    );
    store.capture(first.clone(), &limits(8)).await.unwrap();
    store
        .commit_batch(
            snapshot("job-1", vec![first.clone()], 50),
            vec![knowledge(&first, "r1", "memory", None)],
        )
        .await
        .unwrap();
    store
        .review(
            &owner("owner"),
            &actor(),
            &promote("project", "r1", None),
            80,
        )
        .await
        .unwrap();
    let extra = event(
        "owner",
        "project",
        "legacy",
        15,
        WorkOutcome::Succeeded,
        "Legacy pending database note.",
    );
    store.capture(extra.clone(), &limits(8)).await.unwrap();
    let jobs = EmbeddedKnowledgeStore::new(db.clone());
    jobs.create_async(snapshot("legacy-job", vec![extra.clone()], 40))
        .await
        .unwrap();
    jobs.complete_async(
        &owner("owner"),
        &job("legacy-job"),
        vec![knowledge(&extra, "legacy", "legacy-memory", None)],
    )
    .await
    .unwrap();
    drop(store);
    drop(jobs);
    drop(db);
    let store = reopen(&path).await;
    let active = store
        .list(
            &owner("owner"),
            &project("project"),
            &[LifecycleState::Active],
            8,
        )
        .await
        .unwrap();
    assert_eq!(active[0].revision.id.as_str(), "r1");
    assert_eq!(active[0].state, LifecycleState::Active);
    assert_eq!(active[0].sources[0].outcome, WorkOutcome::Failed);
    assert_eq!(active[0].reviewed_at_ms, Some(80));
    let pending = store
        .list(
            &owner("owner"),
            &project("project"),
            &[LifecycleState::Processing],
            8,
        )
        .await
        .unwrap();
    assert_eq!(
        pending
            .iter()
            .map(|item| item.revision.id.as_str())
            .collect::<Vec<_>>(),
        vec!["legacy"]
    );
    assert_eq!(
        store
            .recall(
                &owner("owner"),
                &[project("project")],
                &["database".into()],
                8
            )
            .await
            .unwrap()[0]
            .item
            .revision
            .id
            .as_str(),
        "r1"
    );
}

#[tokio::test]
async fn pending_backlog_counts_unconsumed_and_rejects_changed_replay() {
    let (db, store) = open(&path()).await;
    let failed = event(
        "owner",
        "project",
        "s1",
        10,
        WorkOutcome::Failed,
        "Original failed database attempt.",
    );
    let next = event(
        "owner",
        "project",
        "s2",
        20,
        WorkOutcome::Succeeded,
        "Second database source.",
    );
    assert_eq!(
        store
            .capture(failed.clone(), &limits(1))
            .await
            .unwrap()
            .status,
        CaptureStatus::SavedPending
    );
    assert_eq!(
        store.capture(next.clone(), &limits(1)).await.unwrap_err(),
        RuntimeError::BacklogFull
    );
    assert_eq!(
        store
            .pending_sources(&owner("owner"), &project("project"), 8, false)
            .await
            .unwrap(),
        vec![failed.clone()]
    );
    let mut procedure = knowledge(&failed, "bad", "memory", None);
    procedure.kind = KnowledgeKind::Procedure;
    procedure.prerequisites = vec!["invented step".into()];
    assert!(matches!(
        store
            .commit_batch(
                snapshot("forged", vec![failed.clone()], 50),
                vec![procedure]
            )
            .await,
        Err(RuntimeError::InvalidInput(_))
    ));
    assert!(
        store
            .commit_batch(
                snapshot("job-1", vec![failed.clone()], 50),
                vec![knowledge(&failed, "r1", "memory", None)]
            )
            .await
            .unwrap()
    );
    assert!(
        !store
            .commit_batch(
                snapshot("job-1", vec![failed.clone()], 50),
                vec![knowledge(&failed, "r1", "memory", None)]
            )
            .await
            .unwrap()
    );
    assert_eq!(
        store
            .pending_sources(&owner("owner"), &project("project"), 8, false)
            .await
            .unwrap(),
        Vec::<CaptureEvent>::new()
    );
    assert_eq!(
        db.pending_async(&owner("owner"))
            .await
            .unwrap()
            .iter()
            .map(|item| item.event.source.as_str())
            .collect::<Vec<_>>(),
        vec!["s1"]
    );
    assert_eq!(
        db.save_pending_async(next.clone(), 1).await.unwrap(),
        CaptureWrite::Full
    );
    assert_eq!(
        store
            .capture(failed.clone(), &limits(1))
            .await
            .unwrap()
            .status,
        CaptureStatus::AlreadySavedPending
    );
    let mut later_clock = failed.clone();
    later_clock.recorded_at_ms = 99_000;
    assert_eq!(
        store.capture(later_clock, &limits(1)).await.unwrap().status,
        CaptureStatus::AlreadySavedPending
    );
    assert_eq!(
        store
            .pending_sources(&owner("owner"), &project("project"), 8, false)
            .await
            .unwrap(),
        Vec::<CaptureEvent>::new()
    );
    let mut changed = failed.clone();
    changed.content = "rewritten after consume".into();
    assert_eq!(
        store.capture(changed, &limits(1)).await.unwrap_err(),
        RuntimeError::Conflict
    );
    assert_eq!(
        store
            .capture(next.clone(), &limits(1))
            .await
            .unwrap()
            .status,
        CaptureStatus::SavedPending
    );
    assert_eq!(
        store
            .pending_sources(&owner("owner"), &project("project"), 8, false)
            .await
            .unwrap(),
        vec![next.clone()]
    );
    let other = snapshot("job-2", vec![failed.clone()], 60);
    assert_eq!(
        store
            .commit_batch(other, vec![knowledge(&failed, "r9", "other", None)])
            .await
            .unwrap_err(),
        RuntimeError::Conflict
    );
    let listed = store
        .list(
            &owner("owner"),
            &project("project"),
            &[LifecycleState::Processing],
            8,
        )
        .await
        .unwrap();
    assert_eq!(listed[0].sources[0], failed);
    assert_eq!(listed[0].sources[0].outcome, WorkOutcome::Failed);
}

#[tokio::test]
async fn dream_ids_accept_max_recorded_watermark_and_replay() {
    let (db, store) = open(&path()).await;
    let failed = event(
        "owner",
        "project",
        "src-a",
        40,
        WorkOutcome::Failed,
        "Interrupted dream failed claim.",
    );
    let later = event(
        "owner",
        "project",
        "src-b",
        55,
        WorkOutcome::Succeeded,
        "Interrupted dream later claim.",
    );
    store.capture(failed.clone(), &limits(8)).await.unwrap();
    store.capture(later.clone(), &limits(8)).await.unwrap();
    let watermark = failed.recorded_at_ms.max(later.recorded_at_ms);
    let batch = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    let key_a = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let key_b = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    let snap = JobSnapshot {
        id: job(&format!("dream-job:{batch}")),
        owner: failed.owner.clone(),
        project: failed.project.clone(),
        watermark_ms: watermark,
        sources: vec![failed.clone(), later.clone()],
    };
    let revs = vec![
        Revision {
            id: revision_id(&format!("dream-revision:{key_a}")),
            memory: memory(&format!("dream-memory:{key_a}")),
            owner: failed.owner.clone(),
            project: failed.project.clone(),
            kind: KnowledgeKind::Knowledge,
            status: RevisionStatus::PendingReview,
            content: failed.content.clone(),
            prerequisites: vec![],
            sources: vec![failed.source.clone()],
            predecessor: None,
            model: "local-extractive-v1".into(),
            prompt_version: "outcome-preserving-v1".into(),
        },
        Revision {
            id: revision_id(&format!("dream-revision:{key_b}")),
            memory: memory(&format!("dream-memory:{key_b}")),
            owner: later.owner.clone(),
            project: later.project.clone(),
            kind: KnowledgeKind::Knowledge,
            status: RevisionStatus::PendingReview,
            content: later.content.clone(),
            prerequisites: vec![],
            sources: vec![later.source.clone()],
            predecessor: None,
            model: "local-extractive-v1".into(),
            prompt_version: "outcome-preserving-v1".into(),
        },
    ];
    assert!(
        store
            .commit_batch(snap.clone(), revs.clone())
            .await
            .unwrap()
    );
    assert!(!store.commit_batch(snap, revs).await.unwrap());
    assert!(
        store
            .pending_sources(&owner("owner"), &project("project"), 8, false)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(db.pending_async(&owner("owner")).await.unwrap().len(), 2);
    let listed = store
        .list(
            &owner("owner"),
            &project("project"),
            &[LifecycleState::Processing],
            8,
        )
        .await
        .unwrap();
    assert_eq!(listed.len(), 2);
    let want = format!("dream-revision:{key_a}");
    let failed_hit = listed
        .iter()
        .find(|item| item.revision.id.as_str() == want)
        .unwrap();
    assert_eq!(failed_hit.sources[0].outcome, WorkOutcome::Failed);
    assert_eq!(failed_hit.sources[0].content, failed.content);
    assert_eq!(failed_hit.revision.content, failed.content);
    assert_eq!(
        failed_hit.revision.memory.as_str(),
        format!("dream-memory:{key_a}").as_str()
    );
}

#[tokio::test]
async fn runtime_capture_ignores_wall_time_and_keeps_original_timestamp() {
    let (db, store) = open(&path()).await;
    let first = event(
        "owner",
        "project",
        "s1",
        10,
        WorkOutcome::Failed,
        "Repeated MCP database claim.",
    );
    assert_eq!(
        store
            .capture(first.clone(), &limits(4))
            .await
            .unwrap()
            .status,
        CaptureStatus::SavedPending
    );
    let mut later = first.clone();
    later.recorded_at_ms = 88_000;
    assert_eq!(
        store
            .capture(later.clone(), &limits(4))
            .await
            .unwrap()
            .status,
        CaptureStatus::AlreadySavedPending
    );
    let pending = store
        .pending_sources(&owner("owner"), &project("project"), 8, false)
        .await
        .unwrap();
    assert_eq!(pending, vec![first.clone()]);
    assert_eq!(pending[0].recorded_at_ms, 10);
    let mut shifted = first.clone();
    shifted.recorded_at_ms = 88_000;
    assert_eq!(
        db.save_pending_async(shifted, 8).await.unwrap(),
        CaptureWrite::Conflict
    );
    assert_eq!(
        db.save_pending_async(first.clone(), 8).await.unwrap(),
        CaptureWrite::ExistingIdentical
    );
    assert!(
        store
            .commit_batch(
                snapshot("job-1", vec![first.clone()], 10),
                vec![knowledge(&first, "r1", "memory", None)]
            )
            .await
            .unwrap()
    );
    let mut stale_time = first.clone();
    stale_time.recorded_at_ms = 88_000;
    assert!(matches!(
        store
            .commit_batch(
                snapshot("job-stale", vec![stale_time], 88_000),
                vec![knowledge(&first, "r-stale", "stale", None)]
            )
            .await,
        Err(RuntimeError::InvalidInput(_)) | Err(RuntimeError::Missing)
    ));
}

#[tokio::test]
async fn commit_batch_replay_rejects_stale_snapshot_after_purge_recapture() {
    let (_db, store) = open(&path()).await;
    let original = event(
        "owner",
        "project",
        "s1",
        10,
        WorkOutcome::Failed,
        "Original failed database claim.",
    );
    store.capture(original.clone(), &limits(8)).await.unwrap();
    let snap = snapshot(
        "dream-job:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        vec![original.clone()],
        10,
    );
    let rev = knowledge(
        &original,
        "dream-revision:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        "dream-memory:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        None,
    );
    assert!(
        store
            .commit_batch(snap.clone(), vec![rev.clone()])
            .await
            .unwrap()
    );
    assert!(
        !store
            .commit_batch(snap.clone(), vec![rev.clone()])
            .await
            .unwrap()
    );
    store
        .purge(&owner("owner"), &project("project"), &[source("s1")])
        .await
        .unwrap();
    assert_eq!(
        store
            .commit_batch(snap.clone(), vec![rev.clone()])
            .await
            .unwrap_err(),
        RuntimeError::Missing
    );
    let mut recaptured = original.clone();
    recaptured.recorded_at_ms = 90_000;
    assert_eq!(
        store
            .capture(recaptured.clone(), &limits(8))
            .await
            .unwrap()
            .status,
        CaptureStatus::SavedPending
    );
    assert_eq!(
        store
            .pending_sources(&owner("owner"), &project("project"), 8, false)
            .await
            .unwrap()[0]
            .recorded_at_ms,
        90_000
    );
    assert!(matches!(
        store.commit_batch(snap, vec![rev.clone()]).await,
        Err(RuntimeError::InvalidInput(_)) | Err(RuntimeError::Missing)
    ));
    let fresh = snapshot(
        "dream-job:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
        vec![recaptured.clone()],
        90_000,
    );
    let fresh_rev = knowledge(
        &recaptured,
        "dream-revision:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd",
        "dream-memory:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd",
        None,
    );
    assert!(
        store
            .commit_batch(fresh.clone(), vec![fresh_rev.clone()])
            .await
            .unwrap()
    );
    assert!(!store.commit_batch(fresh, vec![fresh_rev]).await.unwrap());
    let listed = store
        .list(
            &owner("owner"),
            &project("project"),
            &[LifecycleState::Processing],
            8,
        )
        .await
        .unwrap();
    assert_eq!(listed[0].sources[0].content, original.content);
    assert_eq!(listed[0].sources[0].outcome, WorkOutcome::Failed);
    assert_eq!(listed[0].revision.content, original.content);
}

#[tokio::test]
async fn purge_removes_jobs_and_successors_and_isolates_owners() {
    let (db, store) = open(&path()).await;
    let alpha = event(
        "alpha",
        "project",
        "s1",
        10,
        WorkOutcome::Failed,
        "PURGE-TEXT-ALPHA-FAILED",
    );
    let successor_src = event(
        "alpha",
        "project",
        "s2",
        20,
        WorkOutcome::Succeeded,
        "alpha retained source",
    );
    let beta = event(
        "beta",
        "project",
        "s1",
        10,
        WorkOutcome::Failed,
        "PURGE-TEXT-BETA-FAILED",
    );
    store.capture(alpha.clone(), &limits(8)).await.unwrap();
    store
        .capture(successor_src.clone(), &limits(8))
        .await
        .unwrap();
    store.capture(beta.clone(), &limits(8)).await.unwrap();
    store
        .commit_batch(
            snapshot("alpha-job", vec![alpha.clone()], 50),
            vec![knowledge(&alpha, "a1", "alpha-memory", None)],
        )
        .await
        .unwrap();
    store
        .commit_batch(
            snapshot("beta-job", vec![beta.clone()], 50),
            vec![knowledge(&beta, "b1", "beta-memory", None)],
        )
        .await
        .unwrap();
    store
        .propose(
            &owner("alpha"),
            &actor(),
            proposal(
                &successor_src,
                "a2",
                "alpha-memory",
                Some("a1"),
                "successor over purged root",
            ),
            80,
        )
        .await
        .unwrap();
    store
        .review(
            &owner("beta"),
            &actor(),
            &promote("project", "b1", None),
            90,
        )
        .await
        .unwrap();
    let receipt = store
        .purge(&owner("alpha"), &project("project"), &[source("s1")])
        .await
        .unwrap();
    assert_eq!(receipt.sources, 1);
    assert!(receipt.jobs >= 2);
    assert!(receipt.revisions >= 2);
    assert!(
        store
            .list(
                &owner("alpha"),
                &project("project"),
                &[LifecycleState::Processing, LifecycleState::Active],
                8
            )
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        store
            .recall(
                &owner("alpha"),
                &[project("project")],
                &["purge".into(), "text".into(), "alpha".into()],
                8
            )
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        EmbeddedKnowledgeStore::new(db.clone())
            .get_async(&owner("alpha"), &job("alpha-job"))
            .await
            .unwrap_err(),
        ConsolidationError::Missing
    );
    assert_eq!(
        store
            .pending_sources(&owner("alpha"), &project("project"), 8, false)
            .await
            .unwrap(),
        vec![successor_src.clone()]
    );
    assert!(
        store
            .commit_batch(
                snapshot("reprocess", vec![successor_src.clone()], 120),
                vec![knowledge(&successor_src, "a3", "fresh", None)]
            )
            .await
            .unwrap()
    );
    let beta_hits = store
        .recall(
            &owner("beta"),
            &[project("project")],
            &["purge".into(), "beta".into()],
            8,
        )
        .await
        .unwrap();
    assert_eq!(
        beta_hits[0].item.sources[0].content,
        "PURGE-TEXT-BETA-FAILED"
    );
    let leftover = event(
        "alpha",
        "other",
        "s1",
        10,
        WorkOutcome::Succeeded,
        "other-project copy",
    );
    store.capture(leftover.clone(), &limits(8)).await.unwrap();
    assert_eq!(
        store
            .pending_sources(&owner("alpha"), &project("other"), 8, false)
            .await
            .unwrap(),
        vec![leftover]
    );
}

#[tokio::test]
async fn retain_and_concurrent_propose_purge_leave_no_source_text() {
    let (_db, store) = open(&path()).await;
    let old = event(
        "owner",
        "project",
        "old",
        10,
        WorkOutcome::Failed,
        "RETAIN-OLD-TEXT",
    );
    let young = event(
        "owner",
        "project",
        "young",
        80,
        WorkOutcome::Succeeded,
        "RETAIN-YOUNG-TEXT",
    );
    store.capture(old.clone(), &limits(8)).await.unwrap();
    store.capture(young.clone(), &limits(8)).await.unwrap();
    store
        .commit_batch(
            snapshot("old-job", vec![old.clone()], 40),
            vec![knowledge(&old, "old-rev", "old-memory", None)],
        )
        .await
        .unwrap();
    let receipt = store
        .retain(&owner("owner"), &project("project"), 50, 1)
        .await
        .unwrap();
    assert_eq!(receipt.sources, 1);
    assert!(receipt.jobs >= 1);
    assert!(
        store
            .recall(
                &owner("owner"),
                &[project("project")],
                &["retain".into(), "old".into()],
                8
            )
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        store
            .pending_sources(&owner("owner"), &project("project"), 8, false)
            .await
            .unwrap(),
        vec![young.clone()]
    );
    let doomed = event(
        "owner",
        "project",
        "race",
        90,
        WorkOutcome::Succeeded,
        "RACE-SOURCE-TEXT",
    );
    store.capture(doomed.clone(), &limits(8)).await.unwrap();
    let proposer = store.clone();
    let purger = store.clone();
    let proposal = proposal(&doomed, "race-rev", "race-memory", None, "RACE-SOURCE-TEXT");
    let race_owner = owner("owner");
    let race_actor = actor();
    let race_project = project("project");
    let race_sources = [source("race")];
    let (proposed, purged) = tokio::join!(
        proposer.propose(&race_owner, &race_actor, proposal, 120),
        purger.purge(&race_owner, &race_project, &race_sources)
    );
    assert_eq!(purged.unwrap().sources, 1);
    if let Err(error) = proposed {
        assert_eq!(error, RuntimeError::Missing);
    }
    assert!(
        store
            .pending_sources(&owner("owner"), &project("project"), 8, false)
            .await
            .unwrap()
            .iter()
            .all(|item| item.source != source("race"))
    );
    let leftovers = store
        .list(
            &owner("owner"),
            &project("project"),
            &[LifecycleState::Processing, LifecycleState::Active],
            8,
        )
        .await
        .unwrap();
    assert!(
        leftovers
            .iter()
            .all(|item| item.revision.id.as_str() != "race-rev")
    );
    assert!(
        store
            .recall(
                &owner("owner"),
                &[project("project")],
                &["race".into(), "source".into()],
                8
            )
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        store
            .pending_sources(&owner("owner"), &project("project"), 8, false)
            .await
            .unwrap()
            .into_iter()
            .map(|item| item.source)
            .collect::<Vec<_>>(),
        vec![young.source]
    );
}

#[tokio::test]
async fn rejecting_a_contender_does_not_lock_out_a_corrected_successor() {
    let (_db, store) = open(&path()).await;
    let first = event(
        "owner",
        "project",
        "root",
        10,
        WorkOutcome::Succeeded,
        "Original database claim.",
    );
    let second = event(
        "owner",
        "project",
        "draft",
        20,
        WorkOutcome::Succeeded,
        "Rejected database draft.",
    );
    let third = event(
        "owner",
        "project",
        "corrected",
        30,
        WorkOutcome::Succeeded,
        "Corrected database claim.",
    );
    for source in [&first, &second, &third] {
        store.capture(source.clone(), &limits(8)).await.unwrap();
    }
    store
        .propose(
            &owner("owner"),
            &actor(),
            proposal(&first, "r1", "memory", None, &first.content),
            50,
        )
        .await
        .unwrap();
    store
        .review(
            &owner("owner"),
            &actor(),
            &promote("project", "r1", None),
            60,
        )
        .await
        .unwrap();
    store
        .propose(
            &owner("owner"),
            &actor(),
            proposal(&second, "r2", "memory", Some("r1"), &second.content),
            70,
        )
        .await
        .unwrap();
    store
        .review(&owner("owner"), &actor(), &reject("project", "r2"), 80)
        .await
        .unwrap();
    store
        .propose(
            &owner("owner"),
            &actor(),
            proposal(&third, "r3", "memory", Some("r1"), &third.content),
            90,
        )
        .await
        .unwrap();
    store
        .review(
            &owner("owner"),
            &actor(),
            &promote("project", "r3", Some("r1")),
            100,
        )
        .await
        .unwrap();
    let hits = store
        .recall(
            &owner("owner"),
            &[project("project")],
            &["database".into()],
            8,
        )
        .await
        .unwrap();
    assert_eq!(
        hits.iter()
            .map(|hit| hit.item.revision.id.as_str())
            .collect::<Vec<_>>(),
        vec!["r3"]
    );
    assert_eq!(
        store
            .review(
                &owner("owner"),
                &actor(),
                &promote("project", "r3", Some("r2")),
                110
            )
            .await
            .unwrap_err(),
        RuntimeError::Conflict
    );
}

#[tokio::test]
async fn resolving_pending_siblings_supersedes_the_active_head_and_can_be_repeated() {
    let (_db, store) = open(&path()).await;
    for index in 1..=4 {
        let source = event(
            "owner",
            "project",
            &format!("s{index}"),
            index * 10,
            WorkOutcome::Succeeded,
            "Database claim.",
        );
        store.capture(source.clone(), &limits(8)).await.unwrap();
        store
            .propose(
                &owner("owner"),
                &actor(),
                proposal(
                    &source,
                    &format!("r{index}"),
                    "memory",
                    if index == 1 { None } else { Some("r1") },
                    &source.content,
                ),
                index * 100,
            )
            .await
            .unwrap();
        if index == 1 {
            store
                .review(
                    &owner("owner"),
                    &actor(),
                    &promote("project", "r1", None),
                    110,
                )
                .await
                .unwrap();
        }
    }
    store
        .review(
            &owner("owner"),
            &actor(),
            &conflict("project", "r2", "r3"),
            500,
        )
        .await
        .unwrap();
    assert_eq!(
        store
            .review(
                &owner("owner"),
                &actor(),
                &conflict("project", "r2", "r4"),
                510
            )
            .await
            .unwrap_err(),
        RuntimeError::Conflict
    );
    store
        .review(
            &owner("owner"),
            &actor(),
            &promote("project", "r2", Some("r1")),
            520,
        )
        .await
        .unwrap();
    let hits = store
        .recall(
            &owner("owner"),
            &[project("project")],
            &["database".into()],
            8,
        )
        .await
        .unwrap();
    assert_eq!(
        hits.iter()
            .map(|hit| hit.item.revision.id.as_str())
            .collect::<Vec<_>>(),
        vec!["r2"]
    );
    store
        .review(
            &owner("owner"),
            &actor(),
            &conflict("project", "r2", "r4"),
            530,
        )
        .await
        .unwrap();
    store
        .review(
            &owner("owner"),
            &actor(),
            &promote("project", "r2", None),
            540,
        )
        .await
        .unwrap();
    assert_eq!(
        store
            .list(
                &owner("owner"),
                &project("project"),
                &[LifecycleState::Active],
                8
            )
            .await
            .unwrap()
            .iter()
            .map(|item| item.revision.id.as_str())
            .collect::<Vec<_>>(),
        vec!["r2"]
    );
}

#[tokio::test]
async fn purge_closes_over_every_revision_in_transitively_removed_jobs() {
    let (_db, store) = open(&path()).await;
    let first = event(
        "owner",
        "project",
        "s1",
        10,
        WorkOutcome::Succeeded,
        "Private database original.",
    );
    let second = event(
        "owner",
        "project",
        "s2",
        20,
        WorkOutcome::Succeeded,
        "Later database original.",
    );
    let third = event(
        "owner",
        "project",
        "s3",
        30,
        WorkOutcome::Succeeded,
        "Unrelated database original.",
    );
    for source in [&first, &second, &third] {
        store.capture(source.clone(), &limits(8)).await.unwrap();
    }
    store
        .commit_batch(
            snapshot("root", vec![first.clone()], 50),
            vec![knowledge(&first, "r1", "memory", None)],
        )
        .await
        .unwrap();
    store
        .commit_batch(
            snapshot("mixed", vec![second.clone(), third.clone()], 60),
            vec![
                knowledge(&second, "r2", "memory", Some("r1")),
                knowledge(&third, "other", "other-memory", None),
            ],
        )
        .await
        .unwrap();
    let deleted = store
        .purge(&owner("owner"), &project("project"), &[first.source])
        .await
        .unwrap();
    assert_eq!(
        deleted,
        PurgeReceipt {
            sources: 1,
            revisions: 3,
            jobs: 2,
            timeline_entries: 0
        }
    );
    assert!(
        store
            .list(
                &owner("owner"),
                &project("project"),
                &[LifecycleState::Processing],
                8
            )
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        store
            .pending_sources(&owner("owner"), &project("project"), 8, false)
            .await
            .unwrap()
            .iter()
            .map(|event| event.source.as_str())
            .collect::<Vec<_>>(),
        vec!["s2", "s3"]
    );
}

#[tokio::test]
async fn correcting_a_reviewed_procedure_preserves_its_prerequisites() {
    let (_db, store) = open(&path()).await;
    let first = event(
        "owner",
        "project",
        "s1",
        10,
        WorkOutcome::Succeeded,
        "Database procedure.",
    );
    let second = event(
        "owner",
        "project",
        "s2",
        20,
        WorkOutcome::Succeeded,
        "Corrected database procedure.",
    );
    for event in [&first, &second] {
        store.capture(event.clone(), &limits(8)).await.unwrap();
    }
    let mut procedure = knowledge(&first, "r1", "memory", None);
    procedure.kind = KnowledgeKind::Procedure;
    procedure.prerequisites = vec!["Database connection available.".into()];
    store
        .commit_batch(
            snapshot("procedure", vec![first.clone()], 50),
            vec![procedure.clone()],
        )
        .await
        .unwrap();
    store
        .review(
            &owner("owner"),
            &actor(),
            &promote("project", "r1", None),
            60,
        )
        .await
        .unwrap();
    let corrected = store
        .propose(
            &owner("owner"),
            &actor(),
            proposal(&second, "r2", "memory", Some("r1"), &second.content),
            70,
        )
        .await
        .unwrap();
    assert_eq!(corrected.revision.kind, KnowledgeKind::Procedure);
    assert_eq!(corrected.revision.prerequisites, procedure.prerequisites);
    store
        .review(
            &owner("owner"),
            &actor(),
            &promote("project", "r2", Some("r1")),
            80,
        )
        .await
        .unwrap();
    let mut invalid_guard = reject("project", "r2");
    invalid_guard.expected_active = Some(revision_id("r1"));
    assert!(matches!(
        store
            .review(&owner("owner"), &actor(), &invalid_guard, 90)
            .await,
        Err(RuntimeError::InvalidInput(_))
    ));
}

#[tokio::test]
async fn pending_capacity_is_per_project_and_counts_only_unconsumed_sources() {
    let (_db, store) = open(&path()).await;
    let first = event(
        "owner",
        "one",
        "a1",
        10,
        WorkOutcome::Succeeded,
        "Project one fact.",
    );
    store.capture(first.clone(), &limits(1)).await.unwrap();
    // A full backlog in project one never blocks project two.
    store
        .capture(
            event(
                "owner",
                "two",
                "b1",
                10,
                WorkOutcome::Succeeded,
                "Project two fact.",
            ),
            &limits(1),
        )
        .await
        .unwrap();
    let second = event(
        "owner",
        "one",
        "a2",
        20,
        WorkOutcome::Succeeded,
        "Second project one fact.",
    );
    assert_eq!(
        store.capture(second.clone(), &limits(1)).await.unwrap_err(),
        RuntimeError::BacklogFull
    );
    store
        .commit_batch(
            snapshot("job-1", vec![first.clone()], 50),
            vec![knowledge(&first, "r1", "memory", None)],
        )
        .await
        .unwrap();
    assert_eq!(
        store.capture(second, &limits(1)).await.unwrap().status,
        CaptureStatus::SavedPending
    );
}

#[tokio::test]
async fn eligible_sources_apply_the_time_window_before_the_batch_limit() {
    let (_db, store) = open(&path()).await;
    for (id, at) in [
        ("expired-1", 10),
        ("expired-2", 20),
        ("fresh", 100),
        ("future", 1_000),
    ] {
        store
            .capture(
                event(
                    "owner",
                    "project",
                    id,
                    at,
                    WorkOutcome::Succeeded,
                    "Window fact.",
                ),
                &limits(8),
            )
            .await
            .unwrap();
    }
    let eligible = store
        .eligible_sources(
            &owner("owner"),
            &project("project"),
            2,
            false,
            Some(50),
            Some(500),
        )
        .await
        .unwrap();
    assert_eq!(
        eligible
            .iter()
            .map(|item| item.source.as_str())
            .collect::<Vec<_>>(),
        vec!["fresh"]
    );
    assert_eq!(
        store
            .pending_sources(&owner("owner"), &project("project"), 2, false)
            .await
            .unwrap()
            .len(),
        2
    );
}

// ---- Decision timeline ----
use lambda_memory::domain_models::timeline::*;

const T0: u64 = 1_700_000_000_000;
fn decision(project_id: &str, source_id: &str, at: u64, content: &str) -> CaptureEvent {
    CaptureEvent {
        kind: WorkEventKind::Decision,
        outcome: WorkOutcome::Unknown,
        origin: "fixture://decision".into(),
        ..event(
            "owner",
            project_id,
            source_id,
            at,
            WorkOutcome::Unknown,
            content,
        )
    }
}
fn entry_id(id: &str) -> TimelineEntryId {
    TimelineEntryId::new(id).unwrap()
}
fn tdraft(entry: &str, occurred: u64, sources: &[&str]) -> TimelineDraft {
    TimelineDraft {
        project: project("project"),
        entry: entry_id(entry),
        expected_version: None,
        title: format!("Decision {entry}"),
        summary: format!("Why {entry} was decided."),
        occurred_at_ms: occurred,
        status: TimelineStatus::Adopted,
        sources: sources.iter().map(|id| source(id)).collect(),
        revisions: vec![],
        memories: vec![],
        supersedes: None,
        tags: vec![],
    }
}
fn tquery(limit: usize) -> TimelineQuery {
    TimelineQuery {
        project: project("project"),
        limit,
        after: None,
        statuses: vec![],
        descending: false,
        entry: None,
    }
}
fn history(entry: &str) -> TimelineQuery {
    TimelineQuery {
        entry: Some(entry_id(entry)),
        ..tquery(32)
    }
}
fn entry_ids(page: &TimelinePage) -> Vec<&str> {
    page.entries.iter().map(|e| e.entry.as_str()).collect()
}
fn uncovered_ids(page: &TimelinePage) -> Vec<&str> {
    page.uncovered.iter().map(|u| u.source.as_str()).collect()
}
async fn write(
    store: &EmbeddedRuntimeStore,
    draft: TimelineDraft,
    now: u64,
) -> Result<TimelineReceipt, RuntimeError> {
    store
        .timeline_write(&owner("owner"), &actor(), draft, now)
        .await
}

#[tokio::test]
async fn timeline_versions_replays_and_conflicts() {
    let (_db, store) = open(&path()).await;
    for (id, at) in [("s1", T0), ("s2", T0 + 1)] {
        store
            .capture(
                decision("project", id, at, "Chose the embedded store."),
                &limits(8),
            )
            .await
            .unwrap();
    }
    let v1 = tdraft("e1", T0, &["s1"]);
    let created = write(&store, v1.clone(), T0 + 10).await.unwrap();
    assert_eq!(created.write, TimelineWrite::Recorded);
    assert_eq!(
        (created.entry.version, created.entry.recorded_at_ms),
        (1, T0 + 10)
    );
    assert_eq!(created.entry.title, "Decision e1");
    // Same content with no guard is a safe replay of version 1.
    let replay = write(&store, v1.clone(), T0 + 20).await.unwrap();
    assert_eq!(replay.write, TimelineWrite::AlreadyRecorded);
    assert_eq!(replay.entry, created.entry);
    // Expected version must equal the head to version an entry.
    let v2 = TimelineDraft {
        expected_version: Some(1),
        title: "Decision e1, revised".into(),
        status: TimelineStatus::Superseded,
        sources: vec![source("s1"), source("s2")],
        tags: vec!["storage".into()],
        ..v1.clone()
    };
    let edited = write(&store, v2.clone(), T0 + 30).await.unwrap();
    assert_eq!(edited.write, TimelineWrite::Recorded);
    assert_eq!(
        (edited.entry.version, edited.entry.recorded_at_ms),
        (2, T0 + 30)
    );
    assert_eq!(edited.entry.status, TimelineStatus::Superseded);
    // Replaying the edit (guard head-1) or without guard returns the head.
    for expected in [Some(1), None] {
        let again = write(
            &store,
            TimelineDraft {
                expected_version: expected,
                ..v2.clone()
            },
            T0 + 40,
        )
        .await
        .unwrap();
        assert_eq!(again.write, TimelineWrite::AlreadyRecorded);
        assert_eq!(again.entry, edited.entry);
    }
    // Stale guard with different content, future guard, unguarded different content.
    let changed = TimelineDraft {
        summary: "Another reason.".into(),
        ..v2.clone()
    };
    for expected in [Some(1), Some(3), None] {
        let error = write(
            &store,
            TimelineDraft {
                expected_version: expected,
                ..changed.clone()
            },
            T0 + 50,
        )
        .await;
        assert_eq!(error.unwrap_err(), RuntimeError::Conflict, "{expected:?}");
    }
    assert_eq!(
        write(&store, v1.clone(), T0 + 50).await.unwrap_err(),
        RuntimeError::Conflict
    );
    // A guard on a brand-new entry conflicts.
    let fresh = TimelineDraft {
        expected_version: Some(1),
        ..tdraft("e2", T0, &["s2"])
    };
    assert_eq!(
        write(&store, fresh, T0 + 50).await.unwrap_err(),
        RuntimeError::Conflict
    );
    // History lists every version ascending, with a version cursor.
    let page = store
        .timeline_page(&owner("owner"), &history("e1"))
        .await
        .unwrap();
    assert_eq!(
        page.entries.iter().map(|e| e.version).collect::<Vec<_>>(),
        vec![1, 2]
    );
    assert_eq!(page.entries[0], created.entry);
    assert!(!page.more && page.uncovered.is_empty() && !page.uncovered_more);
    let first = store
        .timeline_page(
            &owner("owner"),
            &TimelineQuery {
                limit: 1,
                ..history("e1")
            },
        )
        .await
        .unwrap();
    assert_eq!((first.entries.len(), first.more), (1, true));
    let next = store
        .timeline_page(
            &owner("owner"),
            &TimelineQuery {
                limit: 1,
                after: Some(TimelineKey::Version(1)),
                ..history("e1")
            },
        )
        .await
        .unwrap();
    assert_eq!((next.entries[0].version, next.more), (2, false));
    // The list view shows the head only.
    let list = store
        .timeline_page(&owner("owner"), &tquery(32))
        .await
        .unwrap();
    assert_eq!(list.entries, vec![edited.entry]);
}

#[tokio::test]
async fn timeline_rejects_missing_and_foreign_links() {
    let (_db, store) = open(&path()).await;
    let s1 = decision("project", "s1", T0, "Chose the embedded store.");
    store.capture(s1.clone(), &limits(8)).await.unwrap();
    store
        .capture(
            decision("other", "foreign", T0, "Other project decision."),
            &limits(8),
        )
        .await
        .unwrap();
    let other_owner = CaptureEvent {
        owner: owner("intruder"),
        ..decision("project", "theirs", T0, "Other owner.")
    };
    store.capture(other_owner, &limits(8)).await.unwrap();
    let proposed = store
        .propose(
            &owner("owner"),
            &actor(),
            proposal(&s1, "r1", "m1", None, "Use SurrealKV."),
            T0 + 5,
        )
        .await
        .unwrap();
    assert_eq!(proposed.watermark_ms, T0 + 5);
    let foreign_event = decision("other", "foreign", T0, "Other project decision.");
    store
        .propose(
            &owner("owner"),
            &actor(),
            proposal(&foreign_event, "r-other", "m-other", None, "Elsewhere."),
            T0 + 5,
        )
        .await
        .unwrap();
    write(
        &store,
        TimelineDraft {
            project: project("other"),
            ..tdraft("other-entry", T0, &["foreign"])
        },
        T0 + 6,
    )
    .await
    .unwrap();

    let base = tdraft("e1", T0, &["s1"]);
    let cases: Vec<(TimelineDraft, RuntimeError)> = vec![
        (tdraft("e1", T0, &["s1", "absent"]), RuntimeError::Missing),
        (tdraft("e1", T0, &["theirs"]), RuntimeError::Missing),
        (tdraft("e1", T0, &["foreign"]), RuntimeError::Unauthorized),
        (
            TimelineDraft {
                revisions: vec![revision_id("absent")],
                ..base.clone()
            },
            RuntimeError::Missing,
        ),
        (
            TimelineDraft {
                revisions: vec![revision_id("r-other")],
                ..base.clone()
            },
            RuntimeError::Missing,
        ),
        (
            TimelineDraft {
                memories: vec![memory("absent")],
                ..base.clone()
            },
            RuntimeError::Missing,
        ),
        (
            TimelineDraft {
                memories: vec![memory("m-other")],
                ..base.clone()
            },
            RuntimeError::Missing,
        ),
        (
            TimelineDraft {
                supersedes: Some(entry_id("absent")),
                ..base.clone()
            },
            RuntimeError::Missing,
        ),
        (
            TimelineDraft {
                supersedes: Some(entry_id("other-entry")),
                ..base.clone()
            },
            RuntimeError::Missing,
        ),
    ];
    for (draft, expected) in cases {
        assert_eq!(
            write(&store, draft.clone(), T0 + 10).await.unwrap_err(),
            expected,
            "{draft:?}"
        );
    }
    assert!(
        store
            .timeline_page(&owner("owner"), &tquery(32))
            .await
            .unwrap()
            .entries
            .is_empty()
    );
    write(&store, tdraft("older", T0, &["s1"]), T0 + 10)
        .await
        .unwrap();
    let linked = TimelineDraft {
        revisions: vec![revision_id("r1")],
        memories: vec![memory("m1")],
        supersedes: Some(entry_id("older")),
        ..base
    };
    let receipt = write(&store, linked, T0 + 10).await.unwrap();
    assert_eq!(receipt.write, TimelineWrite::Recorded);
    assert_eq!(receipt.entry.supersedes, Some(entry_id("older")));
    assert_eq!(receipt.entry.memories, vec![memory("m1")]);
    // Another owner cannot see this project's entries.
    let page = store
        .timeline_page(&owner("intruder"), &tquery(32))
        .await
        .unwrap();
    assert!(page.entries.is_empty());
}

#[tokio::test]
async fn timeline_orders_by_occurrence_with_entry_tiebreak_and_cursor() {
    let (_db, store) = open(&path()).await;
    store
        .capture(decision("project", "s1", T0, "Decision."), &limits(8))
        .await
        .unwrap();
    for (entry, at) in [
        ("a", T0 + 2000),
        ("b", T0 + 1000),
        ("c", T0 + 2000),
        ("d", T0 + 3000),
    ] {
        write(&store, tdraft(entry, at, &["s1"]), T0 + 5000)
            .await
            .unwrap();
    }
    let owner = owner("owner");
    let all = store.timeline_page(&owner, &tquery(32)).await.unwrap();
    assert_eq!(entry_ids(&all), vec!["b", "a", "c", "d"]);
    let first = store.timeline_page(&owner, &tquery(2)).await.unwrap();
    assert_eq!((entry_ids(&first), first.more), (vec!["b", "a"], true));
    let after = TimelineKey::Position {
        occurred_at_ms: T0 + 2000,
        entry: entry_id("a"),
    };
    let second = store
        .timeline_page(
            &owner,
            &TimelineQuery {
                after: Some(after),
                ..tquery(2)
            },
        )
        .await
        .unwrap();
    assert_eq!((entry_ids(&second), second.more), (vec!["c", "d"], false));
    let desc = store
        .timeline_page(
            &owner,
            &TimelineQuery {
                descending: true,
                ..tquery(2)
            },
        )
        .await
        .unwrap();
    assert_eq!((entry_ids(&desc), desc.more), (vec!["d", "c"], true));
    let after = TimelineKey::Position {
        occurred_at_ms: T0 + 2000,
        entry: entry_id("c"),
    };
    let rest = store
        .timeline_page(
            &owner,
            &TimelineQuery {
                descending: true,
                after: Some(after),
                ..tquery(2)
            },
        )
        .await
        .unwrap();
    assert_eq!((entry_ids(&rest), rest.more), (vec!["a", "b"], false));
    // Status filter applies to the head version.
    let retract = TimelineDraft {
        expected_version: Some(1),
        status: TimelineStatus::Retracted,
        ..tdraft("d", T0 + 3000, &["s1"])
    };
    write(&store, retract, T0 + 6000).await.unwrap();
    let retracted = store
        .timeline_page(
            &owner,
            &TimelineQuery {
                statuses: vec![TimelineStatus::Retracted],
                ..tquery(32)
            },
        )
        .await
        .unwrap();
    assert_eq!(entry_ids(&retracted), vec!["d"]);
    assert_eq!(retracted.entries[0].version, 2);
    let adopted = store
        .timeline_page(
            &owner,
            &TimelineQuery {
                statuses: vec![TimelineStatus::Adopted, TimelineStatus::Proposed],
                ..tquery(32)
            },
        )
        .await
        .unwrap();
    assert_eq!(entry_ids(&adopted), vec!["b", "a", "c"]);
    // Bounds and mode mixing are invalid input.
    for bad in [
        tquery(0),
        tquery(33),
        TimelineQuery {
            entry: Some(entry_id("a")),
            descending: true,
            ..tquery(8)
        },
        TimelineQuery {
            entry: Some(entry_id("a")),
            statuses: vec![TimelineStatus::Adopted],
            ..tquery(8)
        },
        TimelineQuery {
            after: Some(TimelineKey::Version(1)),
            ..tquery(8)
        },
        TimelineQuery {
            after: Some(TimelineKey::Position {
                occurred_at_ms: 1,
                entry: entry_id("a"),
            }),
            ..history("a")
        },
    ] {
        assert!(
            matches!(
                store.timeline_page(&owner, &bad).await,
                Err(RuntimeError::InvalidInput(_))
            ),
            "{bad:?}"
        );
    }
}

#[tokio::test]
async fn uncovered_decisions_follow_the_latest_version_and_first_page_only() {
    let (_db, store) = open(&path()).await;
    let long = format!("{}é tail", "x".repeat(TIMELINE_PREVIEW_BYTES - 1));
    for (id, at, content) in [
        ("dA", T0 + 10, "Decision A."),
        ("dB", T0 + 20, "Decision B."),
        ("dC", T0 + 30, long.as_str()),
    ] {
        store
            .capture(decision("project", id, at, content), &limits(64))
            .await
            .unwrap();
    }
    store
        .capture(
            event(
                "owner",
                "project",
                "result",
                T0 + 40,
                WorkOutcome::Succeeded,
                "Not a decision.",
            ),
            &limits(64),
        )
        .await
        .unwrap();
    store
        .capture(
            decision("other", "elsewhere", T0 + 50, "Other project."),
            &limits(64),
        )
        .await
        .unwrap();
    let owner = owner("owner");
    let empty = store.timeline_page(&owner, &tquery(32)).await.unwrap();
    assert_eq!(uncovered_ids(&empty), vec!["dC", "dB", "dA"]);
    let newest = &empty.uncovered[0];
    assert_eq!(
        (
            newest.origin.as_str(),
            newest.recorded_at_ms,
            &newest.outcome
        ),
        ("fixture://decision", T0 + 30, &WorkOutcome::Unknown)
    );
    assert!(newest.preview_truncated);
    assert_eq!(newest.preview, "x".repeat(TIMELINE_PREVIEW_BYTES - 1));
    assert_eq!(
        (
            empty.uncovered[1].preview.as_str(),
            empty.uncovered[1].preview_truncated
        ),
        ("Decision B.", false)
    );
    write(&store, tdraft("e1", T0, &["dB"]), T0 + 100)
        .await
        .unwrap();
    assert_eq!(
        uncovered_ids(&store.timeline_page(&owner, &tquery(32)).await.unwrap()),
        vec!["dC", "dA"]
    );
    // Coverage is decided by the latest version; a retracted entry still covers.
    write(
        &store,
        TimelineDraft {
            expected_version: Some(1),
            ..tdraft("e1", T0, &["dA"])
        },
        T0 + 110,
    )
    .await
    .unwrap();
    assert_eq!(
        uncovered_ids(&store.timeline_page(&owner, &tquery(32)).await.unwrap()),
        vec!["dC", "dB"]
    );
    write(
        &store,
        TimelineDraft {
            expected_version: Some(2),
            status: TimelineStatus::Retracted,
            ..tdraft("e1", T0, &["dA"])
        },
        T0 + 120,
    )
    .await
    .unwrap();
    let page = store.timeline_page(&owner, &tquery(32)).await.unwrap();
    assert_eq!(
        (uncovered_ids(&page), page.uncovered_more),
        (vec!["dC", "dB"], false)
    );
    // Not on a continuation page or in history mode.
    let after = TimelineKey::Position {
        occurred_at_ms: 0,
        entry: entry_id("0"),
    };
    assert!(
        store
            .timeline_page(
                &owner,
                &TimelineQuery {
                    after: Some(after),
                    ..tquery(32)
                }
            )
            .await
            .unwrap()
            .uncovered
            .is_empty()
    );
    assert!(
        store
            .timeline_page(&owner, &history("e1"))
            .await
            .unwrap()
            .uncovered
            .is_empty()
    );
    // Bounded at 16, newest first, with an overflow flag.
    for i in 0..15 {
        store
            .capture(
                decision("project", &format!("n{i:02}"), T0 + 200 + i, "More."),
                &limits(64),
            )
            .await
            .unwrap();
    }
    let page = store.timeline_page(&owner, &tquery(32)).await.unwrap();
    assert_eq!(page.uncovered.len(), TIMELINE_UNCOVERED_LIMIT);
    assert!(page.uncovered_more);
    assert_eq!(page.uncovered[0].source.as_str(), "n14");
    assert_eq!(page.uncovered[15].source.as_str(), "dC");
}

#[tokio::test]
async fn purge_cascades_timeline_entries_citing_purged_sources_or_revisions() {
    let (_db, store) = open(&path()).await;
    let s2 = decision("project", "s2", T0 + 2, "Revision source.");
    for event in [
        decision("project", "s1", T0 + 1, "Purged directly."),
        s2.clone(),
        decision("project", "s3", T0 + 3, "Kept."),
    ] {
        store.capture(event, &limits(8)).await.unwrap();
    }
    store
        .propose(
            &owner("owner"),
            &actor(),
            proposal(&s2, "r1", "m1", None, "Derived."),
            T0 + 5,
        )
        .await
        .unwrap();
    // A: v1 cites s1, v2 no longer does. Any version citing a purged source removes the entry.
    write(&store, tdraft("a", T0, &["s1"]), T0 + 10)
        .await
        .unwrap();
    write(
        &store,
        TimelineDraft {
            expected_version: Some(1),
            ..tdraft("a", T0, &["s3"])
        },
        T0 + 11,
    )
    .await
    .unwrap();
    write(
        &store,
        TimelineDraft {
            revisions: vec![revision_id("r1")],
            ..tdraft("b", T0, &["s3"])
        },
        T0 + 12,
    )
    .await
    .unwrap();
    write(
        &store,
        TimelineDraft {
            supersedes: Some(entry_id("a")),
            memories: vec![memory("m1")],
            ..tdraft("c", T0, &["s3"])
        },
        T0 + 13,
    )
    .await
    .unwrap();
    let owner = owner("owner");
    let receipt = store
        .purge(&owner, &project("project"), &[source("s1")])
        .await
        .unwrap();
    assert_eq!(
        receipt,
        PurgeReceipt {
            sources: 1,
            revisions: 0,
            jobs: 0,
            timeline_entries: 1
        }
    );
    assert_eq!(
        entry_ids(&store.timeline_page(&owner, &tquery(32)).await.unwrap()),
        vec!["b", "c"]
    );
    assert!(
        store
            .timeline_page(&owner, &history("a"))
            .await
            .unwrap()
            .entries
            .is_empty()
    );
    // Purging s2 cascades r1, which removes entry b. c keeps dangling supersedes/memories links.
    let receipt = store
        .purge(&owner, &project("project"), &[source("s2")])
        .await
        .unwrap();
    assert_eq!(receipt.timeline_entries, 1);
    assert_eq!(receipt.revisions, 1);
    let page = store.timeline_page(&owner, &tquery(32)).await.unwrap();
    assert_eq!(entry_ids(&page), vec!["c"]);
    assert_eq!(page.entries[0].supersedes, Some(entry_id("a")));
    assert_eq!(page.entries[0].memories, vec![memory("m1")]);
    // A new version that resends the head's dangling supersedes/memories still writes.
    let c = TimelineDraft {
        supersedes: Some(entry_id("a")),
        memories: vec![memory("m1")],
        ..tdraft("c", T0, &["s3"])
    };
    let retract = TimelineDraft {
        expected_version: Some(1),
        status: TimelineStatus::Retracted,
        ..c.clone()
    };
    let retracted = write(&store, retract, T0 + 14).await.unwrap();
    assert_eq!(
        (retracted.write, retracted.entry.version),
        (TimelineWrite::Recorded, 2)
    );
    assert_eq!(retracted.entry.supersedes, Some(entry_id("a")));
    // Links that are new versus the head are still checked.
    for draft in [
        TimelineDraft {
            memories: vec![memory("m1"), memory("absent")],
            ..c.clone()
        },
        TimelineDraft {
            supersedes: Some(entry_id("absent")),
            ..c.clone()
        },
    ] {
        let draft = TimelineDraft {
            expected_version: Some(2),
            ..draft
        };
        assert_eq!(
            write(&store, draft, T0 + 15).await.unwrap_err(),
            RuntimeError::Missing
        );
    }
    // The purged entry ID can be recreated from scratch.
    let again = write(&store, tdraft("a", T0, &["s3"]), T0 + 20)
        .await
        .unwrap();
    assert_eq!(
        (again.write, again.entry.version),
        (TimelineWrite::Recorded, 1)
    );
}

#[tokio::test]
async fn retention_cascades_timeline_entries_and_migration_reopens() {
    let path = path();
    {
        let (_db, store) = open(&path).await;
        store
            .capture(decision("project", "old", T0, "Old decision."), &limits(8))
            .await
            .unwrap();
        store
            .capture(
                decision("project", "new", T0 + 1000, "New decision."),
                &limits(8),
            )
            .await
            .unwrap();
        write(&store, tdraft("old-entry", T0, &["old"]), T0 + 2000)
            .await
            .unwrap();
        write(&store, tdraft("new-entry", T0, &["new"]), T0 + 2000)
            .await
            .unwrap();
        let receipt = store
            .retain(&owner("owner"), &project("project"), T0 + 500, 8)
            .await
            .unwrap();
        assert_eq!((receipt.sources, receipt.timeline_entries), (1, 1));
        assert_eq!(
            entry_ids(
                &store
                    .timeline_page(&owner("owner"), &tquery(32))
                    .await
                    .unwrap()
            ),
            vec!["new-entry"]
        );
    }
    // Migration 10 is fingerprinted and idempotent across reopen; rows persist.
    let store = reopen(&path).await;
    let page = store
        .timeline_page(&owner("owner"), &tquery(32))
        .await
        .unwrap();
    assert_eq!(entry_ids(&page), vec!["new-entry"]);
}

fn every_state() -> Vec<LifecycleState> {
    vec![
        LifecycleState::Processing,
        LifecycleState::Active,
        LifecycleState::Rejected,
        LifecycleState::Conflicted,
        LifecycleState::Superseded,
    ]
}
fn list_query(
    limit: usize,
    memory_id: Option<&str>,
    after: Option<(u64, RevisionId)>,
) -> ListQuery {
    ListQuery {
        project: project("project"),
        states: every_state(),
        limit,
        memory: memory_id.map(memory),
        after,
    }
}

#[tokio::test]
async fn list_page_cursor_visits_every_revision_once() {
    let (_db, store) = open(&path()).await;
    let mut expected = Vec::new();
    for batch in 0..10u64 {
        let sources: Vec<_> = (0..4)
            .map(|i| {
                event(
                    "owner",
                    "project",
                    &format!("s{batch}-{i}"),
                    batch * 10 + 1,
                    WorkOutcome::Succeeded,
                    "Database claim.",
                )
            })
            .collect();
        for source in &sources {
            store.capture(source.clone(), &limits(64)).await.unwrap();
        }
        // Revision ids within a batch share one watermark, so ties break on revision id.
        let revisions: Vec<_> = sources
            .iter()
            .enumerate()
            .map(|(i, s)| {
                knowledge(
                    s,
                    &format!("r{batch:02}-{}", 3 - i),
                    &format!("m{batch}-{i}"),
                    None,
                )
            })
            .collect();
        for revision in &revisions {
            expected.push((batch * 100 + 50, revision.id.as_str().to_owned()));
        }
        assert!(
            store
                .commit_batch(
                    snapshot(&format!("job-{batch}"), sources, batch * 100 + 50),
                    revisions
                )
                .await
                .unwrap()
        );
    }
    expected.sort();
    let mut seen = Vec::new();
    let mut after = None;
    loop {
        let page = store
            .list_page(&owner("owner"), &list_query(7, None, after.clone()), false)
            .await
            .unwrap();
        assert!(page.items.len() <= 7);
        assert!(page.audit.is_empty());
        for item in &page.items {
            seen.push((item.watermark_ms, item.revision.id.as_str().to_owned()));
        }
        let last = page.items.last().unwrap();
        after = Some((last.watermark_ms, last.revision.id.clone()));
        if !page.more {
            break;
        }
    }
    assert_eq!(seen.len(), 40);
    assert_eq!(seen, expected);
    let full = store
        .list_page(&owner("owner"), &list_query(32, None, None), false)
        .await
        .unwrap();
    assert_eq!(full.items.len(), 32);
    assert!(full.more);
    assert!(
        store
            .list_page(&owner("owner"), &list_query(33, None, None), false)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn memory_filter_returns_lineage_with_audit_in_order() {
    let (_db, store) = open(&path()).await;
    for (index, (rev, mem, pred)) in [
        ("r1", "memory", None),
        ("r2", "memory", Some("r1")),
        ("r3", "memory", Some("r1")),
        ("r9", "other", None),
    ]
    .into_iter()
    .enumerate()
    {
        let at = (index as u64 + 1) * 10;
        let source = event(
            "owner",
            "project",
            &format!("s{rev}"),
            at,
            WorkOutcome::Succeeded,
            "Database claim.",
        );
        store.capture(source.clone(), &limits(8)).await.unwrap();
        store
            .propose(
                &owner("owner"),
                &actor(),
                proposal(&source, rev, mem, pred, &source.content),
                at * 10,
            )
            .await
            .unwrap();
    }
    let ok = |request: ReviewRequest, at: u64| {
        let store = &store;
        async move {
            store
                .review(&owner("owner"), &actor(), &request, at)
                .await
                .unwrap()
        }
    };
    ok(promote("project", "r1", None), 110).await;
    ok(conflict("project", "r2", "r3"), 500).await;
    ok(promote("project", "r9", None), 510).await;
    ok(promote("project", "r2", Some("r1")), 520).await;
    let page = store
        .list_page(&owner("owner"), &list_query(32, Some("memory"), None), true)
        .await
        .unwrap();
    assert!(!page.more);
    assert_eq!(
        page.items
            .iter()
            .map(|i| i.revision.id.as_str())
            .collect::<Vec<_>>(),
        vec!["r1", "r2", "r3"]
    );
    let audit: Vec<_> = page
        .audit
        .iter()
        .map(|a| {
            (
                a.revision.as_str(),
                a.action,
                a.from,
                a.to,
                a.expected_active.as_ref().map(RevisionId::as_str),
                a.conflict_with.as_ref().map(RevisionId::as_str),
                a.at_ms,
            )
        })
        .collect();
    assert_eq!(
        audit[0],
        (
            "r1",
            ReviewAction::Promote,
            LifecycleState::Processing,
            LifecycleState::Active,
            None,
            None,
            110
        )
    );
    assert_eq!(audit[1].0, "r2");
    assert_eq!(audit[1].1, ReviewAction::Conflict);
    assert_eq!(audit[1].3, LifecycleState::Conflicted);
    assert_eq!(audit[1].5, Some("r3"));
    assert_eq!(audit[1].6, 500);
    assert_eq!(audit.last().unwrap().0, "r2");
    assert_eq!(audit.last().unwrap().1, ReviewAction::Promote);
    assert_eq!(audit.last().unwrap().4, Some("r1"));
    assert_eq!(audit.last().unwrap().6, 520);
    assert!(audit.windows(2).all(|w| w[0].6 <= w[1].6));
    assert!(audit.iter().all(|a| a.0 != "r9"));
    // Without the audit flag, lineage pages carry no audit rows; paging stays within the memory.
    let first = store
        .list_page(&owner("owner"), &list_query(2, Some("memory"), None), false)
        .await
        .unwrap();
    assert!(first.more && first.audit.is_empty());
    let last = first.items.last().unwrap();
    let rest = store
        .list_page(
            &owner("owner"),
            &list_query(
                2,
                Some("memory"),
                Some((last.watermark_ms, last.revision.id.clone())),
            ),
            false,
        )
        .await
        .unwrap();
    assert_eq!(
        rest.items
            .iter()
            .map(|i| i.revision.id.as_str())
            .collect::<Vec<_>>(),
        vec!["r3"]
    );
    assert!(!rest.more);
}

#[tokio::test]
async fn action_and_legacy_tool_sources_skip_backlog_and_derivation() {
    let (_db, store) = open(&path()).await;
    let owner_id = owner("owner");
    let project_id = project("project");
    for id in ["act1", "act2"] {
        let mut action = event(
            "owner",
            "project",
            id,
            10,
            WorkOutcome::Succeeded,
            "{\"v\":1}",
        );
        action.kind = WorkEventKind::Action;
        action.origin = "omp://action".into();
        store.capture(action, &limits(2)).await.unwrap();
    }
    let mut legacy = event(
        "owner",
        "project",
        "legacy",
        11,
        WorkOutcome::Succeeded,
        "Tool bash reported outcome succeeded",
    );
    legacy.origin = "omp://tool_result".into();
    store.capture(legacy, &limits(2)).await.unwrap();
    for (id, at) in [("r1", 20), ("r2", 21)] {
        store
            .capture(
                event(
                    "owner",
                    "project",
                    id,
                    at,
                    WorkOutcome::Succeeded,
                    "Real fact.",
                ),
                &limits(2),
            )
            .await
            .unwrap();
    }
    assert_eq!(
        store
            .capture(
                event(
                    "owner",
                    "project",
                    "r3",
                    22,
                    WorkOutcome::Succeeded,
                    "Third fact."
                ),
                &limits(2)
            )
            .await
            .unwrap_err(),
        RuntimeError::BacklogFull
    );
    // A full backlog never rejects evidence that does not count toward it.
    let mut action = event(
        "owner",
        "project",
        "act3",
        23,
        WorkOutcome::Succeeded,
        "{\"v\":1}",
    );
    action.kind = WorkEventKind::Action;
    action.origin = "omp://action".into();
    assert_eq!(
        store.capture(action, &limits(2)).await.unwrap().status,
        CaptureStatus::SavedPending
    );
    let mut legacy = event(
        "owner",
        "project",
        "legacy2",
        24,
        WorkOutcome::Succeeded,
        "Tool bash reported outcome succeeded",
    );
    legacy.origin = "omp://tool_result".into();
    store.capture(legacy, &limits(2)).await.unwrap();
    let eligible = store
        .eligible_sources(&owner_id, &project_id, 32, false, None, None)
        .await
        .unwrap();
    let ids: Vec<_> = eligible
        .iter()
        .map(|e| e.source.as_str().to_string())
        .collect();
    assert_eq!(ids, vec!["r1", "r2"]);
    let stored = store
        .pending_sources(&owner_id, &project_id, 32, false)
        .await
        .unwrap();
    assert_eq!(stored.len(), 2);
}

#[tokio::test]
async fn backlog_counts_only_unconsumed_eligible_sources_of_the_project() {
    let (_db, store) = open(&path()).await;
    let mut sources = Vec::new();
    for (i, id) in ["s1", "s2", "s3", "s4", "s5"].into_iter().enumerate() {
        sources.push(event(
            "owner",
            "project",
            id,
            10 + i as u64,
            WorkOutcome::Succeeded,
            "Real fact.",
        ));
    }
    let mut decision = event(
        "owner",
        "project",
        "s6",
        16,
        WorkOutcome::Succeeded,
        "A decision.",
    );
    decision.kind = WorkEventKind::Decision;
    sources.push(decision);
    let mut legacy = event(
        "owner",
        "project",
        "tool1",
        17,
        WorkOutcome::Succeeded,
        "Tool bash reported outcome succeeded",
    );
    legacy.origin = "omp://tool_result".into();
    sources.push(legacy);
    let mut action = event(
        "owner",
        "project",
        "act1",
        18,
        WorkOutcome::Succeeded,
        "{\"v\":1}",
    );
    action.kind = WorkEventKind::Action;
    action.origin = "omp://action".into();
    sources.push(action);
    sources.push(event(
        "owner",
        "other",
        "o1",
        19,
        WorkOutcome::Succeeded,
        "Other project fact.",
    ));
    for source in &sources {
        store.capture(source.clone(), &limits(100)).await.unwrap();
    }
    // s1 and s2 are derived; so is the legacy tool source, as in databases that predate the filter.
    for (id, ids) in [("job-1", vec!["s1", "s2"]), ("job-legacy", vec!["tool1"])] {
        let batch: Vec<_> = sources
            .iter()
            .filter(|s| ids.contains(&s.source.as_str()))
            .cloned()
            .collect();
        let first = batch[0].clone();
        assert!(
            store
                .commit_batch(
                    snapshot(id, batch, 100),
                    vec![knowledge(
                        &first,
                        &format!("rev-{id}"),
                        &format!("mem-{id}"),
                        None
                    )]
                )
                .await
                .unwrap()
        );
    }
    // Unconsumed and eligible: s3, s4, s5, s6 (4). The consumed, legacy, action and other-project rows do not count.
    let next = |id: &str, at: u64| {
        event(
            "owner",
            "project",
            id,
            at,
            WorkOutcome::Succeeded,
            "Next fact.",
        )
    };
    assert_eq!(
        store.capture(next("n1", 30), &limits(4)).await.unwrap_err(),
        RuntimeError::BacklogFull
    );
    assert_eq!(
        store.capture(next("n1", 30), &limits(3)).await.unwrap_err(),
        RuntimeError::BacklogFull
    );
    assert_eq!(
        store
            .capture(next("n1", 30), &limits(5))
            .await
            .unwrap()
            .status,
        CaptureStatus::SavedPending
    );
    assert_eq!(
        store.capture(next("n2", 31), &limits(5)).await.unwrap_err(),
        RuntimeError::BacklogFull
    );
    assert_eq!(
        store
            .capture(next("n2", 31), &limits(6))
            .await
            .unwrap()
            .status,
        CaptureStatus::SavedPending
    );
    // Retrying a stored source is not a new backlog entry.
    assert_eq!(
        store
            .capture(next("n2", 31), &limits(1))
            .await
            .unwrap()
            .status,
        CaptureStatus::AlreadySavedPending
    );
    // The other project only has its own single unconsumed source.
    let other = |id: &str, at: u64| {
        event(
            "owner",
            "other",
            id,
            at,
            WorkOutcome::Succeeded,
            "Other fact.",
        )
    };
    assert_eq!(
        store
            .capture(other("o2", 40), &limits(1))
            .await
            .unwrap_err(),
        RuntimeError::BacklogFull
    );
    assert_eq!(
        store
            .capture(other("o2", 40), &limits(2))
            .await
            .unwrap()
            .status,
        CaptureStatus::SavedPending
    );
}

#[tokio::test]
async fn active_playbook_reads_stay_consistent_during_a_capture_burst() {
    let (_db, store) = open(&path()).await;
    let first = event(
        "owner",
        "project",
        "s1",
        10,
        WorkOutcome::Succeeded,
        "Playbook evidence.",
    );
    store.capture(first.clone(), &limits(8)).await.unwrap();
    let mut playbook = knowledge(
        &first,
        "pb1",
        lambda_memory::domain_models::procedural::PLAYBOOK_MEMORY,
        None,
    );
    playbook.content = "Playbook text.".into();
    assert!(
        store
            .commit_batch(snapshot("job-pb", vec![first], 50), vec![playbook])
            .await
            .unwrap()
    );
    let none = store
        .active_playbook(&owner("owner"), &project("project"))
        .await
        .unwrap();
    assert!(none.is_none(), "an unreviewed playbook is not active");
    store
        .review(
            &owner("owner"),
            &actor(),
            &promote("project", "pb1", None),
            100,
        )
        .await
        .unwrap();
    let burst = async {
        for n in 0..40 {
            let mut action = event(
                "owner",
                "project",
                &format!("act{n}"),
                200 + n,
                WorkOutcome::Succeeded,
                "{\"v\":1}",
            );
            action.kind = WorkEventKind::Action;
            action.origin = "omp://action".into();
            store.capture(action, &limits(8)).await.unwrap();
        }
    };
    let reads = async {
        for _ in 0..40 {
            let active = store
                .active_playbook(&owner("owner"), &project("project"))
                .await
                .unwrap()
                .unwrap();
            assert_eq!(active.revision.id.as_str(), "pb1");
            assert_eq!(active.revision.content, "Playbook text.");
            assert_eq!(active.state, LifecycleState::Active);
        }
    };
    tokio::join!(burst, reads);
    assert_eq!(
        store
            .action_episode_count(&owner("owner"), &project("project"))
            .await
            .unwrap(),
        40
    );
}

#[tokio::test]
async fn action_episodes_newest_first_and_bounded() {
    let (_db, store) = open(&path()).await;
    let owner_id = owner("owner");
    let project_id = project("project");
    for (id, at) in [("act1", 10), ("act2", 30), ("act3", 20)] {
        let mut action = event(
            "owner",
            "project",
            id,
            at,
            WorkOutcome::Succeeded,
            "{\"v\":1}",
        );
        action.kind = WorkEventKind::Action;
        action.origin = "omp://action".into();
        store.capture(action, &limits(8)).await.unwrap();
    }
    for (id, at) in [("r1", 40), ("r2", 50)] {
        store
            .capture(
                event(
                    "owner",
                    "project",
                    id,
                    at,
                    WorkOutcome::Succeeded,
                    "Real fact.",
                ),
                &limits(8),
            )
            .await
            .unwrap();
    }
    let mut other = event(
        "owner",
        "elsewhere",
        "act9",
        60,
        WorkOutcome::Succeeded,
        "{\"v\":1}",
    );
    other.kind = WorkEventKind::Action;
    store.capture(other, &limits(8)).await.unwrap();
    let episodes = store
        .action_episodes(&owner_id, &project_id, 5000)
        .await
        .unwrap();
    let ids: Vec<_> = episodes
        .iter()
        .map(|e| e.source.as_str().to_string())
        .collect();
    assert_eq!(ids, vec!["act2", "act3", "act1"]);
    assert!(episodes.iter().all(|e| e.kind == WorkEventKind::Action));
    let newest = store
        .action_episodes(&owner_id, &project_id, 2)
        .await
        .unwrap();
    assert_eq!(
        newest.iter().map(|e| e.source.as_str()).collect::<Vec<_>>(),
        vec!["act2", "act3"]
    );
    assert!(matches!(
        store.action_episodes(&owner_id, &project_id, 5001).await,
        Err(RuntimeError::InvalidInput(_))
    ));
    assert!(matches!(
        store.action_episodes(&owner_id, &project_id, 0).await,
        Err(RuntimeError::InvalidInput(_))
    ));
}

#[tokio::test]
async fn action_episode_count_counts_only_action() {
    let (_db, store) = open(&path()).await;
    let owner_id = owner("owner");
    let project_id = project("project");
    assert_eq!(
        store
            .action_episode_count(&owner_id, &project_id)
            .await
            .unwrap(),
        0
    );
    for (id, at) in [("act1", 10), ("act2", 30), ("act3", 20)] {
        let mut action = event(
            "owner",
            "project",
            id,
            at,
            WorkOutcome::Succeeded,
            "{\"v\":1}",
        );
        action.kind = WorkEventKind::Action;
        action.origin = "omp://action".into();
        store.capture(action, &limits(8)).await.unwrap();
    }
    store
        .capture(
            event(
                "owner",
                "project",
                "r1",
                40,
                WorkOutcome::Succeeded,
                "Real fact.",
            ),
            &limits(8),
        )
        .await
        .unwrap();
    let mut other_project = event(
        "owner",
        "elsewhere",
        "act9",
        60,
        WorkOutcome::Succeeded,
        "{\"v\":1}",
    );
    other_project.kind = WorkEventKind::Action;
    store.capture(other_project, &limits(8)).await.unwrap();
    let mut other_owner = event(
        "intruder",
        "project",
        "act8",
        61,
        WorkOutcome::Succeeded,
        "{\"v\":1}",
    );
    other_owner.kind = WorkEventKind::Action;
    store.capture(other_owner, &limits(8)).await.unwrap();
    assert_eq!(
        store
            .action_episode_count(&owner_id, &project_id)
            .await
            .unwrap(),
        3
    );
}
