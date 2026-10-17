#![cfg(feature = "memory-runtime")]
use lambda_memory::domain_models::{
    ActivityId, ActorId, JobId, MemoryId, OwnerId, ProjectId, RevisionId, SourceId,
    WorkerInstanceId,
};
use lambda_memory::domain_models::{
    capture::*, consolidation::*, embeddings::*, runtime::*, runtime_diagnostics::*,
};
use lambda_memory::integrations::consolidation_store::AsyncConsolidationStore;
use lambda_memory::integrations::database::embedded_capture::{
    EmbeddedCaptureStore, EmbeddedKnowledgeStore, EmbeddedRuntimeStore,
};
use sha2::{Digest, Sha256};
use std::sync::atomic::{AtomicUsize, Ordering};
static NEXT: AtomicUsize = AtomicUsize::new(0);
fn owner() -> OwnerId {
    OwnerId::new("owner").unwrap()
}
fn project(name: &str) -> ProjectId {
    ProjectId::new(name).unwrap()
}
fn context(instance: &str) -> ActivityReadContext {
    ActivityReadContext {
        instance: WorkerInstanceId::new(instance).unwrap(),
        active_entries: Vec::new(),
        active_paid_operations: Vec::new(),
    }
}
async fn fixture() -> (String, EmbeddedRuntimeStore) {
    let path = format!(
        "{}/lambda-diagnostics-store-{}-{}",
        std::env::temp_dir().display(),
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    );
    let store = EmbeddedRuntimeStore::new(EmbeddedCaptureStore::open(&path).await.unwrap());
    store
        .initialize_activity(
            &owner(),
            &[project("a"), project("b")],
            &context("worker").instance,
            10,
        )
        .await
        .unwrap();
    (path, store)
}
fn event(id: &str, p: &str) -> CaptureEvent {
    CaptureEvent {
        source: SourceId::new(id).unwrap(),
        owner: owner(),
        actor: ActorId::new("actor").unwrap(),
        project: project(p),
        origin: "fixture://PRIVATE_ORIGIN".into(),
        content: "PRIVATE_SOURCE_TEXT database decision".into(),
        recorded_at_ms: 20,
        kind: WorkEventKind::Decision,
        outcome: WorkOutcome::Unknown,
    }
}
fn record(id: &str, p: &str, parent: Option<&str>, operation: ActivityOperation) -> ActivityRecord {
    ActivityRecord {
        entry: ActivityEntry {
            id: ActivityId::new(id).unwrap(),
            parent: parent.map(|s| ActivityId::new(s).unwrap()),
            project: project(p),
            entry_kind: if parent.is_some() {
                ActivityEntryKind::Stage
            } else {
                ActivityEntryKind::Operation
            },
            operation,
            phase: ActivityPhase::Preparation,
            instance: context("worker").instance,
            started_at_ms: 30,
            finished_at_ms: None,
            classified_at_ms: None,
            duration_ms: None,
            outcome: ActivityOutcome::Running,
            reason: None,
            counts: ActivityCounts::default(),
        },
        sources: Vec::new(),
        revisions: Vec::new(),
        paid_operation: None,
    }
}
fn finish(id: &str, outcome: ActivityOutcome, time: u64) -> ActivityFinish {
    ActivityFinish {
        id: ActivityId::new(id).unwrap(),
        phase: ActivityPhase::Preparation,
        outcome,
        finished_at_ms: Some(time),
        classified_at_ms: None,
        duration_ms: Some(time - 30),
        reason: (outcome == ActivityOutcome::Failed).then_some(ActivityReason::ProviderUnavailable),
        counts: ActivityCounts::default(),
    }
}
fn request(p: &str, limit: usize) -> ActivityRequest {
    ActivityRequest {
        project: project(p),
        limit,
        operations: Vec::new(),
        outcomes: Vec::new(),
        cursor: None,
    }
}

