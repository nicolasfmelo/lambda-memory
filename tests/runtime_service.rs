#![cfg(feature = "memory-runtime")]
use lambda_memory::domain_models::{ActorId, OwnerId, ProjectId, SourceId};
use lambda_memory::domain_models::{MemoryId, RevisionId};
use lambda_memory::domain_models::{capture::*, runtime::*, runtime_diagnostics::*, timeline::*};
use lambda_memory::integrations::database::embedded_capture::{
    EmbeddedCaptureStore, EmbeddedRuntimeStore,
};
use lambda_memory::services::runtime_service::MemoryRuntime;
use std::sync::atomic::{AtomicUsize, Ordering};
static NEXT: AtomicUsize = AtomicUsize::new(0);
fn project(id: &str) -> ProjectId {
    ProjectId::new(id).unwrap()
}
async fn runtime() -> MemoryRuntime {
    runtime_with_idle(1).await
}
async fn runtime_with_idle(idle_ms: u64) -> MemoryRuntime {
    let path = format!(
        "{}/lambda-service-{}-{}",
        std::env::temp_dir().display(),
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    );
    let store = EmbeddedRuntimeStore::new(EmbeddedCaptureStore::open(&path).await.unwrap());
    let limits = RuntimeLimits {
        idle_ms,
        ..RuntimeLimits::default()
    };
    MemoryRuntime::new(
        store,
        RuntimeScope {
            owner: OwnerId::new("owner").unwrap(),
            actor: ActorId::new("actor").unwrap(),
            projects: vec![project("one"), project("two")],
        },
        limits,
    )
    .unwrap()
}
async fn dreamed(runtime: &MemoryRuntime) -> DreamReceipt {
    for _ in 0..100 {
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        let receipt = runtime.dream_once().await.result.unwrap();
        if receipt.sources != 0 {
            return receipt;
        }
    }
    panic!("idle batch never completed");
}
#[tokio::test]
async fn full_bound_failed_evidence_is_available_without_becoming_a_successful_procedure() {
    let runtime = runtime().await;
    let content = "x".repeat(8192);
    runtime
        .capture(
            &project("one"),
            SourceId::new("failure").unwrap(),
            WorkEventKind::Error,
            WorkOutcome::Failed,
            "fixture://failed".into(),
            content.clone(),
        )
        .await
        .result
        .unwrap();
    assert_eq!(
        dreamed(&runtime).await,
        DreamReceipt {
            batches: 1,
            sources: 1
        }
    );
    let available = runtime
        .list(listing(&project("one"), &[LifecycleState::Active], 8))
        .await
        .result
        .unwrap()
        .items;
    assert_eq!(available[0].revision.content, content);
    assert_eq!(available[0].sources[0].outcome, WorkOutcome::Failed);
    assert_eq!(
        available[0].revision.kind,
        lambda_memory::domain_models::consolidation::KnowledgeKind::Knowledge
    );
    // A full-bound memory exceeds the context budget: it is returned as a flagged excerpt.
    let recall = runtime
        .recall("x", &[project("one")], 2000)
        .await
        .result
        .unwrap();
    assert_eq!(recall.hits.len(), 1);
    assert!(recall.truncated && recall.context.len() <= 2000);
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    assert_eq!(
        runtime.dream_once().await.result.unwrap(),
        DreamReceipt::default()
    );
}
#[tokio::test]
async fn every_runtime_entrypoint_rejects_scope_widening_and_budget_overflow() {
    let runtime = runtime().await;
    let foreign = project("foreign");
    assert_eq!(
        runtime
            .capture(
                &foreign,
                SourceId::new("source").unwrap(),
                WorkEventKind::Decision,
                WorkOutcome::Unknown,
                "fixture://scope".into(),
                "Database choice.".into()
            )
            .await
            .result
            .unwrap_err(),
        RuntimeError::Unauthorized
    );
    assert_eq!(
        runtime
            .list(listing(&foreign, &[LifecycleState::Processing], 1))
            .await
            .result
            .unwrap_err(),
        RuntimeError::Unauthorized
    );
    assert_eq!(
        runtime
            .recall("database", &[project("one"), foreign.clone()], 300)
            .await
            .result
            .unwrap_err(),
        RuntimeError::Unauthorized
    );
    assert_eq!(
        runtime
            .purge(&foreign, &[SourceId::new("source").unwrap()])
            .await
            .result
            .unwrap_err(),
        RuntimeError::Unauthorized
    );
    assert!(matches!(
        runtime
            .recall(
                "database",
                &[project("one")],
                runtime.limits().context_bytes + 1
            )
            .await
            .result,
        Err(RuntimeError::InvalidInput(_))
    ));
    assert_eq!(
        runtime
            .recall("database", &[project("one"), project("one")], 300)
            .await
            .result
            .unwrap_err(),
        RuntimeError::Unauthorized
    );
}
#[tokio::test]
async fn recall_provenance_and_purge_are_uncached_across_authorized_projects() {
    let runtime = runtime().await;
    for name in ["one", "two"] {
        runtime
            .capture(
                &project(name),
                SourceId::new(name).unwrap(),
                WorkEventKind::Decision,
                WorkOutcome::Succeeded,
                "fixture://authorization".into(),
                format!("Database choice for {name}."),
            )
            .await
            .result
            .unwrap();
    }
    for _ in 0..2 {
        dreamed(&runtime).await;
    }
    let both = runtime
        .recall("database", &[project("one"), project("two")], 2000)
        .await
        .result
        .unwrap();
    assert_eq!(
        both.hits
            .iter()
            .map(|hit| hit.item.revision.project.as_str())
            .collect::<std::collections::BTreeSet<_>>(),
        ["one", "two"].into_iter().collect()
    );
    assert_eq!(
        both.hits
            .iter()
            .flat_map(|hit| &hit.item.sources)
            .map(|source| source.source.as_str())
            .collect::<std::collections::BTreeSet<_>>(),
        ["one", "two"].into_iter().collect()
    );
    assert!(
        both.hits
            .iter()
            .flat_map(|hit| &hit.item.sources)
            .all(|source| source.outcome == WorkOutcome::Succeeded)
    );
    assert!(
        runtime
            .recall("database", &[project("one")], 1)
            .await
            .result
            .unwrap()
            .context
            .is_empty()
    );
    runtime
        .purge(&project("one"), &[SourceId::new("one").unwrap()])
        .await
        .result
        .unwrap();
    let remaining = runtime
        .recall("database", &[project("one"), project("two")], 2000)
        .await
        .result
        .unwrap();
    assert_eq!(
        remaining
            .hits
            .iter()
            .map(|hit| hit.item.revision.project.as_str())
            .collect::<Vec<_>>(),
        vec!["two"]
    );
}

