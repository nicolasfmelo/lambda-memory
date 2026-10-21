//! Consumer-visible operational status and history transitions.
#![cfg(feature = "memory-runtime")]
use lambda_memory::domain_models::{ActorId, MemoryId, OwnerId, ProjectId, RevisionId, SourceId};
use lambda_memory::domain_models::{capture::*, runtime::*, runtime_diagnostics::*};
use lambda_memory::integrations::database::embedded_capture::{
    EmbeddedCaptureStore, EmbeddedRuntimeStore,
};
use lambda_memory::services::runtime_service::MemoryRuntime;
use std::sync::atomic::{AtomicUsize, Ordering};
static NEXT: AtomicUsize = AtomicUsize::new(0);
fn project(name: &str) -> ProjectId {
    ProjectId::new(name).unwrap()
}
fn owner() -> OwnerId {
    OwnerId::new("owner").unwrap()
}
async fn setup(idle_ms: u64) -> (MemoryRuntime, EmbeddedRuntimeStore) {
    let path = format!(
        "{}/lambda-diagnostics-service-{}-{}",
        std::env::temp_dir().display(),
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    );
    let store = EmbeddedRuntimeStore::new(EmbeddedCaptureStore::open(&path).await.unwrap());
    let runtime = MemoryRuntime::new(
        store.clone(),
        RuntimeScope {
            owner: owner(),
            actor: ActorId::new("actor").unwrap(),
            projects: vec![project("a"), project("b")],
        },
        RuntimeLimits {
            idle_ms,
            context_bytes: 2000,
            ..Default::default()
        },
    )
    .unwrap();
    runtime.initialize_diagnostics().await.unwrap();
    (runtime, store)
}
fn request(name: &str, operation: Option<ActivityOperation>) -> ActivityRequest {
    ActivityRequest {
        project: project(name),
        limit: 32,
        operations: operation.into_iter().collect(),
        outcomes: Vec::new(),
        cursor: None,
    }
}
async fn capture(runtime: &MemoryRuntime, name: &str, id: &str) {
    runtime
        .capture(
            &project(name),
            SourceId::new(id).unwrap(),
            WorkEventKind::Error,
            WorkOutcome::Failed,
            "fixture://PRIVATE_ORIGIN".into(),
            "PRIVATE_CONTENT database decision".into(),
        )
        .await
        .result
        .unwrap();
}

#[tokio::test]
async fn repeated_diagnostic_reads_do_not_postpone_idle_and_failed_evidence_processing_can_succeed()
{
    let (runtime, _) = setup(100).await;
    capture(&runtime, "a", "original-failure").await;
    for _ in 0..7 {
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        let status = runtime.status(&project("a")).await.unwrap();
        assert_eq!(status.stored.pending.unconsumed_sources, 1);
        let history = runtime.activity(request("a", None)).await.unwrap();
        assert_eq!(
            history
                .entries
                .iter()
                .filter(|e| e.entry_kind == ActivityEntryKind::Operation)
                .count(),
            1
        );
    }
    let dreamed = runtime.dream_once().await.result.unwrap();
    assert_eq!(dreamed.sources, 1);
    let status = runtime.status(&project("a")).await.unwrap();
    assert_eq!(status.stored.pending.unconsumed_sources, 0);
    assert_eq!(status.stored.pending.processing_revisions, 0);
    assert_eq!(status.stored.lifecycle.active, 1);
    assert_eq!(
        status
            .stored
            .last_dream_success
            .as_ref()
            .unwrap()
            .counts
            .revisions_published,
        Some(1)
    );
    let available = runtime
        .list(listing(&project("a"), &[LifecycleState::Active], 8))
        .await
        .result
        .unwrap()
        .items;
    assert_eq!(available[0].sources[0].outcome, WorkOutcome::Failed);
    let before = runtime
        .activity(request("a", None))
        .await
        .unwrap()
        .entries
        .into_iter()
        .map(|e| e.id)
        .collect::<Vec<_>>();
    for _ in 0..10 {
        assert_eq!(
            runtime.dream_once().await.result.unwrap(),
            DreamReceipt::default()
        );
    }
    let after = runtime
        .activity(request("a", None))
        .await
        .unwrap()
        .entries
        .into_iter()
        .map(|e| e.id)
        .collect::<Vec<_>>();
    assert_eq!(before, after);
}

