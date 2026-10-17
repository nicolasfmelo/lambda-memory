use lambda_memory::domain_models::capture::*;
use lambda_memory::domain_models::consolidation::*;
use lambda_memory::domain_models::*;
use lambda_memory::integrations::consolidation_store::InMemoryConsolidationStore;
use lambda_memory::services::consolidation_service::ConsolidationService;
fn owner() -> OwnerId {
    OwnerId::new("owner").unwrap()
}
fn job_id(id: &str) -> JobId {
    JobId::new(id).unwrap()
}
fn snapshot(id: &str) -> JobSnapshot {
    JobSnapshot {
        id: job_id(id),
        owner: owner(),
        project: ProjectId::new("project").unwrap(),
        watermark_ms: 100,
        sources: vec![CaptureEvent {
            source: SourceId::new("source").unwrap(),
            owner: owner(),
            actor: ActorId::new("actor").unwrap(),
            project: ProjectId::new("project").unwrap(),
            origin: "fixture://original".into(),
            recorded_at_ms: 90,
            kind: WorkEventKind::Result,
            outcome: WorkOutcome::Succeeded,
            content: "Original verified local result.".into(),
        }],
    }
}
fn revision(id: &str) -> Revision {
    Revision {
        id: RevisionId::new(id).unwrap(),
        memory: MemoryId::new("memory").unwrap(),
        owner: owner(),
        project: ProjectId::new("project").unwrap(),
        kind: KnowledgeKind::Knowledge,
        status: RevisionStatus::PendingReview,
        content: "Derived knowledge.".into(),
        prerequisites: vec![],
        sources: vec![SourceId::new("source").unwrap()],
        predecessor: None,
        model: "offline-fixture".into(),
        prompt_version: "fixture-v1".into(),
    }
}
fn service() -> ConsolidationService<InMemoryConsolidationStore> {
    ConsolidationService::new(InMemoryConsolidationStore::default())
}
#[test]
fn snapshot_replay_is_exact_and_originals_are_preserved() {
    let mut service = service();
    let original = snapshot("job");
    assert_eq!(service.create(original.clone()), Ok(JobReceipt::Created));
    assert_eq!(
        service.create(original.clone()),
        Ok(JobReceipt::AlreadyCreated)
    );
    let mut changed = original.clone();
    changed.sources[0].content = "changed".into();
    assert_eq!(service.create(changed), Err(ConsolidationError::Conflict));
    service
        .complete(&owner(), &job_id("job"), vec![revision("rev")])
        .unwrap();
    let job = service.get(&owner(), &job_id("job")).unwrap();
    assert_eq!(job.snapshot, original);
    assert_eq!(job.status, JobStatus::Completed);
    assert_eq!(job.revisions[0].status, RevisionStatus::PendingReview);
}
#[test]
fn completion_retry_is_idempotent_and_changed_output_conflicts() {
    let mut service = service();
    service.create(snapshot("job")).unwrap();
    let revisions = vec![revision("rev")];
    assert_eq!(
        service.complete(&owner(), &job_id("job"), revisions.clone()),
        Ok(JobReceipt::Completed)
    );
    assert_eq!(
        service.complete(&owner(), &job_id("job"), revisions.clone()),
        Ok(JobReceipt::AlreadyCompleted)
    );
    let mut changed = revisions;
    changed[0].content = "different".into();
    assert_eq!(
        service.complete(&owner(), &job_id("job"), changed),
        Err(ConsolidationError::Conflict)
    );
}
#[test]
fn snapshots_reject_cross_owner_project_future_and_duplicate_sources() {
    for mode in 0..4 {
        let mut service = service();
        let mut invalid = snapshot("job");
        match mode {
            0 => invalid.sources[0].owner = OwnerId::new("other").unwrap(),
            1 => invalid.sources[0].project = ProjectId::new("other").unwrap(),
            2 => invalid.sources[0].recorded_at_ms = 101,
            _ => invalid.sources.push(invalid.sources[0].clone()),
        }
        assert_eq!(
            service.create(invalid),
            Err(ConsolidationError::InvalidSnapshot)
        );
    }
}
#[test]
fn unsupported_or_cross_scope_derivation_keeps_job_pending() {
    let mut service = service();
    service.create(snapshot("job")).unwrap();
    for mode in 0..3 {
        let mut invalid = revision("rev");
        match mode {
            0 => invalid.owner = OwnerId::new("other").unwrap(),
            1 => invalid.project = ProjectId::new("other").unwrap(),
            _ => invalid.sources = vec![SourceId::new("not-in-snapshot").unwrap()],
        }
        assert_eq!(
            service.complete(&owner(), &job_id("job"), vec![invalid]),
            Err(ConsolidationError::InvalidRevision)
        );
    }
    assert_eq!(
        service.get(&owner(), &job_id("job")).unwrap().status,
        JobStatus::Pending
    );
    assert_eq!(
        service.get(&OwnerId::new("other").unwrap(), &job_id("job")),
        Err(ConsolidationError::Missing)
    );
}
#[test]
fn procedure_requires_prerequisites_and_successful_supporting_source() {
    let mut service = service();
    let mut failed = snapshot("failed");
    failed.sources[0].outcome = WorkOutcome::Failed;
    service.create(failed).unwrap();
    service.create(snapshot("success")).unwrap();
    let mut procedure = revision("procedure");
    procedure.kind = KnowledgeKind::Procedure;
    assert_eq!(
        service.complete(&owner(), &job_id("success"), vec![procedure.clone()]),
        Err(ConsolidationError::InvalidRevision)
    );
    procedure.prerequisites = vec!["Use only against disposable local fixtures.".into()];
    assert_eq!(
        service.complete(&owner(), &job_id("failed"), vec![procedure.clone()]),
        Err(ConsolidationError::InvalidRevision)
    );
    assert_eq!(
        service.complete(&owner(), &job_id("success"), vec![procedure]),
        Ok(JobReceipt::Completed)
    );
}
#[test]
fn invalid_batch_is_atomic_and_revision_ids_are_unique_across_jobs() {
    let mut service = service();
    service.create(snapshot("job")).unwrap();
    let mut invalid = revision("second");
    invalid.memory = MemoryId::new("other-memory").unwrap();
    invalid.content.clear();
    assert_eq!(
        service.complete(&owner(), &job_id("job"), vec![revision("rev"), invalid]),
        Err(ConsolidationError::InvalidRevision)
    );
    assert!(
        service
            .get(&owner(), &job_id("job"))
            .unwrap()
            .revisions
            .is_empty()
    );
    service
        .complete(&owner(), &job_id("job"), vec![revision("rev")])
        .unwrap();
    service.create(snapshot("next")).unwrap();
    let mut duplicate = revision("rev");
    duplicate.memory = MemoryId::new("other-memory").unwrap();
    assert_eq!(
        service.complete(&owner(), &job_id("next"), vec![duplicate]),
        Err(ConsolidationError::Conflict)
    );
}
#[test]
fn version_history_preserves_predecessors_and_rejects_forks() {
    let mut service = service();
    service.create(snapshot("first")).unwrap();
    service
        .complete(&owner(), &job_id("first"), vec![revision("v1")])
        .unwrap();
    service.create(snapshot("second")).unwrap();
    let mut next = revision("v2");
    next.predecessor = Some(RevisionId::new("v1").unwrap());
    next.content = "Corrected version.".into();
    service
        .complete(&owner(), &job_id("second"), vec![next.clone()])
        .unwrap();
    assert_eq!(
        service.get(&owner(), &job_id("first")).unwrap().revisions[0].content,
        "Derived knowledge."
    );
    service.create(snapshot("fork")).unwrap();
    next.id = RevisionId::new("v3").unwrap();
    assert_eq!(
        service.complete(&owner(), &job_id("fork"), vec![next]),
        Err(ConsolidationError::Conflict)
    );
}
#[test]
fn missing_or_incompatible_predecessor_does_not_commit() {
    let mut service = service();
    service.create(snapshot("job")).unwrap();
    let mut invalid = revision("rev");
    invalid.predecessor = Some(RevisionId::new("missing").unwrap());
    assert_eq!(
        service.complete(&owner(), &job_id("job"), vec![invalid]),
        Err(ConsolidationError::InvalidRevision)
    );
    assert_eq!(
        service.get(&owner(), &job_id("job")).unwrap().status,
        JobStatus::Pending
    );
}
#[test]
fn storage_failure_never_returns_a_completion_receipt() {
    use lambda_memory::integrations::consolidation_store::ConsolidationStore;
    struct Broken;
    impl ConsolidationStore for Broken {
        fn create(&mut self, _: JobSnapshot) -> Result<JobReceipt, ConsolidationError> {
            Err(ConsolidationError::StorageUnavailable)
        }
        fn get(&self, _: &OwnerId, _: &JobId) -> Result<ConsolidationJob, ConsolidationError> {
            Err(ConsolidationError::StorageUnavailable)
        }
        fn complete(
            &mut self,
            _: &OwnerId,
            _: &JobId,
            _: Vec<Revision>,
        ) -> Result<JobReceipt, ConsolidationError> {
            Err(ConsolidationError::StorageUnavailable)
        }
    }
    let mut service = ConsolidationService::new(Broken);
    assert_eq!(
        service.create(snapshot("job")),
        Err(ConsolidationError::StorageUnavailable)
    );
    assert_eq!(
        service.complete(&owner(), &job_id("job"), vec![revision("rev")]),
        Err(ConsolidationError::StorageUnavailable)
    );
}