#[tokio::test]
async fn totals_are_scoped_exact_and_include_legacy_processing_beyond_listing_bounds() {
    let (_, store) = fixture().await;
    for i in 0..40 {
        store
            .capture(event(&format!("a-{i}"), "a"), &RuntimeLimits::default())
            .await
            .unwrap();
    }
    store
        .capture(event("b-only", "b"), &RuntimeLimits::default())
        .await
        .unwrap();
    let before = store
        .activity_snapshot(&owner(), &project("a"), None, &context("worker"), 100)
        .await
        .unwrap();
    assert_eq!(before.pending.unconsumed_sources, 40);
    assert_eq!(before.pending.processing_revisions, 0);
    for i in 0..40 {
        store
            .propose(
                &owner(),
                &ActorId::new("actor").unwrap(),
                RevisionProposal {
                    project: project("a"),
                    memory: MemoryId::new(format!("m{i}")).unwrap(),
                    revision: RevisionId::new(format!("r{i}")).unwrap(),
                    sources: vec![SourceId::new(format!("a-{i}")).unwrap()],
                    content: "Database decision".into(),
                    predecessor: None,
                },
                100,
            )
            .await
            .unwrap();
    }
    let after = store
        .activity_snapshot(&owner(), &project("a"), None, &context("worker"), 110)
        .await
        .unwrap();
    assert_eq!(after.pending.unconsumed_sources, 0);
    assert_eq!(after.pending.processing_revisions, 40);
    assert_eq!(after.lifecycle.processing, 40);
    let other = store
        .activity_snapshot(&owner(), &project("b"), None, &context("worker"), 110)
        .await
        .unwrap();
    assert_eq!(other.pending.unconsumed_sources, 1);
    assert_eq!(other.lifecycle.processing, 0);
    drop(store);
    // Write through the pre-runtime knowledge API: no revision-state row is created.
    let (path, legacy_store) = fixture().await;
    drop(legacy_store);
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    let capture = EmbeddedCaptureStore::open(&path).await.unwrap();
    let store = EmbeddedRuntimeStore::new(capture.clone());
    let knowledge = EmbeddedKnowledgeStore::new(capture);
    let source = event("legacy", "a");
    store
        .capture(source.clone(), &RuntimeLimits::default())
        .await
        .unwrap();
    let snapshot = JobSnapshot {
        id: JobId::new("legacy-job").unwrap(),
        owner: owner(),
        project: project("a"),
        watermark_ms: 20,
        sources: vec![source],
    };
    knowledge.create_async(snapshot.clone()).await.unwrap();
    knowledge
        .complete_async(
            &owner(),
            &snapshot.id,
            vec![Revision {
                id: RevisionId::new("legacy-revision").unwrap(),
                memory: MemoryId::new("legacy-memory").unwrap(),
                owner: owner(),
                project: project("a"),
                kind: KnowledgeKind::Knowledge,
                status: RevisionStatus::PendingReview,
                content: "Legacy database claim".into(),
                prerequisites: Vec::new(),
                sources: vec![SourceId::new("legacy").unwrap()],
                predecessor: None,
                model: "fixture".into(),
                prompt_version: "fixture".into(),
            }],
        )
        .await
        .unwrap();
    let legacy = store
        .activity_snapshot(&owner(), &project("a"), None, &context("worker"), 110)
        .await
        .unwrap();
    assert_eq!(legacy.lifecycle.processing, 1);
    assert_eq!(legacy.pending.processing_revisions, 1);
}

