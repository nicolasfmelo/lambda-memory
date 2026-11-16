use super::*;
use crate::domain_models::embeddings::Embedding;
use crate::domain_models::memory_tree::*;

#[tokio::test]
async fn first_selection_child_replaces_parent_without_another_call() {
    let fake = Fake::start();
    fake.select_all.store(true, Ordering::SeqCst);
    let runtime = tree_fixture(&fake).await;
    let mut parent_vector = vec![0.0; 128];
    parent_vector[0] = 1.0;
    let parent = add_tree_node(
        &runtime,
        "parent",
        ParentRef::Root(TreeRoot::Decisions),
        "Parent",
        "SQLite parent context",
        parent_vector,
    )
    .await;
    let child = add_tree_node(
        &runtime,
        "child",
        ParentRef::Node(parent.revision.memory.clone()),
        "Child",
        "SQLite child context",
        query_vector(),
    )
    .await;
    fake.plan(200, "{\"terms\":[\"sqlite\"]}");
    let result = runtime
        .recall("storage", &[project()], 4096)
        .await
        .result
        .unwrap();
    assert_eq!(
        result
            .tree_hits
            .iter()
            .map(|hit| &hit.node)
            .collect::<Vec<_>>(),
        vec![&child.revision.memory]
    );
    assert_eq!(fake.calls("/api/alpha/decisions"), 1);
    let bodies = fake.luna_bodies.lock().unwrap();
    assert!(bodies[0].contains("SQLite parent context"));
    assert!(bodies[0].contains("SQLite child context"));
}

