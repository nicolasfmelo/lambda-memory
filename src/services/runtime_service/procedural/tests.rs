//! Playbook generation against a loopback fake OpenRouter: the integration tests cannot reach
//! the fixed provider endpoint, so these run in-crate over `RouterRuntimeProviders::mock`.
use super::*;
use crate::domain_models::runtime_diagnostics::{
    ActivityOutcome, ActivityRequest, RecordedOperation,
};
use crate::domain_models::{ActorId, OwnerId};
use crate::integrations::database::embedded_capture::EmbeddedCaptureStore;
use crate::integrations::external_apis::router_runtime::RouterRuntimeProviders;
use std::collections::VecDeque;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::Arc;

static NEXT: AtomicU64 = AtomicU64::new(0);

#[derive(Clone)]
struct Fake {
    base: String,
    paths: Arc<std::sync::Mutex<Vec<String>>>,
    playbooks: Arc<std::sync::Mutex<VecDeque<String>>>,
}
impl Fake {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let fake = Fake {
            base: format!("http://{}", listener.local_addr().unwrap()),
            paths: Arc::default(),
            playbooks: Arc::default(),
        };
        let shared = fake.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut socket) = stream else { return };
                let mut bytes = Vec::new();
                let mut buffer = [0; 4096];
                let mut needed = None;
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
                        needed = Some(end + 4 + length);
                    }
                }
                let head = String::from_utf8_lossy(&bytes);
                let path = head
                    .split_whitespace()
                    .nth(1)
                    .unwrap_or_default()
                    .to_owned();
                shared.paths.lock().unwrap().push(path.clone());
                let usage = r#"{"prompt_tokens":12,"completion_tokens":3,"cost":0.000001}"#;
                let body = if path == "/api/v1/embeddings" {
                    let vector = vec!["1.0"; 128].join(",");
                    format!(
                        r#"{{"model":"google/gemini-embedding-2","usage":{usage},"data":[{{"index":0,"embedding":[{vector}]}}]}}"#
                    )
                } else {
                    // The playbook text is plain ASCII without quotes, so only the nesting escapes.
                    let text = shared
                        .playbooks
                        .lock()
                        .unwrap()
                        .pop_front()
                        .unwrap_or_default();
                    let inner = format!(r#"{{"playbook":"{text}"}}"#).replace('"', "\\\"");
                    format!(
                        r#"{{"model":"anthropic/claude-haiku-5.5","usage":{usage},"choices":[{{"finish_reason":"stop","message":{{"content":"{inner}"}}}}]}}"#
                    )
                };
                let _ = write!(
                    socket,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
            }
        });
        fake
    }
    fn answer(&self, text: &str) {
        self.playbooks.lock().unwrap().push_back(text.into());
    }
    fn calls(&self, path: &str) -> usize {
        self.paths
            .lock()
            .unwrap()
            .iter()
            .filter(|p| *p == path)
            .count()
    }
    fn chats(&self) -> usize {
        self.calls("/api/v1/chat/completions")
    }
    fn embeddings(&self) -> usize {
        self.calls("/api/v1/embeddings")
    }
}