#[tokio::test]
async fn seven_day_expiration_is_per_tree_and_never_evicts_by_operation_count() {
    let (_, store) = fixture().await;
    let mut live = context("worker");
    for i in 0..1001 {
        let id = format!("root-{i:04}");
        store
            .start_activity(&owner(), record(&id, "a", None, ActivityOperation::List))
            .await
            .unwrap();
        store
            .finish_activity(
                &owner(),
                &project("a"),
                &finish(&id, ActivityOutcome::Succeeded, 100),
            )
            .await
            .unwrap();
    }
    store
        .start_activity(
            &owner(),
            record("tree", "a", None, ActivityOperation::Dream),
        )
        .await
        .unwrap();
    store
        .start_activity(
            &owner(),
            record("child", "a", Some("tree"), ActivityOperation::Dream),
        )
        .await
        .unwrap();
    store
        .finish_activity(
            &owner(),
            &project("a"),
            &finish("child", ActivityOutcome::Succeeded, 50),
        )
        .await
        .unwrap();
    store
        .finish_activity(
            &owner(),
            &project("a"),
            &finish("tree", ActivityOutcome::Succeeded, 100),
        )
        .await
        .unwrap();
    store
        .start_activity(
            &owner(),
            record("live", "a", None, ActivityOperation::Dream),
        )
        .await
        .unwrap();
    live.active_entries.push(ActivityId::new("live").unwrap());
    store
        .start_activity(
            &owner(),
            record("other-project", "b", None, ActivityOperation::List),
        )
        .await
        .unwrap();
    store
        .finish_activity(
            &owner(),
            &project("b"),
            &finish("other-project", ActivityOutcome::Succeeded, 100),
        )
        .await
        .unwrap();
    let early = 100 + ACTIVITY_RETENTION_MS - 1;
    store
        .maintain_activity(&owner(), &project("a"), &live, early, 2000)
        .await
        .unwrap();
    let mut query = request("a", 32);
    let mut all = Vec::new();
    loop {
        let page = store
            .activity_page(&owner(), &query, &live, early)
            .await
            .unwrap();
        all.extend(page.entries.into_iter().map(|e| e.id.to_string()));
        if page.next_cursor.is_none() {
            break;
        }
        query.cursor = page.next_cursor;
    }
    assert_eq!(all.len(), 1004);
    assert!(all.contains(&"root-0000".into()));
    assert!(all.contains(&"child".into()));
    store
        .maintain_activity(&owner(), &project("a"), &live, early + 1, 2000)
        .await
        .unwrap();
    let remaining = store
        .activity_page(&owner(), &request("a", 32), &live, early + 1)
        .await
        .unwrap();
    assert_eq!(
        remaining
            .entries
            .iter()
            .map(|e| e.id.as_str())
            .collect::<Vec<_>>(),
        vec!["live"]
    );
    let other = store
        .activity_page(&owner(), &request("b", 32), &context("worker"), early)
        .await
        .unwrap();
    assert_eq!(other.entries[0].id.as_str(), "other-project");
}

