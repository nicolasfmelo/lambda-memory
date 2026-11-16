#![cfg(feature = "memory-runtime")]
use lambda_memory as lm;
use lm::domain_models::{capture::*, memory_tree::*, reflection::*, runtime::*, *};
use lm::engines::reflection_policy::{self, POLICY};
use lm::integrations::database::embedded_capture::{EmbeddedCaptureStore, EmbeddedRuntimeStore};
use std::sync::atomic::{AtomicUsize, Ordering};
static NEXT: AtomicUsize = AtomicUsize::new(0);
struct Fixture {
    store: EmbeddedRuntimeStore,
    owner: OwnerId,
    actor: ActorId,
    project: ProjectId,
}
impl Fixture {
    async fn new(project: &str) -> Self {
        let path = format!(
            "/tmp/lambda-reflection-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        );
        Self {
            store: EmbeddedRuntimeStore::new(EmbeddedCaptureStore::open(&path).await.unwrap()),
            owner: OwnerId::new("owner").unwrap(),
            actor: ActorId::new("actor").unwrap(),
            project: ProjectId::new(project).unwrap(),
        }
    }
    async fn write(&self, tag: &str, parent: ParentRef, merge: Option<&TreeNode>) -> TreeNode {
        let source = SourceId::new(tag).unwrap();
        self.store
            .capture(
                CaptureEvent {
                    source: source.clone(),
                    owner: self.owner.clone(),
                    actor: self.actor.clone(),
                    project: self.project.clone(),
                    kind: WorkEventKind::Decision,
                    outcome: WorkOutcome::Unknown,
                    origin: "fixture://reflection".into(),
                    recorded_at_ms: 100,
                    content: format!("Evidence {tag}"),
                },
                &RuntimeLimits::default(),
            )
            .await
            .unwrap();
        let mut plan = CurationCommit {
            links: vec![],
            operation_id: tag.into(),
            sources: vec![source.clone()],
            promotes: vec![],
            merges: vec![],
            supersedes: vec![],
            creates: vec![],
            parked: vec![],
            stale: vec![],
            split: vec![],
            operation: LinkOperation::Create,
        };
        if let Some(n) = merge {
            plan.merges.push((
                n.memory.clone(),
                n.revision.clone(),
                tag.into(),
                format!("Evidence {tag}"),
                vec![source],
            ));
        } else {
            plan.creates
                .push((parent, tag.into(), format!("Evidence {tag}"), vec![source]));
        }
        let item = self
            .store
            .commit_curation(&self.owner, &self.actor, &self.project, plan, 200)
            .await
            .unwrap()
            .pop()
            .unwrap();
        self.publish(item).await
    }
    async fn publish(&self, item: RuntimeRevision) -> TreeNode {
        self.store
            .review(
                &self.owner,
                &self.actor,
                &ReviewRequest {
                    project: self.project.clone(),
                    revision: item.revision.id.clone(),
                    action: ReviewAction::Promote,
                    expected_active: item.revision.predecessor.clone(),
                    conflict_with: None,
                },
                300,
            )
            .await
            .unwrap();
        self.nodes()
            .await
            .into_iter()
            .find(|n| n.memory == item.revision.memory)
            .unwrap()
    }
    async fn nodes(&self) -> Vec<TreeNode> {
        let mut nodes = self
            .store
            .tree_nodes(&self.owner, &self.project)
            .await
            .unwrap();
        nodes.sort_by(|a, b| a.depth.cmp(&b.depth).then(a.memory.cmp(&b.memory)));
        nodes
    }
    async fn input(&self) -> ReflectionInput {
        let nodes = self.nodes().await;
        let branch = nodes.iter().find(|n| n.depth == 1).unwrap().memory.clone();
        ReflectionInput {
            branch,
            nodes: nodes
                .into_iter()
                .map(|node| ReflectionNode {
                    node,
                    path: vec![],
                    inside: true,
                })
                .collect(),
        }
    }
    async fn seed(&self) -> (TreeNode, TreeNode) {
        let a = self
            .write("branch", ParentRef::Root(TreeRoot::Preferences), None)
            .await;
        let b = self
            .write("child", ParentRef::Node(a.memory.clone()), None)
            .await;
        (a, b)
    }
    async fn commit(
        &self,
        input: &ReflectionInput,
        answer: ReflectionAnswer,
        op: &str,
    ) -> Result<Vec<RuntimeRevision>, RuntimeError> {
        self.store
            .commit_reflection(
                &self.owner,
                &self.actor,
                &self.project,
                input,
                answer,
                op,
                400,
                POLICY,
            )
            .await
    }
    async fn conclude(&self, confidence: Confidence) -> TreeNode {
        let input = self.input().await;
        let items = self
            .commit(&input, new_conclusion(confidence), "reflect-1")
            .await
            .unwrap();
        self.publish(items.into_iter().next().unwrap()).await
    }
    async fn due(&self) -> Option<ReflectionWork> {
        self.store
            .reflection_work(&self.owner, &self.project, 500, POLICY)
            .await
            .unwrap()
    }
}
fn new_conclusion(confidence: Confidence) -> ReflectionAnswer {
    ReflectionAnswer {
        conclusions: vec![Conclusion {
            title: "Combined guidance".into(),
            content: "The two observations support combined guidance.".into(),
            confidence,
            derived_from: vec![0, 1],
            parent: 0,
        }],
        ..Default::default()
    }
}
#[tokio::test]
async fn existing_and_curated_revisions_are_observed() {
    let f = Fixture::new("project").await;
    f.seed().await;
    for n in f.nodes().await {
        assert_eq!(n.basis, Basis::Observed);
        assert_eq!(n.confidence, None);
        assert!(n.derived_from.is_empty());
    }
}
#[tokio::test]
async fn reflection_commits_conclusions_with_lineage() {
    let f = Fixture::new("project").await;
    let (a, b) = f.seed().await;
    let c = f.conclude(Confidence::High).await;
    assert_eq!(c.basis, Basis::Concluded);
    assert_eq!(c.confidence, Some(Confidence::High));
    assert_eq!(c.parent, Some(a.memory));
    assert_eq!(c.derived_from, vec![a.revision, b.revision]);
    let item = f
        .store
        .runtime_revision(&f.owner, &f.project, &c.revision)
        .await
        .unwrap();
    assert_eq!(item.revision.sources.len(), 2);
    assert!(
        item.revision
            .sources
            .contains(&SourceId::new("branch").unwrap())
    );
    assert!(
        item.revision
            .sources
            .contains(&SourceId::new("child").unwrap())
    );
}
#[tokio::test]
async fn conclusion_from_one_node_is_invalid() {
    let f = Fixture::new("project").await;
    f.seed().await;
    let input = f.input().await;
    let mut a = new_conclusion(Confidence::Low);
    a.conclusions[0].derived_from = vec![0, 0];
    assert!(!reflection_policy::validate(&input, &a));
}
#[tokio::test]
async fn update_targeting_observed_node_is_invalid() {
    let f = Fixture::new("project").await;
    f.seed().await;
    let input = f.input().await;
    assert!(!reflection_policy::validate(
        &input,
        &ReflectionAnswer {
            confidence_updates: vec![(0, Confidence::High)],
            ..Default::default()
        }
    ));
}
#[tokio::test]
async fn invalid_reflection_commits_nothing() {
    let f = Fixture::new("project").await;
    f.seed().await;
    let input = f.input().await;
    let mut a = new_conclusion(Confidence::High);
    a.confidence_updates.push((0, Confidence::Low));
    assert_eq!(
        f.commit(&input, a, "invalid").await.unwrap_err(),
        RuntimeError::ProviderInvalidOutput
    );
    assert_eq!(
        f.nodes().await,
        input.nodes.into_iter().map(|n| n.node).collect::<Vec<_>>()
    );
}
#[tokio::test]
async fn reflection_never_edits_observed_nodes() {
    let f = Fixture::new("project").await;
    f.seed().await;
    let before = f.nodes().await;
    let old = before
        .iter()
        .map(|n| n.revision.clone())
        .collect::<Vec<_>>();
    let mut bytes = Vec::new();
    for id in &old {
        bytes.push(format!(
            "{:?}",
            f.store
                .runtime_revision(&f.owner, &f.project, id)
                .await
                .unwrap()
        ));
    }
    f.conclude(Confidence::Medium).await;
    for (i, id) in old.iter().enumerate() {
        assert_eq!(
            bytes[i],
            format!(
                "{:?}",
                f.store
                    .runtime_revision(&f.owner, &f.project, id)
                    .await
                    .unwrap()
            )
        );
    }
    assert_eq!(
        before,
        f.nodes()
            .await
            .into_iter()
            .filter(|n| n.basis == Basis::Observed)
            .collect::<Vec<_>>()
    );
}
#[tokio::test]
async fn surviving_conclusion_keeps_every_contradiction_pair() {
    let f = Fixture::new("project").await;
    f.seed().await;
    let c = f.conclude(Confidence::High).await;
    let input = f.input().await;
    let ci = input
        .nodes
        .iter()
        .position(|n| n.node.memory == c.memory)
        .unwrap();
    let observed = input
        .nodes
        .iter()
        .enumerate()
        .filter(|(_, n)| n.node.basis == Basis::Observed)
        .map(|(i, _)| i)
        .collect::<Vec<_>>();
    let item = f
        .commit(
            &input,
            ReflectionAnswer {
                contradictions: vec![(ci, observed[0]), (ci, observed[1])],
                ..Default::default()
            },
            "two-pairs-survive",
        )
        .await
        .unwrap()
        .pop()
        .unwrap();
    // One confidence step for the node, but both observed conflicts are recorded.
    assert_eq!(f.publish(item).await.confidence, Some(Confidence::Medium));
    assert_eq!(
        f.store
            .reflection_contradictions(&f.owner, &f.project)
            .await
            .unwrap()
            .len(),
        2
    );
}
#[tokio::test]
async fn two_contradiction_pairs_retire_once_and_leave_no_rows() {
    let f = Fixture::new("project").await;
    f.seed().await;
    let c = f.conclude(Confidence::Medium).await;
    let input = f.input().await;
    let ci = input
        .nodes
        .iter()
        .position(|n| n.node.memory == c.memory)
        .unwrap();
    let observed = input
        .nodes
        .iter()
        .enumerate()
        .filter(|(_, n)| n.node.basis == Basis::Observed)
        .map(|(i, _)| i)
        .collect::<Vec<_>>();
    let item = f
        .commit(
            &input,
            ReflectionAnswer {
                contradictions: vec![(ci, observed[0])],
                ..Default::default()
            },
            "lower-before-retirement",
        )
        .await
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(f.publish(item).await.confidence, Some(Confidence::Low));
    assert_eq!(
        f.store
            .reflection_contradictions(&f.owner, &f.project)
            .await
            .unwrap()
            .len(),
        1
    );
    let input = f.input().await;
    let ci = input
        .nodes
        .iter()
        .position(|n| n.node.memory == c.memory)
        .unwrap();
    let observed = input
        .nodes
        .iter()
        .enumerate()
        .filter(|(_, n)| n.node.basis == Basis::Observed)
        .map(|(i, _)| i)
        .collect::<Vec<_>>();
    assert!(
        f.commit(
            &input,
            ReflectionAnswer {
                contradictions: vec![(ci, observed[0]), (ci, observed[1])],
                ..Default::default()
            },
            "two-retirement-pairs"
        )
        .await
        .unwrap()
        .is_empty()
    );
    assert!(
        f.store
            .reflection_contradictions(&f.owner, &f.project)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(f.nodes().await.iter().all(|n| n.basis == Basis::Observed));
    let page = f
        .store
        .list_page(
            &f.owner,
            &ListQuery {
                exclude_tree: false,
                project: f.project.clone(),
                states: vec![LifecycleState::Rejected],
                memory: Some(c.memory),
                limit: 32,
                after: None,
            },
            true,
        )
        .await
        .unwrap();
    assert_eq!(page.items.len(), 1);
    assert_eq!(
        page.audit
            .iter()
            .filter(|a| a.action == ReviewAction::Reject)
            .count(),
        1
    );
}
#[tokio::test]
async fn contradiction_lowers_then_retires() {
    let f = Fixture::new("project").await;
    f.seed().await;
    let c = f.conclude(Confidence::High).await;
    for (run, expected) in [Some(Confidence::Medium), Some(Confidence::Low), None]
        .into_iter()
        .enumerate()
    {
        let input = f.input().await;
        let i = input
            .nodes
            .iter()
            .position(|n| n.node.memory == c.memory)
            .unwrap();
        let o = input
            .nodes
            .iter()
            .position(|n| n.node.basis == Basis::Observed)
            .unwrap();
        let items = f
            .commit(
                &input,
                ReflectionAnswer {
                    confidence_updates: vec![(i, Confidence::High)],
                    contradictions: vec![(i, o), (i, o)],
                    ..Default::default()
                },
                &format!("contradict-{run}"),
            )
            .await
            .unwrap();
        if let Some(expected) = expected {
            assert_eq!(items.len(), 1);
            assert_eq!(
                f.publish(items.into_iter().next().unwrap())
                    .await
                    .confidence,
                Some(expected)
            );
            assert_eq!(
                f.store
                    .reflection_contradictions(&f.owner, &f.project)
                    .await
                    .unwrap()
                    .len(),
                1
            );
        } else {
            assert!(items.is_empty());
            assert!(!f.nodes().await.iter().any(|n| n.memory == c.memory));
            let view = f
                .store
                .list_page(
                    &f.owner,
                    &ListQuery {
                        exclude_tree: false,
                        project: f.project.clone(),
                        states: vec![LifecycleState::Rejected],
                        memory: Some(c.memory.clone()),
                        limit: 32,
                        after: None,
                    },
                    true,
                )
                .await;
            assert!(view.is_ok());
        }
    }
}
#[tokio::test]
async fn purge_of_derived_revision_retires_conclusion() {
    let f = Fixture::new("project").await;
    f.seed().await;
    let c = f.conclude(Confidence::High).await;
    f.store
        .purge(&f.owner, &f.project, &[SourceId::new("child").unwrap()])
        .await
        .unwrap();
    assert!(!f.nodes().await.iter().any(|n| n.memory == c.memory));
    assert!(
        f.store
            .tree_queue_peek(&f.owner, &f.project, TreeQueue::Rebuild)
            .await
            .unwrap()
            .is_some_and(|id| id != c.memory)
    );
}
#[tokio::test]
async fn reflection_backfill_handles_history_from_uncounted_refreshes() {
    let f = Fixture::new("project").await;
    let (mut branch, _) = f.seed().await;
    for i in 0..3 {
        let plan = MaintenanceCommit {
            operation_id: format!("refresh-{i}"),
            operation: LinkOperation::Merge,
            rewrites: vec![(
                branch.memory.clone(),
                branch.revision.clone(),
                branch.title.clone(),
                branch.content.clone(),
                vec![SourceId::new("branch").unwrap()],
            )],
            creates: vec![],
            reparent: vec![],
            expected_reparent: vec![],
            move_sources: vec![],
            stale: vec![],
            queue_item: None,
        };
        let item = f
            .store
            .commit_tree_maintenance(&f.owner, &f.actor, &f.project, plan, 400)
            .await
            .unwrap()
            .pop()
            .unwrap();
        branch = f.publish(item).await;
    }
    assert!(f.due().await.is_none());
    let plan = f
        .store
        .inspect_reflection_backfill(&f.owner, &f.project)
        .await
        .unwrap();
    assert_eq!(plan.branches, vec![branch.memory]);
    f.store
        .reflection_backfill(&f.owner, &f.project, &plan, 500)
        .await
        .unwrap();
    assert!(f.due().await.is_some());
}
#[tokio::test]
async fn own_writes_do_not_retrigger() {
    let f = Fixture::new("project").await;
    f.seed().await;
    let mut branch = f.nodes().await.into_iter().find(|n| n.depth == 1).unwrap();
    for i in 0..4 {
        branch = f
            .write(
                &format!("rev-{i}"),
                ParentRef::Root(branch.root),
                Some(&branch),
            )
            .await;
    }
    let work = f.due().await.unwrap();
    f.conclude(Confidence::High).await;
    f.store
        .reflection_finish(&f.owner, &f.project, &work, 600)
        .await
        .unwrap();
    assert!(f.due().await.is_none());
}
#[tokio::test]
async fn reflection_runs_on_user_store() {
    let f = Fixture::new("user:owner").await;
    f.seed().await;
    assert_eq!(f.conclude(Confidence::Medium).await.basis, Basis::Concluded);
}
#[tokio::test]
async fn purge_cascades_even_when_the_derived_source_was_outside_the_lineage_cap() {
    let f = Fixture::new("project").await;
    let (branch, _) = f.seed().await;
    for i in 0..31 {
        f.write(
            &format!("z-{i:02}"),
            ParentRef::Node(branch.memory.clone()),
            None,
        )
        .await;
    }
    let input = f.input().await;
    let mut answer = new_conclusion(Confidence::High);
    answer.conclusions[0].derived_from = (0..input.nodes.len()).collect();
    let c = f
        .publish(
            f.commit(&input, answer, "capped-lineage")
                .await
                .unwrap()
                .pop()
                .unwrap(),
        )
        .await;
    let item = f
        .store
        .runtime_revision(&f.owner, &f.project, &c.revision)
        .await
        .unwrap();
    assert_eq!(item.revision.sources.len(), 32);
    let omitted = SourceId::new("z-30").unwrap();
    assert!(!item.revision.sources.contains(&omitted));
    f.store
        .purge(&f.owner, &f.project, &[omitted])
        .await
        .unwrap();
    assert!(!f.nodes().await.iter().any(|n| n.memory == c.memory));
}
#[tokio::test]
async fn retirement_cascades_through_conclusions_from_conclusions() {
    let f = Fixture::new("project").await;
    let (a, b) = f.seed().await;
    let mut previous = f.conclude(Confidence::Medium).await;
    for i in 0..3 {
        let input = f.input().await;
        let mut answer = new_conclusion(Confidence::Medium);
        let p = input
            .nodes
            .iter()
            .position(|n| n.node.memory == previous.memory)
            .unwrap();
        let o = input
            .nodes
            .iter()
            .position(|n| n.node.memory == a.memory)
            .unwrap();
        answer.conclusions[0].derived_from = vec![p, o];
        answer.conclusions[0].parent = o;
        previous = f
            .publish(
                f.commit(&input, answer, &format!("chain-{i}"))
                    .await
                    .unwrap()
                    .pop()
                    .unwrap(),
            )
            .await;
    }
    f.store
        .retire_node(&f.owner, &f.actor, &f.project, &b.memory, 500)
        .await
        .unwrap();
    assert!(f.nodes().await.iter().all(|n| n.basis == Basis::Observed));
}
#[tokio::test]
async fn concluded_nodes_are_excluded_from_observed_maintenance_queues() {
    let f = Fixture::new("project").await;
    f.seed().await;
    let c = f.conclude(Confidence::Medium).await;
    let plan = CurationCommit {
        links: vec![],
        operation_id: "legacy-queue".into(),
        sources: vec![],
        promotes: vec![],
        merges: vec![],
        supersedes: vec![],
        creates: vec![],
        parked: vec![],
        stale: vec![c.memory.clone()],
        split: vec![c.memory],
        operation: LinkOperation::Create,
    };
    f.store
        .commit_curation(&f.owner, &f.actor, &f.project, plan, 500)
        .await
        .unwrap();
    for queue in [TreeQueue::Stale, TreeQueue::Split, TreeQueue::Rebuild] {
        assert!(
            f.store
                .tree_queue_peek(&f.owner, &f.project, queue)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            f.store
                .tree_queue_pop(&f.owner, &f.project, queue)
                .await
                .unwrap()
                .is_none()
        );
    }
}
#[tokio::test]
async fn read_only_context_cannot_be_a_conclusion_parent() {
    let f = Fixture::new("project").await;
    f.seed().await;
    let mut input = f.input().await;
    input.nodes[0].inside = false;
    assert!(!reflection_policy::validate(
        &input,
        &new_conclusion(Confidence::High)
    ));
}
#[tokio::test]
async fn contradiction_precedence_drops_new_conclusion_with_retired_support() {
    let f = Fixture::new("project").await;
    f.seed().await;
    let c = f.conclude(Confidence::Low).await;
    let input = f.input().await;
    let i = input
        .nodes
        .iter()
        .position(|n| n.node.memory == c.memory)
        .unwrap();
    let o = input
        .nodes
        .iter()
        .position(|n| n.node.basis == Basis::Observed)
        .unwrap();
    let mut answer = new_conclusion(Confidence::High);
    answer.conclusions[0].derived_from = vec![i, o];
    answer.confidence_updates = vec![(i, Confidence::High), (i, Confidence::Medium)];
    answer.contradictions = vec![(i, o), (i, o)];
    assert!(
        f.commit(&input, answer, "retire-and-derive")
            .await
            .unwrap()
            .is_empty()
    );
    assert!(f.nodes().await.iter().all(|n| n.basis == Basis::Observed));
}
#[tokio::test]
async fn bounded_input_keeps_a_conclusion_and_discards_deepest_oldest_observations_first() {
    let f = Fixture::new("project").await;
    f.seed().await;
    f.conclude(Confidence::Low).await;
    let mut input = f.input().await;
    let branch = input.nodes[0].clone();
    for i in 0..80 {
        let mut n = branch.clone();
        n.node.depth = 3;
        n.node.content = "x".repeat(1200);
        n.node.last_linked_ms = i;
        input.nodes.push(n);
    }
    let input = reflection_policy::bound(input);
    assert!(reflection_policy::input_bytes(&input) <= INPUT_BYTES);
    assert!(input.nodes.iter().any(|n| n.node.basis == Basis::Concluded));
    assert!(
        input
            .nodes
            .iter()
            .all(|n| n.node.depth != 3 || n.node.last_linked_ms > 40)
    );
}
#[test]
fn reflection_triggers_after_five_revisions_and_on_lesson_threshold() {
    let m = MemoryId::new("lesson").unwrap();
    assert!(!reflection_policy::due(14, 10, &[], &[]));
    assert!(reflection_policy::due(15, 10, &[], &[]));
    assert!(reflection_policy::due(
        10,
        10,
        std::slice::from_ref(&m),
        &[]
    ));
    assert!(!reflection_policy::due(
        10,
        10,
        std::slice::from_ref(&m),
        std::slice::from_ref(&m)
    ));
}
#[tokio::test]
async fn recall_labels_concluded_and_drops_contradicted_conclusion() {
    async fn selected(f: &Fixture, observed_selected: bool) -> RuntimeRecall {
        let nodes = f
            .nodes()
            .await
            .into_iter()
            .filter(|n| observed_selected || n.basis == Basis::Concluded)
            .collect::<Vec<_>>();
        let observed = nodes
            .iter()
            .filter(|n| n.basis == Basis::Observed)
            .map(|n| n.memory.clone())
            .collect::<Vec<_>>();
        let contradictions = f
            .store
            .reflection_contradictions(&f.owner, &f.project)
            .await
            .unwrap();
        let mut tree = Vec::new();
        for n in nodes {
            if reflection_policy::conflicts(&n.memory, &observed, &contradictions) {
                continue;
            }
            let item = f
                .store
                .runtime_revision(&f.owner, &f.project, &n.revision)
                .await
                .unwrap();
            tree.push(TreeHit {
                basis: n.basis,
                confidence: n.confidence,
                derived_from: n.derived_from,
                content: format!(
                    "{}{}",
                    reflection_policy::label(n.basis, n.confidence),
                    n.content
                ),
                valid_from_ms: None,
                replaced: None,
                root: n.root,
                hit: RuntimeHit {
                    item,
                    lexical_score: 1,
                },
                path: vec![n.title],
                node: n.memory,
                source_count: 1,
            });
        }
        lm::engines::runtime_policy::pack_with_paths(
            tree.iter().map(|t| t.hit.clone()).collect(),
            &tree,
            8192,
        )
    }
    let f = Fixture::new("project").await;
    let (a, _) = f.seed().await;
    let c = f.conclude(Confidence::High).await;
    let recall = selected(&f, true).await;
    let concluded = recall
        .tree_hits
        .iter()
        .find(|h| h.node == c.memory)
        .unwrap();
    assert!(concluded.content.starts_with("Concluded (high): "));
    assert!(recall.context.contains("Concluded (high): "));
    assert!(
        recall
            .hits
            .iter()
            .any(|h| h.item.revision.memory == a.memory)
    );
    let input = f.input().await;
    let ci = input
        .nodes
        .iter()
        .position(|n| n.node.memory == c.memory)
        .unwrap();
    let oi = input
        .nodes
        .iter()
        .position(|n| n.node.memory == a.memory)
        .unwrap();
    let items = f
        .commit(
            &input,
            ReflectionAnswer {
                contradictions: vec![(ci, oi)],
                ..Default::default()
            },
            "recall-contradiction",
        )
        .await
        .unwrap();
    f.publish(items.into_iter().next().unwrap()).await;
    let recall = selected(&f, true).await;
    assert!(
        recall
            .hits
            .iter()
            .any(|h| h.item.revision.memory == a.memory)
    );
    assert!(
        !recall
            .hits
            .iter()
            .any(|h| h.item.revision.memory == c.memory)
    );
    assert!(!recall.context.contains("Concluded"));
    let alone = selected(&f, false).await;
    assert!(alone.tree_hits.iter().any(|h| h.node == c.memory));
}
