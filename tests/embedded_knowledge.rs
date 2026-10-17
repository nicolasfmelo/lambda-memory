#![cfg(feature = "embedded-integration")]
use lambda_memory::domain_models::*;
use lambda_memory::domain_models::{capture::*, consolidation::*, retrieval::*};
use lambda_memory::integrations::database::embedded_capture::{
    EmbeddedCaptureStore, EmbeddedKnowledgeStore,
};
use lambda_memory::services::{
    async_capture_service::AsyncCaptureService,
    async_consolidation_service::AsyncConsolidationService,
    review_retrieval_service::ReviewRetrievalService,
};
use std::sync::atomic::{AtomicUsize, Ordering};
static NEXT: AtomicUsize = AtomicUsize::new(0);
fn path() -> String {
    format!(
        "{}/lambda-knowledge-{}-{}",
        std::env::temp_dir().display(),
        std::process::id(),
        NEXT.fetch_add(1, Ordering::SeqCst)
    )
}
fn owner(value: &str) -> OwnerId {
    OwnerId::new(value).unwrap()
}
fn source(id: &str) -> CaptureEvent {
    CaptureEvent {
        source: SourceId::new(id).unwrap(),
        owner: owner("owner"),
        actor: ActorId::new("actor").unwrap(),
        project: ProjectId::new("project").unwrap(),
        origin: "fixture://knowledge".into(),
        recorded_at_ms: 10,
        kind: WorkEventKind::Result,
        outcome: WorkOutcome::Succeeded,
        content: "Database storage successful local fixture.".into(),
    }
}
fn snapshot(id: &str, event: CaptureEvent, time: u64) -> JobSnapshot {
    JobSnapshot {
        id: JobId::new(id).unwrap(),
        owner: event.owner.clone(),
        project: event.project.clone(),
        watermark_ms: time,
        sources: vec![event],
    }
}
fn revision(id: &str) -> Revision {
    Revision {
        id: RevisionId::new(id).unwrap(),
        memory: MemoryId::new("memory").unwrap(),
        owner: owner("owner"),
        project: ProjectId::new("project").unwrap(),
        kind: KnowledgeKind::Knowledge,
        status: RevisionStatus::PendingReview,
        content: "Database storage knowledge.".into(),
        prerequisites: vec![],
        sources: vec![SourceId::new("source").unwrap()],
        predecessor: None,
        model: "offline-fixture".into(),
        prompt_version: "fixture-v1".into(),
    }
}
fn query(time: u64, terms: &[&str]) -> RevisionRetrievalRequest {
    RevisionRetrievalRequest {
        owner: owner("owner"),
        project: ProjectId::new("project").unwrap(),
        terms: terms.iter().map(|t| t.to_string()).collect(),
        as_of_ms: time,
        limit: 4,
    }
}
async fn setup() -> (
    EmbeddedCaptureStore,
    AsyncConsolidationService<EmbeddedKnowledgeStore>,
    ReviewRetrievalService<EmbeddedKnowledgeStore>,
) {
    let db = EmbeddedCaptureStore::open(&path()).await.unwrap();
    let capture = AsyncCaptureService::new(
        db.clone(),
        CaptureLimits {
            max_content_bytes: 1024,
            max_pending_per_owner: 10,
        },
    );
    capture.capture(source("source")).await.unwrap();
    (
        db.clone(),
        AsyncConsolidationService::new(EmbeddedKnowledgeStore::new(db.clone())),
        ReviewRetrievalService::new(EmbeddedKnowledgeStore::new(db)),
    )
}
#[tokio::test]
async fn persistent_jobs_replay_exact_sources_and_completion() {
    let (_db, jobs, retrieval) = setup().await;
    let snap = snapshot("job", source("source"), 100);
    assert_eq!(jobs.create(snap.clone()).await, Ok(JobReceipt::Created));
    assert_eq!(
        jobs.create(snap.clone()).await,
        Ok(JobReceipt::AlreadyCreated)
    );
    let outputs = vec![revision("v1")];
    assert_eq!(
        jobs.complete(&owner("owner"), &snap.id, outputs.clone())
            .await,
        Ok(JobReceipt::Completed)
    );
    assert_eq!(
        jobs.complete(&owner("owner"), &snap.id, outputs.clone())
            .await,
        Ok(JobReceipt::AlreadyCompleted)
    );
    let mut changed = outputs.clone();
    changed[0].content = "changed derivation".into();
    assert_eq!(
        jobs.complete(&owner("owner"), &snap.id, changed).await,
        Err(ConsolidationError::Conflict)
    );
    let persisted = jobs.get(&owner("owner"), &snap.id).await.unwrap();
    assert_eq!(persisted.snapshot, snap);
    assert_eq!(persisted.revisions, outputs);
    let hits = retrieval
        .retrieve(query(100, &["database", "storage"]))
        .await
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].lexical_score, 2);
    assert!(hits[0].provisional);
    assert_eq!(hits[0].sources, vec![source("source")]);
}
#[tokio::test]
async fn snapshots_cannot_fabricate_or_change_persisted_evidence() {
    let (_db, jobs, _retrieval) = setup().await;
    let mut fabricated = source("source");
    fabricated.content = "invented".into();
    assert_eq!(
        jobs.create(snapshot("forged", fabricated, 100)).await,
        Err(ConsolidationError::InvalidSnapshot)
    );
    assert_eq!(
        jobs.create(snapshot("missing", source("not-saved"), 100))
            .await,
        Err(ConsolidationError::InvalidSnapshot)
    );
    let snap = snapshot("job", source("source"), 100);
    jobs.create(snap.clone()).await.unwrap();
    let mut changed = snap;
    changed.watermark_ms = 101;
    assert_eq!(
        jobs.create(changed).await,
        Err(ConsolidationError::Conflict)
    );
}
#[tokio::test]
async fn owner_project_time_and_query_bindings_control_retrieval() {
    let (_db, jobs, retrieval) = setup().await;
    let snap = snapshot("job", source("source"), 100);
    jobs.create(snap.clone()).await.unwrap();
    jobs.complete(&owner("owner"), &snap.id, vec![revision("v1")])
        .await
        .unwrap();
    assert!(
        retrieval
            .retrieve(query(99, &["database"]))
            .await
            .unwrap()
            .is_empty()
    );
    let mut other = query(100, &["database"]);
    other.owner = owner("other");
    assert!(retrieval.retrieve(other).await.unwrap().is_empty());
    let mut other = query(100, &["database"]);
    other.project = ProjectId::new("other").unwrap();
    assert!(retrieval.retrieve(other).await.unwrap().is_empty());
    assert_eq!(
        jobs.get(&owner("other"), &snap.id).await,
        Err(ConsolidationError::Missing)
    );
    assert!(
        retrieval
            .retrieve(query(100, &["'; DELETE memory_revision; --"]))
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        retrieval
            .retrieve(query(100, &["database"]))
            .await
            .unwrap()
            .len(),
        1
    );
}
#[tokio::test]
async fn current_and_historical_versions_are_distinct_and_forks_conflict() {
    let (db, jobs, retrieval) = setup().await;
    let first = snapshot("first", source("source"), 100);
    jobs.create(first.clone()).await.unwrap();
    let mut old = revision("v1");
    old.content = "Obsolete database approach.".into();
    jobs.complete(&owner("owner"), &first.id, vec![old.clone()])
        .await
        .unwrap();
    let capture = AsyncCaptureService::new(
        db,
        CaptureLimits {
            max_content_bytes: 1024,
            max_pending_per_owner: 10,
        },
    );
    let mut next_source = source("new-source");
    next_source.recorded_at_ms = 150;
    capture.capture(next_source.clone()).await.unwrap();
    let next = snapshot("next", next_source, 200);
    jobs.create(next.clone()).await.unwrap();
    let mut updated = revision("v2");
    updated.content = "Corrected database approach.".into();
    updated.sources = vec![SourceId::new("new-source").unwrap()];
    updated.predecessor = Some(old.id.clone());
    jobs.complete(&owner("owner"), &next.id, vec![updated.clone()])
        .await
        .unwrap();
    assert_eq!(
        retrieval.retrieve(query(150, &["database"])).await.unwrap()[0]
            .revision
            .id,
        old.id
    );
    assert_eq!(
        retrieval.retrieve(query(250, &["database"])).await.unwrap()[0]
            .revision
            .id,
        updated.id
    );
    assert!(
        retrieval
            .retrieve(query(250, &["obsolete"]))
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        jobs.get(&owner("owner"), &first.id)
            .await
            .unwrap()
            .revisions[0],
        old
    );
    let fork = snapshot("fork", source("source"), 300);
    jobs.create(fork.clone()).await.unwrap();
    let mut branched = revision("v3");
    branched.predecessor = Some(RevisionId::new("v1").unwrap());
    assert_eq!(
        jobs.complete(&owner("owner"), &fork.id, vec![branched])
            .await,
        Err(ConsolidationError::Conflict)
    );
}
#[tokio::test]
async fn concurrent_completion_and_procedures_remain_provisional() {
    let (db, jobs, retrieval) = setup().await;
    let other = AsyncConsolidationService::new(EmbeddedKnowledgeStore::new(db));
    let snap = snapshot("job", source("source"), 100);
    jobs.create(snap.clone()).await.unwrap();
    let mut output = revision("v1");
    output.kind = KnowledgeKind::Procedure;
    output.prerequisites = vec!["Disposable local fixture only.".into()];
    let outputs = vec![output];
    let principal = owner("owner");
    let (a, b) = tokio::join!(
        jobs.complete(&principal, &snap.id, outputs.clone()),
        other.complete(&principal, &snap.id, outputs.clone())
    );
    let receipts = [a.unwrap(), b.unwrap()];
    assert!(receipts.contains(&JobReceipt::Completed));
    assert!(receipts.contains(&JobReceipt::AlreadyCompleted));
    let hits = retrieval.retrieve(query(100, &["database"])).await.unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].revision.kind, KnowledgeKind::Procedure);
    assert!(hits[0].provisional);
}
#[tokio::test]
async fn lexical_ties_limits_and_duplicate_terms_are_deterministic() {
    let (_db, jobs, retrieval) = setup().await;
    let snap = snapshot("ties", source("source"), 100);
    jobs.create(snap.clone()).await.unwrap();
    let mut a = revision("a-revision");
    a.memory = MemoryId::new("a-memory").unwrap();
    let mut z = revision("z-revision");
    z.memory = MemoryId::new("z-memory").unwrap();
    jobs.complete(&owner("owner"), &snap.id, vec![z, a])
        .await
        .unwrap();
    let hits = retrieval.retrieve(query(100, &["database"])).await.unwrap();
    assert_eq!(
        hits.iter()
            .map(|hit| hit.revision.memory.as_str())
            .collect::<Vec<_>>(),
        vec!["a-memory", "z-memory"]
    );
    let mut bounded = query(100, &["Database", " database "]);
    bounded.limit = 1;
    let hits = retrieval.retrieve(bounded).await.unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].revision.memory.as_str(), "a-memory");
    assert_eq!(hits[0].lexical_score, 1);
}
