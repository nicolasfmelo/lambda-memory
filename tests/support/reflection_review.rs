use super::*;
use crate::domain_models::memory_tree::Confidence;

async fn commit_conclusions(
    r: &MemoryRuntime,
    input: &ReflectionInput,
    conclusions: Vec<Conclusion>,
    operation: &str,
    now: u64,
) -> Vec<RuntimeRevision> {
    r.store
        .commit_reflection(
            &r.scope.owner,
            &r.scope.actor,
            &r.scope.projects[0],
            input,
            ReflectionAnswer {
                conclusions,
                ..Default::default()
            },
            operation,
            now,
            POLICY,
        )
        .await
        .unwrap()
}
async fn publish(r: &MemoryRuntime, item: &RuntimeRevision) {
    r.store
        .review(
            &r.scope.owner,
            &r.scope.actor,
            &ReviewRequest {
                project: r.scope.projects[0].clone(),
                revision: item.revision.id.clone(),
                action: ReviewAction::Promote,
                expected_active: item.revision.predecessor.clone(),
                conflict_with: None,
            },
            900,
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn purge_rejects_pending_conclusion_outside_the_source_cap_before_preparation() {
    let fake = Fake::start();
    let r = setup(&fake).await;
    let branch = seed(&r, "branch", ParentRef::Root(TreeRoot::Preferences), None).await;
    for i in 0..32 {
        seed(
            &r,
            &format!("z-{i:02}"),
            ParentRef::Node(branch.memory.clone()),
            None,
        )
        .await;
    }
    let work = due(&r).await;
    assert_eq!(work.input.nodes.len(), 33);
    let parent = work
        .input
        .nodes
        .iter()
        .position(|n| n.node.memory == branch.memory)
        .unwrap();
    let mut items = commit_conclusions(
        &r,
        &work.input,
        vec![Conclusion {
            title: "Pending inference".into(),
            content: "An inference over the observed evidence".into(),
            confidence: Confidence::Medium,
            parent,
            derived_from: (0..33).collect(),
        }],
        "pending-capped",
        500,
    )
    .await;
    let pending = items.pop().unwrap();
    assert_eq!(pending.state, LifecycleState::Processing);
    let omitted = SourceId::new("z-31").unwrap();
    assert!(!pending.revision.sources.contains(&omitted));
    r.store
        .purge(&r.scope.owner, &r.scope.projects[0], &[omitted])
        .await
        .unwrap();
    let rejected = r
        .store
        .runtime_revision(&r.scope.owner, &r.scope.projects[0], &pending.revision.id)
        .await
        .unwrap();
    assert_eq!(rejected.state, LifecycleState::Rejected);
    let p = &r.scope.projects[0];
    let mut trace = r
        .begin_trace(
            std::slice::from_ref(p),
            crate::domain_models::runtime_diagnostics::ActivityOperation::Dream,
            ActivityPhase::Preparation,
            true,
        )
        .await
        .unwrap();
    let result = r.prepare_project(p, 32, &mut trace).await;
    assert_eq!(r.finish_trace(trace, result).await.result.unwrap(), 0);
    assert!(
        r.store
            .active_runtime_hits(
                &r.scope.owner,
                std::slice::from_ref(p),
                std::slice::from_ref(&pending.revision.id)
            )
            .await
            .unwrap()
            .is_empty()
    );
    assert!(fake.embeddings.lock().unwrap().is_empty());
    assert_eq!(fake.chats.load(Ordering::SeqCst), 0);
    assert!(
        r.store
            .review(
                &r.scope.owner,
                &r.scope.actor,
                &ReviewRequest {
                    project: p.clone(),
                    revision: pending.revision.id,
                    action: ReviewAction::Promote,
                    expected_active: None,
                    conflict_with: None,
                },
                1000
            )
            .await
            .is_err()
    );
}

#[tokio::test]
async fn concluded_pressure_reserves_observed_input_and_admits_the_call() {
    let fake = Fake::start();
    let r = setup(&fake).await;
    let branch = seeded(&r).await;
    let p = &r.scope.projects[0];
    for batch in 0..14 {
        let input = ReflectionInput {
            branch: branch.memory.clone(),
            nodes: r
                .store
                .tree_nodes(&r.scope.owner, p)
                .await
                .unwrap()
                .into_iter()
                .filter(|n| n.basis == Basis::Observed)
                .map(|node| ReflectionNode {
                    node,
                    path: vec![],
                    inside: true,
                })
                .collect(),
        };
        let parent = input
            .nodes
            .iter()
            .position(|n| n.node.memory == branch.memory)
            .unwrap();
        let conclusions = (0..3)
            .map(|i| Conclusion {
                title: format!("Inference {batch}-{i}"),
                content: "x".repeat(1200),
                confidence: Confidence::Medium,
                derived_from: vec![0, 1],
                parent,
            })
            .collect();
        for item in commit_conclusions(
            &r,
            &input,
            conclusions,
            &format!("crowded-{batch}"),
            500 + batch,
        )
        .await
        {
            publish(&r, &item).await;
        }
    }
    let nodes = r.store.tree_nodes(&r.scope.owner, p).await.unwrap();
    let all = ReflectionInput {
        branch: branch.memory.clone(),
        nodes: nodes
            .into_iter()
            .filter(|n| n.basis == Basis::Concluded)
            .map(|node| ReflectionNode {
                node,
                path: vec![],
                inside: true,
            })
            .collect(),
    };
    assert!(reflection_policy::input_bytes(&all) > INPUT_BYTES);
    let work = due(&r).await;
    assert_eq!(
        work.input
            .nodes
            .iter()
            .filter(|n| n.node.basis == Basis::Observed)
            .count(),
        2
    );
    assert!(reflection_policy::input_bytes(&work.input) <= INPUT_BYTES);
    let concluded = ReflectionInput {
        branch: branch.memory,
        nodes: work
            .input
            .nodes
            .iter()
            .filter(|n| n.node.basis == Basis::Concluded)
            .cloned()
            .collect(),
    };
    assert!(reflection_policy::input_bytes(&concluded) - 128 <= (INPUT_BYTES - 128) / 2);
    assert!(concluded.nodes.iter().all(|n| n.node.last_linked_ms > 500));
    // A genuinely evidence-free request is not a run and must leave its due baseline alone.
    let mut no_observed = work.clone();
    no_observed
        .input
        .nodes
        .retain(|n| n.node.basis == Basis::Concluded);
    assert!(reflect(&r, no_observed).await.unwrap().is_none());
    assert_eq!(fake.chats.load(Ordering::SeqCst), 0);
    assert!(
        r.store
            .reflection_work(&r.scope.owner, p, 1000, POLICY)
            .await
            .unwrap()
            .is_some()
    );
    fake.reply(200, &empty().to_string());
    reflect(&r, work).await.unwrap();
    assert_eq!(fake.chats.load(Ordering::SeqCst), 1);
    assert!(
        r.store
            .reflection_work(&r.scope.owner, p, 1000, POLICY)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn concluded_descendant_at_max_depth_does_not_block_observed_root_split() {
    let fake = Fake::start();
    let mut r = setup(&fake).await;
    r.limits.tree_max_depth = 3;
    let a = seed(&r, "root-a", ParentRef::Root(TreeRoot::Preferences), None).await;
    let child = seed(
        &r,
        "observed-child",
        ParentRef::Node(a.memory.clone()),
        None,
    )
    .await;
    seed(&r, "root-b", ParentRef::Root(TreeRoot::Preferences), None).await;
    let p = &r.scope.projects[0];
    let input = ReflectionInput {
        branch: a.memory.clone(),
        nodes: r
            .store
            .tree_nodes(&r.scope.owner, p)
            .await
            .unwrap()
            .into_iter()
            .filter(|n| n.memory == a.memory || n.memory == child.memory)
            .map(|node| ReflectionNode {
                node,
                path: vec![],
                inside: true,
            })
            .collect(),
    };
    let parent = input
        .nodes
        .iter()
        .position(|n| n.node.memory == child.memory)
        .unwrap();
    let items = commit_conclusions(
        &r,
        &input,
        vec![Conclusion {
            title: "Deep inference".into(),
            content: "Concluded context".into(),
            confidence: Confidence::Medium,
            derived_from: vec![0, 1],
            parent,
        }],
        "deep-inference",
        500,
    )
    .await;
    publish(&r, &items[0]).await;
    let nodes = r.store.tree_nodes(&r.scope.owner, p).await.unwrap();
    assert!(
        nodes
            .iter()
            .any(|n| n.basis == Basis::Concluded && n.depth == r.limits.tree_max_depth)
    );
    assert!(
        nodes
            .iter()
            .filter(|n| n.basis == Basis::Observed)
            .all(|n| n.depth < r.limits.tree_max_depth)
    );
    let root = TreeRoot::Preferences.memory_id();
    r.store
        .test_queue_split(&r.scope.owner, p, &root)
        .await
        .unwrap();
    fake.reply(200, r#"{"node_title":"Preferences","node_content":"Observed groups","children":[{"title":"First group","content":"First observed group","sources":["n0"]},{"title":"Second group","content":"Second observed group","sources":["n1"]}]}"#);
    let mut trace = r
        .begin_trace(
            std::slice::from_ref(p),
            crate::domain_models::runtime_diagnostics::ActivityOperation::Dream,
            ActivityPhase::TreeSplit,
            true,
        )
        .await
        .unwrap();
    let result = r.tree_maintenance_tick(p, &mut trace).await;
    assert_eq!(
        r.finish_trace(trace, result)
            .await
            .result
            .unwrap()
            .unwrap()
            .batches,
        1
    );
    assert_eq!(fake.chats.load(Ordering::SeqCst), 1);
    let nodes = r.store.tree_nodes(&r.scope.owner, p).await.unwrap();
    assert_eq!(
        nodes
            .iter()
            .filter(|n| n.depth == 1 && n.basis == Basis::Observed)
            .count(),
        2
    );
    assert!(
        nodes
            .iter()
            .filter(|n| n.basis == Basis::Observed)
            .all(|n| n.depth <= r.limits.tree_max_depth)
    );
}
