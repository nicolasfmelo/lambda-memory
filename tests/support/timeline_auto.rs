use super::*;
use crate::domain_models::runtime_diagnostics::{ActivityReason, ActivityRequest};
use crate::domain_models::timeline::*;

#[tokio::test]
async fn position_ids_stay_unique_after_purging_the_original_source() {
    let fake = Fake::start();
    let runtime = setup(&fake).await;
    let node = seed(&runtime, &fake).await;
    let original = entries(&runtime).await.remove(0);
    let event = evidence(&runtime, "first-change", JAN_1 + DAY).await;
    curate(
        &runtime,
        &fake,
        vec![event],
        vec![vec![update(
            "supersede",
            "first change",
            Some("2025-01-02"),
        )]],
    )
    .await;
    let first_change = adopted(&entries(&runtime).await).clone();
    runtime
        .store
        .purge(
            &runtime.scope.owner,
            &runtime.scope.projects[0],
            &[SourceId::new("original").unwrap()],
        )
        .await
        .unwrap();
    assert!(versions(&runtime, &original.entry).await.is_empty());
    let event = evidence(&runtime, "second-change", JAN_1 + 2 * DAY).await;
    curate(
        &runtime,
        &fake,
        vec![event],
        vec![vec![update(
            "supersede",
            "second change",
            Some("2025-01-03"),
        )]],
    )
    .await;
    let latest = adopted(&entries(&runtime).await).clone();
    let positions = [original, first_change, latest];
    let ids: std::collections::HashSet<_> = positions.iter().map(|entry| &entry.entry).collect();
    assert_eq!(ids.len(), 3);
    for entry in &positions {
        assert_eq!(
            entry.entry.as_str(),
            format!("auto:{}:{}", node.memory, entry.revisions[0])
        );
        let mut seen = std::collections::HashSet::new();
        let mut cursor = Some(&entry.entry);
        while let Some(id) = cursor {
            assert!(seen.insert(id), "supersedes cycle at {id}");
            cursor = positions
                .iter()
                .find(|position| &position.entry == id)
                .and_then(|position| position.supersedes.as_ref());
        }
    }
    assert_eq!(positions[2].supersedes, Some(positions[1].entry.clone()));
}

#[tokio::test]
async fn rejecting_a_loser_after_the_winner_is_retired_does_not_restore_predecessor() {
    let fake = Fake::start();
    let runtime = setup(&fake).await;
    let node = seed(&runtime, &fake).await;
    let original = entries(&runtime).await.remove(0);
    let a = pending_update(&runtime, &node, "A", Some(JAN_1 + DAY)).await;
    let b = pending_update(&runtime, &node, "B", Some(JAN_1 + 2 * DAY)).await;
    review(
        &runtime,
        &b.revision.id,
        ReviewAction::Promote,
        Some(node.revision.clone()),
        None,
    )
    .await;
    runtime
        .store
        .retire_node(
            &runtime.scope.owner,
            &runtime.scope.actor,
            &runtime.scope.projects[0],
            &node.memory,
            now_ms().unwrap(),
        )
        .await
        .unwrap();
    review(&runtime, &a.revision.id, ReviewAction::Reject, None, None).await;
    let all = entries(&runtime).await;
    assert!(
        all.iter()
            .all(|entry| entry.status != TimelineStatus::Adopted)
    );
    let predecessor = all
        .iter()
        .find(|entry| entry.entry == original.entry)
        .unwrap();
    assert_eq!(
        (predecessor.version, predecessor.status),
        (2, TimelineStatus::Superseded)
    );
    assert!(
        all.iter()
            .filter(|entry| entry.entry != original.entry)
            .all(|entry| entry.status == TimelineStatus::Retracted)
    );
}

