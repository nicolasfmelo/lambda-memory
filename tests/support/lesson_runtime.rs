use super::*;
use crate::domain_models::runtime_diagnostics::*;
use crate::domain_models::{ActorId, OwnerId};
use crate::integrations::{
    database::embedded_capture::EmbeddedCaptureStore,
    external_apis::router_runtime::RouterRuntimeProviders,
};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    io::{Read, Write},
    net::TcpListener,
    sync::{Arc, Mutex as StdMutex, atomic::AtomicUsize},
};
static NEXT: AtomicUsize = AtomicUsize::new(0);
const KEY: &str = "0123456789abcdef";
struct Fake {
    base: String,
    replies: Arc<StdMutex<VecDeque<(u16, String)>>>,
    calls: Arc<AtomicUsize>,
    inputs: Arc<StdMutex<Vec<Value>>>,
}
impl Fake {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let replies: Arc<StdMutex<VecDeque<(u16, String)>>> = Arc::default();
        let calls = Arc::new(AtomicUsize::new(0));
        let inputs: Arc<StdMutex<Vec<Value>>> = Arc::default();
        let fake = Self {
            base: format!("http://{}", listener.local_addr().unwrap()),
            replies: replies.clone(),
            calls: calls.clone(),
            inputs: inputs.clone(),
        };
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut socket) = stream else {
                    break;
                };
                let mut bytes = Vec::new();
                let mut buf = [0; 4096];
                let mut end = None;
                while end.is_none_or(|n| bytes.len() < n) {
                    let n = socket.read(&mut buf).unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    bytes.extend_from_slice(&buf[..n]);
                    if end.is_none()
                        && let Some(i) = bytes.windows(4).position(|w| w == b"\r\n\r\n")
                    {
                        let head = String::from_utf8_lossy(&bytes[..i]).to_lowercase();
                        let len = head
                            .lines()
                            .find_map(|l| {
                                l.strip_prefix("content-length: ")
                                    .and_then(|s| s.parse::<usize>().ok())
                            })
                            .unwrap_or(0);
                        end = Some(i + 4 + len);
                    }
                }
                let start = bytes.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4;
                inputs
                    .lock()
                    .unwrap()
                    .push(serde_json::from_slice(&bytes[start..]).unwrap());
                calls.fetch_add(1, Ordering::SeqCst);
                let (status, content) = replies
                    .lock()
                    .unwrap()
                    .pop_front()
                    .expect("unexpected provider call");
                if status == 0 {
                    continue;
                }
                let body = json!({"model":"anthropic/claude-haiku-5.5","usage":{"prompt_tokens":10,"completion_tokens":5,"cost":0.000001},"choices":[{"finish_reason":"stop","message":{"content":content}}]}).to_string();
                let _ = write!(
                    socket,
                    "HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
            }
        });
        fake
    }
    fn reply(&self, status: u16, answer: Value) {
        self.replies
            .lock()
            .unwrap()
            .push_back((status, answer.to_string()));
    }
    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}