fn project() -> ProjectId {
    ProjectId::new("one").unwrap()
}
fn owner() -> OwnerId {
    OwnerId::new("owner").unwrap()
}
async fn setup(fake: &Fake) -> (MemoryRuntime, EmbeddedRuntimeStore) {
    let path = format!(
        "/tmp/lambda-playbook-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    );
    let store = EmbeddedRuntimeStore::new(EmbeddedCaptureStore::open(&path).await.unwrap());
    let runtime = MemoryRuntime::with_openrouter(
        store.clone(),
        RuntimeScope {
            owner: owner(),
            actor: ActorId::new("actor").unwrap(),
            projects: vec![project()],
        },
        RuntimeLimits {
            idle_ms: 1,
            ..RuntimeLimits::default()
        },
        RouterRuntimeProviders::mock(&fake.base, 128),
    )
    .unwrap();
    (runtime, store)
}
/// `count` successful `cargo test` episodes, two sessions, recorded from `at_ms` on.
async fn episodes(store: &EmbeddedRuntimeStore, tag: &str, count: usize, at_ms: u64) {
    episodes_in(store, tag, count, at_ms, 2).await
}
async fn episodes_in(
    store: &EmbeddedRuntimeStore,
    tag: &str,
    count: usize,
    at_ms: u64,
    sessions: usize,
) {
    for i in 0..count {
        let content = format!(
            r#"{{"v":1,"tool":"bash","intent":"","summary":"cargo test","error_head":null,"session":"{:016x}","seq":{i}}}"#,
            i % sessions
        );
        let event = CaptureEvent {
            source: SourceId::new(format!("{tag}-{i}")).unwrap(),
            owner: owner(),
            actor: ActorId::new("actor").unwrap(),
            project: project(),
            origin: "omp://action".into(),
            recorded_at_ms: at_ms + i as u64,
            kind: WorkEventKind::Action,
            outcome: WorkOutcome::Succeeded,
            content,
        };
        store
            .capture(event, &RuntimeLimits::default())
            .await
            .unwrap();
    }
}
async fn idle_tick(runtime: &MemoryRuntime) -> RecordedOperation<DreamReceipt> {
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    runtime.dream_once().await
}
fn activity() -> ActivityRequest {
    ActivityRequest {
        project: project(),
        limit: 32,
        operations: Vec::new(),
        outcomes: Vec::new(),
        cursor: None,
    }
}

#[tokio::test]
async fn playbook_generated_after_twenty_episodes_and_superseded() {
    let fake = Fake::start();
    let (runtime, store) = setup(&fake).await;
    let start = now_ms().unwrap() - 60_000;
    episodes(&store, "a", 19, start).await;
    fake.answer("- Run the tests with cargo test [F1]");
    idle_tick(&runtime).await.result.unwrap();
    assert_eq!(runtime.playbook(&project()).await.unwrap().content, None);
    assert_eq!(fake.chats(), 0);

    episodes(&store, "b", 1, start + 100).await;
    let receipt = idle_tick(&runtime).await.result.unwrap();
    assert_eq!(receipt.batches, 1);
    let first = runtime.playbook(&project()).await.unwrap();
    assert_eq!(
        first.content.as_deref(),
        Some("- Run the tests with cargo test [F1]")
    );
    assert_eq!(first.episodes, 20);
    assert_eq!(fake.chats(), 1);
    let status = runtime.status(&project()).await.unwrap();
    assert_eq!(
        status.stored.pending.active_without_current_embedding,
        Some(0)
    );
    assert_eq!(status.stored.lifecycle.active, 1);
    assert_eq!(fake.embeddings(), 0, "the playbook is never embedded");
    let stored = store
        .active_playbook(&owner(), &project())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.revision.memory.as_str(), PLAYBOOK_MEMORY);
    assert!(!stored.revision.sources.is_empty() && stored.revision.sources.len() <= 32);

    // The same snapshot is never phrased twice.
    idle_tick(&runtime).await.result.unwrap();
    assert_eq!(fake.chats(), 1);

    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    episodes(&store, "c", 20, now_ms().unwrap()).await;
    fake.answer("- Run the tests with cargo test [F1].");
    idle_tick(&runtime).await.result.unwrap();
    assert_eq!(fake.chats(), 2);
    let second = runtime.playbook(&project()).await.unwrap();
    assert_ne!(second.revision, first.revision);
    let successor = store
        .active_playbook(&owner(), &project())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(successor.revision.predecessor, first.revision);
    let old = store
        .runtime_revision(&owner(), &project(), first.revision.as_ref().unwrap())
        .await
        .unwrap();
    assert_eq!(old.state, LifecycleState::Superseded);
    let status = runtime.status(&project()).await.unwrap();
    assert_eq!(
        status.stored.pending.active_without_current_embedding,
        Some(0)
    );
    assert_eq!(fake.embeddings(), 0);
}