#[tokio::test]
async fn rollback_draft_validation_failures_are_recorded_as_skipped() {
    use crate::domain_models::runtime_diagnostics::ActivityEntryKind;
    for path in ["reject", "retire-stale", "retire-node"] {
        let fake = Fake::start();
        let mut runtime = setup(&fake).await;
        let node = seed(&runtime, &fake).await;
        let pending = pending_update(&runtime, &node, "pending", Some(JAN_1 + DAY)).await;
        if path == "retire-stale" {
            let winner = pending_update(&runtime, &node, "winner", None).await;
            review(
                &runtime,
                &winner.revision.id,
                ReviewAction::Promote,
                Some(node.revision.clone()),
                None,
            )
            .await;
        }
        // Entries were valid when written, but a lower bound now makes rollback drafts invalid.
        runtime.store = runtime
            .store
            .clone()
            .with_timeline_policy(crate::engines::timeline_policy::AUTO_POLICY, 1);
        let (operation, expected_skips) = if path == "reject" {
            let result = runtime
                .review(ReviewRequest {
                    project: runtime.scope.projects[0].clone(),
                    revision: pending.revision.id.clone(),
                    action: ReviewAction::Reject,
                    expected_active: None,
                    conflict_with: None,
                })
                .await;
            assert_eq!(result.result.unwrap().state, LifecycleState::Rejected);
            (ActivityOperation::Review, 2)
        } else {
            let mut trace = runtime
                .begin_trace(
                    &runtime.scope.projects,
                    ActivityOperation::Dream,
                    ActivityPhase::TreeRebuild,
                    true,
                )
                .await
                .unwrap();
            if path == "retire-stale" {
                assert!(matches!(
                    runtime.prepare_revision(pending.clone(), &mut trace).await,
                    Err(RuntimeError::Conflict)
                ));
                assert_eq!(
                    runtime
                        .store
                        .runtime_revision(
                            &runtime.scope.owner,
                            &runtime.scope.projects[0],
                            &pending.revision.id
                        )
                        .await
                        .unwrap()
                        .state,
                    LifecycleState::Rejected
                );
            } else {
                let skipped = runtime
                    .store
                    .retire_node_with_timeline_skips(
                        &runtime.scope.owner,
                        &runtime.scope.actor,
                        &runtime.scope.projects[0],
                        &node.memory,
                        now_ms().unwrap(),
                    )
                    .await
                    .unwrap();
                trace.timeline_skipped(&runtime.scope.projects[0], skipped);
                assert!(
                    runtime
                        .store
                        .tree_nodes(&runtime.scope.owner, &runtime.scope.projects[0])
                        .await
                        .unwrap()
                        .is_empty()
                );
            }
            runtime.finish_trace(trace, Ok(())).await.result.unwrap();
            // retire-stale also restores the predecessor's entry (the node stays active), so both
            // rollback drafts are skipped; retiring the node only retracts its entry.
            (
                ActivityOperation::Dream,
                if path == "retire-stale" { 2 } else { 1 },
            )
        };
        let activity = runtime
            .activity(ActivityRequest {
                project: runtime.scope.projects[0].clone(),
                limit: 32,
                operations: vec![operation],
                outcomes: vec![],
                cursor: None,
            })
            .await
            .unwrap();
        let root = activity
            .entries
            .iter()
            .find(|entry| {
                entry.entry_kind == ActivityEntryKind::Operation
                    && entry.reason == Some(ActivityReason::TimelineAutoSkipped)
            })
            .unwrap();
        assert_eq!(
            root.counts.operations_dropped,
            Some(expected_skips),
            "{path}"
        );
        if path == "reject" {
            let stage = activity
                .entries
                .iter()
                .find(|entry| {
                    entry.entry_kind == ActivityEntryKind::Stage
                        && entry.reason == Some(ActivityReason::TimelineAutoSkipped)
                })
                .unwrap();
            assert_eq!(stage.counts.operations_dropped, Some(expected_skips));
        }
        // Failed automation must not abort lifecycle effects or write partial timeline versions.
        assert!(
            entries(&runtime)
                .await
                .iter()
                .any(|entry| entry.status == TimelineStatus::Adopted)
        );
    }
}

