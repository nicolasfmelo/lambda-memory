use lambda_memory::domain_models::capture::{CaptureEvent, WorkEventKind, WorkOutcome};
use lambda_memory::domain_models::consolidation::{KnowledgeKind, Revision, RevisionStatus};
use lambda_memory::domain_models::runtime::{LifecycleState, RuntimeHit, RuntimeRevision};
use lambda_memory::domain_models::{ActorId, MemoryId, OwnerId, ProjectId, RevisionId, SourceId};
use lambda_memory::engines::runtime_policy::pack;

#[test]
fn packed_context_labels_concluded_confidence() {
    use lambda_memory::domain_models::memory_tree::{Basis, Confidence, TreeHit, TreeRoot};
    use lambda_memory::engines::{reflection_policy, runtime_policy::pack_with_paths};
    for confidence in [Confidence::Low, Confidence::Medium, Confidence::High] {
        let hit = hit("inference", "Inferred guidance.", 1);
        let content = format!(
            "{}{}",
            reflection_policy::label(Basis::Concluded, Some(confidence)),
            hit.item.revision.content
        );
        let tree = TreeHit {
            basis: Basis::Concluded,
            confidence: Some(confidence),
            derived_from: vec![],
            content: content.clone(),
            valid_from_ms: None,
            replaced: None,
            root: TreeRoot::Preferences,
            node: hit.item.revision.memory.clone(),
            hit: hit.clone(),
            path: vec!["Preferences".into()],
            source_count: 1,
        };
        let recall = pack_with_paths(vec![hit], &[tree], 8192);
        assert!(recall.context.contains(&format!(
            "Concluded ({}): Inferred guidance.",
            confidence.as_str()
        )));
        assert_eq!(recall.tree_hits[0].content, content);
    }
}
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

#[test]
fn recall_returns_at_most_two_lessons() {
    use lambda_memory::domain_models::memory_tree::{TreeHit, TreeRoot};
    use lambda_memory::engines::runtime_policy::pack_with_paths;
    let roots = [
        TreeRoot::Lessons,
        TreeRoot::Decisions,
        TreeRoot::GeneralLessons,
        TreeRoot::Lessons,
        TreeRoot::Preferences,
        TreeRoot::GeneralLessons,
    ];
    let tree: Vec<_> = roots
        .into_iter()
        .enumerate()
        .map(|(i, root)| {
            let mut hit = hit(&format!("node-{i}"), "A remembered fact.", 1);
            if root == TreeRoot::GeneralLessons {
                hit.item.revision.project = ProjectId::new("user:owner").unwrap();
            }
            TreeHit {
                basis: Default::default(),
                confidence: None,
                derived_from: vec![],
                content: hit.item.revision.content.clone(),
                valid_from_ms: None,
                replaced: None,
                root,
                node: hit.item.revision.memory.clone(),
                hit,
                path: vec![root.label().into(), "Fact".into()],
                source_count: 1,
            }
        })
        .collect();
    let result = pack_with_paths(tree.iter().map(|t| t.hit.clone()).collect(), &tree, 30_000);
    assert_eq!(
        result
            .tree_hits
            .iter()
            .filter(|t| t.root.is_lesson())
            .count(),
        2
    );
    assert_eq!(
        result
            .hits
            .iter()
            .map(|h| h.item.revision.memory.as_str())
            .collect::<Vec<_>>(),
        ["node-0", "node-1", "node-2", "node-4"]
    );
    assert!(!result.context.contains("node-3") && !result.context.contains("node-5"));
}

#[test]
fn recall_renders_since_and_replaced() {
    use lambda_memory::domain_models::memory_tree::{ReplacedPosition, TreeHit, TreeRoot};
    use lambda_memory::engines::{runtime_policy::pack_with_paths, tree_policy::validity_note};
    let start = 1_735_689_600_000;
    for replaced in [
        None,
        Some(ReplacedPosition {
            title: "Earlier position".into(),
            valid_until_ms: start,
        }),
    ] {
        let hit = hit("current", "Current position only.", 1);
        let note = validity_note(Some(start), replaced.as_ref());
        let content = format!("{}{}", hit.item.revision.content, note);
        let tree = TreeHit {
            basis: Default::default(),
            confidence: None,
            derived_from: vec![],
            content: content.clone(),
            valid_from_ms: Some(start),
            replaced,
            root: TreeRoot::Decisions,
            node: hit.item.revision.memory.clone(),
            hit: hit.clone(),
            path: vec!["Decisions".into()],
            source_count: 1,
        };
        let result = pack_with_paths(vec![hit.clone()], std::slice::from_ref(&tree), 4096);
        assert_eq!(result.hits.len(), 1);
        assert!(result.context.contains(&content));
        assert!(result.tree_hits[0].content.ends_with(&note));
        assert_eq!(
            result.hits[0].item.revision.content,
            "Current position only."
        );
        let too_small = pack_with_paths(vec![hit], &[tree], 64);
        assert!(too_small.hits.is_empty());
        assert!(too_small.truncated);
    }
}
