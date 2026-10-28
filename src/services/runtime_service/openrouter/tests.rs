//! Recall degradation against a loopback fake OpenRouter (planner, embeddings and Luna).
use super::*;
use crate::domain_models::runtime_diagnostics::{
    ActivityEntryKind, ActivityOutcome, ActivityReason, ActivityRequest,
};
use crate::domain_models::{ActorId, OwnerId};
use crate::integrations::database::embedded_capture::EmbeddedCaptureStore;
use std::collections::VecDeque;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);

#[derive(Clone)]
struct Fake {
    base: String,
    paths: Arc<std::sync::Mutex<Vec<String>>>,
    luna_bodies: Arc<std::sync::Mutex<Vec<String>>>,
    plans: Arc<std::sync::Mutex<VecDeque<(u16, String)>>>,
    delay_ms: Arc<AtomicU64>,
}
impl Fake {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let fake = Fake {
            base: format!("http://{}", listener.local_addr().unwrap()),
            paths: Arc::default(),
            luna_bodies: Arc::default(),
            plans: Arc::default(),
            delay_ms: Arc::default(),
        };
        let shared = fake.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut socket) = stream else { return };
                let shared = shared.clone();
                std::thread::spawn(move || {
                    let mut bytes = Vec::new();
                    let mut buffer = [0; 4096];
                    let mut needed = None;
                    let mut body_start = 0;
                    while needed.is_none_or(|n| bytes.len() < n) {
                        let n = socket.read(&mut buffer).unwrap_or(0);
                        if n == 0 {
                            break;
                        }
                        bytes.extend_from_slice(&buffer[..n]);
                        if needed.is_none()
                            && let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n")
                        {
                            let head = std::str::from_utf8(&bytes[..end]).unwrap().to_lowercase();
                            let length = head
                                .lines()
                                .find_map(|l| {
                                    l.strip_prefix("content-length: ")
                                        .map(|v| v.parse::<usize>().unwrap())
                                })
                                .unwrap_or(0);
                            body_start = end + 4;
                            needed = Some(body_start + length);
                        }
                    }
                    let text = String::from_utf8_lossy(&bytes).into_owned();
                    let path = text
                        .split_whitespace()
                        .nth(1)
                        .unwrap_or_default()
                        .to_owned();
                    shared.paths.lock().unwrap().push(path.clone());
                    let delayed =
                        path == "/api/v1/embeddings" || path == "/api/v1/chat/completions";
                    if delayed {
                        std::thread::sleep(std::time::Duration::from_millis(
                            shared.delay_ms.load(Ordering::SeqCst),
                        ));
                    }
                    let usage = r#"{"prompt_tokens":12,"completion_tokens":3,"cost":0.000001}"#;
                    let (status, body) = match path.as_str() {
                        "/api/v1/embeddings" => {
                            let vector = vec!["1.0"; 128].join(",");
                            (
                                200,
                                format!(
                                    r#"{{"model":"google/gemini-embedding-2","usage":{usage},"data":[{{"index":0,"embedding":[{vector}]}}]}}"#
                                ),
                            )
                        }
                        "/api/alpha/decisions" => {
                            shared
                                .luna_bodies
                                .lock()
                                .unwrap()
                                .push(text[body_start..].to_owned());
                            let body = &text[body_start..];
                            let slots: Vec<_> = (0..8)
                                .filter(|i| body.contains(&format!("\"relevant:c{i}\"")))
                                .collect();
                            let best = slots.iter().copied().find(|i| body.contains(&format!("\"id\":\"c{i}\",\"source\":\"s{i}\",\"text\":\"SQLite vault\""))).unwrap_or(0);
                            let mut answers = Vec::new();
                            let mut probabilities = vec!["\"none\":0.04".to_owned()];
                            for slot in slots {
                                probabilities.push(format!(
                                    "\"c{slot}\":{}",
                                    if slot == best { 0.96 } else { 0.0 }
                                ));
                                answers.push(format!(
                                    "\"relevant:c{slot}\":{{\"type\":\"noul\",\"noul\":0.99}}"
                                ));
                                answers.push(format!("\"utility:c{slot}\":{{\"type\":\"score\",\"score\":1.9,\"confidence\":0.8}}"));
                                answers.push(format!(
                                    "\"supports:c{slot}\":{{\"type\":\"noul\",\"noul\":{}}}",
                                    if slot == best { 0.97 } else { 0.1 }
                                ));
                            }
                            answers.push(format!("\"best\":{{\"type\":\"choice\",\"choice\":\"c{best}\",\"confidence\":0.95,\"probabilities\":{{{}}}}}", probabilities.join(",")));
                            (
                                200,
                                format!(
                                    "{{\"model\":\"openai/gpt-6-luna-decisions\",\"usage\":{usage},\"answers\":{{{}}}}}",
                                    answers.join(",")
                                ),
                            )
                        }
                        _ => {
                            let (status, content) = shared
                                .plans
                                .lock()
                                .unwrap()
                                .pop_front()
                                .unwrap_or((200, String::new()));
                            let inner = content.replace('"', "\\\"").replace('\n', "\\n");
                            (
                                status,
                                format!(
                                    r#"{{"model":"anthropic/claude-haiku-5.5","usage":{usage},"choices":[{{"finish_reason":"stop","message":{{"content":"{inner}"}}}}]}}"#
                                ),
                            )
                        }
                    };
                    let _ = write!(
                        socket,
                        "HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                });
            }
        });
        fake
    }
    fn plan(&self, status: u16, content: &str) {
        self.plans
            .lock()
            .unwrap()
            .push_back((status, content.into()));
    }
    fn calls(&self, path: &str) -> usize {
        self.paths
            .lock()
            .unwrap()
            .iter()
            .filter(|p| *p == path)
            .count()
    }
}

