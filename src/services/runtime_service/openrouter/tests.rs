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
                            (
                                200,
                                format!(
                                    r#"{{"model":"openai/gpt-6-luna-decisions","usage":{usage},"answers":{{
                            "best":{{"type":"choice","choice":"c0","confidence":0.95,"probabilities":{{"c0":0.96,"none":0.04}}}},
                            "relevant:c0":{{"type":"noul","noul":0.99}},
                            "utility:c0":{{"type":"score","score":1.9,"confidence":0.8}},
                            "supports:c0":{{"type":"noul","noul":0.97}}}}}}"#
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