#[tokio::test]
async fn keyset_pages_keep_equal_time_order_and_reject_scope_or_filter_reuse() {
    let (_, store) = fixture().await;
    for i in 0..8 {
        let id = format!("id-{i}");
        store
            .start_activity(&owner(), record(&id, "a", None, ActivityOperation::Capture))
            .await
            .unwrap();
        store
            .finish_activity(
                &owner(),
                &project("a"),
                &finish(&id, ActivityOutcome::Succeeded, 100),
            )
            .await
            .unwrap();
    }
    let mut query = request("a", 2);
    query.operations = vec![ActivityOperation::Capture];
    query.outcomes = vec![ActivityOutcome::Succeeded];
    let first = store
        .activity_page(&owner(), &query, &context("worker"), 110)
        .await
        .unwrap();
    assert_eq!(
        first
            .entries
            .iter()
            .map(|e| e.id.as_str())
            .collect::<Vec<_>>(),
        vec!["id-7", "id-6"]
    );
    let cursor = first.next_cursor.unwrap();
    let mut newly_arrived = record("new", "a", None, ActivityOperation::Capture);
    newly_arrived.entry.started_at_ms = 31;
    store.start_activity(&owner(), newly_arrived).await.unwrap();
    store
        .finish_activity(
            &owner(),
            &project("a"),
            &finish("new", ActivityOutcome::Succeeded, 100),
        )
        .await
        .unwrap();
    query.cursor = Some(cursor.clone());
    let second = store
        .activity_page(&owner(), &query, &context("worker"), 110)
        .await
        .unwrap();
    assert_eq!(
        second
            .entries
            .iter()
            .map(|e| e.id.as_str())
            .collect::<Vec<_>>(),
        vec!["id-5", "id-4"]
    );
    query.project = project("b");
    assert!(matches!(
        store
            .activity_page(&owner(), &query, &context("worker"), 110)
            .await,
        Err(RuntimeError::InvalidInput(_))
    ));
    query.project = project("a");
    query.operations = vec![ActivityOperation::List];
    assert!(matches!(
        store
            .activity_page(&owner(), &query, &context("worker"), 110)
            .await,
        Err(RuntimeError::InvalidInput(_))
    ));
    query.operations = vec![ActivityOperation::Capture];
    query.cursor = Some("zz".into());
    assert!(matches!(
        store
            .activity_page(&owner(), &query, &context("worker"), 110)
            .await,
        Err(RuntimeError::InvalidInput(_))
    ));
    let bytes: Vec<u8> = cursor
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
        .collect();
    let mut malformed: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    malformed["before_time"] = u64::MAX.into();
    malformed["ceiling_time"] = u64::MAX.into();
    let bytes = serde_json::to_vec(&malformed).unwrap();
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write;
        write!(encoded, "{byte:02x}").unwrap();
    }
    query.cursor = Some(encoded);
    assert!(matches!(
        store
            .activity_page(&owner(), &query, &context("worker"), 110)
            .await,
        Err(RuntimeError::InvalidInput(_))
    ));
}

#[tokio::test]
async fn restart_classifies_orphans_without_altering_confirmed_outcomes_or_paid_admission() {
    let (path, store) = fixture().await;
    let source = event("paid-source", "a");
    store
        .capture(source.clone(), &RuntimeLimits::default())
        .await
        .unwrap();
    store
        .admit_runtime_operation(
            &owner(),
            &project("a"),
            "dream-job:uncertain",
            std::slice::from_ref(&source),
        )
        .await
        .unwrap();
    store
        .start_activity(
            &owner(),
            record("unfinished", "a", None, ActivityOperation::Dream),
        )
        .await
        .unwrap();
    store
        .start_activity(
            &owner(),
            record(
                "unfinished-stage",
                "a",
                Some("unfinished"),
                ActivityOperation::Dream,
            ),
        )
        .await
        .unwrap();
    store
        .start_activity(
            &owner(),
            record("confirmed", "a", None, ActivityOperation::List),
        )
        .await
        .unwrap();
    store
        .finish_activity(
            &owner(),
            &project("a"),
            &finish("confirmed", ActivityOutcome::Succeeded, 100),
        )
        .await
        .unwrap();
    let before = store
        .activity_page(&owner(), &request("a", 32), &context("worker"), 110)
        .await
        .unwrap();
    assert_eq!(
        before
            .entries
            .iter()
            .find(|e| e.id.as_str() == "unfinished")
            .unwrap()
            .outcome,
        ActivityOutcome::Indeterminate
    );
    drop(store);
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    let reopened = EmbeddedRuntimeStore::new(EmbeddedCaptureStore::open(&path).await.unwrap());
    let next = context("next-worker");
    reopened
        .initialize_activity(&owner(), &[project("a")], &next.instance, 200)
        .await
        .unwrap();
    let history = reopened
        .activity_page(&owner(), &request("a", 32), &next, 200)
        .await
        .unwrap();
    for id in ["unfinished", "unfinished-stage"] {
        let entry = history
            .entries
            .iter()
            .find(|e| e.id.as_str() == id)
            .unwrap();
        assert_eq!(entry.outcome, ActivityOutcome::Indeterminate);
        assert_eq!(entry.classified_at_ms, Some(200));
        assert_eq!(entry.finished_at_ms, None);
        assert_eq!(entry.duration_ms, None);
    }
    let confirmed = history
        .entries
        .iter()
        .find(|e| e.id.as_str() == "confirmed")
        .unwrap();
    assert_eq!(confirmed.outcome, ActivityOutcome::Succeeded);
    assert_eq!(confirmed.finished_at_ms, Some(100));
    reopened
        .maintain_activity(
            &owner(),
            &project("a"),
            &next,
            200 + ACTIVITY_RETENTION_MS,
            100,
        )
        .await
        .unwrap();
    assert_eq!(
        reopened
            .admit_runtime_operation(
                &owner(),
                &project("a"),
                "dream-job:uncertain",
                std::slice::from_ref(&source)
            )
            .await
            .unwrap_err(),
        RuntimeError::UncertainAdmission
    );
}

