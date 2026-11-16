use super::*;
#[tokio::test]
async fn existing_and_curated_revisions_are_observed() {
    let path = format!("/tmp/lambda-reflection-upgrade-{}", std::process::id());
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
        include_str!("../../../../migrations/0019_memory_link.surql"),
    ];
    for (i, body) in migrations.into_iter().enumerate() {
        capture.migrate((i + 1) as u64, body).await.unwrap();
    }
    capture.db.query("CREATE memory_tree_revision SET owner_id='owner', project_id='project', revision_id='old', memory_id='branch',tree_root='preferences',parent_memory='tree-root:preferences',depth=1,title='Old';").await.unwrap().check().unwrap();
    capture
        .migrate(
            20,
            include_str!("../../../../migrations/0020_memory_reflection.surql"),
        )
        .await
        .unwrap();
    #[derive(Debug, SurrealValue)]
    struct Fields {
        basis: String,
        confidence: Option<String>,
        derived_from: Vec<String>,
    }
    let mut r = capture
        .db
        .query("SELECT basis,confidence,derived_from FROM memory_tree_revision;")
        .await
        .unwrap()
        .check()
        .unwrap();
    let rows: Vec<Fields> = r.take(0).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].basis, "observed");
    assert_eq!(rows[0].confidence, None);
    assert!(rows[0].derived_from.is_empty());
}
