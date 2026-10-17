#![cfg(feature = "embedded-integration")]
use lambda_memory::domain_models::capture::*;
use lambda_memory::domain_models::*;
use lambda_memory::integrations::database::embedded_capture::EmbeddedCaptureStore;
use lambda_memory::services::async_capture_service::AsyncCaptureService;
use std::sync::atomic::{AtomicUsize, Ordering};
static NEXT: AtomicUsize = AtomicUsize::new(0);
fn path() -> String {
    format!(
        "{}/lambda-memory-real-{}-{}",
        std::env::temp_dir().display(),
        std::process::id(),
        NEXT.fetch_add(1, Ordering::SeqCst)
    )
}
fn event(owner: &str, source: &str) -> CaptureEvent {
    CaptureEvent {
        source: SourceId::new(source).unwrap(),
        owner: OwnerId::new(owner).unwrap(),
        actor: ActorId::new("actor").unwrap(),
        project: ProjectId::new("project").unwrap(),
        origin: "fixture://container".into(),
        recorded_at_ms: 100,
        kind: WorkEventKind::Attempt,
        outcome: WorkOutcome::Failed,
        content: "Synthetic failed attempt. No user data.".into(),
    }
}
fn service(
    store: EmbeddedCaptureStore,
    capacity: usize,
) -> AsyncCaptureService<EmbeddedCaptureStore> {
    AsyncCaptureService::new(
        store,
        CaptureLimits {
            max_content_bytes: 1024,
            max_pending_per_owner: capacity,
        },
    )
}
#[tokio::test]
async fn durable_originals_replay_conflict_and_owner_scope() {
    let path = path();
    let store = EmbeddedCaptureStore::open(&path).await.unwrap();
    let capture = service(store.clone(), 1);
    let original = event("a", "same");
    assert_eq!(
        capture.capture(original.clone()).await.unwrap().status,
        CaptureStatus::SavedPending
    );
    assert_eq!(
        capture.capture(original.clone()).await.unwrap().status,
        CaptureStatus::AlreadySavedPending
    );
    let mut changed = original.clone();
    changed.content = "changed".into();
    assert_eq!(
        capture.capture(changed).await,
        Err(CaptureError::SourceConflict)
    );
    assert_eq!(
        capture.capture(event("a", "second")).await,
        Err(CaptureError::BacklogFull)
    );
    capture.capture(event("b", "same")).await.unwrap();
    let injection = OwnerId::new("a'; DELETE memory_source; --").unwrap();
    assert!(capture.pending(&injection).await.unwrap().is_empty());
    let rows = capture.pending(&OwnerId::new("a").unwrap()).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].event, original);
    assert!(rows[0].provisional);
    drop(capture);
    drop(store);
}
#[tokio::test]
async fn concurrent_capture_preserves_identity_and_capacity() {
    let store = EmbeddedCaptureStore::open(&path()).await.unwrap();
    let a = service(store.clone(), 1);
    let b = service(store.clone(), 1);
    let original = event("owner", "same");
    let (left, right) = tokio::join!(a.capture(original.clone()), b.capture(original.clone()));
    let statuses = [left.unwrap().status, right.unwrap().status];
    assert!(statuses.contains(&CaptureStatus::SavedPending));
    assert!(statuses.contains(&CaptureStatus::AlreadySavedPending));
    let (left, right) = tokio::join!(
        a.capture(event("new-owner", "one")),
        b.capture(event("new-owner", "two"))
    );
    assert_eq!(usize::from(left.is_ok()) + usize::from(right.is_ok()), 1);
    assert!(left == Err(CaptureError::BacklogFull) || right == Err(CaptureError::BacklogFull));
    assert_eq!(
        a.pending(&OwnerId::new("new-owner").unwrap())
            .await
            .unwrap()
            .len(),
        1
    );
}
