#![cfg(feature = "memory-runtime")]
use lambda_memory::domain_models::capture::*;
use lambda_memory::domain_models::memory_tree::*;
use lambda_memory::domain_models::promotion::*;
use lambda_memory::domain_models::runtime::*;
use lambda_memory::domain_models::{ActorId, MemoryId, OwnerId, ProjectId, RevisionId, SourceId};
use lambda_memory::engines::promotion_policy;
use lambda_memory::integrations::database::embedded_capture::{
    EmbeddedCaptureStore, EmbeddedRuntimeStore,
};
use lambda_memory::services::runtime_service::MemoryRuntime;
use serde_json::{Value, json};
use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT: AtomicUsize = AtomicUsize::new(0);
fn owner() -> OwnerId {
    OwnerId::new("owner").unwrap()
}
fn actor() -> ActorId {
    ActorId::new("operator").unwrap()
}
struct Fixture {
    path: String,
    project: ProjectId,
}
impl Fixture {
    fn new(project: &str) -> Self {
        let path = format!(
            "{}/.cache/lambda-memory/promotion-{}-{}",
            std::env::var("HOME").unwrap(),
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        );
        std::fs::create_dir_all(&path).unwrap();
        Self {
            path,
            project: ProjectId::new(project).unwrap(),
        }
    }
    async fn open(&self) -> EmbeddedRuntimeStore {
        for _ in 0..30 {
            if let Ok(c) = EmbeddedCaptureStore::open(&self.path).await {
                return EmbeddedRuntimeStore::new(c);
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        panic!("store lock not released");
    }
    async fn run(&self, flags: &[&str], input: Option<Value>) -> std::process::Output {
        for _ in 0..30 {
            let mut command = Command::new(env!("CARGO_BIN_EXE_lambda-memory"));
            command
                .args(flags)
                .env("LAMBDA_MEMORY_DATA_DIR", &self.path)
                .env("LAMBDA_MEMORY_OWNER", "owner")
                .env("LAMBDA_MEMORY_ACTOR", "operator")
                .env("LAMBDA_MEMORY_PROJECT", self.project.as_str())
                .env("LAMBDA_MEMORY_TOKEN", "x".repeat(40))
                .env("LAMBDA_MEMORY_MODE", "local")
                .env_remove("LAMBDA_MEMORY_ALLOWED_PROJECTS")
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            let mut child = command.spawn().unwrap();
            if let Some(ref input) = input {
                let _ = child
                    .stdin
                    .take()
                    .unwrap()
                    .write_all(input.to_string().as_bytes());
            }
            let output = child.wait_with_output().unwrap();
            if !String::from_utf8_lossy(&output.stderr).contains("storage unavailable") {
                return output;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        panic!("native lock not released");
    }
    async fn json(&self, flags: &[&str], input: Option<Value>) -> Value {
        let output = self.run(flags, input).await;
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}
fn event(project: &ProjectId, id: &str) -> CaptureEvent {
    CaptureEvent {
        source: SourceId::new(id).unwrap(),
        owner: owner(),
        actor: actor(),
        project: project.clone(),
        origin: "fixture://promotion".into(),
        recorded_at_ms: 123,
        kind: WorkEventKind::Decision,
        outcome: WorkOutcome::Unknown,
        content: "Delegate only coding tasks.".into(),
    }
}
async fn create(
    store: &EmbeddedRuntimeStore,
    project: &ProjectId,
    operation: &str,
    parent: ParentRef,
    sources: &[&str],
) -> TreeNode {
    create_at(store, project, operation, parent, sources, 1000, 1001).await
}
async fn create_at(
    store: &EmbeddedRuntimeStore,
    project: &ProjectId,
    operation: &str,
    parent: ParentRef,
    sources: &[&str],
    created: u64,
    promoted: u64,
) -> TreeNode {
    let mut ids = Vec::new();
    for id in sources {
        let e = event(project, id);
        store
            .capture(e.clone(), &RuntimeLimits::default())
            .await
            .unwrap();
        ids.push(e.source);
    }
    let items = store
        .commit_curation(
            &owner(),
            &actor(),
            project,
            CurationCommit {
                links: vec![],
                operation_id: operation.into(),
                supersedes: vec![],
                sources: ids.clone(),
                promotes: vec![],
                merges: vec![],
                creates: vec![(
                    parent,
                    operation.into(),
                    "Delegate only coding tasks.".into(),
                    ids,
                )],
                parked: vec![],
                stale: vec![],
                split: vec![],
                operation: LinkOperation::Create,
            },
            created,
        )
        .await
        .unwrap();
    for item in &items {
        store
            .review(
                &owner(),
                &actor(),
                &ReviewRequest {
                    project: project.clone(),
                    revision: item.revision.id.clone(),
                    action: ReviewAction::Promote,
                    expected_active: None,
                    conflict_with: None,
                },
                promoted,
            )
            .await
            .unwrap();
    }
    store
        .tree_nodes(&owner(), project)
        .await
        .unwrap()
        .into_iter()
        .find(|n| n.revision == items[0].revision.id)
        .unwrap()
}
async fn review(
    store: &EmbeddedRuntimeStore,
    project: &ProjectId,
    node: &TreeNode,
    verdict: PromotionVerdict,
) {
    review_at(store, project, node, verdict, 2000).await;
}
async fn review_at(
    store: &EmbeddedRuntimeStore,
    project: &ProjectId,
    node: &TreeNode,
    verdict: PromotionVerdict,
    now: u64,
) {
    let sources = store
        .promotion_sources(&owner(), project, &node.memory)
        .await
        .unwrap();
    store
        .commit_promotion_reviews(
            &owner(),
            project,
            &[(
                PromotionCandidate {
                    node: node.clone(),
                    path: vec![],
                    sources,
                },
                verdict,
            )],
            now,
        )
        .await
        .unwrap();
}
fn pure_node(id: &str, root: TreeRoot, children: u32, time: u64) -> TreeNode {
    TreeNode {
        basis: Default::default(),
        confidence: None,
        derived_from: vec![],
        memory: MemoryId::new(id).unwrap(),
        revision: RevisionId::new(format!("r-{id}")).unwrap(),
        root,
        parent: None,
        depth: 3,
        title: id.into(),
        content: "evidence".into(),
        direct_sources: 1,
        children,
        last_linked_ms: time,
        valid_from_ms: None,
    }
}
#[test]
fn promotion_candidates_are_childless_preferences_and_decisions() {
    let nodes = vec![
        pure_node("d", TreeRoot::Decisions, 0, 99),
        pure_node("p2", TreeRoot::Preferences, 0, 2),
        pure_node("p1", TreeRoot::Preferences, 0, 2),
        pure_node("parent", TreeRoot::Preferences, 1, 20),
        pure_node("arch", TreeRoot::Architecture, 0, 30),
        pure_node("old", TreeRoot::Decisions, 0, 40),
    ];
    let project = ProjectId::new("project").unwrap();
    let selected =
        promotion_policy::candidates(&project, &nodes, &[nodes[5].revision.clone()]).unwrap();
    assert_eq!(
        selected
            .iter()
            .map(|n| n.memory.as_str())
            .collect::<Vec<_>>(),
        ["p1", "p2", "d"]
    );
    assert!(matches!(
        promotion_policy::candidates(&ProjectId::new("user:owner").unwrap(), &nodes, &[]),
        Err(RuntimeError::InvalidInput(_))
    ));
    assert!(
        promotion_policy::validate_output(
            &[(0, PromotionVerdict::User), (0, PromotionVerdict::Project)],
            2
        )
        .is_err()
    );
    assert!(promotion_policy::validate_output(&[(1, PromotionVerdict::User)], 1).is_err());
}
#[tokio::test]
async fn promotion_reviews_follow_purge_and_retention_cascades() {
    for retention in [false, true] {
        let f = Fixture::new("project");
        let store = f.open().await;
        let target = create(
            &store,
            &f.project,
            "target",
            ParentRef::Root(TreeRoot::Preferences),
            &["a"],
        )
        .await;
        let kept = create(
            &store,
            &f.project,
            "kept",
            ParentRef::Root(TreeRoot::Decisions),
            &["z"],
        )
        .await;
        review(&store, &f.project, &target, PromotionVerdict::User).await;
        review(&store, &f.project, &kept, PromotionVerdict::Project).await;
        let purged = if retention {
            store.retain(&owner(), &f.project, 124, 1).await.unwrap()
        } else {
            store
                .purge(&owner(), &f.project, &[SourceId::new("a").unwrap()])
                .await
                .unwrap()
        };
        assert_eq!(purged.revisions, 1);
        let reviews = store.promotion_reviews(&owner(), &f.project).await.unwrap();
        assert_eq!(reviews.len(), 1, "retention={retention}");
        assert_eq!(reviews[0].memory, kept.memory);
        assert_eq!(
            store
                .runtime_revision(&owner(), &f.project, &target.revision)
                .await
                .unwrap_err(),
            RuntimeError::Missing
        );
        // Tree cleanup removes the orphan edge; reusing the original source and operation
        // recreates the same memory/revision IDs, which must not inherit a deleted review.
        store
            .retire_node(&owner(), &actor(), &f.project, &target.memory, 3000)
            .await
            .unwrap();
        let recreated = create(
            &store,
            &f.project,
            "target",
            ParentRef::Root(TreeRoot::Preferences),
            &["a"],
        )
        .await;
        assert_eq!(recreated.memory, target.memory);
        assert_eq!(recreated.revision, target.revision);
        let runtime = MemoryRuntime::new(
            store.clone(),
            RuntimeScope {
                owner: owner(),
                actor: actor(),
                projects: vec![f.project.clone()],
            },
            RuntimeLimits::default(),
        )
        .unwrap();
        assert_eq!(
            runtime.inspect_promotions(&f.project).await.unwrap().nodes,
            vec![recreated.memory.clone()]
        );
        review(&store, &f.project, &recreated, PromotionVerdict::User).await;
    }
}

#[tokio::test]
async fn export_forwards_keeps_recapture_behind_pending_purge() {
    let f = Fixture::new("project");
    let store = f.open().await;
    let original = create(
        &store,
        &f.project,
        "original",
        ParentRef::Root(TreeRoot::Preferences),
        &["s"],
    )
    .await;
    review(&store, &f.project, &original, PromotionVerdict::User).await;
    store
        .forward_done(&owner(), &f.project, &["capture:s".into()], &[], 2100)
        .await
        .unwrap();
    store
        .purge(&owner(), &f.project, &[SourceId::new("s").unwrap()])
        .await
        .unwrap();
    let purge = store
        .forwards(&owner(), &f.project, 64)
        .await
        .unwrap()
        .into_iter()
        .find(|f| f.action == "purge")
        .unwrap();
    let recaptured = create(
        &store,
        &f.project,
        "recaptured",
        ParentRef::Root(TreeRoot::Preferences),
        &["s"],
    )
    .await;
    let sources = store
        .promotion_sources(&owner(), &f.project, &recaptured.memory)
        .await
        .unwrap();
    // Give the recapture its actual later queue time, after the purge's wall-clock timestamp.
    store
        .commit_promotion_reviews(
            &owner(),
            &f.project,
            &[(
                PromotionCandidate {
                    node: recaptured,
                    path: vec![],
                    sources,
                },
                PromotionVerdict::User,
            )],
            u64::try_from(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_millis(),
            )
            .unwrap()
                + 1,
        )
        .await
        .unwrap();
    // An unrelated pending capture must still be exportable.
    let unrelated = create(
        &store,
        &f.project,
        "unrelated",
        ParentRef::Root(TreeRoot::Decisions),
        &["other"],
    )
    .await;
    review(&store, &f.project, &unrelated, PromotionVerdict::User).await;
    assert_eq!(
        store
            .export_capture_forwards(&owner(), &f.project)
            .await
            .unwrap()
            .iter()
            .map(|f| f.source_id.as_str())
            .collect::<Vec<_>>(),
        vec!["other"]
    );
    assert_eq!(
        store
            .forwards(&owner(), &f.project, 64)
            .await
            .unwrap()
            .iter()
            .filter(|f| f.source_id.as_str() == "s")
            .map(|f| f.action)
            .collect::<Vec<_>>(),
        vec!["purge", "capture"]
    );
    let purge_id = purge.forward_id;
    drop(store);
    let output = f.run(&["--export-forwards"], None).await;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rows: Vec<Value> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(
        rows.iter()
            .map(|r| r["source_id"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["other"]
    );
    f.json(
        &["--forward-done"],
        Some(json!({"done":[purge_id],"failed":[]})),
    )
    .await;
    let output = f.run(&["--export-forwards"], None).await;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rows: Vec<Value> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert!(rows.iter().any(|row| {
        row["forward_id"]
            .as_str()
            .unwrap()
            .starts_with("recapture:")
            && row["forward_id"].as_str().unwrap().ends_with(":s")
            && row["source"]["source"] == "s"
    }));
}

#[tokio::test]
async fn stale_capture_ack_cannot_acknowledge_recapture() {
    let f = Fixture::new("project");
    let store = f.open().await;
    let first = create_at(
        &store,
        &f.project,
        "first",
        ParentRef::Root(TreeRoot::Preferences),
        &["s"],
        1000,
        1001,
    )
    .await;
    review_at(&store, &f.project, &first, PromotionVerdict::User, 1100).await;
    assert_eq!(
        store.forwards(&owner(), &f.project, 64).await.unwrap()[0].forward_id,
        "capture:s"
    );
    store
        .purge(&owner(), &f.project, &[SourceId::new("s").unwrap()])
        .await
        .unwrap();
    let second = create_at(
        &store,
        &f.project,
        "second",
        ParentRef::Root(TreeRoot::Preferences),
        &["s"],
        1200,
        1201,
    )
    .await;
    review_at(&store, &f.project, &second, PromotionVerdict::User, 1300).await;
    let forwards = store.forwards(&owner(), &f.project, 64).await.unwrap();
    let f2 = forwards
        .iter()
        .find(|f| f.action == "capture")
        .unwrap()
        .forward_id
        .clone();
    assert_eq!(f2, "recapture:1300:s");
    assert_eq!(
        store
            .forward_done(&owner(), &f.project, &["capture:s".into()], &[], 1400)
            .await
            .unwrap(),
        (0, 0)
    );
    let items = store
        .commit_tree_maintenance(
            &owner(),
            &actor(),
            &f.project,
            MaintenanceCommit {
                operation_id: "rewrite-second".into(),
                operation: LinkOperation::Merge,
                rewrites: vec![(
                    second.memory.clone(),
                    second.revision.clone(),
                    "updated".into(),
                    "updated".into(),
                    vec![SourceId::new("s").unwrap()],
                )],
                creates: vec![],
                reparent: vec![],
                expected_reparent: vec![],
                move_sources: vec![],
                stale: vec![],
                queue_item: None,
            },
            1500,
        )
        .await
        .unwrap();
    store
        .review(
            &owner(),
            &actor(),
            &ReviewRequest {
                project: f.project.clone(),
                revision: items[0].revision.id.clone(),
                action: ReviewAction::Promote,
                expected_active: Some(second.revision),
                conflict_with: None,
            },
            1501,
        )
        .await
        .unwrap();
    let updated = store
        .tree_nodes(&owner(), &f.project)
        .await
        .unwrap()
        .into_iter()
        .find(|n| n.memory == second.memory)
        .unwrap();
    let sources = store
        .promotion_sources(&owner(), &f.project, &updated.memory)
        .await
        .unwrap();
    assert_eq!(
        store
            .commit_promotion_reviews(
                &owner(),
                &f.project,
                &[(
                    PromotionCandidate {
                        node: updated,
                        path: vec![],
                        sources
                    },
                    PromotionVerdict::User
                )],
                1600
            )
            .await
            .unwrap(),
        0
    );
    let captures: Vec<_> = store
        .forwards(&owner(), &f.project, 64)
        .await
        .unwrap()
        .into_iter()
        .filter(|f| f.action == "capture")
        .collect();
    assert_eq!(captures.len(), 1);
    assert_eq!(captures[0].forward_id, f2);
}

#[tokio::test]
async fn recapture_id_does_not_collide_with_colon_source_legacy_id() {
    let f = Fixture::new("project");
    let store = f.open().await;
    let first = create_at(
        &store,
        &f.project,
        "first",
        ParentRef::Root(TreeRoot::Preferences),
        &["s"],
        1000,
        1001,
    )
    .await;
    review_at(&store, &f.project, &first, PromotionVerdict::User, 1100).await;
    store
        .purge(&owner(), &f.project, &[SourceId::new("s").unwrap()])
        .await
        .unwrap();
    let second = create_at(
        &store,
        &f.project,
        "second",
        ParentRef::Root(TreeRoot::Preferences),
        &["s"],
        1200,
        1201,
    )
    .await;
    review_at(&store, &f.project, &second, PromotionVerdict::User, 1300).await;
    let other = create_at(
        &store,
        &f.project,
        "other",
        ParentRef::Root(TreeRoot::Decisions),
        &["s:1300"],
        1400,
        1401,
    )
    .await;
    review_at(&store, &f.project, &other, PromotionVerdict::User, 1500).await;
    let forwards = store.forwards(&owner(), &f.project, 64).await.unwrap();
    let captures: Vec<_> = forwards
        .iter()
        .filter(|f| f.action == "capture")
        .map(|f| (f.source_id.as_str(), f.forward_id.as_str()))
        .collect();
    assert_eq!(
        captures,
        vec![("s", "recapture:1300:s"), ("s:1300", "capture:s:1300")]
    );
}

#[tokio::test]
async fn inspect_copies_excludes_flat_successor() {
    let f = Fixture::new("user:owner");
    let store = f.open().await;
    let node = create_at(
        &store,
        &f.project,
        "tree",
        ParentRef::Root(TreeRoot::Preferences),
        &["copy"],
        1000,
        1001,
    )
    .await;
    let flat = store
        .propose(
            &owner(),
            &actor(),
            RevisionProposal {
                project: f.project.clone(),
                memory: node.memory.clone(),
                revision: RevisionId::new("flat-successor").unwrap(),
                sources: vec![SourceId::new("copy").unwrap()],
                content: "flat".into(),
                predecessor: Some(node.revision.clone()),
            },
            1100,
        )
        .await
        .unwrap();
    store
        .review(
            &owner(),
            &actor(),
            &ReviewRequest {
                project: f.project.clone(),
                revision: flat.revision.id,
                action: ReviewAction::Promote,
                expected_active: Some(node.revision),
                conflict_with: None,
            },
            1101,
        )
        .await
        .unwrap();
    let copies = store
        .inspect_copies(&owner(), &f.project, &[SourceId::new("copy").unwrap()])
        .await
        .unwrap();
    assert!(copies.active.is_empty());
    assert_eq!(copies.consumed.len() + copies.pending.len(), 1);
}

#[tokio::test]
async fn export_and_ack_forwards() {
    let f = Fixture::new("project");
    let store = f.open().await;
    let n = create(
        &store,
        &f.project,
        "preference",
        ParentRef::Root(TreeRoot::Preferences),
        &["s1", "s2"],
    )
    .await;
    review(&store, &f.project, &n, PromotionVerdict::User).await;
    drop(store);
    let output = f.run(&["--export-forwards"], None).await;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let lines: Vec<Value> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0]["source"]["source"], "s1");
    assert_eq!(lines[0]["source"]["recorded_at_ms"], 123);
    let ack = f
        .json(
            &["--forward-done"],
            Some(json!({"done":["capture:s1"],"failed":["capture:s2"]})),
        )
        .await;
    assert_eq!(ack, json!({"done":1,"failed":1}));
    assert_eq!(
        f.json(
            &["--forward-done"],
            Some(json!({"done":["capture:s1"],"failed":["capture:s2"]}))
        )
        .await,
        json!({"done":0,"failed":0})
    );
    let listed = f.json(&["--retire-promoted"], None).await;
    assert_eq!(listed["blocked"], json!([n.memory.as_str()]));
    let output = f.run(&["--export-forwards"], None).await;
    assert!(output.status.success());
    assert!(output.stdout.is_empty());
}
#[tokio::test]
async fn inspect_copies_reports_only_active_links() {
    let f = Fixture::new("user:owner");
    let store = f.open().await;
    let active = create(
        &store,
        &f.project,
        "active",
        ParentRef::Root(TreeRoot::Preferences),
        &["active"],
    )
    .await;
    let rejected = create(
        &store,
        &f.project,
        "rejected",
        ParentRef::Root(TreeRoot::Preferences),
        &["rejected"],
    )
    .await;
    store
        .retire_node(&owner(), &actor(), &f.project, &rejected.memory, 2000)
        .await
        .unwrap();
    store
        .capture(event(&f.project, "pending"), &RuntimeLimits::default())
        .await
        .unwrap();
    let discarded = event(&f.project, "discarded");
    store
        .capture(discarded.clone(), &RuntimeLimits::default())
        .await
        .unwrap();
    store
        .commit_curation(
            &owner(),
            &actor(),
            &f.project,
            CurationCommit {
                links: vec![],
                operation_id: "discard".into(),
                supersedes: vec![],
                sources: vec![discarded.source],
                promotes: vec![],
                merges: vec![],
                creates: vec![],
                parked: vec![],
                stale: vec![],
                split: vec![],
                operation: LinkOperation::Create,
            },
            2000,
        )
        .await
        .unwrap();
    assert_eq!(
        store.tree_nodes(&owner(), &f.project).await.unwrap()[0].memory,
        active.memory
    );
    drop(store);
    let result = f
        .json(
            &["--inspect-copies"],
            Some(json!([
                "active",
                "rejected",
                "discarded",
                "pending",
                "missing"
            ])),
        )
        .await;
    assert_eq!(
        result,
        json!({"active":["active"],"consumed":["discarded","rejected"],"pending":["pending"],"missing":["missing"]})
    );
    let f2 = Fixture::new("project");
    assert!(
        !f2.run(&["--inspect-copies"], Some(json!([])))
            .await
            .status
            .success()
    );
}
#[tokio::test]
async fn retire_only_listed_eligible_nodes() {
    let f = Fixture::new("project");
    let store = f.open().await;
    let parent = create(
        &store,
        &f.project,
        "parent",
        ParentRef::Root(TreeRoot::Preferences),
        &["parent-source"],
    )
    .await;
    let a = create(
        &store,
        &f.project,
        "a",
        ParentRef::Node(parent.memory.clone()),
        &["a-source"],
    )
    .await;
    let b = create(
        &store,
        &f.project,
        "b",
        ParentRef::Root(TreeRoot::Decisions),
        &["b-source"],
    )
    .await;
    let pending = create(
        &store,
        &f.project,
        "pending",
        ParentRef::Root(TreeRoot::Decisions),
        &["pending-source"],
    )
    .await;
    for n in [&a, &b, &pending] {
        review(&store, &f.project, n, PromotionVerdict::User).await;
    }
    store
        .forward_done(
            &owner(),
            &f.project,
            &["capture:a-source".into(), "capture:b-source".into()],
            &[],
            3000,
        )
        .await
        .unwrap();
    drop(store);
    let listed = f.json(&["--retire-promoted"], None).await;
    assert_eq!(listed["eligible"].as_array().unwrap().len(), 2);
    assert!(
        !f.run(&["--retire-promoted", "--apply"], None)
            .await
            .status
            .success()
    );
    let applied = f
        .json(
            &["--retire-promoted", "--apply"],
            Some(json!([
                a.memory.as_str(),
                pending.memory.as_str(),
                "unknown"
            ])),
        )
        .await;
    assert_eq!(applied["retired"], 1);
    let store = f.open().await;
    let nodes = store.tree_nodes(&owner(), &f.project).await.unwrap();
    assert!(!nodes.iter().any(|n| n.memory == a.memory));
    assert!(nodes.iter().any(|n| n.memory == b.memory));
    assert_eq!(
        nodes
            .iter()
            .find(|n| n.memory == parent.memory)
            .unwrap()
            .children,
        0
    );
    assert_eq!(
        store
            .runtime_revision(&owner(), &f.project, &a.revision)
            .await
            .unwrap()
            .state,
        LifecycleState::Rejected
    );
    assert_eq!(
        store
            .tree_queue_peek(&owner(), &f.project, TreeQueue::Stale)
            .await
            .unwrap(),
        Some(parent.memory.clone())
    );
    assert_eq!(
        store
            .promotion_sources(&owner(), &f.project, &a.memory)
            .await
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        store.inspect_sources(&owner(), &f.project).await.unwrap().0,
        4
    );
    let page = store
        .list_page(
            &owner(),
            &ListQuery {
                exclude_tree: false,
                project: f.project.clone(),
                states: vec![LifecycleState::Rejected],
                limit: 8,
                memory: Some(a.memory.clone()),
                after: None,
            },
            true,
        )
        .await
        .unwrap();
    assert!(
        page.audit
            .iter()
            .any(|e| e.to == LifecycleState::Rejected && e.action == ReviewAction::Reject)
    );
    let runtime = MemoryRuntime::new(
        store,
        RuntimeScope {
            owner: owner(),
            actor: actor(),
            projects: vec![f.project.clone()],
        },
        RuntimeLimits::default(),
    )
    .unwrap();
    let brief = runtime.brief(&f.project).await.unwrap();
    assert!(
        !brief
            .roots
            .iter()
            .flat_map(|r| &r.nodes)
            .any(|n| n.node == a.memory)
    );
    drop(runtime);
    assert_eq!(
        f.json(
            &["--retire-promoted", "--apply"],
            Some(json!([a.memory.as_str()]))
        )
        .await["retired"],
        0
    );
}
#[tokio::test]
async fn inspect_copies_accepts_ten_thousand_long_ids() {
    let f = Fixture::new("user:owner");
    let ids: Vec<_> = (0..10_000)
        .map(|n| format!("{}-{n}", "x".repeat(450)))
        .collect();
    let result = f.json(&["--inspect-copies"], Some(json!(ids))).await;
    assert_eq!(result["missing"].as_array().unwrap().len(), 10_000);
    let too_many: Vec<_> = (0..10_001).map(|n| format!("s{n}")).collect();
    assert!(
        !f.run(&["--inspect-copies"], Some(json!(too_many)))
            .await
            .status
            .success()
    );
}

#[tokio::test]
async fn promotion_reviews_are_atomic_and_revision_scoped() {
    let f = Fixture::new("project");
    let store = f.open().await;
    let a = create(
        &store,
        &f.project,
        "a",
        ParentRef::Root(TreeRoot::Preferences),
        &["a"],
    )
    .await;
    let b = create(
        &store,
        &f.project,
        "b",
        ParentRef::Root(TreeRoot::Decisions),
        &["b"],
    )
    .await;
    let mut batch = Vec::new();
    for n in [&a, &b] {
        batch.push((
            PromotionCandidate {
                node: n.clone(),
                path: vec![],
                sources: store
                    .promotion_sources(&owner(), &f.project, &n.memory)
                    .await
                    .unwrap(),
            },
            PromotionVerdict::User,
        ));
    }
    batch[1].0.node.revision = RevisionId::new("wrong-revision").unwrap();
    assert_eq!(
        store
            .commit_promotion_reviews(&owner(), &f.project, &batch, 2000)
            .await
            .unwrap_err(),
        RuntimeError::Conflict
    );
    assert!(
        store
            .promotion_reviews(&owner(), &f.project)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        store
            .export_capture_forwards(&owner(), &f.project)
            .await
            .unwrap()
            .is_empty()
    );
    review(&store, &f.project, &a, PromotionVerdict::User).await;
    store
        .forward_done(&owner(), &f.project, &["capture:a".into()], &[], 2100)
        .await
        .unwrap();
    let items = store
        .commit_tree_maintenance(
            &owner(),
            &actor(),
            &f.project,
            MaintenanceCommit {
                operation_id: "rewrite".into(),
                operation: LinkOperation::Merge,
                rewrites: vec![(
                    a.memory.clone(),
                    a.revision.clone(),
                    "Revised".into(),
                    "Project-specific statement.".into(),
                    vec![SourceId::new("a").unwrap()],
                )],
                creates: vec![],
                reparent: vec![],
                expected_reparent: vec![],
                move_sources: vec![],
                stale: vec![],
                queue_item: None,
            },
            2200,
        )
        .await
        .unwrap();
    store
        .review(
            &owner(),
            &actor(),
            &ReviewRequest {
                project: f.project.clone(),
                revision: items[0].revision.id.clone(),
                action: ReviewAction::Promote,
                expected_active: Some(a.revision),
                conflict_with: None,
            },
            2201,
        )
        .await
        .unwrap();
    assert!(
        store
            .retire_promoted(
                &owner(),
                &actor(),
                &f.project,
                Some(std::slice::from_ref(&a.memory)),
                2300
            )
            .await
            .unwrap()
            .eligible
            .is_empty()
    );
    let runtime = MemoryRuntime::new(
        store,
        RuntimeScope {
            owner: owner(),
            actor: actor(),
            projects: vec![f.project.clone()],
        },
        RuntimeLimits::default(),
    )
    .unwrap();
    assert!(
        runtime
            .inspect_promotions(&f.project)
            .await
            .unwrap()
            .nodes
            .contains(&a.memory)
    );
}

#[tokio::test]
async fn native_inspect_and_consent_bounds() {
    let f = Fixture::new("project");
    let store = f.open().await;
    let n = create(
        &store,
        &f.project,
        "n",
        ParentRef::Root(TreeRoot::Decisions),
        &["s"],
    )
    .await;
    drop(store);
    let plan = f.json(&["--inspect-promotions"], None).await;
    assert_eq!(
        plan,
        json!({"project":"project","nodes":[n.memory.as_str()],"batches":1,"linked_sources":1,"accept_charges":false})
    );
    #[cfg(feature = "router-http")]
    {
        let output = f.run(&["--promote-existing"], Some(plan)).await;
        assert!(!output.status.success());
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("exact promotion consent required")
        );
    }
    let user = Fixture::new("user:owner");
    assert!(
        !user
            .run(&["--inspect-promotions"], None)
            .await
            .status
            .success()
    );
}