#[tokio::test]
async fn uncertain_subsets_exclude_active_paid_work_and_match_legacy_embedding_keys() {
    let (_, store) = fixture().await;
    let source = event("derive", "a");
    store
        .capture(source.clone(), &RuntimeLimits::default())
        .await
        .unwrap();
    store
        .admit_runtime_operation(
            &owner(),
            &project("a"),
            "dream-job:old",
            std::slice::from_ref(&source),
        )
        .await
        .unwrap();
    let space = EmbeddingSpace {
        provider: "fixture".into(),
        model: "embedding".into(),
        revision: "v1".into(),
        dimensions: 2,
    };
    let mut live = context("worker");
    assert_eq!(
        store
            .activity_snapshot(&owner(), &project("a"), Some(&space), &live, 100)
            .await
            .unwrap()
            .pending
            .blocked_sources,
        1
    );
    live.active_paid_operations.push("dream-job:old".into());
    assert_eq!(
        store
            .activity_snapshot(&owner(), &project("a"), Some(&space), &live, 100)
            .await
            .unwrap()
            .pending
            .blocked_sources,
        0
    );
    let source = event("index", "a");
    store
        .capture(source.clone(), &RuntimeLimits::default())
        .await
        .unwrap();
    let item = store
        .propose(
            &owner(),
            &ActorId::new("actor").unwrap(),
            RevisionProposal {
                project: project("a"),
                memory: MemoryId::new("index-memory").unwrap(),
                revision: RevisionId::new("index-revision").unwrap(),
                sources: vec![source.source.clone()],
                content: "Database claim".into(),
                predecessor: None,
            },
            100,
        )
        .await
        .unwrap();
    let mut hash = Sha256::new();
    for part in [
        item.revision.id.as_str(),
        space.provider.as_str(),
        space.model.as_str(),
        space.revision.as_str(),
    ] {
        hash.update((part.len() as u64).to_be_bytes());
        hash.update(part.as_bytes());
    }
    let operation = format!("embedding:{:x}", hash.finalize());
    store
        .admit_runtime_operation(&owner(), &project("a"), &operation, &item.sources)
        .await
        .unwrap();
    assert_eq!(
        store
            .activity_snapshot(&owner(), &project("a"), Some(&space), &live, 110)
            .await
            .unwrap()
            .pending
            .blocked_revisions,
        1
    );
    live.active_paid_operations.push(operation);
    assert_eq!(
        store
            .activity_snapshot(&owner(), &project("a"), Some(&space), &live, 110)
            .await
            .unwrap()
            .pending
            .blocked_revisions,
        0
    );
    store
        .save_runtime_embedding(
            &item,
            &Embedding {
                space: space.clone(),
                values: vec![1.0, 0.0],
            },
        )
        .await
        .unwrap();
    live.active_paid_operations.clear();
    assert_eq!(
        store
            .activity_snapshot(&owner(), &project("a"), Some(&space), &live, 110)
            .await
            .unwrap()
            .pending
            .blocked_revisions,
        0
    );
}