#[tokio::test]
async fn recall_adds_one_hop_links_to_descent_without_extra_calls() {
    let fake = Fake::start();
    let runtime = tree_fixture(&fake).await;
    let endpoint = add_tree_node(
        &runtime,
        "endpoint",
        ParentRef::Root(TreeRoot::Decisions),
        "Vault",
        "SQLite vault",
        query_vector(),
    )
    .await;
    let mut low_score = vec![0.0; 128];
    low_score[0] = 1.0;
    let linked = add_tree_node(
        &runtime,
        "linked",
        ParentRef::Root(TreeRoot::Architecture),
        "Other branch",
        "Linked context",
        low_score,
    )
    .await;
    let evidence = SourceId::new("edge").unwrap();
    runtime
        .store
        .capture(
            CaptureEvent {
                source: evidence.clone(),
                owner: owner(),
                actor: runtime.scope.actor.clone(),
                project: project(),
                origin: "fixture://link".into(),
                recorded_at_ms: 10,
                kind: WorkEventKind::Decision,
                outcome: WorkOutcome::Unknown,
                content: "Vault relates to the other branch".into(),
            },
            &runtime.limits,
        )
        .await
        .unwrap();
    let plan = CurationCommit {
        links: vec![CurationLink {
            from: LinkFrom::Node(endpoint.revision.memory.clone()),
            to: linked.revision.memory.clone(),
            relation: LinkRelation::Related,
            sources: vec![evidence.clone()],
        }],
        operation_id: "link-endpoint".into(),
        sources: vec![evidence.clone()],
        promotes: vec![],
        supersedes: vec![],
        merges: vec![(
            endpoint.revision.memory.clone(),
            endpoint.revision.id.clone(),
            "Vault".into(),
            "SQLite vault".into(),
            vec![evidence],
        )],
        creates: vec![],
        parked: vec![],
        stale: vec![],
        split: vec![],
        operation: LinkOperation::Create,
    };
    let updated = runtime
        .store
        .commit_curation(&owner(), &runtime.scope.actor, &project(), plan, 120)
        .await
        .unwrap()
        .remove(0);
    runtime
        .store
        .save_runtime_embedding(
            &updated,
            &Embedding {
                space: runtime.providers.as_ref().unwrap().space().unwrap(),
                values: query_vector(),
            },
        )
        .await
        .unwrap();
    runtime
        .store
        .review(
            &owner(),
            &runtime.scope.actor,
            &ReviewRequest {
                project: project(),
                revision: updated.revision.id.clone(),
                action: ReviewAction::Promote,
                expected_active: Some(endpoint.revision.id),
                conflict_with: None,
            },
            121,
        )
        .await
        .unwrap();
    fake.plan(200, "{\"terms\":[\"sqlite\"]}");
    let result = runtime
        .recall("storage", &[project()], 4096)
        .await
        .result
        .unwrap();
    assert_eq!(
        result.tree_hits.len(),
        2,
        "the linked endpoint never replaces the endpoint that reached it"
    );
    assert!(
        result
            .tree_hits
            .iter()
            .any(|hit| hit.node == endpoint.revision.memory)
    );
    let hit = result
        .tree_hits
        .iter()
        .find(|hit| hit.node == linked.revision.memory)
        .unwrap();
    assert_eq!(hit.path, vec!["Architecture", "Other branch"]);
    assert_eq!(fake.calls("/api/alpha/decisions"), 2);
    let bodies = fake.luna_bodies.lock().unwrap().clone();
    assert!(bodies[1].contains("Linked context"));
    assert!(!bodies[1].contains("SQLite vault"));
    fake.luna_bodies.lock().unwrap().clear();
    fake.plan(200, "{\"terms\":[\"sqlite\"]}");
    let result = runtime
        .recall_excluding("storage", &[project()], 4096, &[linked.revision.memory])
        .await
        .result
        .unwrap();
    assert_eq!(result.tree_hits.len(), 1);
    assert_eq!(fake.luna_bodies.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn observed_parent_defeats_conflicting_concluded_child_but_not_other_children() {
    use crate::domain_models::reflection::{
        Conclusion, ReflectionAnswer, ReflectionInput, ReflectionNode,
    };
    use crate::engines::reflection_policy::POLICY;
    for conflicting in [true, false] {
        let fake = Fake::start();
        let runtime = tree_fixture(&fake).await;
        let mut low = vec![0.0; 128];
        low[0] = 1.0;
        let parent = add_tree_node(
            &runtime,
            "observed-parent",
            ParentRef::Root(TreeRoot::Decisions),
            "Vault",
            "SQLite vault",
            low,
        )
        .await;
        let evidence = add_tree_node(
            &runtime,
            "observed-evidence",
            ParentRef::Node(parent.revision.memory.clone()),
            "Evidence",
            "Other evidence",
            vec![-1.0; 128],
        )
        .await;
        let nodes = runtime
            .store
            .tree_nodes(&owner(), &project())
            .await
            .unwrap();
        let input = ReflectionInput {
            branch: parent.revision.memory.clone(),
            nodes: [
                parent.revision.memory.clone(),
                evidence.revision.memory.clone(),
            ]
            .iter()
            .map(|id| ReflectionNode {
                node: nodes.iter().find(|n| &n.memory == id).unwrap().clone(),
                path: vec![],
                inside: true,
            })
            .collect(),
        };
        let item = runtime
            .store
            .commit_reflection(
                &owner(),
                &runtime.scope.actor,
                &project(),
                &input,
                ReflectionAnswer {
                    conclusions: vec![Conclusion {
                        title: "Conclusion".into(),
                        content: "Durable inferred guidance".into(),
                        confidence: Confidence::High,
                        derived_from: vec![0, 1],
                        parent: 0,
                    }],
                    ..Default::default()
                },
                "conclude",
                200,
                POLICY,
            )
            .await
            .unwrap()
            .remove(0);
        runtime
            .store
            .review(
                &owner(),
                &runtime.scope.actor,
                &ReviewRequest {
                    project: project(),
                    revision: item.revision.id.clone(),
                    action: ReviewAction::Promote,
                    expected_active: None,
                    conflict_with: None,
                },
                201,
            )
            .await
            .unwrap();
        let child = if conflicting {
            let nodes = runtime
                .store
                .tree_nodes(&owner(), &project())
                .await
                .unwrap();
            let observed = nodes
                .iter()
                .find(|n| n.memory == parent.revision.memory)
                .unwrap()
                .clone();
            let concluded = nodes
                .iter()
                .find(|n| n.memory == item.revision.memory)
                .unwrap()
                .clone();
            let input = ReflectionInput {
                branch: parent.revision.memory.clone(),
                nodes: vec![
                    ReflectionNode {
                        node: concluded,
                        path: vec![],
                        inside: true,
                    },
                    ReflectionNode {
                        node: observed,
                        path: vec![],
                        inside: true,
                    },
                ],
            };
            let updated = runtime
                .store
                .commit_reflection(
                    &owner(),
                    &runtime.scope.actor,
                    &project(),
                    &input,
                    ReflectionAnswer {
                        contradictions: vec![(0, 1)],
                        ..Default::default()
                    },
                    "contradict",
                    202,
                    POLICY,
                )
                .await
                .unwrap()
                .remove(0);
            runtime
                .store
                .review(
                    &owner(),
                    &runtime.scope.actor,
                    &ReviewRequest {
                        project: project(),
                        revision: updated.revision.id.clone(),
                        action: ReviewAction::Promote,
                        expected_active: Some(item.revision.id),
                        conflict_with: None,
                    },
                    203,
                )
                .await
                .unwrap();
            updated
        } else {
            item
        };
        runtime
            .store
            .save_runtime_embedding(
                &child,
                &Embedding {
                    space: runtime.providers.as_ref().unwrap().space().unwrap(),
                    values: query_vector(),
                },
            )
            .await
            .unwrap();
        fake.plan(200, "{\"terms\":[\"sqlite\"]}");
        let result = runtime
            .recall("storage", &[project()], 4096)
            .await
            .result
            .unwrap();
        assert_eq!(
            fake.calls("/api/alpha/decisions"),
            2,
            "must exercise descent"
        );
        assert_eq!(result.tree_hits.len(), 1);
        assert_eq!(
            result.tree_hits[0].node,
            if conflicting {
                parent.revision.memory
            } else {
                child.revision.memory
            }
        );
    }
}