#[tokio::test]
async fn resuming_committed_processing_is_recorded_despite_an_empty_dream_receipt() {
    let (runtime, store) = setup(1).await;
    capture(&runtime, "a", "committed").await;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    store
        .propose(
            &owner(),
            &ActorId::new("actor").unwrap(),
            RevisionProposal {
                project: project("a"),
                memory: MemoryId::new("committed-memory").unwrap(),
                revision: RevisionId::new("committed-revision").unwrap(),
                sources: vec![SourceId::new("committed").unwrap()],
                content: "Database decision".into(),
                predecessor: None,
            },
            now,
        )
        .await
        .unwrap();
    assert_eq!(
        runtime
            .status(&project("a"))
            .await
            .unwrap()
            .stored
            .pending
            .processing_revisions,
        1
    );
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    assert_eq!(
        runtime.dream_once().await.result.unwrap(),
        DreamReceipt::default()
    );
    let status = runtime.status(&project("a")).await.unwrap();
    assert_eq!(status.stored.lifecycle.active, 1);
    assert_eq!(status.stored.pending.processing_revisions, 0);
    assert_eq!(
        status
            .stored
            .last_dream_success
            .unwrap()
            .counts
            .revisions_published,
        Some(1)
    );
    let history = runtime
        .activity(request("a", Some(ActivityOperation::Dream)))
        .await
        .unwrap();
    assert!(
        history
            .entries
            .iter()
            .any(|e| e.entry_kind == ActivityEntryKind::Stage
                && e.phase == ActivityPhase::Publication
                && e.outcome == ActivityOutcome::Succeeded)
    );
}

#[tokio::test]
async fn multi_project_recall_history_has_project_local_counts_and_purge_removes_only_its_links() {
    let (runtime, _) = setup(1).await;
    capture(&runtime, "a", "a-source").await;
    capture(&runtime, "b", "b-source").await;
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    for _ in 0..2 {
        runtime.dream_once().await.result.unwrap();
    }
    let recalled = runtime
        .recall("database", &[project("a"), project("b")], 2000)
        .await
        .result
        .unwrap();
    assert_eq!(
        recalled
            .hits
            .iter()
            .map(|h| h.item.revision.project.as_str())
            .collect::<std::collections::BTreeSet<_>>(),
        ["a", "b"].into_iter().collect()
    );
    let a = runtime
        .activity(request("a", Some(ActivityOperation::Recall)))
        .await
        .unwrap();
    let b = runtime
        .activity(request("b", Some(ActivityOperation::Recall)))
        .await
        .unwrap();
    let a_root = a
        .entries
        .iter()
        .find(|e| e.entry_kind == ActivityEntryKind::Operation)
        .unwrap();
    let b_root = b
        .entries
        .iter()
        .find(|e| e.entry_kind == ActivityEntryKind::Operation)
        .unwrap();
    assert_eq!(a_root.id, b_root.id);
    assert_eq!(a_root.counts.hits_returned, Some(1));
    assert_eq!(b_root.counts.hits_returned, Some(1));
    assert!(a.entries.iter().all(|e| e.project == project("a")));
    assert!(b.entries.iter().all(|e| e.project == project("b")));
    runtime
        .purge(&project("a"), &[SourceId::new("a-source").unwrap()])
        .await
        .result
        .unwrap();
    assert!(
        runtime
            .activity(request("a", Some(ActivityOperation::Recall)))
            .await
            .unwrap()
            .entries
            .is_empty()
    );
    assert_eq!(
        runtime
            .activity(request("b", Some(ActivityOperation::Recall)))
            .await
            .unwrap()
            .entries
            .iter()
            .find(|e| e.entry_kind == ActivityEntryKind::Operation)
            .unwrap()
            .id,
        b_root.id
    );
    assert!(matches!(
        runtime.status(&project("foreign")).await,
        Err(RuntimeError::Unauthorized)
    ));
    assert!(matches!(
        runtime.activity(request("foreign", None)).await,
        Err(RuntimeError::Unauthorized)
    ));
}

