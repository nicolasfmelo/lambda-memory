use super::*;

#[tokio::test]
async fn links_upgrade_backfills_existing_tree_state() {
    let path = format!("/tmp/lambda-link-upgrade-{}", std::process::id());
    let capture = EmbeddedCaptureStore::open_raw(&path).await.unwrap();
    let migrations = [
        MIGRATION,
        include_str!("../../../../migrations/0002_knowledge.surql"),
        include_str!("../../../../migrations/0003_experiment_index.surql"),
        include_str!("../../../../migrations/0004_runtime.surql"),
        include_str!("../../../../migrations/0005_runtime_semantic.surql"),
        include_str!("../../../../migrations/0006_runtime_automatic.surql"),
        include_str!("../../../../migrations/0007_runtime_diagnostics.surql"),
        include_str!("../../../../migrations/0008_runtime_recovery.surql"),
        include_str!("../../../../migrations/0009_runtime_scope_indexes.surql"),
        include_str!("../../../../migrations/0010_timeline.surql"),
        include_str!("../../../../migrations/0011_memory_tree.surql"),
        include_str!("../../../../migrations/0012_tree_migration_progress.surql"),
        include_str!("../../../../migrations/0013_forward.surql"),
        include_str!("../../../../migrations/0014_promotion_review.surql"),
        include_str!("../../../../migrations/0015_memory_session.surql"),
        include_str!("../../../../migrations/0016_source_citation.surql"),
        include_str!("../../../../migrations/0017_skill_candidate.surql"),
        include_str!("../../../../migrations/0018_tree_validity.surql"),
    ];
    for (i, body) in migrations.iter().enumerate() {
        capture.migrate((i + 1) as u64, body).await.unwrap();
    }
    capture.db.query("CREATE memory_tree_state SET owner_id='owner', project_id='project', mode='tree', stale=['m'], split_queue=[], rebuild_queue=[], parked=[];").await.unwrap().check().unwrap();
    capture
        .migrate(
            19,
            include_str!("../../../../migrations/0019_memory_link.surql"),
        )
        .await
        .unwrap();
    let mut response = capture
        .db
        .query("SELECT VALUE dependency_queue FROM memory_tree_state;")
        .await
        .unwrap()
        .check()
        .unwrap();
    assert_eq!(
        response.take::<Vec<Vec<String>>>(0).unwrap(),
        vec![Vec::<String>::new()]
    );
    let store = runtime::EmbeddedRuntimeStore::new(capture);
    let owner = OwnerId::new("owner").unwrap();
    let project = crate::domain_models::ProjectId::new("project").unwrap();
    assert!(
        store
            .tree_dependency_peek(&owner, &project)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        store
            .tree_queue_peek(
                &owner,
                &project,
                crate::domain_models::memory_tree::TreeQueue::Stale
            )
            .await
            .unwrap()
            .unwrap()
            .as_str(),
        "m"
    );
}