#[tokio::test]
async fn automatically_available_evidence_fits_the_default_recall_budget() {
    let runtime = runtime().await;
    let source = SourceId::new("550e8400-e29b-41d4-a716-446655440000/tool-call-001").unwrap();
    let content = "Database decision: use local Postgres.";
    runtime
        .capture(
            &project("one"),
            source.clone(),
            WorkEventKind::Decision,
            WorkOutcome::Unknown,
            "omp://command".into(),
            content.into(),
        )
        .await
        .result
        .unwrap();
    dreamed(&runtime).await;
    let available = runtime
        .list(listing(&project("one"), &[LifecycleState::Active], 8))
        .await
        .result
        .unwrap()
        .items;
    let revision = available[0].revision.id.clone();
    let budget = RuntimeLimits::default().context_bytes;
    let recalled = runtime
        .recall("database decision", &[project("one")], budget)
        .await
        .result
        .unwrap();
    assert_eq!(
        recalled
            .hits
            .iter()
            .map(|hit| &hit.item.revision.id)
            .collect::<Vec<_>>(),
        vec![&revision]
    );
    assert!(recalled.context.contains(content));
    assert!(recalled.context.contains(source.as_str()));
    assert!(recalled.context.len() <= budget);
}

#[tokio::test]
async fn explicit_correction_is_published_automatically_and_preserves_its_lineage() {
    use lambda_memory::domain_models::{MemoryId, RevisionId};
    let runtime = runtime().await;
    let project = project("one");
    for (source, content) in [
        ("old", "Use Postgres for the database."),
        ("new", "Replace Postgres with SQLite for the database."),
    ] {
        runtime
            .capture(
                &project,
                SourceId::new(source).unwrap(),
                WorkEventKind::Decision,
                WorkOutcome::Unknown,
                "fixture://automatic-correction".into(),
                content.into(),
            )
            .await
            .result
            .unwrap();
    }
    let first = runtime
        .propose(RevisionProposal {
            project: project.clone(),
            memory: MemoryId::new("database").unwrap(),
            revision: RevisionId::new("r1").unwrap(),
            sources: vec![SourceId::new("old").unwrap()],
            content: "Use Postgres for the database.".into(),
            predecessor: None,
        })
        .await
        .result
        .unwrap();
    assert_eq!(first.state, LifecycleState::Active);
    let second = runtime
        .propose(RevisionProposal {
            project: project.clone(),
            memory: first.revision.memory.clone(),
            revision: RevisionId::new("r2").unwrap(),
            sources: vec![SourceId::new("new").unwrap()],
            content: "Use SQLite for the database.".into(),
            predecessor: Some(first.revision.id.clone()),
        })
        .await
        .result
        .unwrap();
    assert_eq!(second.state, LifecycleState::Active);
    let recall = runtime
        .recall("database", std::slice::from_ref(&project), 2000)
        .await
        .result
        .unwrap();
    assert_eq!(
        recall
            .hits
            .iter()
            .map(|hit| hit.item.revision.id.as_str())
            .collect::<Vec<_>>(),
        vec!["r2"]
    );
    assert_eq!(recall.hits[0].item.sources[0].source.as_str(), "new");
    let history = runtime
        .list(listing(&project, &[LifecycleState::Superseded], 8))
        .await
        .result
        .unwrap()
        .items;
    assert_eq!(history[0].revision.id, first.revision.id);
    assert_eq!(history[0].sources[0].source.as_str(), "old");
}
#[tokio::test]
async fn a_successor_that_loses_its_promotion_race_is_retired_without_blocking_recall_or_dreaming()
{
    use lambda_memory::domain_models::{MemoryId, RevisionId};
    let runtime = runtime().await;
    let project = project("one");
    for source in ["root", "winner", "loser"] {
        runtime
            .capture(
                &project,
                SourceId::new(source).unwrap(),
                WorkEventKind::Decision,
                WorkOutcome::Unknown,
                "fixture://race".into(),
                format!("database {source} claim"),
            )
            .await
            .result
            .unwrap();
    }
    let proposal = |revision: &str, source: &str, predecessor: Option<&str>| RevisionProposal {
        project: project.clone(),
        memory: MemoryId::new("database").unwrap(),
        revision: RevisionId::new(revision).unwrap(),
        sources: vec![SourceId::new(source).unwrap()],
        content: format!("database {source} claim"),
        predecessor: predecessor.map(|id| RevisionId::new(id).unwrap()),
    };
    runtime
        .propose(proposal("r1", "root", None))
        .await
        .result
        .unwrap();
    runtime
        .propose(proposal("r2", "winner", Some("r1")))
        .await
        .result
        .unwrap();
    assert_eq!(
        runtime
            .propose(proposal("r3", "loser", Some("r1")))
            .await
            .result
            .unwrap_err(),
        RuntimeError::Conflict
    );
    let rejected = runtime
        .list(listing(&project, &[LifecycleState::Rejected], 8))
        .await
        .result
        .unwrap()
        .items;
    assert_eq!(
        rejected
            .iter()
            .map(|item| item.revision.id.as_str())
            .collect::<Vec<_>>(),
        vec!["r3"]
    );
    assert!(
        runtime
            .list(listing(&project, &[LifecycleState::Processing], 8))
            .await
            .result
            .unwrap()
            .items
            .is_empty()
    );
    let recall = runtime
        .recall("database", std::slice::from_ref(&project), 2000)
        .await
        .result
        .unwrap();
    assert_eq!(
        recall
            .hits
            .iter()
            .map(|hit| hit.item.revision.id.as_str())
            .collect::<Vec<_>>(),
        vec!["r2"]
    );
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    runtime.dream_once().await.result.unwrap();
}
#[tokio::test]
async fn memory_identities_are_independent_per_project() {
    use lambda_memory::domain_models::{MemoryId, RevisionId};
    let runtime = runtime().await;
    for (name, revision) in [("one", "r-one"), ("two", "r-two")] {
        runtime
            .capture(
                &project(name),
                SourceId::new(format!("s-{name}")).unwrap(),
                WorkEventKind::Decision,
                WorkOutcome::Unknown,
                "fixture://lineage".into(),
                format!("{name} deploy notes"),
            )
            .await
            .result
            .unwrap();
        let created = runtime
            .propose(RevisionProposal {
                project: project(name),
                memory: MemoryId::new("deploy-notes").unwrap(),
                revision: RevisionId::new(revision).unwrap(),
                sources: vec![SourceId::new(format!("s-{name}")).unwrap()],
                content: format!("{name} deploy notes"),
                predecessor: None,
            })
            .await
            .result
            .unwrap();
        assert_eq!(created.state, LifecycleState::Active);
    }
}
#[tokio::test]
async fn an_oversized_memory_is_recalled_as_a_bounded_excerpt_and_flagged() {
    use lambda_memory::domain_models::{MemoryId, RevisionId};
    let runtime = runtime().await;
    let project = project("one");
    let content = format!("database migration notes {}", "detail ".repeat(150));
    runtime
        .capture(
            &project,
            SourceId::new("large").unwrap(),
            WorkEventKind::Decision,
            WorkOutcome::Unknown,
            "fixture://large".into(),
            content.clone(),
        )
        .await
        .result
        .unwrap();
    runtime
        .propose(RevisionProposal {
            project: project.clone(),
            memory: MemoryId::new("large").unwrap(),
            revision: RevisionId::new("r-large").unwrap(),
            sources: vec![SourceId::new("large").unwrap()],
            content,
            predecessor: None,
        })
        .await
        .result
        .unwrap();
    let recall = runtime
        .recall("database migration", std::slice::from_ref(&project), 1000)
        .await
        .result
        .unwrap();
    assert_eq!(recall.hits.len(), 1);
    assert!(recall.truncated);
    assert!(recall.context.len() <= 1000);
    assert!(recall.context.contains("[truncated]") && recall.context.contains("Source: large"));
    let roomy = runtime
        .recall("database migration", std::slice::from_ref(&project), 2000)
        .await
        .result
        .unwrap();
    assert!(!roomy.truncated);
}

