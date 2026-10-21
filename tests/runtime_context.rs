use lambda_memory::domain_models::capture::{CaptureEvent, WorkEventKind, WorkOutcome};
use lambda_memory::domain_models::consolidation::{KnowledgeKind, Revision, RevisionStatus};
use lambda_memory::domain_models::runtime::{LifecycleState, RuntimeHit, RuntimeRevision};
use lambda_memory::domain_models::{ActorId, MemoryId, OwnerId, ProjectId, RevisionId, SourceId};
use lambda_memory::engines::runtime_policy::pack;

fn hit(id: &str, content: &str, sources: usize) -> RuntimeHit {
    let owner = OwnerId::new("owner").unwrap();
    let project = ProjectId::new("project").unwrap();
    let sources: Vec<_> = (0..sources)
        .map(|index| CaptureEvent {
            source: SourceId::new(format!("{id}-source-{index}")).unwrap(),
            owner: owner.clone(),
            actor: ActorId::new("actor").unwrap(),
            project: project.clone(),
            recorded_at_ms: 1,
            kind: WorkEventKind::Decision,
            outcome: WorkOutcome::Failed,
            origin: format!("fixture://{}", "x".repeat(2038)),
            content: content.into(),
        })
        .collect();
    RuntimeHit {
        item: RuntimeRevision {
            revision: Revision {
                id: RevisionId::new(format!("revision-{id}")).unwrap(),
                memory: MemoryId::new(id).unwrap(),
                owner,
                project,
                kind: KnowledgeKind::Knowledge,
                status: RevisionStatus::PendingReview,
                content: content.into(),
                prerequisites: Vec::new(),
                sources: sources.iter().map(|source| source.source.clone()).collect(),
                predecessor: None,
                model: "fixture".into(),
                prompt_version: "fixture".into(),
            },
            state: LifecycleState::Active,
            sources,
            reviewed_at_ms: Some(1),
            watermark_ms: 1,
        },
        lexical_score: 1,
    }
}

#[test]
fn long_origin_does_not_hide_a_short_fact_and_full_provenance_survives() {
    let original = hit(
        "database",
        "The database migration failed; rollback is required.",
        1,
    );
    let sources = original.item.sources.clone();
    let result = pack(vec![original], 1000);
    assert!(
        result
            .context
            .contains("The database migration failed; rollback is required.")
    );
    assert!(result.context.contains("database-source-0"));
    assert_eq!(result.hits[0].item.sources, sources);
    assert!(result.context.len() <= 1000 && result.truncated);
}

#[test]
fn a_long_first_fact_leaves_room_for_complementary_evidence_with_valid_utf8() {
    let first = hit(
        "database",
        &format!("Database migration: {}", "ação técnica; ".repeat(300)),
        1,
    );
    let second = hit(
        "backup",
        "Backup snapshots must be retained for thirty days.",
        1,
    );
    let result = pack(vec![first, second], 2000);
    assert_eq!(
        result
            .hits
            .iter()
            .map(|hit| hit.item.revision.memory.as_str())
            .collect::<Vec<_>>(),
        vec!["database", "backup"]
    );
    assert!(result.context.contains("Database migration:"));
    assert!(
        result
            .context
            .contains("Backup snapshots must be retained for thirty days.")
    );
    assert!(result.context.len() <= 2000 && result.truncated);
}

#[test]
fn many_sources_keep_exact_metadata_and_resolvable_revision_when_context_is_compact() {
    let original = hit("migration", "The database migration failed.", 32);
    let sources = original.item.sources.clone();
    let result = pack(vec![original], 1000);
    assert_eq!(result.hits[0].item.sources, sources);
    assert!(result.context.contains("The database migration failed."));
    assert!(result.context.contains("revision-migration") && result.context.contains("project"));
    assert!(result.context.len() <= 1000 && result.truncated);
}
