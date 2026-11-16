use super::*;
use lambda_memory::domain_models::memory_tree::{CurationLink, LinkFrom, LinkRelation};
async fn connect(
    store: &EmbeddedRuntimeStore,
    old: &RuntimeRevision,
    to: &RuntimeRevision,
    source: &str,
    at: u64,
) -> RuntimeRevision {
    capture(store, source).await;
    let (o, a, p) = ids();
    let mut commit = plan(source, &[source]);
    commit.merges.push((
        old.revision.memory.clone(),
        old.revision.id.clone(),
        "Dependent".into(),
        "Dependent text".into(),
        commit.sources.clone(),
    ));
    commit.links.push(CurationLink {
        from: LinkFrom::Node(old.revision.memory.clone()),
        to: to.revision.memory.clone(),
        relation: LinkRelation::DependsOn,
        sources: commit.sources.clone(),
    });
    let item = store
        .commit_curation(&o, &a, &p, commit, at)
        .await
        .unwrap()
        .remove(0);
    promote_tree(store, &item).await;
    item
}
#[tokio::test]
async fn curator_links_are_stored_with_sources() {
    let store = store().await;
    let (o, _, p) = ids();
    let a = create(
        &store,
        "a",
        ParentRef::Root(TreeRoot::Decisions),
        "A",
        "A text",
    )
    .await;
    let b = create(
        &store,
        "b",
        ParentRef::Root(TreeRoot::Architecture),
        "B",
        "B text",
    )
    .await;
    let a = connect(&store, &a, &b, "edge-evidence", 20).await;
    let links = store.tree_links(&o, &p, &a.revision.memory).await.unwrap();
    assert_eq!(links.len(), 1);
    assert_eq!(links[0].node, b.revision.memory);
    assert!(links[0].outgoing);
    assert_eq!(links[0].relation, LinkRelation::DependsOn);
    // A duplicate in a later answer must not roll the revision transaction back.
    connect(&store, &a, &b, "duplicate-edge", 21).await;
    assert_eq!(
        store
            .tree_links(&o, &p, &b.revision.memory)
            .await
            .unwrap()
            .len(),
        1
    );
    // Evidence remains the original operation's, not the duplicate's.
    store
        .purge(&o, &p, &[SourceId::new("edge-evidence").unwrap()])
        .await
        .unwrap();
    assert!(
        store
            .tree_links(&o, &p, &b.revision.memory)
            .await
            .unwrap()
            .is_empty()
    );
}
#[tokio::test]
async fn new_revision_queues_dependents() {
    let store = store().await;
    let (o, a, p) = ids();
    let dependency = create(
        &store,
        "dependency",
        ParentRef::Root(TreeRoot::Architecture),
        "N",
        "N text",
    )
    .await;
    let mut dependents = Vec::new();
    for i in 0..10 {
        let dependent = create(
            &store,
            &format!("m{i}"),
            ParentRef::Root(TreeRoot::Decisions),
            "M",
            "M text",
        )
        .await;
        dependents
            .push(connect(&store, &dependent, &dependency, &format!("link{i}"), 20 + i).await);
    }
    capture(&store, "update").await;
    let mut commit = plan("update", &["update"]);
    commit.merges.push((
        dependency.revision.memory.clone(),
        dependency.revision.id.clone(),
        "Current N".into(),
        "Current text".into(),
        commit.sources.clone(),
    ));
    store.commit_curation(&o, &a, &p, commit, 40).await.unwrap();
    for i in (2..10).rev() {
        let pair = store.tree_dependency_peek(&o, &p).await.unwrap().unwrap();
        assert_eq!(
            pair,
            (
                dependents[i].revision.memory.clone(),
                dependency.revision.memory.clone()
            )
        );
        store
            .tree_dependency_pop(&o, &p, &pair.0, &pair.1)
            .await
            .unwrap();
    }
    assert!(store.tree_dependency_peek(&o, &p).await.unwrap().is_none());
}
#[tokio::test]
async fn retired_node_drops_links() {
    let store = store().await;
    let (o, a, p) = ids();
    let dependency = create(
        &store,
        "n",
        ParentRef::Root(TreeRoot::Architecture),
        "N",
        "N text",
    )
    .await;
    let dependent = create(
        &store,
        "m",
        ParentRef::Root(TreeRoot::Decisions),
        "M",
        "M text",
    )
    .await;
    let dependent = connect(&store, &dependent, &dependency, "edge", 20).await;
    capture(&store, "update").await;
    let mut commit = plan("update", &["update"]);
    commit.merges.push((
        dependency.revision.memory.clone(),
        dependency.revision.id.clone(),
        "New".into(),
        "New text".into(),
        commit.sources.clone(),
    ));
    store.commit_curation(&o, &a, &p, commit, 30).await.unwrap();
    assert!(store.tree_dependency_peek(&o, &p).await.unwrap().is_some());
    store
        .retire_node(&o, &a, &p, &dependent.revision.memory, 40)
        .await
        .unwrap();
    assert!(
        store
            .tree_links(&o, &p, &dependency.revision.memory)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(store.tree_dependency_peek(&o, &p).await.unwrap().is_none());
}
#[tokio::test]
async fn purge_removes_links_and_source_ids() {
    let store = store().await;
    let (o, a, p) = ids();
    let dependency = create(
        &store,
        "n",
        ParentRef::Root(TreeRoot::Architecture),
        "N",
        "N text",
    )
    .await;
    let dependent = create(
        &store,
        "m",
        ParentRef::Root(TreeRoot::Decisions),
        "M",
        "M text",
    )
    .await;
    capture(&store, "s1").await;
    capture(&store, "s2").await;
    let mut commit = plan("edge", &["s1", "s2"]);
    commit.merges.push((
        dependent.revision.memory.clone(),
        dependent.revision.id.clone(),
        "M".into(),
        "M text".into(),
        commit.sources.clone(),
    ));
    commit.links.push(CurationLink {
        from: LinkFrom::Node(dependent.revision.memory.clone()),
        to: dependency.revision.memory.clone(),
        relation: LinkRelation::Related,
        sources: commit.sources.clone(),
    });
    // Keep the old active head so source purge tests the edge's evidence, not removal of an end.
    store.commit_curation(&o, &a, &p, commit, 20).await.unwrap();
    store
        .purge(&o, &p, &[SourceId::new("s1").unwrap()])
        .await
        .unwrap();
    assert_eq!(
        store
            .tree_links(&o, &p, &dependent.revision.memory)
            .await
            .unwrap()
            .len(),
        1
    );
    store
        .purge(&o, &p, &[SourceId::new("s2").unwrap()])
        .await
        .unwrap();
    assert!(
        store
            .tree_links(&o, &p, &dependent.revision.memory)
            .await
            .unwrap()
            .is_empty()
    );
    // End deletion cascades even when the link has independent evidence.
    let dependent = connect(&store, &dependent, &dependency, "other-edge", 30).await;
    store
        .purge(&o, &p, &[SourceId::new("n").unwrap()])
        .await
        .unwrap();
    assert!(
        store
            .tree_links(&o, &p, &dependent.revision.memory)
            .await
            .unwrap()
            .is_empty()
    );
}
