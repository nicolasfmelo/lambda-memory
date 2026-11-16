use super::*;
use crate::integrations::database::embedded_capture::EmbeddedCaptureStore;

#[tokio::test]
async fn purge_and_retirement_physically_remove_links_and_evidence() {
    let path = format!("/tmp/lambda-link-cascade-{}", std::process::id());
    let store = EmbeddedRuntimeStore::new(EmbeddedCaptureStore::open(&path).await.unwrap());
    let owner = OwnerId::new("owner").unwrap();
    let actor = ActorId::new("actor").unwrap();
    let project = ProjectId::new("project").unwrap();
    store.capture.db.query("CREATE memory_link SET owner_id='owner', project_id='project', from_memory='m', to_memory='n', relation='related', sources=['s1','s2'], created_at_ms=1; CREATE memory_link SET owner_id='other', project_id='project', from_memory='m', to_memory='n', relation='related', sources=['s1'], created_at_ms=1;").await.unwrap().check().unwrap();
    let tx = store.capture.db.clone().begin().await.unwrap();
    store
        .purge_tree_tx(&tx, &owner, &project, &["s1".into()], &[])
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let links = store
        .cross_links_in(
            &store.capture.db,
            &owner,
            &project,
            &MemoryId::new("m").unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(links.len(), 1);
    assert_eq!(links[0].sources, vec!["s2"]);
    let tx = store.capture.db.clone().begin().await.unwrap();
    store
        .purge_tree_tx(&tx, &owner, &project, &["s2".into()], &[])
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert!(
        store
            .cross_links_in(
                &store.capture.db,
                &owner,
                &project,
                &MemoryId::new("m").unwrap()
            )
            .await
            .unwrap()
            .is_empty()
    );
    store.capture.db.query("CREATE memory_link SET owner_id='owner', project_id='project', from_memory='m', to_memory='n', relation='depends_on', sources=['s3'], created_at_ms=2; CREATE memory_tree_edge SET owner_id='owner', project_id='project', parent_memory='tree-root:decisions', child_memory='n', created_at_ms=1;").await.unwrap().check().unwrap();
    let tx = store.capture.db.clone().begin().await.unwrap();
    let mut state = store
        .ensure_tree_state(&tx, &owner, &project)
        .await
        .unwrap();
    state.dependency_queue = vec!["m|n".into(), "x|y".into()];
    store.save_tree_state(&tx, &state).await.unwrap();
    tx.commit().await.unwrap();
    store
        .retire_node(&owner, &actor, &project, &MemoryId::new("n").unwrap(), 3)
        .await
        .unwrap();
    assert!(
        store
            .cross_links_in(
                &store.capture.db,
                &owner,
                &project,
                &MemoryId::new("m").unwrap()
            )
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        store
            .state_in(&store.capture.db, &owner, &project)
            .await
            .unwrap()
            .unwrap()
            .dependency_queue,
        vec!["x|y"]
    );
    let other = OwnerId::new("other").unwrap();
    assert_eq!(
        store
            .cross_links_in(
                &store.capture.db,
                &other,
                &project,
                &MemoryId::new("m").unwrap()
            )
            .await
            .unwrap()[0]
            .sources,
        vec!["s1"]
    );
}