#[tokio::test]
async fn invalid_playbook_keeps_previous() {
    let fake = Fake::start();
    let (runtime, store) = setup(&fake).await;
    let start = now_ms().unwrap() - 60_000;
    episodes(&store, "a", 20, start).await;
    fake.answer("- Run the tests with cargo test [F1]");
    idle_tick(&runtime).await.result.unwrap();
    let before = runtime.playbook(&project()).await.unwrap();
    assert!(before.content.is_some());

    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    episodes(&store, "b", 20, now_ms().unwrap()).await;
    fake.answer("- Always run cargo test");
    let recorded = idle_tick(&runtime).await;
    assert_eq!(recorded.result.unwrap(), DreamReceipt::default());
    let same = |view: PlaybookView| (view.revision, view.content, view.generated_at_ms);
    assert_eq!(
        same(runtime.playbook(&project()).await.unwrap()),
        same(before.clone())
    );
    let history = runtime.activity(activity()).await.unwrap();
    assert!(
        history
            .entries
            .iter()
            .any(|e| e.phase == ActivityPhase::Playbook
                && e.outcome == ActivityOutcome::Failed
                && e.reason == Some(ActivityReason::PlaybookInvalidOutput))
    );

    // No retry: the rejected snapshot is not sent again, and dreaming keeps running.
    idle_tick(&runtime).await.result.unwrap();
    assert_eq!(fake.chats(), 2);
    assert_eq!(
        same(runtime.playbook(&project()).await.unwrap()),
        same(before)
    );
}

async fn playbook_version(
    store: &EmbeddedRuntimeStore,
    revision: &str,
    text: &str,
    parent: Option<&str>,
) -> RuntimeRevision {
    store.capture(CaptureEvent {
        source: SourceId::new(format!("source-{revision}")).unwrap(),
        owner: owner(), actor: ActorId::new("actor").unwrap(), project: project(),
        origin: "omp://action".into(), recorded_at_ms: 10, kind: WorkEventKind::Action,
        outcome: WorkOutcome::Succeeded,
        content: r#"{"v":1,"tool":"bash","intent":"","summary":"cargo test","error_head":null,"session":"0000000000000000","seq":1}"#.into(),
    }, &RuntimeLimits::default()).await.unwrap();
    store
        .propose(
            &owner(),
            &ActorId::new("actor").unwrap(),
            RevisionProposal {
                project: project(),
                memory: MemoryId::new(PLAYBOOK_MEMORY).unwrap(),
                revision: RevisionId::new(revision).unwrap(),
                sources: vec![SourceId::new(format!("source-{revision}")).unwrap()],
                content: text.into(),
                predecessor: parent.map(|p| RevisionId::new(p).unwrap()),
            },
            100,
        )
        .await
        .unwrap()
}
fn promote(revision: &RuntimeRevision) -> ReviewRequest {
    ReviewRequest {
        project: project(),
        revision: revision.revision.id.clone(),
        action: ReviewAction::Promote,
        expected_active: None,
        conflict_with: None,
    }
}