#[cfg(feature = "router-http")]
#[tokio::test]
async fn unsettled_indexing_stops_background_without_replaying_the_provider_call() {
    use lambda_memory::integrations::{
        external_apis::router_runtime::RouterRuntimeProviders, provider_config::ProviderConfig,
    };
    use sha2::{Digest, Sha256};
    let (_, store) = setup(1).await;
    let source = CaptureEvent {
        source: SourceId::new("index-source").unwrap(),
        owner: owner(),
        actor: ActorId::new("actor").unwrap(),
        project: project("a"),
        origin: "fixture://index".into(),
        content: "Database decision".into(),
        recorded_at_ms: 20,
        kind: WorkEventKind::Decision,
        outcome: WorkOutcome::Unknown,
    };
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
                sources: vec![source.source],
                content: "Database decision".into(),
                predecessor: None,
            },
            100,
        )
        .await
        .unwrap();
    let providers = RouterRuntimeProviders::new(
        "synthetic-unused-provider-credential",
        ProviderConfig::default(),
        1536,
    )
    .unwrap();
    let space = providers.space().unwrap();
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
    let admission = format!("embedding:{:x}", hash.finalize());
    store
        .admit_runtime_operation(&owner(), &project("a"), &admission, &item.sources)
        .await
        .unwrap();
    let runtime = MemoryRuntime::with_openrouter(
        store,
        RuntimeScope {
            owner: owner(),
            actor: ActorId::new("actor").unwrap(),
            projects: vec![project("a")],
        },
        RuntimeLimits {
            idle_ms: 1,
            ..Default::default()
        },
        providers,
    )
    .unwrap();
    runtime.initialize_diagnostics().await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    assert_eq!(
        runtime.dream_once().await.result.unwrap_err(),
        RuntimeError::UncertainAdmission
    );
    let status = runtime.status(&project("a")).await.unwrap();
    assert_eq!(status.worker.dreaming.state, DreamState::Stopped);
    assert_eq!(
        status.worker.dreaming.stop_reason,
        Some(ActivityReason::UncertainAdmission)
    );
    assert_eq!(status.stored.pending.blocked_revisions, 1);
    assert_eq!(status.stored.lifecycle.active, 0);
    let history = runtime
        .activity(request("a", Some(ActivityOperation::Dream)))
        .await
        .unwrap();
    assert!(
        history
            .entries
            .iter()
            .any(|e| e.phase == ActivityPhase::Indexing
                && e.outcome == ActivityOutcome::Failed
                && e.reason == Some(ActivityReason::UncertainAdmission))
    );
    let before = history
        .entries
        .into_iter()
        .map(|e| e.id)
        .collect::<Vec<_>>();
    assert_eq!(
        runtime.dream_once().await.result.unwrap(),
        DreamReceipt::default()
    );
    let after = runtime
        .activity(request("a", Some(ActivityOperation::Dream)))
        .await
        .unwrap()
        .entries
        .into_iter()
        .map(|e| e.id)
        .collect::<Vec<_>>();
    assert_eq!(before, after);
}
fn listing(project: &ProjectId, states: &[LifecycleState], limit: usize) -> ListQuery {
    ListQuery {
        project: project.clone(),
        states: states.to_vec(),
        limit,
        memory: None,
        after: None,
    }
}