async fn setup(fake: &Fake) -> MemoryRuntime {
    let path = format!(
        "/tmp/lambda-lessons-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    );
    MemoryRuntime::with_openrouter(
        EmbeddedRuntimeStore::new(EmbeddedCaptureStore::open(&path).await.unwrap()),
        RuntimeScope {
            owner: OwnerId::new("owner").unwrap(),
            actor: ActorId::new("actor").unwrap(),
            projects: vec![ProjectId::new("project").unwrap()],
        },
        RuntimeLimits {
            idle_ms: 1,
            session_idle_ms: 60_000,
            ..Default::default()
        },
        RouterRuntimeProviders::mock(&fake.base, 128),
    )
    .unwrap()
}
fn answer() -> Value {
    json!({"tasks":[{"title":"Fix tests","outcome":"succeeded","evidence":["s0"]},{"title":"User correction","outcome":"corrected_by_user","evidence":["s0"]}],"lessons":[{"kind":"strategy","scope":"project","title":"Run the targeted tests","applies_when":"Changing storage","guidance":"1. Run targeted tests.\n2. Check the result.","task":0,"evidence":["s0"]},{"kind":"pitfall","scope":"user","title":"Avoid unsolicited rewrites","applies_when":"Delegating work","guidance":"Avoid rewriting code outside the request.","task":1,"evidence":["s0"]}]})
}
async fn exchange(runtime: &MemoryRuntime, n: u64) {
    let now = now_ms().unwrap() - 120_000 + n;
    let event = CaptureEvent { source:SourceId::new(format!("{KEY}/turn-{n}")).unwrap(),owner:runtime.scope.owner.clone(),actor:runtime.scope.actor.clone(),project:runtime.scope.projects[0].clone(),origin:"fixture://exchange".into(),recorded_at_ms:now,kind:WorkEventKind::Exchange,outcome:WorkOutcome::Unknown,content:json!({"v":1,"prompt":format!("Turn {n}: please run tests, and do not rewrite unrelated code"),"reply":"Tests passed."}).to_string() };
    runtime.store.capture(event, &runtime.limits).await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
}
async fn tick(runtime: &MemoryRuntime) -> RecordedOperation<Option<DreamReceipt>> {
    let project = &runtime.scope.projects[0];
    let mut trace = runtime
        .begin_trace(
            std::slice::from_ref(project),
            ActivityOperation::Dream,
            ActivityPhase::Derivation,
            true,
        )
        .await
        .unwrap();
    let result = runtime.lesson_tick(project, &mut trace).await;
    runtime.finish_trace(trace, result).await
}
async fn lessons(runtime: &MemoryRuntime) -> Vec<CaptureEvent> {
    runtime
        .store
        .export_sources(&runtime.scope.owner, &runtime.scope.projects[0])
        .await
        .unwrap()
        .into_iter()
        .filter(|v| v["kind"] == "lesson")
        .map(|v| CaptureEvent {
            source: SourceId::new(v["source"].as_str().unwrap()).unwrap(),
            owner: runtime.scope.owner.clone(),
            actor: runtime.scope.actor.clone(),
            project: runtime.scope.projects[0].clone(),
            origin: v["origin"].as_str().unwrap().into(),
            recorded_at_ms: v["recorded_at_ms"].as_u64().unwrap(),
            kind: WorkEventKind::Lesson,
            outcome: WorkOutcome::Unknown,
            content: v["content"].as_str().unwrap().into(),
        })
        .collect()
}
async fn assert_marked(runtime: &MemoryRuntime) {
    let session = runtime
        .store
        .sessions(&runtime.scope.owner, &runtime.scope.projects[0])
        .await
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(session.extracted_through_ms, Some(session.last_activity_ms));
}
async fn reasons(runtime: &MemoryRuntime) -> Vec<ActivityReason> {
    runtime
        .activity(ActivityRequest {
            project: runtime.scope.projects[0].clone(),
            limit: 32,
            operations: vec![],
            outcomes: vec![],
            cursor: None,
        })
        .await
        .unwrap()
        .entries
        .into_iter()
        .filter_map(|e| e.reason)
        .collect()
}
#[tokio::test]
async fn lesson_job_commits_project_and_user_lessons() {
    let fake = Fake::start();
    fake.reply(200, answer());
    let runtime = setup(&fake).await;
    exchange(&runtime, 1).await;
    tick(&runtime).await.result.unwrap();
    let rows = lessons(&runtime).await;
    assert_eq!(rows.len(), 2);
    let (owner, project) = (&runtime.scope.owner, &runtime.scope.projects[0]);
    for (i, row) in rows.iter().enumerate() {
        assert_eq!(row.source.as_str(), format!("lesson/{KEY}/{}", i + 1));
        assert_eq!(row.origin, format!("lesson://{KEY}/{}", i + 1));
        assert_eq!(row.outcome, WorkOutcome::Unknown);
        let v: Value = serde_json::from_str(&row.content).unwrap();
        assert_eq!(v["v"], 1);
        assert_eq!(v["evidence"], json!([format!("{KEY}/turn-1")]));
        assert_eq!(
            v["task_outcome"],
            if i == 0 {
                "succeeded"
            } else {
                "corrected_by_user"
            }
        );
        assert_eq!(
            runtime
                .store
                .lesson_citations(owner, project, &row.source)
                .await
                .unwrap(),
            vec![SourceId::new(format!("{KEY}/turn-1")).unwrap()]
        );
    }
    let pending = runtime
        .store
        .pending_sources(owner, project, 32, false)
        .await
        .unwrap();
    assert!(pending.iter().any(|e| e.source == rows[0].source));
    assert!(!pending.iter().any(|e| e.source == rows[1].source));
    let forwards = runtime.forwards(project, 32).await.unwrap();
    assert_eq!(forwards.len(), 1);
    assert_eq!(forwards[0].forward_id, format!("capture:lesson/{KEY}/2"));
    assert!(
        runtime
            .store
            .tree_nodes(owner, project)
            .await
            .unwrap()
            .is_empty()
    );
    assert_marked(&runtime).await;
    assert_eq!(fake.calls(), 1);
    let request = &fake.inputs.lock().unwrap()[0];
    assert_eq!(
        request["response_format"]["json_schema"]["name"],
        "runtime_lessons_v1"
    );
    assert_eq!(request["response_format"]["json_schema"]["strict"], true);
}
async fn invalid(mut change: impl FnMut(&mut Value)) {
    let fake = Fake::start();
    let mut a = answer();
    change(&mut a);
    fake.reply(200, a);
    let runtime = setup(&fake).await;
    exchange(&runtime, 1).await;
    tick(&runtime).await.result.unwrap();
    assert!(lessons(&runtime).await.is_empty());
    assert_marked(&runtime).await;
    assert!(
        reasons(&runtime)
            .await
            .contains(&ActivityReason::ProviderInvalidOutput)
    );
    assert!(tick(&runtime).await.result.unwrap().is_none());
    assert_eq!(fake.calls(), 1);
}
#[tokio::test]
async fn strategy_from_failed_task_is_invalid() {
    invalid(|a| a["tasks"][0]["outcome"] = json!("failed")).await;
}
#[tokio::test]
async fn lesson_from_unknown_task_is_invalid() {
    invalid(|a| a["tasks"][0]["outcome"] = json!("unknown")).await;
}
#[tokio::test]
async fn unknown_evidence_slot_is_invalid() {
    invalid(|a| a["lessons"][0]["evidence"] = json!(["s99"])).await;
}
#[tokio::test]
async fn secret_in_lesson_is_invalid() {
    invalid(|a| a["lessons"][0]["guidance"] = json!("Use sk-syntheticfixturekey")).await;
}
#[tokio::test]
async fn seven_lessons_is_invalid() {
    invalid(|a| {
        let l = a["lessons"][0].clone();
        a["lessons"] = json!(vec![l; 7]);
    })
    .await;
}
#[tokio::test]
async fn session_without_exchanges_is_skipped_without_call() {
    let fake = Fake::start();
    let runtime = setup(&fake).await;
    exchange(&runtime, 1).await;
    let (owner, project) = (&runtime.scope.owner, &runtime.scope.projects[0]);
    let session = runtime
        .store
        .sessions(owner, project)
        .await
        .unwrap()
        .pop()
        .unwrap();
    runtime
        .store
        .mark_extracted(
            owner,
            project,
            &session.session_key,
            session.last_activity_ms,
        )
        .await
        .unwrap();
    let now = now_ms().unwrap() - 60_001;
    runtime.store.capture(CaptureEvent { owner:owner.clone(),actor:runtime.scope.actor.clone(),project:project.clone(),source:SourceId::new(format!("{KEY}/action")).unwrap(),origin:"fixture://action".into(),recorded_at_ms:now,kind:WorkEventKind::Action,outcome:WorkOutcome::Failed,content:json!({"v":1,"tool":"Bash","intent":"test storage","summary":"cargo test storage","error_head":"failure","session":KEY,"seq":1}).to_string() },&runtime.limits).await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    tick(&runtime).await.result.unwrap();
    assert_marked(&runtime).await;
    assert_eq!(fake.calls(), 0);
}
#[tokio::test]
async fn imported_lesson_ids_do_not_collide_with_live_extraction() {
    let fake_a = Fake::start();
    fake_a.reply(200, answer());
    let a = setup(&fake_a).await;
    exchange(&a, 1).await;
    tick(&a).await.result.unwrap();
    let exported = a
        .store
        .export_sources(&a.scope.owner, &a.scope.projects[0])
        .await
        .unwrap();
    let fake_b = Fake::start();
    let mut next = answer();
    next["lessons"].as_array_mut().unwrap().truncate(1);
    fake_b.reply(200, next);
    let b = setup(&fake_b).await;
    for row in exported {
        let event = CaptureEvent {
            source: SourceId::new(row["source"].as_str().unwrap()).unwrap(),
            owner: b.scope.owner.clone(),
            actor: b.scope.actor.clone(),
            project: b.scope.projects[0].clone(),
            origin: row["origin"].as_str().unwrap().into(),
            recorded_at_ms: row["recorded_at_ms"].as_u64().unwrap(),
            kind: if row["kind"] == "lesson" {
                WorkEventKind::Lesson
            } else {
                WorkEventKind::Exchange
            },
            outcome: WorkOutcome::Unknown,
            content: row["content"].as_str().unwrap().into(),
        };
        b.store.import_source(event, &b.limits).await.unwrap();
    }
    exchange(&b, 2).await;
    tick(&b).await.result.unwrap();
    let rows = lessons(&b).await;
    assert_eq!(rows.len(), 3);
    let new = rows
        .iter()
        .find(|e| e.source.as_str() == format!("lesson/{KEY}/3"))
        .expect("new lesson /3");
    assert_eq!(
        serde_json::from_str::<Value>(&new.content).unwrap()["evidence"],
        json!([format!("{KEY}/turn-2")])
    );
    assert_marked(&b).await;
    assert_eq!(fake_b.calls(), 1);
}

#[tokio::test]
async fn new_activity_extracts_only_the_new_part() {
    let fake = Fake::start();
    fake.reply(200, answer());
    fake.reply(200, answer());
    let runtime = setup(&fake).await;
    exchange(&runtime, 1).await;
    tick(&runtime).await.result.unwrap();
    exchange(&runtime, 2).await;
    tick(&runtime).await.result.unwrap();
    let rows = lessons(&runtime).await;
    assert_eq!(rows.len(), 4);
    assert_eq!(rows[3].source.as_str(), format!("lesson/{KEY}/4"));
    let v: Value = serde_json::from_str(&rows[2].content).unwrap();
    assert_eq!(v["evidence"], json!([format!("{KEY}/turn-2")]));
    assert_marked(&runtime).await;
    let requests = fake.inputs.lock().unwrap();
    let input: &str = requests[1]["messages"].as_array().unwrap().last().unwrap()["content"]
        .as_str()
        .unwrap();
    assert!(!input.contains("Turn 1:"));
    assert!(input.contains("Turn 2:"));
}
#[tokio::test]
async fn lesson_admission_released_on_rejection() {
    let fake = Fake::start();
    fake.reply(429, json!({}));
    fake.reply(200, answer());
    let runtime = setup(&fake).await;
    exchange(&runtime, 1).await;
    assert_eq!(
        tick(&runtime).await.result.unwrap_err(),
        RuntimeError::ProviderRejected
    );
    assert!(
        runtime
            .store
            .sessions(&runtime.scope.owner, &runtime.scope.projects[0])
            .await
            .unwrap()[0]
            .extracted_through_ms
            .is_none()
    );
    tick(&runtime).await.result.unwrap();
    assert_eq!(lessons(&runtime).await.len(), 2);
    assert_eq!(fake.calls(), 2);
}
#[tokio::test]
async fn uncertain_admission_marks_session_and_keeps_background_running() {
    let fake = Fake::start();
    fake.reply(0, json!({}));
    let runtime = setup(&fake).await;
    exchange(&runtime, 1).await;
    assert_eq!(
        tick(&runtime).await.result.unwrap_err(),
        RuntimeError::ProviderUnavailable
    );
    tick(&runtime).await.result.unwrap();
    assert_marked(&runtime).await;
    assert_eq!(fake.calls(), 1);
    assert!(
        reasons(&runtime)
            .await
            .contains(&ActivityReason::UncertainAdmission)
    );
    fake.reply(200,json!({"results":{"s0":[{"op":"discard","node":null,"parent":null,"title":null,"content":null}]}}));
    runtime.dream_once().await.result.unwrap();
    assert_eq!(fake.calls(), 2);
}
#[tokio::test]
async fn full_backlog_skips_session_without_call() {
    let fake = Fake::start();
    let mut runtime = setup(&fake).await;
    runtime.limits.max_pending = 7;
    exchange(&runtime, 1).await;
    assert!(tick(&runtime).await.result.unwrap().is_none());
    assert_eq!(fake.calls(), 0);
    assert!(
        runtime
            .store
            .sessions(&runtime.scope.owner, &runtime.scope.projects[0])
            .await
            .unwrap()[0]
            .extracted_through_ms
            .is_none()
    );
}
#[tokio::test]
async fn lesson_job_runs_after_curation_and_stops_on_provider_unavailable() {
    let fake = Fake::start();
    fake.reply(200,json!({"results":{"s0":[{"op":"discard","node":null,"parent":null,"title":null,"content":null}]}}));
    fake.reply(0, json!({}));
    let runtime = setup(&fake).await;
    exchange(&runtime, 1).await;
    runtime.dream_once().await.result.unwrap();
    assert!(lessons(&runtime).await.is_empty());
    assert_eq!(
        runtime.dream_once().await.result.unwrap_err(),
        RuntimeError::ProviderUnavailable
    );
    assert_eq!(fake.calls(), 2);
    assert_eq!(runtime.dream_once().await.result.unwrap().batches, 0);
    assert_eq!(fake.calls(), 2);
}

#[tokio::test]
async fn lesson_tick_handles_at_most_two_sessions() {
    let fake = Fake::start();
    for _ in 0..3 {
        fake.reply(200, json!({"tasks":[],"lessons":[]}));
    }
    let runtime = setup(&fake).await;
    exchange(&runtime, 1).await;
    let event = runtime
        .store
        .eligible_sources(
            &runtime.scope.owner,
            &runtime.scope.projects[0],
            32,
            false,
            None,
            None,
            true,
        )
        .await
        .unwrap()
        .remove(0);
    for key in ["1111111111111111", "2222222222222222"] {
        let mut other = event.clone();
        other.source = SourceId::new(format!("{key}/turn-1")).unwrap();
        runtime.store.capture(other, &runtime.limits).await.unwrap();
    }
    tick(&runtime).await.result.unwrap();
    assert_eq!(fake.calls(), 2);
    assert_eq!(
        runtime
            .ready_sessions(&runtime.scope.projects[0], now_ms().unwrap(), 8)
            .await
            .unwrap()
            .len(),
        1
    );
    tick(&runtime).await.result.unwrap();
    assert_eq!(fake.calls(), 3);
}

#[tokio::test]
async fn admission_key_covers_all_slots_even_beyond_32() {
    let fake = Fake::start();
    let runtime = setup(&fake).await;
    exchange(&runtime, 1).await;
    let (owner, project) = (&runtime.scope.owner, &runtime.scope.projects[0]);
    let mut event = runtime
        .store
        .eligible_sources(owner, project, 32, false, None, None, true)
        .await
        .unwrap()
        .remove(0);
    for n in 2..=40 {
        event.source = SourceId::new(format!("{KEY}/turn-{n:03}")).unwrap();
        event.recorded_at_ms += 1;
        runtime
            .store
            .capture(event.clone(), &runtime.limits)
            .await
            .unwrap();
    }
    let session = runtime
        .store
        .sessions(owner, project)
        .await
        .unwrap()
        .pop()
        .unwrap();
    let sources = runtime
        .store
        .session_sources(owner, project, &session)
        .await
        .unwrap();
    assert_eq!(sources.len(), 40);
    let mut parts = vec![
        lesson_policy::PROMPT_VERSION,
        owner.as_str(),
        project.as_str(),
        KEY,
    ];
    parts.extend(sources.iter().map(|s| s.source.as_str()));
    let key = format!("lesson-job:{}", stable_id(&parts));
    runtime
        .store
        .admit_runtime_operation(owner, project, &key, &sources[8..])
        .await
        .unwrap();
    tick(&runtime).await.result.unwrap();
    assert_marked(&runtime).await;
    assert_eq!(fake.calls(), 0);
    assert!(
        reasons(&runtime)
            .await
            .contains(&ActivityReason::UncertainAdmission)
    );
}

#[test]
fn lesson_serialized_input_is_bounded_including_escapes() {
    use crate::domain_models::procedural::{ActionEpisode, EpisodeRecord};
    let event = CaptureEvent {
        owner: OwnerId::new("owner").unwrap(),
        actor: ActorId::new("actor").unwrap(),
        project: ProjectId::new("project").unwrap(),
        source: SourceId::new(format!("{KEY}/turn-1")).unwrap(),
        origin: "fixture://exchange".into(),
        recorded_at_ms: 1,
        kind: WorkEventKind::Exchange,
        outcome: WorkOutcome::Unknown,
        content: String::new(),
    };
    let exchanges = (0..100)
        .map(|_| {
            (
                event.clone(),
                "\u{0000}".repeat(8000),
                "\u{0001}".repeat(8000),
            )
        })
        .collect();
    let actions = (0..24)
        .map(|i| {
            let failed = i % 2 == 0;
            let mut e = event.clone();
            e.source = SourceId::new(format!("{KEY}/action-{i}")).unwrap();
            (
                e,
                EpisodeRecord {
                    episode: ActionEpisode {
                        tool: "Bash".into(),
                        intent: "test storage".into(),
                        summary: "\u{0000}".repeat(200),
                        error_head: Some("\u{0001}".repeat(200)),
                        session: KEY.into(),
                        seq: i,
                    },
                    outcome: if failed {
                        WorkOutcome::Failed
                    } else {
                        WorkOutcome::Succeeded
                    },
                    recorded_at_ms: i,
                },
            )
        })
        .collect();
    let input = lesson_policy::build_input(exchanges, actions).unwrap();
    let actual = crate::integrations::external_apis::router_runtime::lesson_input_value(&input)
        .to_string()
        .len();
    assert!(actual <= lesson_policy::input_bytes(&input));
    assert!(actual <= lesson_policy::INPUT_BYTES);
    assert!(!input.exchanges.is_empty());
    assert_eq!(input.digest.len(), 12);
}

#[tokio::test]
async fn client_cannot_capture_lesson_kind() {
    let fake = Fake::start();
    let runtime = setup(&fake).await;
    for origin in ["lesson://session/1", "fixture://client"] {
        assert!(matches!(
            runtime
                .capture(
                    &runtime.scope.projects[0],
                    SourceId::new("client-lesson").unwrap(),
                    WorkEventKind::Lesson,
                    WorkOutcome::Unknown,
                    origin.into(),
                    "A fabricated lesson".into()
                )
                .await
                .result,
            Err(RuntimeError::InvalidInput(_))
        ));
    }
    assert!(lessons(&runtime).await.is_empty());
}

#[tokio::test]
async fn flat_dream_does_not_extract_lessons_from_ready_session() {
    use sha2::{Digest, Sha256};
    let fake = Fake::start();
    fake.reply(0, json!({})); // Flat derivation may still run, but must not be lesson extraction.
    let runtime = setup(&fake).await;
    // An empty store counts as a tree; pin flat mode so the flat branch is the one exercised.
    runtime
        .store
        .set_tree_mode(
            &runtime.scope.owner,
            &runtime.scope.projects[0],
            crate::domain_models::memory_tree::TreeMode::Flat,
            now_ms().unwrap(),
        )
        .await
        .unwrap();
    let raw = "flat-session";
    let digest = format!("{:x}", Sha256::digest(raw.as_bytes()));
    let key = &digest[..16];
    let event = CaptureEvent {
        source: SourceId::new(format!("{key}/turn-1")).unwrap(),
        owner: runtime.scope.owner.clone(),
        actor: runtime.scope.actor.clone(),
        project: runtime.scope.projects[0].clone(),
        origin: "fixture://exchange".into(),
        recorded_at_ms: now_ms().unwrap() - 120_000,
        kind: WorkEventKind::Exchange,
        outcome: WorkOutcome::Unknown,
        content: json!({"v":1,"prompt":"test","reply":"done"}).to_string(),
    };
    runtime.store.capture(event, &runtime.limits).await.unwrap();
    runtime
        .store
        .session_end(
            &runtime.scope.owner,
            &runtime.scope.projects[0],
            raw,
            now_ms().unwrap(),
        )
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    assert_eq!(
        runtime
            .ready_sessions(&runtime.scope.projects[0], now_ms().unwrap(), 1)
            .await
            .unwrap()
            .len(),
        1
    );
    let _ = runtime.dream_once().await;
    assert!(lessons(&runtime).await.is_empty());
    assert!(
        fake.inputs
            .lock()
            .unwrap()
            .iter()
            .all(|input| input["response_format"]["json_schema"]["name"] != "runtime_lessons_v1")
    );
    assert_eq!(
        runtime
            .ready_sessions(&runtime.scope.projects[0], now_ms().unwrap(), 1)
            .await
            .unwrap()
            .len(),
        1
    );
}