fn timeline_draft(project_id: &str, entry: &str, source: &str) -> TimelineDraft {
    TimelineDraft {
        project: project(project_id),
        entry: TimelineEntryId::new(entry).unwrap(),
        expected_version: None,
        title: "Database".into(),
        summary: "Chose the embedded store.".into(),
        occurred_at_ms: 1_700_000_000_000,
        status: TimelineStatus::Adopted,
        sources: vec![SourceId::new(source).unwrap()],
        revisions: Vec::new(),
        memories: Vec::new(),
        supersedes: None,
        tags: Vec::new(),
    }
}
fn timeline_query(project_id: &str) -> TimelineQuery {
    TimelineQuery {
        project: project(project_id),
        limit: TIMELINE_MAX_PAGE,
        after: None,
        statuses: Vec::new(),
        descending: false,
        entry: None,
    }
}
#[tokio::test]
async fn timeline_is_authorized_validated_traced_and_reads_do_not_postpone_idle() {
    let runtime = runtime_with_idle(60_000).await;
    runtime.initialize_diagnostics().await.unwrap();
    assert_eq!(
        runtime
            .timeline_write(timeline_draft("foreign", "a", "s"))
            .await
            .result
            .unwrap_err(),
        RuntimeError::Unauthorized
    );
    assert_eq!(
        runtime
            .timeline(timeline_query("foreign"))
            .await
            .unwrap_err(),
        RuntimeError::Unauthorized
    );
    for limit in [0, TIMELINE_MAX_PAGE + 1] {
        let mut query = timeline_query("one");
        query.limit = limit;
        assert!(matches!(
            runtime.timeline(query).await,
            Err(RuntimeError::InvalidInput(_))
        ));
    }
    runtime
        .capture(
            &project("one"),
            SourceId::new("s").unwrap(),
            WorkEventKind::Decision,
            WorkOutcome::Unknown,
            "fixture://timeline".into(),
            "decision".into(),
        )
        .await
        .result
        .unwrap();
    // Duplicate links are pure-policy failures before the store is reached.
    let mut duplicate = timeline_draft("one", "a", "s");
    duplicate.memories = vec![MemoryId::new("m").unwrap(), MemoryId::new("m").unwrap()];
    assert_eq!(
        runtime.timeline_write(duplicate).await.result.unwrap_err(),
        RuntimeError::InvalidInput("timeline memories")
    );
    let mut oversized = timeline_draft("one", "a", "s");
    oversized.summary = "x".repeat(runtime.limits().max_content_bytes + 1);
    assert_eq!(
        runtime.timeline_write(oversized).await.result.unwrap_err(),
        RuntimeError::InvalidInput("timeline summary")
    );
    let receipt = runtime
        .timeline_write(timeline_draft("one", "a", "s"))
        .await
        .result
        .unwrap();
    assert_eq!(receipt.write, TimelineWrite::Recorded);
    let traced = runtime
        .activity(ActivityRequest {
            project: project("one"),
            limit: 32,
            operations: vec![ActivityOperation::Timeline],
            outcomes: Vec::new(),
            cursor: None,
        })
        .await
        .unwrap();
    let operation = traced
        .entries
        .iter()
        .find(|e| {
            e.entry_kind == ActivityEntryKind::Operation && e.outcome == ActivityOutcome::Succeeded
        })
        .expect("timeline operation traced");
    assert_eq!(operation.operation, ActivityOperation::Timeline);
    assert!(traced.entries.iter().any(|e| e.entry_kind == ActivityEntryKind::Stage && e.phase == ActivityPhase::Timeline));
    let before = runtime
        .status(&project("one"))
        .await
        .unwrap()
        .worker
        .dreaming
        .remaining_idle_ms
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    let page = runtime.timeline(timeline_query("one")).await.unwrap();
    assert_eq!(page.entries.len(), 1);
    let after = runtime
        .status(&project("one"))
        .await
        .unwrap()
        .worker
        .dreaming
        .remaining_idle_ms
        .unwrap();
    assert!(
        after + 25 <= before,
        "timeline read postponed idle: {before} -> {after}"
    );
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

#[tokio::test]
async fn rejected_pending_root_can_be_corrected_and_losing_successor_does_not_block_recall() {
    let path = format!(
        "/tmp/lambda-rejected-root-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    );
    let store = EmbeddedRuntimeStore::new(EmbeddedCaptureStore::open(&path).await.unwrap());
    let owner = OwnerId::new("owner").unwrap();
    let actor = ActorId::new("actor").unwrap();
    let project = project("one");
    let limits = RuntimeLimits {
        idle_ms: 86_400_000,
        ..Default::default()
    };
    for source in ["root", "winner", "loser", "healthy"] {
        store
            .capture(
                CaptureEvent {
                    source: SourceId::new(source).unwrap(),
                    owner: owner.clone(),
                    actor: actor.clone(),
                    project: project.clone(),
                    recorded_at_ms: 1,
                    kind: WorkEventKind::Decision,
                    outcome: WorkOutcome::Unknown,
                    origin: "fixture://rejected-root".into(),
                    content: format!("database {source} evidence"),
                },
                &limits,
            )
            .await
            .unwrap();
    }
    let proposal =
        |revision: &str, source: &str, memory: &str, predecessor: Option<&str>| RevisionProposal {
            project: project.clone(),
            memory: MemoryId::new(memory).unwrap(),
            revision: RevisionId::new(revision).unwrap(),
            sources: vec![SourceId::new(source).unwrap()],
            content: format!("database {source} evidence"),
            predecessor: predecessor.map(|value| RevisionId::new(value).unwrap()),
        };
    let root = store
        .propose(
            &owner,
            &actor,
            proposal("r-root", "root", "database", None),
            2,
        )
        .await
        .unwrap();
    assert_eq!(root.state, LifecycleState::Processing);
    store
        .review(
            &owner,
            &actor,
            &ReviewRequest {
                project: project.clone(),
                revision: root.revision.id.clone(),
                action: ReviewAction::Reject,
                expected_active: None,
                conflict_with: None,
            },
            3,
        )
        .await
        .unwrap();
    let runtime = MemoryRuntime::new(
        store.clone(),
        RuntimeScope {
            owner: owner.clone(),
            actor,
            projects: vec![project.clone()],
        },
        limits,
    )
    .unwrap();
    let corrected = runtime
        .propose(proposal("r-winner", "winner", "database", Some("r-root")))
        .await
        .result
        .unwrap();
    assert_eq!(corrected.state, LifecycleState::Active);
    assert_eq!(corrected.revision.predecessor, Some(root.revision.id));
    assert_eq!(
        runtime
            .propose(proposal("r-loser", "loser", "database", Some("r-root")))
            .await
            .result
            .unwrap_err(),
        RuntimeError::Conflict
    );
    let loser = store
        .runtime_revision(&owner, &project, &RevisionId::new("r-loser").unwrap())
        .await
        .unwrap();
    assert_eq!(loser.state, LifecycleState::Rejected);
    runtime
        .propose(proposal("r-healthy", "healthy", "healthy", None))
        .await
        .result
        .unwrap();
    let recalled = runtime
        .recall("healthy", std::slice::from_ref(&project), 2000)
        .await
        .result
        .unwrap();
    assert_eq!(
        recalled
            .hits
            .iter()
            .map(|hit| hit.item.revision.id.as_str())
            .collect::<Vec<_>>(),
        vec!["r-healthy"]
    );
    assert!(
        runtime
            .list(listing(&project, &[LifecycleState::Processing], 8))
            .await
            .result
            .unwrap()
            .items
            .is_empty()
    );
}

async fn playbook_version(
    store: &EmbeddedRuntimeStore,
    revision: &str,
    text: &str,
    predecessor: Option<&str>,
) -> RuntimeRevision {
    let owner = OwnerId::new("owner").unwrap();
    let actor = ActorId::new("actor").unwrap();
    let source = SourceId::new(format!("source-{revision}")).unwrap();
    store.capture(CaptureEvent {
        source: source.clone(), owner: owner.clone(), actor: actor.clone(), project: project("one"),
        origin: "omp://action".into(), recorded_at_ms: 10, kind: WorkEventKind::Action,
        outcome: WorkOutcome::Succeeded,
        content: r#"{"v":1,"tool":"bash","intent":"","summary":"cargo test","error_head":null,"session":"0000000000000000","seq":1}"#.into(),
    }, &RuntimeLimits::default()).await.unwrap();
    store
        .propose(
            &owner,
            &actor,
            RevisionProposal {
                project: project("one"),
                memory: MemoryId::new("procedural:playbook").unwrap(),
                revision: RevisionId::new(revision).unwrap(),
                sources: vec![source],
                content: text.into(),
                predecessor: predecessor.map(|value| RevisionId::new(value).unwrap()),
            },
            100,
        )
        .await
        .unwrap()
}
fn review_of(item: &RuntimeRevision, action: ReviewAction) -> ReviewRequest {
    ReviewRequest {
        project: project("one"),
        revision: item.revision.id.clone(),
        action,
        expected_active: None,
        conflict_with: None,
    }
}

#[tokio::test]
async fn rejecting_playbook_version_keeps_previous_active() {
    let path = format!(
        "/tmp/lambda-playbook-reject-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    );
    let store = EmbeddedRuntimeStore::new(EmbeddedCaptureStore::open(&path).await.unwrap());
    let runtime = MemoryRuntime::new(
        store.clone(),
        RuntimeScope {
            owner: OwnerId::new("owner").unwrap(),
            actor: ActorId::new("actor").unwrap(),
            projects: vec![project("one")],
        },
        RuntimeLimits::default(),
    )
    .unwrap();
    let first = playbook_version(&store, "playbook-one", "- Run cargo test [F1]", None).await;
    runtime
        .review(review_of(&first, ReviewAction::Promote))
        .await
        .result
        .unwrap();
    let second = playbook_version(
        &store,
        "playbook-two",
        "- Run cargo test --all [F1]",
        Some("playbook-one"),
    )
    .await;
    assert_eq!(second.state, LifecycleState::Processing);
    runtime
        .review(review_of(&second, ReviewAction::Reject))
        .await
        .result
        .unwrap();
    let view = runtime.playbook(&project("one")).await.unwrap();
    assert_eq!(view.revision, Some(first.revision.id));
    assert_eq!(view.content.as_deref(), Some("- Run cargo test [F1]"));
    assert_eq!(view.episodes, 2);
}

#[tokio::test]
async fn playbook_read_is_side_effect_free() {
    let runtime = runtime_with_idle(60_000).await;
    assert_eq!(
        runtime.playbook(&project("one")).await.unwrap(),
        Default::default()
    );
    assert_eq!(
        runtime.playbook(&project("foreign")).await.unwrap_err(),
        RuntimeError::Unauthorized
    );
    runtime.initialize_diagnostics().await.unwrap();
    let before = runtime
        .status(&project("one"))
        .await
        .unwrap()
        .worker
        .dreaming
        .remaining_idle_ms
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    let view = runtime.playbook(&project("one")).await.unwrap();
    assert_eq!(
        (
            view.revision,
            view.content,
            view.generated_at_ms,
            view.episodes
        ),
        (None, None, None, 0)
    );
    let after = runtime
        .status(&project("one"))
        .await
        .unwrap()
        .worker
        .dreaming
        .remaining_idle_ms
        .unwrap();
    assert!(
        after + 25 <= before,
        "playbook read postponed idle: {before} -> {after}"
    );
}