async fn entries(runtime: &MemoryRuntime) -> Vec<TimelineEntry> {
    runtime
        .timeline(TimelineQuery {
            project: runtime.scope.projects[0].clone(),
            limit: 32,
            after: None,
            statuses: vec![],
            descending: false,
            entry: None,
        })
        .await
        .unwrap()
        .entries
}
async fn versions(runtime: &MemoryRuntime, entry: &TimelineEntryId) -> Vec<TimelineEntry> {
    runtime
        .timeline(TimelineQuery {
            project: runtime.scope.projects[0].clone(),
            limit: 32,
            after: None,
            statuses: vec![],
            descending: false,
            entry: Some(entry.clone()),
        })
        .await
        .unwrap()
        .entries
}
fn adopted(entries: &[TimelineEntry]) -> &TimelineEntry {
    entries
        .iter()
        .find(|e| e.status == TimelineStatus::Adopted)
        .unwrap()
}

#[tokio::test]
async fn create_of_decision_writes_auto_entry() {
    let fake = Fake::start();
    let runtime = setup(&fake).await;
    let node = seed(&runtime, &fake).await;
    let all = entries(&runtime).await;
    assert_eq!(all.len(), 1);
    let entry = &all[0];
    assert_eq!(
        entry.entry.as_str(),
        format!("auto:{}:{}", node.memory, node.revision)
    );
    assert_eq!(entry.version, 1);
    assert_eq!(entry.status, TimelineStatus::Adopted);
    assert_eq!(entry.tags, [TIMELINE_AUTO_TAG]);
    assert_eq!(entry.memories, [node.memory]);
    assert_eq!(entry.revisions, [node.revision]);
    assert_eq!(entry.sources, [SourceId::new("original").unwrap()]);
    assert_eq!(entry.occurred_at_ms, JAN_1);
    assert_eq!(
        (entry.title.as_str(), entry.summary.as_str()),
        ("SQLite original", "SQLite original position")
    );
}

#[tokio::test]
async fn create_under_preferences_writes_nothing() {
    let fake = Fake::start();
    let runtime = setup(&fake).await;
    let event = evidence(&runtime, "preference", JAN_1).await;
    curate(&runtime, &fake, vec![event], vec![vec![json!({"op":"create","node":null,"parent":"preferences","title":"SQLite preference","content":"SQLite preference","valid_from":null})]]).await;
    assert!(entries(&runtime).await.is_empty());
}

#[tokio::test]
async fn supersede_closes_old_entry_and_opens_new_one() {
    let fake = Fake::start();
    let runtime = setup(&fake).await;
    let node = seed(&runtime, &fake).await;
    let original = entries(&runtime).await.remove(0);
    let event = evidence(&runtime, "changed", JAN_1 + 3 * DAY).await;
    curate(
        &runtime,
        &fake,
        vec![event],
        vec![vec![update("supersede", "changed", Some("2025-01-02"))]],
    )
    .await;
    let all = entries(&runtime).await;
    let old = all.iter().find(|e| e.entry == original.entry).unwrap();
    assert_eq!((old.version, old.status), (2, TimelineStatus::Superseded));
    assert_eq!(old.summary, original.summary);
    let new = adopted(&all);
    let current = current(&runtime).await;
    assert_eq!(
        new.entry.as_str(),
        format!("auto:{}:{}", node.memory, current.revision)
    );
    assert_eq!(new.version, 1);
    assert_eq!(new.supersedes, Some(original.entry));
    assert_eq!(new.revisions, [current.revision]);
    assert_eq!(new.occurred_at_ms, JAN_1 + DAY);
    assert_eq!(new.sources, [SourceId::new("changed").unwrap()]);
}

#[tokio::test]
async fn merge_updates_summary_keeps_status() {
    let fake = Fake::start();
    let runtime = setup(&fake).await;
    seed(&runtime, &fake).await;
    let original = entries(&runtime).await.remove(0);
    let event = evidence(&runtime, "refine", JAN_1 + DAY).await;
    curate(
        &runtime,
        &fake,
        vec![event],
        vec![vec![update("merge", "refined", None)]],
    )
    .await;
    let all = entries(&runtime).await;
    assert_eq!(all.len(), 1);
    let entry = &all[0];
    assert_eq!(entry.entry, original.entry);
    assert_eq!(entry.version, 2);
    assert_eq!(entry.status, original.status);
    assert_eq!(entry.occurred_at_ms, JAN_1 + DAY);
    assert_eq!(entry.title, "refined");
    assert_eq!(entry.summary, "SQLite refined");
    assert_eq!(
        entry.sources,
        [
            SourceId::new("refine").unwrap(),
            SourceId::new("original").unwrap()
        ]
    );
    assert_eq!(entry.revisions, [current(&runtime).await.revision]);
}

