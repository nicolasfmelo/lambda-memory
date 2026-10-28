use lambda_memory::domain_models::capture::*;
use lambda_memory::domain_models::*;
use lambda_memory::engines::capture_policy::dreaming_batch;
use lambda_memory::integrations::capture_store::{CaptureStore, InMemoryCaptureStore};
use lambda_memory::services::capture_service::CaptureService;

fn event(source: &str, owner: &str) -> CaptureEvent {
    CaptureEvent {
        source: SourceId::new(source).unwrap(),
        owner: OwnerId::new(owner).unwrap(),
        actor: ActorId::new("actor").unwrap(),
        project: ProjectId::new("project").unwrap(),
        origin: "fixture://attempt".into(),
        recorded_at_ms: 10,
        kind: WorkEventKind::Attempt,
        outcome: WorkOutcome::Failed,
        content: "Attempt failed: rejected input; no deployment occurred.".into(),
    }
}
fn service(capacity: usize) -> CaptureService<InMemoryCaptureStore> {
    CaptureService::new(
        InMemoryCaptureStore::default(),
        CaptureLimits {
            max_content_bytes: 512,
            max_pending_per_owner: capacity,
        },
    )
}
#[test]
fn replay_preserves_failed_evidence_and_provisional_status() {
    let mut service = service(1);
    let original = event("one", "owner");
    assert_eq!(
        service.capture(original.clone()).unwrap().status,
        CaptureStatus::SavedPending
    );
    assert_eq!(
        service.capture(original.clone()).unwrap().status,
        CaptureStatus::AlreadySavedPending
    );
    let pending = service.pending(&original.owner).unwrap();
    assert_eq!(pending.len(), 1);
    assert!(pending[0].provisional);
    assert_eq!(pending[0].event, original);
}
#[test]
fn conflicting_replay_never_overwrites_and_capacity_never_evicts() {
    let mut service = service(1);
    let original = event("one", "owner");
    service.capture(original.clone()).unwrap();
    let mut altered = original.clone();
    altered.outcome = WorkOutcome::Succeeded;
    assert_eq!(service.capture(altered), Err(CaptureError::SourceConflict));
    assert_eq!(
        service.capture(event("two", "owner")),
        Err(CaptureError::BacklogFull)
    );
    assert_eq!(service.pending(&original.owner).unwrap()[0].event, original);
}
#[test]
fn owner_identity_isolates_sources_and_backlog() {
    let mut service = service(1);
    service.capture(event("same", "a")).unwrap();
    service.capture(event("same", "b")).unwrap();
    let pending = service.pending(&OwnerId::new("a").unwrap()).unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].event.owner.as_str(), "a");
    assert!(
        service
            .pending(&OwnerId::new("other").unwrap())
            .unwrap()
            .is_empty()
    );
}
#[test]
fn invalid_evidence_does_not_consume_backlog() {
    let mut service = service(1);
    let mut invalid = event("bad", "owner");
    invalid.origin = "\n".into();
    assert_eq!(service.capture(invalid), Err(CaptureError::InvalidEvidence));
    let mut invalid = event("large", "owner");
    invalid.content = "x".repeat(513);
    assert_eq!(service.capture(invalid), Err(CaptureError::InvalidEvidence));
    service.capture(event("ok", "owner")).unwrap();
}
#[test]
fn dreaming_observes_recall_activity_and_in_flight_work() {
    let owner = OwnerId::new("owner").unwrap();
    let policy = DreamingPolicy {
        idle_for_ms: 50,
        max_batch: 1,
    };
    let pending = vec![PendingEvidence {
        event: event("one", "owner"),
        provisional: true,
    }];
    let mut activity = ActivitySnapshot {
        last_capture_ms: 10,
        last_recall_ms: 90,
        capture_in_flight: false,
        recall_in_flight: false,
    };
    assert!(dreaming_batch(&owner, 100, &activity, &policy, &pending).is_none());
    assert!(dreaming_batch(&owner, 140, &activity, &policy, &pending).is_some());
    activity.recall_in_flight = true;
    assert!(dreaming_batch(&owner, 200, &activity, &policy, &pending).is_none());
    activity.recall_in_flight = false;
    activity.capture_in_flight = true;
    assert!(dreaming_batch(&owner, 200, &activity, &policy, &pending).is_none());
    activity.capture_in_flight = false;
    activity.last_recall_ms = 201;
    assert!(dreaming_batch(&owner, 200, &activity, &policy, &pending).is_none());
}
#[test]
fn batch_snapshot_is_bounded_stable_and_owner_scoped() {
    let owner = OwnerId::new("owner").unwrap();
    let activity = ActivitySnapshot {
        last_capture_ms: 10,
        last_recall_ms: 10,
        capture_in_flight: false,
        recall_in_flight: false,
    };
    let policy = DreamingPolicy {
        idle_for_ms: 50,
        max_batch: 2,
    };
    let mut future = event("future", "owner");
    future.recorded_at_ms = 101;
    let pending = vec![
        PendingEvidence {
            event: event("b", "owner"),
            provisional: true,
        },
        PendingEvidence {
            event: event("a", "owner"),
            provisional: true,
        },
        PendingEvidence {
            event: event("a", "owner"),
            provisional: true,
        },
        PendingEvidence {
            event: event("private", "other"),
            provisional: true,
        },
        PendingEvidence {
            event: future,
            provisional: true,
        },
    ];
    let batch = dreaming_batch(&owner, 100, &activity, &policy, &pending).unwrap();
    assert_eq!(batch.watermark_ms, 100);
    assert_eq!(
        batch
            .sources
            .iter()
            .map(SourceId::as_str)
            .collect::<Vec<_>>(),
        vec!["a", "b"]
    );
    assert_eq!(
        Some(batch),
        dreaming_batch(&owner, 100, &activity, &policy, &pending)
    );
}
#[test]
fn store_failure_is_not_reported_as_saved() {
    struct Broken;
    impl CaptureStore for Broken {
        fn save_pending(
            &mut self,
            _: CaptureEvent,
            _: usize,
        ) -> Result<CaptureWrite, CaptureError> {
            Err(CaptureError::StorageUnavailable)
        }
        fn pending(&self, _: &OwnerId) -> Result<Vec<PendingEvidence>, CaptureError> {
            Err(CaptureError::StorageUnavailable)
        }
    }
    let mut service = CaptureService::new(
        Broken,
        CaptureLimits {
            max_content_bytes: 512,
            max_pending_per_owner: 1,
        },
    );
    assert_eq!(
        service.capture(event("one", "owner")),
        Err(CaptureError::StorageUnavailable)
    );
}