fn project() -> ProjectId {
    ProjectId::new("one").unwrap()
}
fn owner() -> OwnerId {
    OwnerId::new("owner").unwrap()
}
/// A runtime over the fake, holding one active, indexed memory about SQLite.
async fn setup(fake: &Fake) -> MemoryRuntime {
    let path = format!(
        "/tmp/lambda-recall-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    );
    let store = EmbeddedRuntimeStore::new(EmbeddedCaptureStore::open(&path).await.unwrap());
    let actor = ActorId::new("actor").unwrap();
    let runtime = MemoryRuntime::with_openrouter(
        store.clone(),
        RuntimeScope {
            owner: owner(),
            actor: actor.clone(),
            projects: vec![project()],
        },
        RuntimeLimits {
            idle_ms: 1,
            ..RuntimeLimits::default()
        },
        RouterRuntimeProviders::mock(&fake.base, 128),
    )
    .unwrap();
    store
        .capture(
            CaptureEvent {
                source: SourceId::new("source").unwrap(),
                owner: owner(),
                actor: actor.clone(),
                project: project(),
                origin: "fixture://recall".into(),
                recorded_at_ms: 10,
                kind: WorkEventKind::Decision,
                outcome: WorkOutcome::Unknown,
                content: "Use SQLite for durable storage.".into(),
            },
            &RuntimeLimits::default(),
        )
        .await
        .unwrap();
    let item = store
        .propose(
            &owner(),
            &actor,
            RevisionProposal {
                project: project(),
                memory: MemoryId::new("memory").unwrap(),
                revision: RevisionId::new("revision").unwrap(),
                sources: vec![SourceId::new("source").unwrap()],
                content: "Use SQLite for durable storage.".into(),
                predecessor: None,
            },
            100,
        )
        .await
        .unwrap();
    runtime
        .review(ReviewRequest {
            project: project(),
            revision: item.revision.id.clone(),
            action: ReviewAction::Promote,
            expected_active: None,
            conflict_with: None,
        })
        .await
        .result
        .unwrap();
    assert_eq!(fake.calls("/api/v1/embeddings"), 1, "indexed during review");
    runtime
}
async fn query_planning(runtime: &MemoryRuntime) -> Vec<(ActivityOutcome, Option<ActivityReason>)> {
    let page = runtime
        .activity(ActivityRequest {
            project: project(),
            limit: 32,
            operations: Vec::new(),
            outcomes: Vec::new(),
            cursor: None,
        })
        .await
        .unwrap();
    page.entries
        .iter()
        .filter(|entry| {
            entry.entry_kind == ActivityEntryKind::Stage
                && entry.phase == ActivityPhase::QueryPlanning
        })
        .map(|entry| (entry.outcome, entry.reason))
        .collect()
}

#[tokio::test]
async fn unusable_planner_output_degrades_to_vector_only_recall() {
    for content in [
        "not json at all",
        "{\"terms\":[]}",
        "texto {\"terms\":[\"sqlite\"]}",
    ] {
        let fake = Fake::start();
        let runtime = setup(&fake).await;
        fake.plan(200, content);
        let recalled = runtime
            .recall("storage", &[project()], 4096)
            .await
            .result
            .unwrap();
        assert_eq!(recalled.hits.len(), 1, "{content}");
        assert_eq!(recalled.hits[0].item.revision.id.as_str(), "revision");
        assert_eq!(fake.calls("/api/v1/chat/completions"), 1, "no retry");
        assert_eq!(
            fake.calls("/api/v1/embeddings"),
            2,
            "the query is still embedded"
        );
        // The selector received no lexical terms, and the vector search supplied the candidate.
        let luna = fake.luna_bodies.lock().unwrap().clone();
        assert_eq!(luna.len(), 1);
        assert!(luna[0].contains("\"retrieval_terms\":[]"), "{}", luna[0]);
        assert_eq!(
            query_planning(&runtime).await,
            vec![(
                ActivityOutcome::Failed,
                Some(ActivityReason::ProviderInvalidOutput)
            )]
        );
    }
}

#[tokio::test]
async fn planner_terms_still_feed_the_lexical_search_and_selector() {
    let fake = Fake::start();
    let runtime = setup(&fake).await;
    fake.plan(200, "```json\n{\"terms\":[\"SQLite\"]}\n```");
    let recalled = runtime
        .recall("storage", &[project()], 4096)
        .await
        .result
        .unwrap();
    assert_eq!(recalled.hits.len(), 1);
    assert!(fake.luna_bodies.lock().unwrap()[0].contains("\"retrieval_terms\":[\"sqlite\"]"));
    assert_eq!(
        query_planning(&runtime).await,
        vec![(ActivityOutcome::Succeeded, None)]
    );
}

#[tokio::test]
async fn other_planner_failures_still_fail_the_recall_closed() {
    // 500: charge uncertain; 402: rejected before billing. Neither is retried or degraded.
    for (status, reason) in [
        (500, ActivityReason::ProviderUnavailable),
        (402, ActivityReason::ProviderRejected),
    ] {
        let fake = Fake::start();
        let runtime = setup(&fake).await;
        fake.plan(status, "{}");
        let error = runtime
            .recall("storage", &[project()], 4096)
            .await
            .result
            .unwrap_err();
        assert_eq!(
            error,
            if status == 500 {
                RuntimeError::ProviderUnavailable
            } else {
                RuntimeError::ProviderRejected
            }
        );
        assert_eq!(fake.calls("/api/v1/chat/completions"), 1, "no retry");
        // Planner and embedding run concurrently: the embedding may already be paid, never retried.
        assert_eq!(
            fake.calls("/api/v1/embeddings"),
            2,
            "one query embedding, no retry"
        );
        assert_eq!(fake.calls("/api/alpha/decisions"), 0);
        assert_eq!(
            query_planning(&runtime).await,
            vec![(ActivityOutcome::Failed, Some(reason))]
        );
    }
}

#[tokio::test]
async fn planner_and_query_embedding_run_concurrently() {
    let fake = Fake::start();
    let runtime = setup(&fake).await;
    fake.plan(200, "{\"terms\":[\"SQLite\"]}");
    fake.delay_ms.store(500, Ordering::SeqCst);
    let started = std::time::Instant::now();
    let recalled = runtime
        .recall("storage", &[project()], 4096)
        .await
        .result
        .unwrap();
    let elapsed = started.elapsed();
    eprintln!("concurrent recall took {} ms", elapsed.as_millis());
    // Serial planner + embedding would take at least 600 ms.
    assert!(
        elapsed < std::time::Duration::from_millis(900),
        "{elapsed:?}"
    );
    assert_eq!(recalled.hits.len(), 1);
    assert_eq!(recalled.hits[0].item.revision.id.as_str(), "revision");
    assert_eq!(fake.calls("/api/v1/chat/completions"), 1);
    assert_eq!(fake.calls("/api/v1/embeddings"), 2);
    assert_eq!(
        query_planning(&runtime).await,
        vec![(ActivityOutcome::Succeeded, None)]
    );
}

async fn tree_fixture(fake: &Fake) -> MemoryRuntime {
    let runtime = setup(fake).await;
    runtime
        .store
        .set_tree_mode(
            &owner(),
            &project(),
            crate::domain_models::memory_tree::TreeMode::Tree,
            101,
        )
        .await
        .unwrap();
    runtime
}

async fn add_tree_node(
    runtime: &MemoryRuntime,
    source: &str,
    parent: crate::domain_models::memory_tree::ParentRef,
    title: &str,
    content: &str,
    vector: Vec<f64>,
) -> RuntimeRevision {
    use crate::domain_models::embeddings::Embedding;
    use crate::domain_models::memory_tree::{CurationCommit, LinkOperation};
    let source = SourceId::new(source).unwrap();
    runtime
        .store
        .capture(
            CaptureEvent {
                source: source.clone(),
                owner: owner(),
                actor: runtime.scope.actor.clone(),
                project: project(),
                origin: "fixture://tree".into(),
                recorded_at_ms: 10,
                kind: WorkEventKind::Decision,
                outcome: WorkOutcome::Unknown,
                content: format!("Original {title}"),
            },
            &runtime.limits,
        )
        .await
        .unwrap();
    let commit = CurationCommit {
        operation_id: format!("create-{source}"),
        sources: vec![source.clone()],
        merges: vec![],
        creates: vec![(parent, title.into(), content.into(), vec![source])],
        parked: vec![],
        stale: vec![],
        split: vec![],
        operation: LinkOperation::Create,
    };
    let item = runtime
        .store
        .commit_curation(&owner(), &runtime.scope.actor, &project(), commit, 110)
        .await
        .unwrap()
        .remove(0);
    runtime
        .store
        .save_runtime_embedding(
            &item,
            &Embedding {
                space: runtime.providers.as_ref().unwrap().space().unwrap(),
                values: vector,
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
                revision: item.revision.id.clone(),
                action: ReviewAction::Promote,
                expected_active: None,
                conflict_with: None,
            },
            111,
        )
        .await
        .unwrap();
    item
}
fn query_vector() -> Vec<f64> {
    vec![1.0; 128]
}

#[tokio::test]
async fn tree_recall_returns_path_and_node() {
    use crate::domain_models::memory_tree::{ParentRef, TreeRoot};
    let fake = Fake::start();
    let runtime = tree_fixture(&fake).await;
    let node = add_tree_node(
        &runtime,
        "tree-source",
        ParentRef::Root(TreeRoot::Decisions),
        "Secrets vault",
        "SQLite vault storage",
        query_vector(),
    )
    .await;
    fake.plan(200, "{\"terms\":[\"sqlite\"]}");
    let result = runtime
        .recall("storage", &[project()], 4096)
        .await
        .result
        .unwrap();
    assert_eq!(result.tree_hits.len(), 1);
    assert_eq!(result.tree_hits[0].node, node.revision.memory);
    assert_eq!(result.tree_hits[0].path, ["Decisions", "Secrets vault"]);
    assert!(result.context.contains("Decisions › Secrets vault\n"));
}
#[tokio::test]
async fn tree_recall_descends_to_selected_child() {
    use crate::domain_models::memory_tree::{ParentRef, TreeRoot};
    let fake = Fake::start();
    let runtime = tree_fixture(&fake).await;
    let mut parent_vector = vec![0.0; 128];
    parent_vector[0] = 1.0;
    let parent = add_tree_node(
        &runtime,
        "parent-source",
        ParentRef::Root(TreeRoot::Decisions),
        "Secrets vault",
        "SQLite vault",
        parent_vector,
    )
    .await;
    let child = add_tree_node(
        &runtime,
        "child-source",
        ParentRef::Node(parent.revision.memory.clone()),
        "Storage",
        "Durable records",
        query_vector(),
    )
    .await;
    fake.plan(200, "{\"terms\":[\"sqlite\"]}");
    let result = runtime
        .recall("storage", &[project()], 4096)
        .await
        .result
        .unwrap();
    assert_eq!(fake.calls("/api/alpha/decisions"), 2);
    assert_eq!(result.tree_hits.len(), 1);
    assert_eq!(result.tree_hits[0].node, child.revision.memory);
    assert_eq!(
        result.tree_hits[0].path,
        ["Decisions", "Secrets vault", "Storage"]
    );
}
#[tokio::test]
async fn tree_recall_without_children_makes_one_luna_call() {
    use crate::domain_models::memory_tree::{ParentRef, TreeRoot};
    let fake = Fake::start();
    let runtime = tree_fixture(&fake).await;
    add_tree_node(
        &runtime,
        "node-source",
        ParentRef::Root(TreeRoot::Preferences),
        "Storage",
        "SQLite storage",
        query_vector(),
    )
    .await;
    fake.plan(200, "{\"terms\":[\"sqlite\"]}");
    assert_eq!(
        runtime
            .recall("storage", &[project()], 4096)
            .await
            .result
            .unwrap()
            .hits
            .len(),
        1
    );
    assert_eq!(fake.calls("/api/alpha/decisions"), 1);
}
#[tokio::test]
async fn tree_recall_on_empty_tree_returns_empty_without_luna() {
    let fake = Fake::start();
    let runtime = tree_fixture(&fake).await;
    fake.plan(200, "{\"terms\":[\"sqlite\"]}");
    let result = runtime
        .recall("storage", &[project()], 4096)
        .await
        .result
        .unwrap();
    assert!(result.hits.is_empty());
    assert_eq!(fake.calls("/api/alpha/decisions"), 0);
}
#[tokio::test]
async fn tree_recall_ignores_flat_revisions() {
    use crate::domain_models::memory_tree::{ParentRef, TreeRoot};
    let fake = Fake::start();
    let runtime = tree_fixture(&fake).await;
    let node = add_tree_node(
        &runtime,
        "tree-source",
        ParentRef::Root(TreeRoot::Architecture),
        "Vault",
        "SQLite vault",
        query_vector(),
    )
    .await;
    fake.plan(200, "{\"terms\":[\"sqlite\"]}");
    let result = runtime
        .recall("storage", &[project()], 4096)
        .await
        .result
        .unwrap();
    assert_eq!(result.hits.len(), 1);
    assert_eq!(result.hits[0].item.revision.id, node.revision.id);
    assert!(!fake.luna_bodies.lock().unwrap()[0].contains("Use SQLite for durable storage."));
}
#[tokio::test]
async fn flat_mode_recall_unchanged() {
    let fake = Fake::start();
    let runtime = setup(&fake).await;
    fake.plan(200, "{\"terms\":[\"sqlite\"]}");
    let result = runtime
        .recall("storage", &[project()], 4096)
        .await
        .result
        .unwrap();
    assert_eq!(result.hits.len(), 1);
    assert_eq!(result.hits[0].item.revision.id.as_str(), "revision");
    assert!(result.tree_hits.is_empty());
    assert_eq!(fake.calls("/api/alpha/decisions"), 1);
}

#[tokio::test]
async fn tree_recall_does_not_descend_to_equal_or_lower_scores() {
    use crate::domain_models::memory_tree::{ParentRef, TreeRoot};
    let fake = Fake::start();
    let runtime = tree_fixture(&fake).await;
    let parent = add_tree_node(
        &runtime,
        "parent-source",
        ParentRef::Root(TreeRoot::Decisions),
        "Vault",
        "SQLite vault",
        query_vector(),
    )
    .await;
    add_tree_node(
        &runtime,
        "child-source",
        ParentRef::Node(parent.revision.memory.clone()),
        "Storage",
        "Records",
        query_vector(),
    )
    .await;
    fake.plan(200, "{\"terms\":[\"sqlite\"]}");
    let result = runtime
        .recall("storage", &[project()], 4096)
        .await
        .result
        .unwrap();
    assert_eq!(result.tree_hits[0].node, parent.revision.memory);
    assert_eq!(fake.calls("/api/alpha/decisions"), 1);
}

#[tokio::test]
async fn tree_recall_caps_descent_candidates_at_eight() {
    use crate::domain_models::memory_tree::{ParentRef, TreeRoot};
    let fake = Fake::start();
    let runtime = tree_fixture(&fake).await;
    let mut parent_vector = vec![0.0; 128];
    parent_vector[0] = 1.0;
    let parent = add_tree_node(
        &runtime,
        "parent-source",
        ParentRef::Root(TreeRoot::Decisions),
        "Vault",
        "SQLite vault",
        parent_vector,
    )
    .await;
    for i in 0..9 {
        add_tree_node(
            &runtime,
            &format!("child-{i}"),
            ParentRef::Node(parent.revision.memory.clone()),
            &format!("Child {i}"),
            "Records",
            query_vector(),
        )
        .await;
    }
    fake.plan(200, "{\"terms\":[\"sqlite\"]}");
    let result = runtime
        .recall("storage", &[project()], 4096)
        .await
        .result
        .unwrap();
    assert_eq!(result.hits.len(), 1);
    let bodies = fake.luna_bodies.lock().unwrap();
    assert_eq!(bodies.len(), 2);
    assert_eq!(bodies[1].matches("\"relevant:c").count(), 8);
}

#[tokio::test]
async fn multi_project_recall_uses_each_projects_mode() {
    use crate::domain_models::embeddings::Embedding;
    use crate::domain_models::memory_tree::{ParentRef, TreeRoot};
    let fake = Fake::start();
    let mut runtime = tree_fixture(&fake).await;
    let tree = add_tree_node(
        &runtime,
        "tree-source",
        ParentRef::Root(TreeRoot::Decisions),
        "Vault",
        "SQLite vault",
        query_vector(),
    )
    .await;
    let flat = ProjectId::new("other").unwrap();
    runtime.scope.projects.push(flat.clone());
    let source = SourceId::new("other-source").unwrap();
    runtime
        .store
        .capture(
            CaptureEvent {
                source: source.clone(),
                owner: owner(),
                actor: runtime.scope.actor.clone(),
                project: flat.clone(),
                origin: "fixture://flat".into(),
                recorded_at_ms: 10,
                kind: WorkEventKind::Decision,
                outcome: WorkOutcome::Unknown,
                content: "Flat original".into(),
            },
            &runtime.limits,
        )
        .await
        .unwrap();
    let item = runtime
        .store
        .propose(
            &owner(),
            &runtime.scope.actor,
            RevisionProposal {
                project: flat.clone(),
                memory: MemoryId::new("flat-other").unwrap(),
                revision: RevisionId::new("flat-other-revision").unwrap(),
                sources: vec![source],
                content: "Other SQLite storage".into(),
                predecessor: None,
            },
            100,
        )
        .await
        .unwrap();
    runtime
        .store
        .save_runtime_embedding(
            &item,
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
                project: flat.clone(),
                revision: item.revision.id.clone(),
                action: ReviewAction::Promote,
                expected_active: None,
                conflict_with: None,
            },
            101,
        )
        .await
        .unwrap();
    fake.plan(200, "{\"terms\":[\"sqlite\"]}");
    fake.plan(200, "{\"terms\":[\"sqlite\"]}");
    let result = runtime
        .recall("storage", &[project(), flat], 4096)
        .await
        .result
        .unwrap();
    assert_eq!(result.hits.len(), 2);
    assert_eq!(result.tree_hits.len(), 1);
    assert_eq!(result.tree_hits[0].node, tree.revision.memory);
    assert!(
        result
            .hits
            .iter()
            .any(|hit| hit.item.revision.id == item.revision.id)
    );
    assert_eq!(fake.calls("/api/alpha/decisions"), 2);
}

#[tokio::test]
async fn tree_path_obeys_packing_budget() {
    use crate::domain_models::memory_tree::{ParentRef, TreeRoot};
    let fake = Fake::start();
    let runtime = tree_fixture(&fake).await;
    add_tree_node(
        &runtime,
        "node-source",
        ParentRef::Root(TreeRoot::OpenItems),
        "Cofre",
        "SQLite storage",
        query_vector(),
    )
    .await;
    fake.plan(200, "{\"terms\":[\"sqlite\"]}");
    let result = runtime
        .recall("storage", &[project()], 200)
        .await
        .result
        .unwrap();
    assert!(result.context.len() <= 200);
    assert_eq!(result.tree_hits.len(), result.hits.len());
    if !result.hits.is_empty() {
        assert!(result.context.contains("Open items › Cofre\n"));
    }
}
