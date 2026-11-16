use super::*;
use crate::domain_models::capture::{CaptureEvent, WorkEventKind, WorkOutcome};

#[tokio::test]
async fn invalid_auto_link_is_skipped_without_aborting_transaction() {
    let path =
        std::env::temp_dir().join(format!("lambda-timeline-link-check-{}", std::process::id()));
    let capture = EmbeddedCaptureStore::open(path.to_str().unwrap())
        .await
        .unwrap();
    let store = EmbeddedRuntimeStore::new(capture).with_timeline_policy(
        TimelineAutoPolicy {
            drafts: |_, _, _, _, _, _, _| Ok(vec![]),
            validate: |_, _, _| Ok(()),
        },
        8192,
    );
    let owner = OwnerId::new("owner").unwrap();
    let actor = ActorId::new("actor").unwrap();
    let project = ProjectId::new("project").unwrap();
    let source = SourceId::new("source").unwrap();
    let now = TIMELINE_MIN_OCCURRED_MS + 1000;
    store
        .capture(
            CaptureEvent {
                owner: owner.clone(),
                actor: actor.clone(),
                project: project.clone(),
                source: source.clone(),
                kind: WorkEventKind::Decision,
                outcome: WorkOutcome::Unknown,
                origin: "fixture://auto".into(),
                recorded_at_ms: now,
                content: "Decision evidence".into(),
            },
            &RuntimeLimits::default(),
        )
        .await
        .unwrap();
    let mut draft = TimelineDraft {
        project: project.clone(),
        entry: TimelineEntryId::new("manual").unwrap(),
        expected_version: None,
        title: "Decision".into(),
        summary: "Decision summary".into(),
        occurred_at_ms: now,
        status: TimelineStatus::Adopted,
        sources: vec![source],
        revisions: vec![],
        memories: vec![],
        supersedes: None,
        tags: vec![],
    };
    let tx = store.capture.db.clone().begin().await.unwrap();
    // A separate valid write in this transaction proves the transaction still commits.
    store
        .timeline_write_tx(&tx, &owner, &actor, &draft, now)
        .await
        .unwrap();
    draft.entry = TimelineEntryId::new("auto:missing-node:missing-revision").unwrap();
    draft.tags.push(TIMELINE_AUTO_TAG.into());
    draft
        .revisions
        .push(RevisionId::new("missing-revision").unwrap());
    assert_eq!(
        store
            .auto_timeline_apply_tx(&tx, &owner, &actor, &[draft], now, 8192)
            .await
            .unwrap(),
        1
    );
    tx.commit().await.unwrap();
    let page = store
        .timeline_page(
            &owner,
            &TimelineQuery {
                project,
                limit: 32,
                after: None,
                statuses: vec![],
                descending: false,
                entry: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(page.entries.len(), 1);
    assert_eq!(page.entries[0].entry.as_str(), "manual");
    drop(store);
    std::fs::remove_dir_all(path).unwrap();
}