fn episode() -> lambda_memory::domain_models::procedural::ActionEpisode {
    lambda_memory::domain_models::procedural::ActionEpisode {
        tool: "bash".into(),
        intent: "Running tests".into(),
        summary: "cargo test".into(),
        error_head: None,
        session: "0123456789abcdef".into(),
        seq: 1,
    }
}
#[test]
fn action_episode_bounds_and_secrets() {
    use lambda_memory::engines::capture_policy::{contains_secret, validate_action};
    assert_eq!(validate_action(&episode()), Ok(()));
    let mut long = episode();
    long.summary = "a".repeat(201);
    assert_eq!(validate_action(&long), Err(CaptureError::InvalidEvidence));
    let mut bad = episode();
    bad.session = "xyz".into();
    assert_eq!(validate_action(&bad), Err(CaptureError::InvalidEvidence));
    let mut bad = episode();
    bad.tool = String::new();
    assert_eq!(validate_action(&bad), Err(CaptureError::InvalidEvidence));
    assert!(contains_secret(
        "curl -H 'Authorization: Bearer abcdefgh123'"
    ));
    assert!(!contains_secret("cargo test"));
    // Word boundary, minimum tail and URL userinfo.
    assert!(contains_secret("export K=sk-or-v1-abc123"));
    assert!(contains_secret("git push https://u:p@h/x"));
    assert!(!contains_secret("git push https://[REDACTED]@h/x"));
    assert!(!contains_secret("npm run task-runner"));
    assert!(!contains_secret("Bearer short"));
}

#[cfg(feature = "memory-runtime")]
#[test]
fn exchange_with_extra_key_or_secret_is_rejected() {
    use lambda_memory::engines::capture_policy::validate_exchange;
    use lambda_memory::integrations::exchange_content::parse_exchange;
    for content in [
        r#"{"v":1,"prompt":"ok","reply":"ok","extra":1}"#,
        r#"{"v":2,"prompt":"ok","reply":"ok"}"#,
        r#"{"v":1.0,"prompt":"ok","reply":"ok"}"#,
        r#"{"v":1,"prompt":2,"reply":"ok"}"#,
        r#"{"v":1,"prompt":"ok"}"#,
    ] {
        assert!(parse_exchange(content).is_none(), "{content}");
    }
    for content in [
        r#"{"v":1,"prompt":"sk-abcd","reply":"ok"}"#,
        r#"{"v":1,"prompt":"ok","reply":"Bearer abcdefgh"}"#,
    ] {
        let (prompt, reply) = parse_exchange(content).unwrap();
        assert_eq!(
            validate_exchange(&prompt, &reply),
            Err(CaptureError::InvalidEvidence)
        );
    }
}