#[tokio::test]
async fn manual_promote_of_playbook_skips_indexing() {
    let fake = Fake::start();
    let (runtime, store) = setup(&fake).await;
    let version = playbook_version(&store, "playbook-manual", "- Run cargo test [F1]", None).await;
    assert_eq!(version.state, LifecycleState::Processing);
    let promoted = runtime.review(promote(&version)).await.result.unwrap();
    assert_eq!(promoted.state, LifecycleState::Active);
    assert_eq!(fake.embeddings(), 0);

    // The fake does see an embedding for an ordinary memory, so the zero above is meaningful.
    store
        .capture(
            CaptureEvent {
                source: SourceId::new("ordinary-source").unwrap(),
                owner: owner(),
                actor: ActorId::new("actor").unwrap(),
                project: project(),
                origin: "fixture://x".into(),
                recorded_at_ms: 10,
                kind: WorkEventKind::Decision,
                outcome: WorkOutcome::Unknown,
                content: "Use SurrealKV".into(),
            },
            &RuntimeLimits::default(),
        )
        .await
        .unwrap();
    let ordinary = store
        .propose(
            &owner(),
            &ActorId::new("actor").unwrap(),
            RevisionProposal {
                project: project(),
                memory: MemoryId::new("ordinary-memory").unwrap(),
                revision: RevisionId::new("ordinary-revision").unwrap(),
                sources: vec![SourceId::new("ordinary-source").unwrap()],
                content: "Use SurrealKV".into(),
                predecessor: None,
            },
            100,
        )
        .await
        .unwrap();
    runtime.review(promote(&ordinary)).await.result.unwrap();
    assert_eq!(fake.embeddings(), 1);
}

fn full_reads(runtime: &MemoryRuntime) -> usize {
    runtime.playbook_full_reads.load(Ordering::SeqCst)
}

#[tokio::test]
async fn playbook_plan_skips_full_read_for_same_snapshot() {
    let fake = Fake::start();
    let (runtime, store) = setup(&fake).await;
    let start = now_ms().unwrap() - 60_000;
    episodes(&store, "a", 20, start).await;
    fake.answer("- Always run cargo test");
    idle_tick(&runtime).await.result.unwrap();
    assert_eq!((fake.chats(), full_reads(&runtime)), (1, 1));
    assert_eq!(runtime.playbook(&project()).await.unwrap().content, None);

    // The attempted snapshot is neither phrased again nor read in full again.
    for _ in 0..3 {
        idle_tick(&runtime).await.result.unwrap();
    }
    assert_eq!((fake.chats(), full_reads(&runtime)), (1, 1));

    // A new episode is a new snapshot: it is evaluated (and phrased) once more.
    episodes(&store, "b", 1, now_ms().unwrap()).await;
    fake.answer("- Run the tests with cargo test [F1]");
    idle_tick(&runtime).await.result.unwrap();
    assert_eq!((fake.chats(), full_reads(&runtime)), (2, 2));
    assert!(
        runtime
            .playbook(&project())
            .await
            .unwrap()
            .content
            .is_some()
    );
}

#[tokio::test]
async fn factless_snapshot_is_not_re_read_until_it_changes() {
    let fake = Fake::start();
    let (runtime, store) = setup(&fake).await;
    // One session only: no procedure reaches the two-session threshold, so there are no facts.
    episodes_in(&store, "a", 20, now_ms().unwrap() - 60_000, 1).await;
    for _ in 0..3 {
        idle_tick(&runtime).await.result.unwrap();
    }
    assert_eq!((fake.chats(), full_reads(&runtime)), (0, 1));

    episodes(&store, "b", 2, now_ms().unwrap()).await;
    fake.answer("- Run the tests with cargo test [F1]");
    idle_tick(&runtime).await.result.unwrap();
    assert_eq!((fake.chats(), full_reads(&runtime)), (1, 2));
}

#[tokio::test]
async fn rejected_provider_attempt_is_not_cached_as_evaluated() {
    // Not exercisable against the loopback fake (it never refuses); pin the cache key lifecycle.
    let fake = Fake::start();
    let (runtime, _) = setup(&fake).await;
    runtime
        .playbook_evaluated
        .lock()
        .unwrap()
        .insert(project(), "key".into());
    let plan_key_cleared = {
        let plan = PlaybookPlan {
            facts: Vec::new(),
            records: Vec::new(),
            active: None,
            newest: SourceId::new("s").unwrap(),
            job: "j".into(),
        };
        runtime.release_playbook(&project(), &plan).await.unwrap();
        runtime.playbook_evaluated.lock().unwrap().is_empty()
    };
    assert!(plan_key_cleared);
}