#[tokio::test]
async fn purge_removes_mixed_source_trees_without_resurrection_or_cross_project_deletion() {
    let (_, store) = fixture().await;
    for id in ["s1", "s2"] {
        store
            .capture(event(id, "a"), &RuntimeLimits::default())
            .await
            .unwrap();
    }
    let mut root = record("mixed", "a", None, ActivityOperation::Dream);
    root.sources = vec![SourceId::new("s1").unwrap(), SourceId::new("s2").unwrap()];
    store.start_activity(&owner(), root).await.unwrap();
    store
        .start_activity(
            &owner(),
            record("mixed-child", "a", Some("mixed"), ActivityOperation::Dream),
        )
        .await
        .unwrap();
    store
        .start_activity(
            &owner(),
            record("unrelated", "b", None, ActivityOperation::List),
        )
        .await
        .unwrap();
    store
        .finish_activity(
            &owner(),
            &project("b"),
            &finish("unrelated", ActivityOutcome::Succeeded, 100),
        )
        .await
        .unwrap();
    store
        .start_activity(
            &owner(),
            record("purge", "a", None, ActivityOperation::Purge),
        )
        .await
        .unwrap();
    let removed = store
        .purge(&owner(), &project("a"), &[SourceId::new("s1").unwrap()])
        .await
        .unwrap();
    assert_eq!(removed.sources, 1);
    store
        .finish_activity(
            &owner(),
            &project("a"),
            &finish("purge", ActivityOutcome::Succeeded, 100),
        )
        .await
        .unwrap();
    assert_eq!(
        store
            .finish_activity(
                &owner(),
                &project("a"),
                &finish("mixed-child", ActivityOutcome::Succeeded, 100)
            )
            .await
            .unwrap(),
        ActivityWrite::Removed
    );
    assert_eq!(
        store
            .start_activity(
                &owner(),
                record("late-child", "a", Some("mixed"), ActivityOperation::Dream)
            )
            .await
            .unwrap(),
        ActivityWrite::Removed
    );
    let own = store
        .activity_page(&owner(), &request("a", 32), &context("worker"), 110)
        .await
        .unwrap();
    assert_eq!(
        own.entries
            .iter()
            .map(|e| e.id.as_str())
            .collect::<Vec<_>>(),
        vec!["purge"]
    );
    let other = store
        .activity_page(&owner(), &request("b", 32), &context("worker"), 110)
        .await
        .unwrap();
    assert_eq!(other.entries[0].id.as_str(), "unrelated");
    assert_eq!(
        store
            .activity_snapshot(&owner(), &project("a"), None, &context("worker"), 110)
            .await
            .unwrap()
            .pending
            .unconsumed_sources,
        1
    );
}

#[tokio::test]
async fn timeline_operation_and_phase_round_trip_through_the_activity_journal() {
    let (_, store) = fixture().await;
    let mut root = record("timeline", "a", None, ActivityOperation::Timeline);
    root.entry.phase = ActivityPhase::Timeline;
    store.start_activity(&owner(), root).await.unwrap();
    let mut done = finish("timeline", ActivityOutcome::Succeeded, 100);
    done.phase = ActivityPhase::Timeline;
    store
        .finish_activity(&owner(), &project("a"), &done)
        .await
        .unwrap();
    store
        .start_activity(
            &owner(),
            record("other", "a", None, ActivityOperation::List),
        )
        .await
        .unwrap();
    let mut query = request("a", 32);
    query.operations = vec![ActivityOperation::Timeline];
    let page = store
        .activity_page(&owner(), &query, &context("worker"), 110)
        .await
        .unwrap();
    assert_eq!(page.entries.len(), 1);
    assert_eq!(page.entries[0].operation, ActivityOperation::Timeline);
    assert_eq!(page.entries[0].phase, ActivityPhase::Timeline);
    assert_eq!(page.entries[0].outcome, ActivityOutcome::Succeeded);
    assert_eq!(
        ActivityOperation::parse("timeline"),
        Some(ActivityOperation::Timeline)
    );
}