#[tokio::test]
async fn manual_edit_detaches_auto_entry() {
    let fake = Fake::start();
    let runtime = setup(&fake).await;
    seed(&runtime, &fake).await;
    let original = entries(&runtime).await.remove(0);
    let mut draft = original.draft();
    draft.summary = "Manually corrected position".into();
    let receipt = runtime.timeline_write(draft).await.result.unwrap();
    assert!(!receipt.entry.is_auto());
    assert_eq!(receipt.entry.version, 2);
    let event = evidence(&runtime, "refine", JAN_1 + DAY).await;
    curate(
        &runtime,
        &fake,
        vec![event],
        vec![vec![update("merge", "refined", None)]],
    )
    .await;
    assert_eq!(
        entries(&runtime).await,
        std::slice::from_ref(&receipt.entry)
    );
    runtime
        .store
        .retire_node(
            &runtime.scope.owner,
            &runtime.scope.actor,
            &runtime.scope.projects[0],
            &current(&runtime).await.memory,
            now_ms().unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(versions(&runtime, &original.entry).await.len(), 2);
}

#[tokio::test]
async fn agents_cannot_create_auto_entries_and_tag_is_stripped() {
    let fake = Fake::start();
    let runtime = setup(&fake).await;
    seed(&runtime, &fake).await;
    let mut draft = entries(&runtime).await.remove(0).draft();
    draft.entry = TimelineEntryId::new("auto:invented:revision").unwrap();
    draft.expected_version = None;
    assert!(matches!(
        runtime.timeline_write(draft.clone()).await.result,
        Err(RuntimeError::InvalidInput(_))
    ));
    draft.entry = TimelineEntryId::new("manual").unwrap();
    let entry = runtime.timeline_write(draft).await.result.unwrap().entry;
    assert!(entry.tags.is_empty());
}

#[tokio::test]
async fn retired_decision_retracts_entry() {
    let fake = Fake::start();
    let runtime = setup(&fake).await;
    let node = seed(&runtime, &fake).await;
    runtime
        .store
        .retire_node(
            &runtime.scope.owner,
            &runtime.scope.actor,
            &runtime.scope.projects[0],
            &node.memory,
            now_ms().unwrap(),
        )
        .await
        .unwrap();
    let all = entries(&runtime).await;
    assert_eq!(
        (all[0].version, all[0].status),
        (2, TimelineStatus::Retracted)
    );
}

#[tokio::test]
async fn unpublished_supersede_reverts_entries_on_retirement_and_rejection() {
    for race in [false, true] {
        let fake = Fake::start();
        let runtime = setup(&fake).await;
        let node = seed(&runtime, &fake).await;
        let original = entries(&runtime).await.remove(0);
        let pending = pending_update(&runtime, &node, "pending", Some(JAN_1 + DAY)).await;
        if race {
            let winner = pending_update(&runtime, &node, "winner", None).await;
            review(
                &runtime,
                &winner.revision.id,
                ReviewAction::Promote,
                Some(node.revision.clone()),
                None,
            )
            .await;
            assert!(
                runtime
                    .store
                    .retire_stale(
                        &runtime.scope.owner,
                        &runtime.scope.actor,
                        &runtime.scope.projects[0],
                        &pending.revision.id,
                        now_ms().unwrap()
                    )
                    .await
                    .unwrap()
            );
        } else {
            review(
                &runtime,
                &pending.revision.id,
                ReviewAction::Reject,
                None,
                None,
            )
            .await;
        }
        let all = entries(&runtime).await;
        let predecessor = all.iter().find(|e| e.entry == original.entry).unwrap();
        if race {
            // The published winner is a merge of P, so P's position is current again.
            assert_eq!(
                (predecessor.version, predecessor.status),
                (3, TimelineStatus::Adopted)
            );
        } else {
            assert_eq!(
                (predecessor.version, predecessor.status),
                (3, TimelineStatus::Adopted)
            );
        }
        assert_eq!(
            all.iter()
                .find(|e| e.entry != original.entry)
                .unwrap()
                .status,
            TimelineStatus::Retracted
        );
    }
}

#[tokio::test]
async fn conflict_rejection_reverts_only_unpublished_positions() {
    for published in [false, true] {
        let fake = Fake::start();
        let runtime = setup(&fake).await;
        let node = seed(&runtime, &fake).await;
        let original = entries(&runtime).await.remove(0);
        let pending = pending_update(&runtime, &node, "pending", Some(JAN_1 + DAY)).await;
        let winner = pending_update(&runtime, &node, "winner", None).await;
        if published {
            review(
                &runtime,
                &pending.revision.id,
                ReviewAction::Promote,
                Some(node.revision.clone()),
                None,
            )
            .await;
        }
        review(
            &runtime,
            &pending.revision.id,
            ReviewAction::Conflict,
            None,
            Some(winner.revision.id.clone()),
        )
        .await;
        review(
            &runtime,
            &winner.revision.id,
            ReviewAction::Promote,
            if published {
                None
            } else {
                Some(node.revision.clone())
            },
            None,
        )
        .await;
        let all = entries(&runtime).await;
        let old = all.iter().find(|e| e.entry == original.entry).unwrap();
        // An unpublished supersede never took effect: the winning merge refines P's position, so
        // P's entry is adopted again. A published supersede keeps its own position adopted.
        assert_eq!(
            old.status,
            if published {
                TimelineStatus::Superseded
            } else {
                TimelineStatus::Adopted
            }
        );
        assert_eq!(
            all.iter()
                .find(|e| e.entry != original.entry)
                .unwrap()
                .status,
            if published {
                TimelineStatus::Adopted
            } else {
                TimelineStatus::Retracted
            }
        );
    }
}

#[tokio::test]
async fn invalid_auto_draft_does_not_fail_commit() {
    let fake = Fake::start();
    let runtime = setup(&fake).await;
    let event = evidence(&runtime, "ancient", 1).await;
    curate(&runtime, &fake, vec![event], vec![vec![json!({"op":"create","node":null,"parent":"decisions","title":"SQLite old","content":"SQLite old position","valid_from":null})]]).await;
    assert_eq!(
        entries(&runtime).await[0].occurred_at_ms,
        TIMELINE_MIN_OCCURRED_MS
    );
    let event = evidence(&runtime, "oversized-title", JAN_1).await;
    curate(&runtime, &fake, vec![event], vec![vec![json!({"op":"create","node":null,"parent":"decisions","title":"決".repeat(70),"content":"SQLite valid node, invalid timeline title","valid_from":null})]]).await;
    assert_eq!(
        runtime
            .store
            .tree_nodes(&runtime.scope.owner, &runtime.scope.projects[0])
            .await
            .unwrap()
            .len(),
        2
    );
    assert_eq!(entries(&runtime).await.len(), 1);
    let activity = runtime
        .activity(ActivityRequest {
            project: runtime.scope.projects[0].clone(),
            limit: 32,
            operations: vec![],
            outcomes: vec![],
            cursor: None,
        })
        .await
        .unwrap();
    assert!(
        activity
            .entries
            .iter()
            .any(|e| e.reason == Some(ActivityReason::TimelineAutoSkipped))
    );
    assert_eq!(
        ActivityReason::parse("timeline_auto_skipped"),
        Some(ActivityReason::TimelineAutoSkipped)
    );
}

#[tokio::test]
async fn backfill_creates_missing_entries_once_and_skips_covered_decisions() {
    let fake = Fake::start();
    let runtime = setup(&fake).await;
    let node = seed(&runtime, &fake).await;
    let original = entries(&runtime).await.remove(0);
    // Existing (manual or detached) coverage is not overwritten by the operator.
    runtime
        .timeline_write(original.draft())
        .await
        .result
        .unwrap();
    let event = evidence(&runtime, "legacy", JAN_1).await;
    let plan = CurationCommit {
        links: vec![],
        operation_id: "legacy-no-hook".into(),
        sources: vec![event.source.clone()],
        promotes: vec![],
        merges: vec![],
        supersedes: vec![],
        creates: vec![(
            ParentRef::Root(TreeRoot::Decisions),
            "Legacy decision".into(),
            "SQLite legacy position".into(),
            vec![event.source],
        )],
        parked: vec![],
        stale: vec![],
        split: vec![],
        operation: LinkOperation::Split,
    };
    let legacy = runtime
        .store
        .commit_curation(
            &runtime.scope.owner,
            &runtime.scope.actor,
            &runtime.scope.projects[0],
            plan,
            now_ms().unwrap(),
        )
        .await
        .unwrap()
        .remove(0);
    review(
        &runtime,
        &legacy.revision.id,
        ReviewAction::Promote,
        None,
        None,
    )
    .await;
    let backfill = |apply| {
        runtime.store.backfill_timeline(
            &runtime.scope.owner,
            &runtime.scope.actor,
            &runtime.scope.projects[0],
            apply,
            now_ms().unwrap(),
            runtime.limits.max_content_bytes,
        )
    };
    assert_eq!(backfill(false).await.unwrap(), (1, 0));
    assert_eq!(backfill(true).await.unwrap(), (1, 1));
    assert_eq!(backfill(true).await.unwrap(), (0, 0));
    let all = entries(&runtime).await;
    assert_eq!(all.len(), 2);
    let backfilled = all.iter().find(|entry| entry.is_auto()).unwrap();
    assert_eq!(
        backfilled.entry.as_str(),
        format!("auto:{}:{}", legacy.revision.memory, legacy.revision.id)
    );
    assert!(
        !all.iter()
            .find(|e| e.memories == [node.memory.clone()])
            .unwrap()
            .is_auto()
    );
    assert_eq!(adopted(&all).status, TimelineStatus::Adopted);
}

#[tokio::test]
async fn split_and_refresh_write_nothing() {
    let fake = Fake::start();
    let runtime = setup(&fake).await;
    let original = seed(&runtime, &fake).await;
    let before = entries(&runtime).await;
    for operation in [
        LinkOperation::Merge,
        LinkOperation::Split,
        LinkOperation::Rebuild,
    ] {
        let node = current(&runtime).await;
        let plan = MaintenanceCommit {
            operation_id: format!("maintenance-{operation:?}"),
            operation,
            rewrites: vec![(
                node.memory.clone(),
                node.revision.clone(),
                "Refreshed grouping".into(),
                "SQLite refreshed summary".into(),
                vec![SourceId::new("original").unwrap()],
            )],
            creates: if operation == LinkOperation::Split {
                vec![(
                    ParentRef::Node(node.memory.clone()),
                    "Split leaf".into(),
                    "SQLite split content".into(),
                    vec![SourceId::new("original").unwrap()],
                )]
            } else {
                vec![]
            },
            reparent: vec![],
            expected_reparent: vec![],
            move_sources: vec![],
            stale: vec![],
            queue_item: None,
        };
        let items = runtime
            .store
            .commit_tree_maintenance(
                &runtime.scope.owner,
                &runtime.scope.actor,
                &runtime.scope.projects[0],
                plan,
                now_ms().unwrap(),
            )
            .await
            .unwrap();
        for item in items {
            review(
                &runtime,
                &item.revision.id,
                ReviewAction::Promote,
                item.revision.predecessor.clone(),
                None,
            )
            .await;
        }
        assert_eq!(entries(&runtime).await, before);
        assert_eq!(versions(&runtime, &before[0].entry).await.len(), 1);
    }
    assert_eq!(before[0].memories, [original.memory]);
}

#[tokio::test]
async fn purge_removes_auto_entry_with_its_sources() {
    let fake = Fake::start();
    let runtime = setup(&fake).await;
    seed(&runtime, &fake).await;
    let original = entries(&runtime).await.remove(0);
    let receipt = runtime
        .store
        .purge(
            &runtime.scope.owner,
            &runtime.scope.projects[0],
            &[SourceId::new("original").unwrap()],
        )
        .await
        .unwrap();
    assert_eq!(receipt.timeline_entries, 1);
    assert!(entries(&runtime).await.is_empty());
    assert!(versions(&runtime, &original.entry).await.is_empty());
}

#[tokio::test]
async fn retired_supersede_keeps_the_winning_merge_content() {
    for predecessor_exists in [true, false] {
        let fake = Fake::start();
        let runtime = setup(&fake).await;
        let node = if predecessor_exists {
            seed(&runtime, &fake).await
        } else {
            let event = evidence(&runtime, "legacy", JAN_1).await;
            let plan = CurationCommit {
                links: vec![],
                operation_id: "legacy".into(),
                sources: vec![event.source.clone()],
                promotes: vec![],
                merges: vec![],
                supersedes: vec![],
                creates: vec![(
                    ParentRef::Root(TreeRoot::Decisions),
                    "Legacy".into(),
                    "SQLite legacy".into(),
                    vec![event.source],
                )],
                parked: vec![],
                stale: vec![],
                split: vec![],
                operation: LinkOperation::Split,
            };
            let item = runtime
                .store
                .commit_curation(
                    &runtime.scope.owner,
                    &runtime.scope.actor,
                    &runtime.scope.projects[0],
                    plan,
                    now_ms().unwrap(),
                )
                .await
                .unwrap()
                .remove(0);
            review(
                &runtime,
                &item.revision.id,
                ReviewAction::Promote,
                None,
                None,
            )
            .await;
            assert!(entries(&runtime).await.is_empty());
            current(&runtime).await
        };
        let original = entries(&runtime).await.first().cloned();
        let pending = pending_update(&runtime, &node, "pending", Some(JAN_1 + DAY)).await;
        let opened = adopted(&entries(&runtime).await).clone();
        let winner = pending_update(&runtime, &node, "winner", None).await;
        review(
            &runtime,
            &winner.revision.id,
            ReviewAction::Promote,
            Some(node.revision.clone()),
            None,
        )
        .await;
        let merged = adopted(&entries(&runtime).await).clone();
        assert_eq!(merged.entry, opened.entry);
        assert_eq!(merged.revisions, std::slice::from_ref(&winner.revision.id));
        assert_eq!(merged.title, "winner");
        assert_eq!(merged.summary, "SQLite winner");
        assert!(merged.sources.contains(&SourceId::new("winner").unwrap()));
        assert!(
            runtime
                .store
                .retire_stale(
                    &runtime.scope.owner,
                    &runtime.scope.actor,
                    &runtime.scope.projects[0],
                    &pending.revision.id,
                    now_ms().unwrap()
                )
                .await
                .unwrap()
        );
        let all = entries(&runtime).await;
        let adopted_entries: Vec<_> = all
            .iter()
            .filter(|e| {
                e.is_auto()
                    && e.status == TimelineStatus::Adopted
                    && e.memories == [node.memory.clone()]
            })
            .collect();
        assert_eq!(adopted_entries.len(), 1);
        let restored = adopted_entries[0];
        assert_eq!(
            (
                &restored.title,
                &restored.summary,
                restored.occurred_at_ms,
                &restored.revisions,
                &restored.sources
            ),
            (
                &merged.title,
                &merged.summary,
                merged.occurred_at_ms,
                &merged.revisions,
                &merged.sources
            )
        );
        if let Some(original) = original {
            assert_eq!(restored.entry, original.entry);
            assert_eq!(restored.supersedes, original.supersedes);
            assert_eq!(restored.tags, original.tags);
            assert_eq!(
                all.iter().find(|e| e.entry == opened.entry).unwrap().status,
                TimelineStatus::Retracted
            );
        } else {
            assert_eq!(restored.entry, opened.entry);
            assert_eq!(restored.version, merged.version);
        }
    }
}
