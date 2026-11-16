#![cfg(feature = "memory-runtime")]
use lambda_memory::delivery::mcp::{MAX_RESPONSE_BYTES, McpSession, serve_io};
use lambda_memory::domain_models::{ActorId, OwnerId, ProjectId, SourceId};
use lambda_memory::domain_models::{capture::*, runtime::*};
use lambda_memory::integrations::database::embedded_capture::{
    EmbeddedCaptureStore, EmbeddedRuntimeStore,
};
use lambda_memory::services::runtime_service::MemoryRuntime;
use serde_json::{Value, json};
use std::io::Cursor;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
static NEXT: AtomicUsize = AtomicUsize::new(0);
const TOKEN: &str = "operator-token-32-bytes-minimum!!";
async fn setup() -> (Arc<MemoryRuntime>, McpSession) {
    let path = format!(
        "{}/lambda-mcp-{}-{}",
        std::env::temp_dir().display(),
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    );
    let store = EmbeddedRuntimeStore::new(EmbeddedCaptureStore::open(&path).await.unwrap());
    let runtime = Arc::new(
        MemoryRuntime::new(
            store,
            RuntimeScope {
                owner: OwnerId::new("owner").unwrap(),
                actor: ActorId::new("actor").unwrap(),
                projects: vec![ProjectId::new("project").unwrap()],
            },
            RuntimeLimits {
                idle_ms: 1,
                ..RuntimeLimits::default()
            },
        )
        .unwrap(),
    );
    runtime.initialize_diagnostics().await.unwrap();
    (
        runtime.clone(),
        McpSession::new(runtime, TOKEN.into()).unwrap(),
    )
}
async fn setup_forwards(count: usize, content: &str) -> (Arc<MemoryRuntime>, McpSession) {
    use lambda_memory::domain_models::memory_tree::{CurationCommit, LinkOperation};
    let path = format!(
        "/tmp/lambda-forward-mcp-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    );
    let store = EmbeddedRuntimeStore::new(EmbeddedCaptureStore::open(&path).await.unwrap());
    let (owner, actor, project) = (
        OwnerId::new("owner").unwrap(),
        ActorId::new("actor").unwrap(),
        ProjectId::new("project").unwrap(),
    );
    for i in 0..count {
        let source = SourceId::new(format!("forward-{i:02}")).unwrap();
        store
            .capture(
                CaptureEvent {
                    owner: owner.clone(),
                    actor: actor.clone(),
                    project: project.clone(),
                    source: source.clone(),
                    origin: "fixture://forward".into(),
                    recorded_at_ms: 100 + i as u64,
                    kind: WorkEventKind::Decision,
                    outcome: WorkOutcome::Unknown,
                    content: content.into(),
                },
                &RuntimeLimits::default(),
            )
            .await
            .unwrap();
        store
            .commit_curation(
                &owner,
                &actor,
                &project,
                CurationCommit {
                    links: vec![],
                    supersedes: vec![],
                    operation_id: format!("forward-op-{i}"),
                    sources: vec![source.clone()],
                    promotes: vec![source],
                    creates: vec![],
                    merges: vec![],
                    parked: vec![],
                    stale: vec![],
                    split: vec![],
                    operation: LinkOperation::Create,
                },
                200 + i as u64,
            )
            .await
            .unwrap();
    }
    let runtime = Arc::new(
        MemoryRuntime::new(
            store,
            RuntimeScope {
                owner,
                actor,
                projects: vec![project],
            },
            RuntimeLimits {
                idle_ms: 1,
                ..Default::default()
            },
        )
        .unwrap(),
    );
    runtime.initialize_diagnostics().await.unwrap();
    let mut session = McpSession::new(runtime.clone(), TOKEN.into()).unwrap();
    ready(&mut session).await;
    (runtime, session)
}

#[tokio::test]
async fn promoted_lessons_scan_parsed_text_in_service_and_mcp() {
    let (runtime, mut session) = setup().await;
    ready(&mut session).await;
    let lesson = json!({"v":1,"kind":"pitfall","scope":"user","title":"Display a redacted header","applies_when":"Logging headers","guidance":"show only \"Bearer \"","task":"Redact logs","task_outcome":"corrected_by_user","evidence":["original"]});
    let safe = lesson.to_string();
    runtime
        .capture(
            &ProjectId::new("project").unwrap(),
            SourceId::new("direct-lesson").unwrap(),
            WorkEventKind::Lesson,
            WorkOutcome::Unknown,
            "promote://other/direct".into(),
            safe.clone(),
        )
        .await
        .result
        .unwrap();
    let response = send(&mut session, call("memory_capture",json!({"project":"project","source":"mcp-lesson","kind":"lesson","outcome":"unknown","origin":"promote://other/mcp","content":safe}))).await;
    assert_eq!(body(&response)["status"], "saved_pending");
    for field in ["title", "applies_when", "guidance", "task"] {
        let mut unsafe_lesson = lesson.clone();
        unsafe_lesson[field] = json!(format!("ghp_{}", "a".repeat(36)));
        let content = unsafe_lesson.to_string();
        assert!(matches!(
            runtime
                .capture(
                    &ProjectId::new("project").unwrap(),
                    SourceId::new(format!("direct-{field}")).unwrap(),
                    WorkEventKind::Lesson,
                    WorkOutcome::Unknown,
                    "promote://other/secret".into(),
                    content.clone()
                )
                .await
                .result,
            Err(RuntimeError::InvalidInput(_))
        ));
        let response = send(&mut session,call("memory_capture",json!({"project":"project","source":format!("mcp-{field}"),"kind":"lesson","outcome":"unknown","origin":"promote://other/secret","content":content}))).await;
        assert_eq!(response["result"]["isError"], true);
    }
}

#[tokio::test]
async fn promoted_lessons_reject_extra_missing_and_secret_evidence_fields() {
    let (runtime, mut session) = setup().await;
    ready(&mut session).await;
    let valid = json!({"v":1,"kind":"strategy","scope":"project","title":"Test storage","applies_when":"Changing storage","guidance":"1. Test it.","task":"Verify","task_outcome":"succeeded","evidence":["source"]});
    let mut extra = valid.clone();
    extra["extra"] = json!(format!("ghp_{}", "a".repeat(36)));
    let mut missing = valid.clone();
    missing.as_object_mut().unwrap().remove("task_outcome");
    let mut secret_evidence = valid.clone();
    secret_evidence["evidence"] = json!([format!("ghp_{}", "a".repeat(36))]);
    for (i, content) in [extra, missing, secret_evidence].into_iter().enumerate() {
        let content = content.to_string();
        assert!(matches!(
            runtime
                .capture(
                    &ProjectId::new("project").unwrap(),
                    SourceId::new(format!("direct-invalid-{i}")).unwrap(),
                    WorkEventKind::Lesson,
                    WorkOutcome::Unknown,
                    "promote://other/lesson".into(),
                    content.clone()
                )
                .await
                .result,
            Err(RuntimeError::InvalidInput(_))
        ));
        let response=send(&mut session,call("memory_capture",json!({"project":"project","source":format!("mcp-invalid-{i}"),"kind":"lesson","outcome":"unknown","origin":"promote://other/lesson","content":content}))).await;
        assert_eq!(response["result"]["isError"], true);
    }
}

#[tokio::test]
async fn forwards_tools_round_trip() {
    let (runtime, mut session) = setup_forwards(2, "Delegate only coding tasks.").await;
    let before = body(
        &send(
            &mut session,
            call("memory_activity", json!({"project":"project"})),
        )
        .await,
    );
    let listed = body(
        &send(
            &mut session,
            call("memory_forwards", json!({"project":"project","limit":1})),
        )
        .await,
    );
    assert_eq!(
        listed,
        json!({"forwards":[{"forward_id":"capture:forward-00","action":"capture","source_id":"forward-00","source":{
            "source":"forward-00","origin":"fixture://forward","recorded_at_ms":100,"kind":"decision","outcome":"unknown","content":"Delegate only coding tasks."
        }}]})
    );
    let receipt = body(&send(&mut session, call("memory_forward_done", json!({"project":"project","forwards":["capture:forward-00","missing"],"failed":["capture:forward-01"]}))).await);
    assert_eq!(receipt, json!({"done":1,"failed":1}));
    assert_eq!(
        body(
            &send(
                &mut session,
                call("memory_forwards", json!({"project":"project"}))
            )
            .await
        ),
        json!({"forwards":[]})
    );
    assert_eq!(body(&send(&mut session, call("memory_forward_done", json!({"project":"project","forwards":["capture:forward-00"],"failed":["capture:forward-01"]}))).await), json!({"done":0,"failed":0}));
    let status = body(
        &send(
            &mut session,
            call("memory_status", json!({"project":"project"})),
        )
        .await,
    );
    assert_eq!(status["forwards_pending"], 0);
    assert_eq!(status["forwards_failed"], 1);
    let after = body(
        &send(
            &mut session,
            call("memory_activity", json!({"project":"project"})),
        )
        .await,
    );
    assert_eq!(before["entries"], after["entries"]);
    tokio::time::sleep(std::time::Duration::from_millis(3)).await;
    assert!(runtime.dream_once().await.result.is_ok()); // reads and acknowledgements did not enter foreground accounting
    for args in [
        json!({}),
        json!({"project":"project","limit":0}),
        json!({"project":"project","limit":65}),
        json!({"project":"project","limit":-1}),
        json!({"project":"project","limit":1.5}),
        json!({"project":"project","extra":1}),
    ] {
        assert_eq!(
            send(&mut session, call("memory_forwards", args)).await["error"]["code"],
            -32602
        );
    }
    for args in [
        json!({"project":"project"}),
        json!({"project":"project","forwards":[]}),
        json!({"project":"project","forwards":[1]}),
        json!({"project":"project","forwards":[""]}),
        json!({"project":"project","forwards":[],"failed":null}),
        json!({"project":"project","forwards":vec!["x";64],"failed":["y"]}),
    ] {
        assert_eq!(
            send(&mut session, call("memory_forward_done", args)).await["error"]["code"],
            -32602
        );
    }
    assert_eq!(
        send(
            &mut session,
            call("memory_forwards", json!({"project":"foreign"}))
        )
        .await["result"]["isError"],
        true
    );
    assert_eq!(
        send(
            &mut session,
            call(
                "memory_forward_done",
                json!({"project":"foreign","forwards":["x"]})
            )
        )
        .await["result"]["isError"],
        true
    );
}

#[tokio::test]
async fn forwards_listing_respects_byte_budget() {
    let (_, mut session) = setup_forwards(40, &"x".repeat(8192)).await;
    let response = send(
        &mut session,
        call("memory_forwards", json!({"project":"project","limit":64})),
    )
    .await;
    let listed = body(&response);
    let forwards = listed["forwards"].as_array().unwrap();
    assert!(!forwards.is_empty() && forwards.len() < 40);
    assert!(serde_json::to_vec(forwards).unwrap().len() <= 256 * 1024);
    assert!(response.to_string().len() < MAX_RESPONSE_BYTES);
    assert_eq!(forwards[0]["forward_id"], "capture:forward-00");
    assert!(
        body(
            &send(
                &mut session,
                call("memory_forwards", json!({"project":"project"}))
            )
            .await
        )["forwards"]
            .as_array()
            .unwrap()
            .len()
            <= 32
    );
}

#[tokio::test]
async fn status_reports_forwards_pending() {
    let (_, mut session) = setup_forwards(2, "No emojis.").await;
    let status = body(
        &send(
            &mut session,
            call("memory_status", json!({"project":"project"})),
        )
        .await,
    );
    assert_eq!(status["forwards_pending"], 2);
    assert_eq!(status["forwards_failed"], 0);
}

#[tokio::test]
async fn promote_capture_rejects_secrets() {
    let (_, mut session) = setup().await;
    ready(&mut session).await;
    for kind in ["decision", "attempt", "result", "error"] {
        let response = send(&mut session, call("memory_capture", json!({"project":"project","source":format!("secret-{kind}"),"kind":kind,"outcome":"unknown","origin":"promote://repo:x/src-1","content":format!("ghp_{}", "a".repeat(36))}))).await;
        assert_eq!(response["result"]["isError"], true);
        assert!(body(&response)["error"].as_str().unwrap().contains("input"));
    }
}

#[tokio::test]
async fn promote_exchange_is_scanned_on_parsed_fields() {
    let (_, mut session) = setup().await;
    ready(&mut session).await;
    let clean = json!({"v":1,"prompt":"Bearer ","reply":"OK"}).to_string();
    let response = send(&mut session, call("memory_capture", json!({"project":"project","source":"promoted-exchange","kind":"exchange","outcome":"unknown","origin":"promote://repo:x/src-1","content":clean}))).await;
    assert_eq!(response["result"]["isError"], false, "{response}");
    let secret = json!({"v":1,"prompt":"p","reply":format!("ghp_{}", "a".repeat(36))}).to_string();
    let response = send(&mut session, call("memory_capture", json!({"project":"project","source":"promoted-secret","kind":"exchange","outcome":"unknown","origin":"promote://repo:x/src-1","content":secret}))).await;
    assert_eq!(response["result"]["isError"], true);
}

async fn assert_promote_timestamp(days: u64) {
    let (runtime, mut session) = setup().await;
    ready(&mut session).await;
    let at = now_ms() - days * 24 * 60 * 60 * 1000;
    let response = send(&mut session, call("memory_capture", json!({"project":"project","source":"promoted-time","kind":"decision","outcome":"unknown","origin":"promote://repo:x/src-1","content":"No emojis.","recorded_at_ms":at}))).await;
    assert_eq!(response["result"]["isError"], false, "{response}");
    let stored = runtime
        .propose(RevisionProposal {
            project: ProjectId::new("project").unwrap(),
            memory: lambda_memory::domain_models::MemoryId::new("time").unwrap(),
            revision: lambda_memory::domain_models::RevisionId::new("time-v1").unwrap(),
            sources: vec![SourceId::new("promoted-time").unwrap()],
            content: "No emojis.".into(),
            predecessor: None,
        })
        .await
        .result
        .unwrap();
    assert_eq!(stored.sources[0].recorded_at_ms, at);
}

#[tokio::test]
async fn promote_capture_accepts_old_timestamp() {
    assert_promote_timestamp(500).await;
}

#[tokio::test]
async fn capture_accepts_promote_timestamp() {
    assert_promote_timestamp(30).await;
    let (_, mut session) = setup().await;
    ready(&mut session).await;
    let response = send(&mut session, call("memory_capture", json!({"project":"project","source":"action","kind":"action","outcome":"unknown","origin":"promote://repo:x/src-1","content":"{}","recorded_at_ms":now_ms() - 1000}))).await;
    assert_eq!(response["error"]["code"], -32602);
    let response = send(&mut session, call("memory_capture", json!({"project":"project","source":"future","kind":"decision","outcome":"unknown","origin":"promote://repo:x/src-1","content":"No emojis.","recorded_at_ms":now_ms() + 60_000}))).await;
    assert_eq!(response["result"]["isError"], true);
}

fn initialize(token: &str) -> Value {
    json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25",
        "capabilities":{},"clientInfo":{"name":"test","version":"1"},"_meta":{"lambdaMemoryToken":token}}})
}
async fn send(session: &mut McpSession, request: Value) -> Value {
    serde_json::from_str(&session.handle_frame(&request.to_string()).await.unwrap()).unwrap()
}
async fn ready(session: &mut McpSession) {
    assert!(
        send(session, initialize(TOKEN))
            .await
            .get("result")
            .is_some()
    );
    assert!(
        session
            .handle_frame(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
            .await
            .is_none()
    );
}
fn call(name: &str, args: Value) -> Value {
    json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":name,"arguments":args}})
}
fn body(reply: &Value) -> Value {
    serde_json::from_str(reply["result"]["content"][0]["text"].as_str().unwrap()).unwrap()
}
#[tokio::test]
async fn authentication_and_initialized_notification_are_required_before_tools() {
    let (_, mut session) = setup().await;
    assert_eq!(
        send(
            &mut session,
            json!({"jsonrpc":"2.0","id":2,"method":"tools/list"})
        )
        .await["error"]["code"],
        -32600
    );
    let denied = send(&mut session, initialize("incorrect token")).await;
    assert_eq!(denied["error"]["code"], -32001);
    assert!(denied.get("result").is_none());
    assert!(session.is_denied());
    assert!(!denied.to_string().contains(TOKEN));
    let (_, mut session) = setup().await;
    send(&mut session, initialize(TOKEN)).await;
    assert_eq!(
        send(
            &mut session,
            json!({"jsonrpc":"2.0","id":2,"method":"tools/list"})
        )
        .await["error"]["code"],
        -32600
    );
    // A request-shaped initialized message must not advance the session.
    assert_eq!(
        send(
            &mut session,
            json!({"jsonrpc":"2.0","id":2,"method":"notifications/initialized"})
        )
        .await["error"]["code"],
        -32600
    );
}
#[tokio::test]
async fn impersonation_and_foreign_project_capture_cannot_mutate_native_storage() {
    let (runtime, mut session) = setup().await;
    ready(&mut session).await;
    let args = json!({"project":"project","source":"source","kind":"decision","outcome":"unknown","origin":"fixture://scope","content":"private claim","owner":"foreign"});
    assert_eq!(
        send(&mut session, call("memory_capture", args)).await["error"]["code"],
        -32602
    );
    let foreign = send(&mut session, call("memory_capture", json!({"project":"foreign","source":"source","kind":"decision","outcome":"unknown","origin":"fixture://scope","content":"private claim"}))).await;
    assert_eq!(foreign["result"]["isError"], true);
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    assert_eq!(
        runtime.dream_once().await.result.unwrap(),
        DreamReceipt::default()
    );
    assert!(
        runtime
            .list(listing(
                &ProjectId::new("project").unwrap(),
                &[LifecycleState::Processing],
                32
            ))
            .await
            .result
            .unwrap()
            .items
            .is_empty()
    );
}
#[tokio::test]
async fn malformed_notifications_are_rejected_and_valid_notifications_do_not_mutate() {
    let (runtime, mut session) = setup().await;
    ready(&mut session).await;
    for invalid in [
        json!({"jsonrpc":"2.0"}),
        json!({"jsonrpc":"2.0","method":7}),
        json!({"jsonrpc":"2.0","id":false,"method":"ping"}),
    ] {
        assert_eq!(send(&mut session, invalid).await["error"]["code"], -32600);
    }
    let notification = json!({"jsonrpc":"2.0","method":"tools/call","params":{"name":"memory_capture",
        "arguments":{"project":"project","source":"notice","kind":"decision","outcome":"unknown","origin":"fixture://notice","content":"Must not be stored"}}});
    assert!(
        session
            .handle_frame(&notification.to_string())
            .await
            .is_none()
    );
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    assert_eq!(
        runtime.dream_once().await.result.unwrap(),
        DreamReceipt::default()
    );
}
#[tokio::test]
async fn escaped_surrogate_pairs_are_captured_as_real_utf8_evidence() {
    let (runtime, mut session) = setup().await;
    ready(&mut session).await;
    let request = r#"{"jsonrpc":"2.0","id":"unicode","method":"tools/call","params":{"name":"memory_capture","arguments":{"project":"project","source":"emoji","kind":"error","outcome":"failed","origin":"fixture://unicode","content":"failed \ud83d\ude00 attempt"}}}"#;
    let response: Value =
        serde_json::from_str(&session.handle_frame(request).await.unwrap()).unwrap();
    assert_eq!(response["id"], "unicode");
    assert_eq!(response["result"]["isError"], false);
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    assert_eq!(runtime.dream_once().await.result.unwrap().sources, 1);
    let available = runtime
        .list(listing(
            &ProjectId::new("project").unwrap(),
            &[LifecycleState::Active],
            32,
        ))
        .await
        .result
        .unwrap()
        .items;
    assert_eq!(available[0].revision.content, "failed 😀 attempt");
    assert_eq!(available[0].sources[0].outcome, WorkOutcome::Failed);
}
#[tokio::test]
async fn eof_wakes_transport_after_last_response_and_large_mutations_have_receipts() {
    let (runtime, mut session) = setup().await;
    ready(&mut session).await;
    let project = ProjectId::new("project").unwrap();
    let mut ids = Vec::new();
    for index in 0..32 {
        let prefix = format!("{index}-");
        let id = format!("{prefix}{}", "s".repeat(512 - prefix.len()));
        runtime
            .capture(
                &project,
                SourceId::new(&id).unwrap(),
                WorkEventKind::Decision,
                WorkOutcome::Unknown,
                "o".repeat(2048),
                "source claim".into(),
            )
            .await
            .result
            .unwrap();
        ids.push(id);
    }
    let revision = "r".repeat(512);
    let response = send(
        &mut session,
        call(
            "memory_propose",
            json!({"project":"project","memory":"large","revision":revision,
        "sources":ids,"content":"c".repeat(8192)}),
        ),
    )
    .await;
    assert_eq!(response["result"]["isError"], false);
    assert_eq!(body(&response)["revision"], revision);
    assert!(response.to_string().len() <= MAX_RESPONSE_BYTES);
    let transcript = format!(
        "{}\n{}\n{}\n",
        initialize(TOKEN),
        json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
        json!({"jsonrpc":"2.0","id":3,"method":"ping"})
    );
    let mut output = Vec::new();
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        serve_io(
            runtime,
            TOKEN.into(),
            Cursor::new(transcript.into_bytes()),
            &mut output,
        ),
    )
    .await
    .unwrap()
    .unwrap();
    let replies: Vec<Value> = std::str::from_utf8(&output)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(replies.last().unwrap()["id"], 3);
}

#[tokio::test]
async fn diagnostics_expose_actual_progress_without_disclosing_captured_content_or_handles() {
    let (runtime, mut session) = setup().await;
    ready(&mut session).await;
    let capture = send(&mut session, call("memory_capture", json!({
        "project":"project","source":"SENSITIVE_SOURCE_HANDLE","kind":"error","outcome":"failed",
        "origin":"fixture://SENSITIVE_ORIGIN","content":"SENSITIVE_CONTENT database decision"
    }))).await;
    assert_eq!(capture["result"]["isError"], false);
    let snapshot = body(
        &send(
            &mut session,
            call("memory_status", json!({"project":"project"})),
        )
        .await,
    );
    assert_eq!(snapshot["pending"]["unconsumed_sources"], 1);
    assert_eq!(snapshot["last_success"]["operation"], "capture");
    assert_eq!(snapshot["last_success"]["counts"]["sources_created"], 1);
    for sensitive in [
        "SENSITIVE_SOURCE_HANDLE",
        "SENSITIVE_ORIGIN",
        "SENSITIVE_CONTENT",
    ] {
        assert!(!snapshot.to_string().contains(sensitive));
    }
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    assert_eq!(runtime.dream_once().await.result.unwrap().sources, 1);
    let snapshot = body(
        &send(
            &mut session,
            call("memory_status", json!({"project":"project"})),
        )
        .await,
    );
    assert_eq!(snapshot["pending"]["unconsumed_sources"], 0);
    assert_eq!(snapshot["lifecycle"]["active"], 1);
    assert_eq!(
        snapshot["last_dream_success"]["counts"]["revisions_published"],
        1
    );
    assert_eq!(snapshot["review"]["activation_requires_review"], false);
    let mut cursor = None;
    let mut ids = std::collections::BTreeSet::new();
    loop {
        let mut args =
            json!({"project":"project","operations":["dream"],"outcomes":["succeeded"],"limit":1});
        if let Some(value) = cursor {
            args["cursor"] = value;
        }
        let page = body(&send(&mut session, call("memory_activity", args)).await);
        for entry in page["entries"].as_array().unwrap() {
            assert_eq!(entry["operation"], "dream");
            assert_eq!(entry["outcome"], "succeeded");
            assert!(ids.insert(entry["id"].as_str().unwrap().to_owned()));
        }
        for sensitive in [
            "SENSITIVE_SOURCE_HANDLE",
            "SENSITIVE_ORIGIN",
            "SENSITIVE_CONTENT",
        ] {
            assert!(!page.to_string().contains(sensitive));
        }
        if page["next_cursor"].is_null() {
            break;
        }
        cursor = Some(page["next_cursor"].clone());
    }
    let expected = runtime
        .activity(
            lambda_memory::domain_models::runtime_diagnostics::ActivityRequest {
                project: ProjectId::new("project").unwrap(),
                limit: 32,
                operations: vec![
                    lambda_memory::domain_models::runtime_diagnostics::ActivityOperation::Dream,
                ],
                outcomes: vec![
                    lambda_memory::domain_models::runtime_diagnostics::ActivityOutcome::Succeeded,
                ],
                cursor: None,
            },
        )
        .await
        .unwrap()
        .entries
        .into_iter()
        .map(|entry| entry.id.to_string())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(ids, expected);
}

#[tokio::test]
async fn invalid_diagnostic_scope_filters_and_cursors_are_errors_not_empty_success() {
    let (_, mut session) = setup().await;
    ready(&mut session).await;
    for args in [
        json!({"project":"project","operations":[]}),
        json!({"project":"project","operations":["capture","capture"]}),
        json!({"project":"project","outcomes":["unknown"]}),
        json!({"project":"project","limit":0}),
        json!({"project":"project","limit":33}),
        json!({"project":"project","cursor":"a".repeat(1025)}),
        json!({"project":"project","owner":"foreign"}),
    ] {
        let reply = send(&mut session, call("memory_activity", args)).await;
        assert_eq!(reply["error"]["code"], -32602);
    }
    for (name, args) in [
        ("memory_status", json!({"project":"foreign"})),
        ("memory_activity", json!({"project":"foreign"})),
        (
            "memory_activity",
            json!({"project":"project","cursor":"not-a-cursor"}),
        ),
    ] {
        let reply = send(&mut session, call(name, args)).await;
        assert_eq!(reply["result"]["isError"], true);
        assert!(body(&reply).get("error").is_some());
        assert!(body(&reply).get("entries").is_none());
    }
}
#[tokio::test]
async fn tool_annotations_mark_reads_and_the_destructive_purge() {
    let (_, mut session) = setup().await;
    ready(&mut session).await;
    let listed = send(
        &mut session,
        json!({"jsonrpc":"2.0","id":3,"method":"tools/list"}),
    )
    .await;
    let tools = listed["result"]["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 20);
    for tool in tools {
        let name = tool["name"].as_str().unwrap();
        let read = matches!(
            name,
            "memory_overview"
                | "memory_nodes"
                | "memory_node"
                | "memory_list"
                | "memory_recall"
                | "memory_status"
                | "memory_activity"
                | "memory_timeline"
                | "memory_brief"
                | "memory_forwards"
                | "memory_skill_candidates"
                | "memory_skill_candidate"
        );
        assert_eq!(tool["annotations"]["readOnlyHint"], read, "{name}");
        assert_eq!(
            tool["annotations"]["idempotentHint"],
            read || name == "memory_session_end",
            "{name}"
        );
        assert_eq!(
            tool["annotations"]["destructiveHint"],
            name == "memory_purge",
            "{name}"
        );
    }
}
#[tokio::test]
async fn memory_session_end_arguments_and_no_activity() {
    use sha2::{Digest, Sha256};
    let (runtime, mut session) = setup().await;
    ready(&mut session).await;
    for args in [
        json!({}),
        json!({"project":"project"}),
        json!({"project":"project","session":null}),
        json!({"project":"project","session":42}),
        json!({"project":"project","session":""}),
        json!({"project":"project","session":"line\nbreak"}),
        json!({"project":"project","session":"x".repeat(513)}),
        json!({"project":"project","session":"raw","extra":true}),
        json!({"project":4,"session":"raw"}),
    ] {
        assert_eq!(
            send(&mut session, call("memory_session_end", args)).await["error"]["code"],
            -32602
        );
    }
    let raw = "raw session / ç";
    let hash = format!("{:x}", Sha256::digest(raw.as_bytes()));
    let key = &hash[..16];
    runtime
        .capture(
            &ProjectId::new("project").unwrap(),
            SourceId::new(format!("{key}/turn-one")).unwrap(),
            WorkEventKind::Exchange,
            WorkOutcome::Unknown,
            "fixture://session".into(),
            "exchange evidence".into(),
        )
        .await
        .result
        .unwrap();
    let before = body(
        &send(
            &mut session,
            call("memory_activity", json!({"project":"project"})),
        )
        .await,
    );
    for _ in 0..2 {
        let result = send(
            &mut session,
            call(
                "memory_session_end",
                json!({"project":"project","session":raw}),
            ),
        )
        .await;
        assert_eq!(result["result"]["isError"], false);
        assert_eq!(body(&result), json!({"session_key":key,"ended":true}));
    }
    assert_eq!(
        body(
            &send(
                &mut session,
                call(
                    "memory_session_end",
                    json!({"project":"project","session":"unknown"})
                )
            )
            .await
        )["ended"],
        false
    );
    let after = body(
        &send(
            &mut session,
            call("memory_activity", json!({"project":"project"})),
        )
        .await,
    );
    assert_eq!(before["entries"], after["entries"]);
    assert_eq!(
        runtime
            .ready_sessions(&ProjectId::new("project").unwrap(), now_ms(), 10)
            .await
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        send(
            &mut session,
            call(
                "memory_session_end",
                json!({"project":"foreign","session":raw})
            )
        )
        .await["result"]["isError"],
        true
    );
    tokio::time::sleep(std::time::Duration::from_millis(3)).await;
    assert!(runtime.dream_once().await.result.is_ok());
}

#[tokio::test]
async fn ping_is_always_answered_and_unknown_tools_are_invalid_params() {
    let (_, mut session) = setup().await;
    let pong = send(
        &mut session,
        json!({"jsonrpc":"2.0","id":9,"method":"ping"}),
    )
    .await;
    assert_eq!(pong["id"], 9);
    assert!(pong.get("error").is_none());
    ready(&mut session).await;
    assert_eq!(
        send(&mut session, call("memory_unknown", json!({}))).await["error"]["code"],
        -32602
    );
    assert_eq!(
        send(&mut session, call("memory_playbook", json!({}))).await["error"]["code"],
        -32602
    );
}

async fn decision(runtime: &MemoryRuntime, id: &str) {
    runtime
        .capture(
            &ProjectId::new("project").unwrap(),
            SourceId::new(id).unwrap(),
            WorkEventKind::Decision,
            WorkOutcome::Unknown,
            "fixture://timeline".into(),
            format!("decision {id}"),
        )
        .await
        .result
        .unwrap();
}
fn entry_args(entry: &str, occurred: u64, source: &str) -> Value {
    json!({"project":"project","entry":entry,"title":format!("Title {entry}"),"summary":"Chose X.",
        "occurred_at_ms":occurred,"status":"adopted","sources":[source]})
}
#[tokio::test]
async fn timeline_tools_validate_strict_keys_and_page_with_fingerprinted_cursors() {
    let (runtime, mut session) = setup().await;
    ready(&mut session).await;
    for id in ["s-a", "s-b", "s-c", "s-uncovered"] {
        decision(&runtime, id).await;
    }
    for key in ["owner", "actor"] {
        let mut args = entry_args("a", 1_700_000_000_000, "s-a");
        args[key] = json!("foreign");
        let reply = send(&mut session, call("memory_timeline_write", args)).await;
        assert_eq!(reply["error"]["code"], -32602, "{key}");
        let reply = send(
            &mut session,
            call(
                "memory_timeline",
                json!({"project":"project",key:"foreign"}),
            ),
        )
        .await;
        assert_eq!(reply["error"]["code"], -32602, "{key}");
    }
    for (entry, at, source) in [
        ("b", 2_000, "s-b"),
        ("a", 1_000, "s-a"),
        ("c", 3_000, "s-c"),
    ] {
        let reply = send(
            &mut session,
            call(
                "memory_timeline_write",
                entry_args(entry, 1_700_000_000_000 + at, source),
            ),
        )
        .await;
        assert_eq!(reply["result"]["isError"], false, "{reply}");
        let written = body(&reply);
        assert_eq!(written["write"], "recorded");
        assert_eq!(written["entry"]["version"], 1);
        assert_eq!(written["entry"]["status"], "adopted");
        assert_eq!(written["entry"]["sources"], json!([source]));
        assert_eq!(written["entry"]["supersedes"], Value::Null);
        assert!(written["entry"].get("actor").is_none() && written["entry"].get("owner").is_none());
    }
    let replay = body(
        &send(
            &mut session,
            call(
                "memory_timeline_write",
                entry_args("a", 1_700_000_001_000, "s-a"),
            ),
        )
        .await,
    );
    assert_eq!(replay["write"], "already_recorded");
    let mut edit = entry_args("a", 1_700_000_001_000, "s-a");
    edit["status"] = json!("superseded");
    edit["expected_version"] = json!(1);
    assert_eq!(
        body(&send(&mut session, call("memory_timeline_write", edit)).await)["entry"]["version"],
        2
    );
    // Policy failures are domain errors; schema violations are invalid params.
    let mut duplicate = entry_args("d", 1_700_000_000_000, "s-a");
    duplicate["revisions"] = json!(["r", "r"]);
    assert_eq!(
        send(&mut session, call("memory_timeline_write", duplicate)).await["result"]["isError"],
        true
    );
    let mut zero = entry_args("d", 1_700_000_000_000, "s-a");
    zero["expected_version"] = json!(0);
    assert_eq!(
        send(&mut session, call("memory_timeline_write", zero)).await["error"]["code"],
        -32602
    );
    for args in [
        json!({"project":"project","entry":"a","statuses":["adopted"]}),
        json!({"project":"project","entry":"a","order":"asc"}),
        json!({"project":"project","statuses":[]}),
        json!({"project":"project","statuses":["adopted","adopted"]}),
        json!({"project":"project","statuses":["unknown"]}),
        json!({"project":"project","order":"sideways"}),
        json!({"project":"project","limit":0}),
        json!({"project":"project","limit":33}),
        json!({"project":"project","cursor":"zz"}),
        json!({"project":"project","cursor":"a".repeat(4097)}),
    ] {
        let reply = send(&mut session, call("memory_timeline", args.clone())).await;
        assert_eq!(reply["error"]["code"], -32602, "{args}");
    }
    let first = body(
        &send(
            &mut session,
            call("memory_timeline", json!({"project":"project","limit":1})),
        )
        .await,
    );
    assert_eq!(first["project"], "project");
    assert_eq!(first["truncated"], false);
    assert_eq!(first["uncovered_truncated"], false);
    let uncovered = first["uncovered_decisions"].as_array().unwrap();
    assert_eq!(uncovered.len(), 1);
    assert_eq!(uncovered[0]["source"], "s-uncovered");
    assert_eq!(uncovered[0]["preview"], "decision s-uncovered");
    assert_eq!(uncovered[0]["preview_truncated"], false);
    let mut seen = vec![first["entries"][0]["entry"].as_str().unwrap().to_owned()];
    let mut cursor = first["next_cursor"].clone();
    let cursor_from_first = cursor.clone();
    while !cursor.is_null() {
        let page = body(
            &send(
                &mut session,
                call(
                    "memory_timeline",
                    json!({"project":"project","limit":1,"cursor":cursor}),
                ),
            )
            .await,
        );
        assert_eq!(page["uncovered_decisions"], json!([]));
        for entry in page["entries"].as_array().unwrap() {
            seen.push(entry["entry"].as_str().unwrap().to_owned());
        }
        cursor = page["next_cursor"].clone();
    }
    assert_eq!(seen, ["a", "b", "c"]);
    for args in [
        json!({"project":"project","order":"desc","cursor":cursor_from_first}),
        json!({"project":"project","statuses":["adopted"],"cursor":cursor_from_first}),
        json!({"project":"project","entry":"a","cursor":cursor_from_first}),
    ] {
        let reply = send(&mut session, call("memory_timeline", args.clone())).await;
        assert_eq!(reply["result"]["isError"], true, "{args}");
    }
    let history = body(
        &send(
            &mut session,
            call(
                "memory_timeline",
                json!({"project":"project","entry":"a","limit":1}),
            ),
        )
        .await,
    );
    assert_eq!(history["entries"][0]["version"], 1);
    assert_eq!(history["uncovered_decisions"], json!([]));
    let rest = body(
        &send(
            &mut session,
            call(
                "memory_timeline",
                json!({"project":"project","entry":"a","cursor":history["next_cursor"]}),
            ),
        )
        .await,
    );
    assert_eq!(
        rest["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["version"].clone())
            .collect::<Vec<_>>(),
        [json!(2)]
    );
    assert!(rest["next_cursor"].is_null());
    let filtered = body(
        &send(
            &mut session,
            call(
                "memory_timeline",
                json!({"project":"project","statuses":["superseded"],"order":"desc"}),
            ),
        )
        .await,
    );
    assert_eq!(filtered["entries"].as_array().unwrap().len(), 1);
    assert_eq!(filtered["entries"][0]["entry"], "a");
    let foreign = send(
        &mut session,
        call("memory_timeline", json!({"project":"foreign"})),
    )
    .await;
    assert_eq!(foreign["result"]["isError"], true);
}
#[tokio::test]
async fn timeline_writes_are_traced_and_purge_reports_removed_entries() {
    let (runtime, mut session) = setup().await;
    ready(&mut session).await;
    decision(&runtime, "s-a").await;
    let reply = send(
        &mut session,
        call(
            "memory_timeline_write",
            entry_args("a", 1_700_000_000_000, "s-a"),
        ),
    )
    .await;
    assert_eq!(reply["result"]["isError"], false);
    let activity = body(
        &send(
            &mut session,
            call(
                "memory_activity",
                json!({"project":"project","operations":["timeline"]}),
            ),
        )
        .await,
    );
    let entries = activity["entries"].as_array().unwrap();
    assert!(entries.iter().any(|e| e["operation"] == "timeline"
        && e["outcome"] == "succeeded"
        && e["entry_kind"] == "operation"));
    assert!(entries.iter().all(|e| e["operation"] == "timeline"));
    let purge = body(
        &send(
            &mut session,
            call(
                "memory_purge",
                json!({"project":"project","sources":["s-a"]}),
            ),
        )
        .await,
    );
    assert_eq!(purge["timeline_entries"], 1);
    assert_eq!(purge["sources"], 1);
    let page = body(
        &send(
            &mut session,
            call("memory_timeline", json!({"project":"project"})),
        )
        .await,
    );
    assert_eq!(page["entries"], json!([]));
}
#[tokio::test]
async fn timeline_truncation_cursor_resumes_after_the_last_packed_entry() {
    let (runtime, mut session) = setup().await;
    ready(&mut session).await;
    let mut ids = Vec::new();
    for index in 0..32 {
        let prefix = format!("{index}-");
        let id = format!("{prefix}{}", "s".repeat(512 - prefix.len()));
        decision(&runtime, &id).await;
        ids.push(id);
    }
    for index in 0..32u64 {
        let mut args = entry_args(&format!("e{index:02}"), 1_700_000_000_000 + index, &ids[0]);
        args["sources"] = json!(ids);
        args["summary"] = json!("\"".repeat(8192));
        let reply = send(&mut session, call("memory_timeline_write", args)).await;
        assert_eq!(reply["result"]["isError"], false);
    }
    let mut seen = Vec::new();
    let mut cursor = Value::Null;
    let mut truncated_pages = 0;
    loop {
        let mut args = json!({"project":"project"});
        if !cursor.is_null() {
            args["cursor"] = cursor.clone();
        }
        let reply = send(&mut session, call("memory_timeline", args)).await;
        assert!(reply.to_string().len() <= MAX_RESPONSE_BYTES);
        let page = body(&reply);
        truncated_pages += usize::from(page["truncated"] == true);
        for entry in page["entries"].as_array().unwrap() {
            seen.push(entry["entry"].as_str().unwrap().to_owned());
        }
        cursor = page["next_cursor"].clone();
        if cursor.is_null() {
            break;
        }
    }
    assert!(truncated_pages >= 1);
    assert_eq!(
        seen,
        (0..32).map(|i| format!("e{i:02}")).collect::<Vec<_>>()
    );
}
fn listing(project: &ProjectId, states: &[LifecycleState], limit: usize) -> ListQuery {
    ListQuery {
        exclude_tree: false,
        project: project.clone(),
        states: states.to_vec(),
        limit,
        memory: None,
        after: None,
    }
}
#[tokio::test]
async fn memory_list_truncation_cursor_resumes_after_the_last_packed_item() {
    let (runtime, mut session) = setup().await;
    ready(&mut session).await;
    for index in 0..32 {
        let source = format!("s{index:02}");
        decision(&runtime, &source).await;
        let reply = send(
            &mut session,
            call(
                "memory_propose",
                json!({"project":"project",
            "memory":format!("m{index:02}"),"revision":format!("r{index:02}"),"sources":[source],
            "content":"\"".repeat(8192)}),
            ),
        )
        .await;
        assert_eq!(reply["result"]["isError"], false);
    }
    let mut seen = Vec::new();
    let mut cursor = Value::Null;
    let mut truncated_pages = 0;
    let mut last_watermark = 0;
    loop {
        let mut args = json!({"project":"project","states":["processing","active"]});
        if !cursor.is_null() {
            args["cursor"] = cursor.clone();
        }
        let reply = send(&mut session, call("memory_list", args)).await;
        assert_eq!(reply["result"]["isError"], false);
        assert!(reply.to_string().len() <= MAX_RESPONSE_BYTES);
        let page = body(&reply);
        truncated_pages += usize::from(page["truncated"] == true);
        for item in page["revisions"].as_array().unwrap() {
            let watermark = item["watermark_ms"].as_u64().unwrap();
            assert!(watermark >= last_watermark);
            last_watermark = watermark;
            assert!(item.get("audit").is_none());
            seen.push(item["revision"].as_str().unwrap().to_owned());
        }
        cursor = page["next_cursor"].clone();
        if cursor.is_null() {
            break;
        }
    }
    assert!(truncated_pages >= 1);
    assert_eq!(
        seen,
        (0..32).map(|i| format!("r{i:02}")).collect::<Vec<_>>()
    );
    // Reordered states share the fingerprint; a different filter does not.
    let first = body(
        &send(
            &mut session,
            call("memory_list", json!({"project":"project","limit":1})),
        )
        .await,
    );
    let token = first["next_cursor"].clone();
    assert!(token.is_string());
    let reordered = send(
        &mut session,
        call(
            "memory_list",
            json!({"project":"project","limit":1,"states":["active","processing"],"cursor":token}),
        ),
    )
    .await;
    assert_eq!(body(&reordered)["revisions"][0]["revision"], "r01");
    let other = send(
        &mut session,
        call(
            "memory_list",
            json!({"project":"project","limit":1,"memory":"m00","cursor":token}),
        ),
    )
    .await;
    assert_eq!(other["result"]["isError"], true);
    let malformed = send(
        &mut session,
        call("memory_list", json!({"project":"project","cursor":"zz"})),
    )
    .await;
    assert_eq!(malformed["error"]["code"], -32602);
}
#[tokio::test]
async fn memory_list_without_cursor_still_works_and_lineage_reads_carry_audit() {
    let (runtime, mut session) = setup().await;
    ready(&mut session).await;
    for (source, revision, predecessor) in [("a", "r1", None), ("b", "r2", Some("r1"))] {
        decision(&runtime, source).await;
        let mut args = json!({"project":"project","memory":"db","revision":revision,"sources":[source],
            "content":format!("database {source}")});
        if let Some(predecessor) = predecessor {
            args["predecessor"] = json!(predecessor);
        }
        assert_eq!(
            send(&mut session, call("memory_propose", args)).await["result"]["isError"],
            false
        );
    }
    let plain = body(
        &send(
            &mut session,
            call("memory_list", json!({"project":"project"})),
        )
        .await,
    );
    assert_eq!(plain["truncated"], false);
    assert!(plain["next_cursor"].is_null());
    let revisions = plain["revisions"].as_array().unwrap();
    assert_eq!(revisions.len(), 1);
    assert!(
        revisions
            .iter()
            .all(|item| item.get("audit").is_none() && item["watermark_ms"].is_u64())
    );
    let lineage = body(
        &send(
            &mut session,
            call(
                "memory_list",
                json!({"project":"project","memory":"db",
        "states":["processing","active","rejected","conflicted","superseded"]}),
            ),
        )
        .await,
    );
    let items = lineage["revisions"].as_array().unwrap();
    assert_eq!(
        items
            .iter()
            .map(|i| i["revision"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["r1", "r2"]
    );
    let audit = items[0]["audit"].as_array().unwrap();
    assert_eq!(audit.len(), 1);
    assert_eq!(audit[0]["action"], "promote");
    assert_eq!(audit[0]["from_state"], "processing");
    assert_eq!(audit[0]["to_state"], "active");
    assert!(audit[0]["expected_active"].is_null() && audit[0]["conflict_with"].is_null());
    assert!(audit[0]["at_ms"].is_u64());
    assert!(audit[0].get("actor").is_none() && !lineage.to_string().contains("actor"));
    let successor = items[1]["audit"].as_array().unwrap();
    assert_eq!(successor.len(), 1);
    assert_eq!(successor[0]["expected_active"], "r1");
    assert!(successor[0]["at_ms"].as_u64() >= audit[0]["at_ms"].as_u64());
    let unknown = send(
        &mut session,
        call("memory_list", json!({"project":"project","memory":""})),
    )
    .await;
    assert_eq!(unknown["error"]["code"], -32602);
}

#[tokio::test]
async fn capture_action_rejects_surviving_secret() {
    let (_, mut session) = setup().await;
    ready(&mut session).await;
    let episode = |summary: &str| {
        json!({"v":1,"tool":"bash","intent":"Pushing","summary":summary,
        "error_head":null,"session":"0123456789abcdef","seq":1})
        .to_string()
    };
    let capture = |source: &str, content: String| {
        call(
            "memory_capture",
            json!({
        "project":"project","source":source,"kind":"action","outcome":"succeeded",
        "origin":"omp://action","content":content}),
        )
    };
    let leaked = send(
        &mut session,
        capture("a/1", episode("git push https://u:p@h/x")),
    )
    .await;
    assert_eq!(leaked["result"]["isError"], true);
    assert!(body(&leaked)["error"].as_str().unwrap().contains("invalid"));
    let clean = send(&mut session, capture("a/2", episode("cargo test"))).await;
    assert_eq!(clean["result"]["isError"], false);
    assert_eq!(body(&clean)["status"], "saved_pending");
    let plain = send(&mut session, capture("a/3", "not json".into())).await;
    assert_eq!(plain["result"]["isError"], true);
    assert!(body(&plain)["error"].as_str().unwrap().contains("invalid"));
    let big = send(&mut session, capture("a/4", episode(&"x".repeat(2000)))).await;
    assert_eq!(big["result"]["isError"], true);
}

fn action_episode(summary: &str) -> String {
    json!({"v":1,"tool":"bash","intent":"Testing","summary":summary,
        "error_head":null,"session":"0123456789abcdef","seq":1})
    .to_string()
}
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}
const DAY_MS: u64 = 24 * 60 * 60 * 1000;
fn timed_capture(source: &str, kind: &str, origin: &str, at: Value) -> Value {
    let content = if kind == "action" {
        action_episode("cargo test")
    } else {
        "imported result".into()
    };
    call(
        "memory_capture",
        json!({"project":"project","source":source,"kind":kind,"outcome":"succeeded",
        "origin":origin,"content":content,"recorded_at_ms":at}),
    )
}

#[tokio::test]
async fn capture_action_import_accepts_past_timestamp() {
    let (_, mut session) = setup().await;
    ready(&mut session).await;
    let listed = send(
        &mut session,
        json!({"jsonrpc":"2.0","id":3,"method":"tools/list"}),
    )
    .await;
    let tool = listed["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == "memory_capture")
        .unwrap();
    assert_eq!(
        tool["inputSchema"]["properties"]["recorded_at_ms"]["type"],
        "integer"
    );
    assert!(
        !tool["inputSchema"]["required"]
            .as_array()
            .unwrap()
            .iter()
            .any(|k| k == "recorded_at_ms")
    );
    let past = now_ms() - 30 * DAY_MS;
    let saved = send(
        &mut session,
        timed_capture("import/1", "action", "import://omp", json!(past)),
    )
    .await;
    assert_eq!(saved["result"]["isError"], false);
    assert_eq!(body(&saved)["status"], "saved_pending");
    let proposed = send(
        &mut session,
        call(
            "memory_propose",
            json!({"project":"project","memory":"m",
        "revision":"r1","sources":["import/1"],"content":"imported episode"}),
        ),
    )
    .await;
    assert_eq!(proposed["result"]["isError"], false);
    let listed = body(
        &send(
            &mut session,
            call("memory_list", json!({"project":"project"})),
        )
        .await,
    );
    let source = &listed["revisions"][0]["sources"][0];
    assert_eq!(source["source"], "import/1");
    assert_eq!(source["recorded_at_ms"], past);
    // A replay with another timestamp is idempotent and keeps the first one.
    let replay = send(
        &mut session,
        timed_capture("import/1", "action", "import://omp", json!(past + 1000)),
    )
    .await;
    assert_eq!(body(&replay)["status"], "already_saved_pending");
}

#[tokio::test]
async fn capture_accepts_imported_exchange_timestamp() {
    let (_, mut session) = setup().await;
    ready(&mut session).await;
    let past = now_ms() - 30 * DAY_MS;
    let capture = |source: &str, origin: &str, at: u64| {
        call(
            "memory_capture",
            json!({
                "project":"project","source":source,"kind":"exchange","outcome":"unknown",
                "origin":origin,"content":json!({"v":1,"prompt":"p","reply":"r"}).to_string(),
                "recorded_at_ms":at
            }),
        )
    };
    let saved = send(
        &mut session,
        capture("exchange/1", "import://claude-code", past),
    )
    .await;
    assert_eq!(saved["result"]["isError"], false, "{saved}");
    assert_eq!(body(&saved)["status"], "saved_pending");
    let proposed = send(
        &mut session,
        call(
            "memory_propose",
            json!({"project":"project","memory":"m",
        "revision":"r1","sources":["exchange/1"],"content":"imported exchange"}),
        ),
    )
    .await;
    assert_eq!(proposed["result"]["isError"], false);
    let listed = body(
        &send(
            &mut session,
            call("memory_list", json!({"project":"project"})),
        )
        .await,
    );
    let source = &listed["revisions"][0]["sources"][0];
    assert_eq!(source["source"], "exchange/1");
    assert_eq!(source["recorded_at_ms"], past);

    let live = send(
        &mut session,
        capture("exchange/2", "claude-code://exchange", past),
    )
    .await;
    assert_eq!(live["error"]["code"], -32602);
    let old = send(
        &mut session,
        capture(
            "exchange/3",
            "import://claude-code",
            now_ms() - 401 * DAY_MS,
        ),
    )
    .await;
    assert_eq!(old["result"]["isError"], true);
    assert!(
        body(&old)["error"]
            .as_str()
            .unwrap()
            .contains("recorded_at_ms out of range")
    );
}

#[tokio::test]
async fn capture_rejects_recorded_at_outside_imported_actions_and_range() {
    let (_, mut session) = setup().await;
    ready(&mut session).await;
    let past = json!(now_ms() - 30 * DAY_MS);
    let live = send(
        &mut session,
        timed_capture("live/1", "action", "omp://action", past.clone()),
    )
    .await;
    assert_eq!(live["error"]["code"], -32602);
    let result = send(
        &mut session,
        timed_capture("res/1", "result", "import://omp", past.clone()),
    )
    .await;
    assert_eq!(result["error"]["code"], -32602);
    let text = send(
        &mut session,
        timed_capture("bad/1", "action", "import://omp", json!("yesterday")),
    )
    .await;
    assert_eq!(text["error"]["code"], -32602);
    for (source, at) in [
        ("future/1", now_ms() + 60 * 60 * 1000),
        ("old/1", now_ms() - 401 * DAY_MS),
    ] {
        let reply = send(
            &mut session,
            timed_capture(source, "action", "import://omp", json!(at)),
        )
        .await;
        assert_eq!(reply["result"]["isError"], true);
        assert!(
            body(&reply)["error"]
                .as_str()
                .unwrap()
                .contains("recorded_at_ms out of range")
        );
    }
    let status = body(
        &send(
            &mut session,
            call("memory_status", json!({"project":"project"})),
        )
        .await,
    );
    assert_eq!(status["pending"]["unconsumed_sources"], 0);
}

#[tokio::test]
async fn recall_exclude_rejects_invalid() {
    let (_, mut session) = setup().await;
    ready(&mut session).await;
    for exclude in [
        json!(vec!["node"; 257]),
        json!(["node", 7]),
        json!([""]),
        json!(["bad\nnode"]),
        json!(["x".repeat(513)]),
        json!(null),
        json!("node"),
    ] {
        let reply = send(
            &mut session,
            call(
                "memory_recall",
                json!({"task":"storage", "exclude":exclude}),
            ),
        )
        .await;
        assert_eq!(reply["error"]["code"], -32602);
        assert_eq!(reply["error"]["message"], "invalid tool arguments");
    }
    for exclude in [json!([]), json!(vec!["node"; 256])] {
        let reply = send(
            &mut session,
            call(
                "memory_recall",
                json!({"task":"storage", "exclude":exclude}),
            ),
        )
        .await;
        assert_eq!(reply["result"]["isError"], false);
    }
    let listed = send(
        &mut session,
        json!({"jsonrpc":"2.0","id":3,"method":"tools/list"}),
    )
    .await;
    let tool = listed["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == "memory_recall")
        .unwrap();
    assert_eq!(
        tool["inputSchema"]["properties"]["exclude"],
        json!({"type":"array","items":{"type":"string"},"maxItems":256})
    );
    assert!(
        !tool["inputSchema"]["required"]
            .as_array()
            .unwrap()
            .contains(&json!("exclude"))
    );
}

#[tokio::test]
async fn recall_exclude_all_candidates_returns_empty() {
    let (runtime, mut session) = setup().await;
    ready(&mut session).await;
    decision(&runtime, "source").await;
    let proposal = send(&mut session, call("memory_propose", json!({"project":"project","memory":"m","revision":"r","sources":["source"],"content":"SQLite storage"}))).await;
    assert_eq!(proposal["result"]["isError"], false);
    let original = send(
        &mut session,
        call("memory_recall", json!({"task":"SQLite"})),
    )
    .await;
    assert_eq!(body(&original)["hits"].as_array().unwrap().len(), 1);
    let empty_exclude = send(
        &mut session,
        call("memory_recall", json!({"task":"SQLite","exclude":[]})),
    )
    .await;
    assert_eq!(body(&original), body(&empty_exclude));
    let reply = send(
        &mut session,
        call("memory_recall", json!({"task":"SQLite","exclude":["m"]})),
    )
    .await;
    assert_eq!(reply["result"]["isError"], false);
    assert_eq!(body(&reply)["hits"], json!([]));
    assert_eq!(body(&reply)["context"], "");
}

async fn brief_fixture() -> (EmbeddedRuntimeStore, Arc<MemoryRuntime>, McpSession) {
    use lambda_memory::domain_models::memory_tree::TreeMode;
    let path = format!(
        "/tmp/lambda-brief-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    );
    let store = EmbeddedRuntimeStore::new(EmbeddedCaptureStore::open(&path).await.unwrap());
    let owner = OwnerId::new("owner").unwrap();
    let project = ProjectId::new("project").unwrap();
    store
        .set_tree_mode(&owner, &project, TreeMode::Tree, 1)
        .await
        .unwrap();
    let runtime = Arc::new(
        MemoryRuntime::new(
            store.clone(),
            RuntimeScope {
                owner,
                actor: ActorId::new("actor").unwrap(),
                projects: vec![project],
            },
            RuntimeLimits {
                idle_ms: 1,
                ..RuntimeLimits::default()
            },
        )
        .unwrap(),
    );
    runtime.initialize_diagnostics().await.unwrap();
    let mut session = McpSession::new(runtime.clone(), TOKEN.into()).unwrap();
    ready(&mut session).await;
    (store, runtime, session)
}

async fn brief_node(
    store: &EmbeddedRuntimeStore,
    source: &str,
    parent: lambda_memory::domain_models::memory_tree::ParentRef,
    title: &str,
    linked_at: u64,
) -> String {
    use lambda_memory::domain_models::memory_tree::{CurationCommit, LinkOperation};
    let owner = OwnerId::new("owner").unwrap();
    let actor = ActorId::new("actor").unwrap();
    let project = ProjectId::new("project").unwrap();
    let source = SourceId::new(source).unwrap();
    store
        .capture(
            CaptureEvent {
                owner: owner.clone(),
                actor: actor.clone(),
                project: project.clone(),
                source: source.clone(),
                kind: WorkEventKind::Decision,
                outcome: WorkOutcome::Unknown,
                origin: "fixture://brief".into(),
                recorded_at_ms: linked_at,
                content: format!("Original {title}"),
            },
            &RuntimeLimits::default(),
        )
        .await
        .unwrap();
    let item = store
        .commit_curation(
            &owner,
            &actor,
            &project,
            CurationCommit {
                links: vec![],
                supersedes: vec![],
                promotes: vec![],
                operation_id: format!("brief-{source}"),
                sources: vec![source.clone()],
                creates: vec![(
                    parent,
                    title.into(),
                    format!("Summary {title}"),
                    vec![source],
                )],
                merges: vec![],
                parked: vec![],
                stale: vec![],
                split: vec![],
                operation: LinkOperation::Create,
            },
            linked_at,
        )
        .await
        .unwrap()
        .remove(0);
    store
        .review(
            &owner,
            &actor,
            &ReviewRequest {
                project,
                revision: item.revision.id.clone(),
                action: ReviewAction::Promote,
                expected_active: None,
                conflict_with: None,
            },
            linked_at + 1,
        )
        .await
        .unwrap();
    item.revision.memory.to_string()
}

#[tokio::test]
async fn user_store_brief_lists_user_roots() {
    use lambda_memory::domain_models::memory_tree::{
        CurationCommit, LinkOperation, ParentRef, TreeRoot,
    };
    let (store, _, _) = brief_fixture().await;
    let owner = OwnerId::new("owner").unwrap();
    let actor = ActorId::new("actor").unwrap();
    let project = ProjectId::new("user:nicolas").unwrap();
    // Insert out of order; neither storage order nor scope.projects[0] determines the brief roots.
    for root in [
        TreeRoot::GeneralLessons,
        TreeRoot::WorkingRules,
        TreeRoot::Preferences,
    ] {
        let source = SourceId::new(root.as_str()).unwrap();
        store
            .capture(
                CaptureEvent {
                    owner: owner.clone(),
                    actor: actor.clone(),
                    project: project.clone(),
                    source: source.clone(),
                    kind: WorkEventKind::Decision,
                    outcome: WorkOutcome::Unknown,
                    origin: "fixture://user".into(),
                    recorded_at_ms: 1,
                    content: format!("Original {}", root.label()),
                },
                &RuntimeLimits::default(),
            )
            .await
            .unwrap();
        let item = store
            .commit_curation(
                &owner,
                &actor,
                &project,
                CurationCommit {
                    links: vec![],
                    supersedes: vec![],
                    promotes: vec![],
                    operation_id: format!("brief-{source}"),
                    sources: vec![source.clone()],
                    creates: vec![(
                        ParentRef::Root(root),
                        root.label().into(),
                        "User statement".into(),
                        vec![source],
                    )],
                    merges: vec![],
                    parked: vec![],
                    stale: vec![],
                    split: vec![],
                    operation: LinkOperation::Create,
                },
                1,
            )
            .await
            .unwrap()
            .remove(0);
        store
            .review(
                &owner,
                &actor,
                &ReviewRequest {
                    project: project.clone(),
                    revision: item.revision.id,
                    action: ReviewAction::Promote,
                    expected_active: None,
                    conflict_with: None,
                },
                2,
            )
            .await
            .unwrap();
    }
    let runtime = Arc::new(
        MemoryRuntime::new(
            store,
            RuntimeScope {
                owner,
                actor,
                projects: vec![ProjectId::new("project").unwrap(), project.clone()],
            },
            RuntimeLimits::default(),
        )
        .unwrap(),
    );
    let mut session = McpSession::new(runtime, TOKEN.into()).unwrap();
    ready(&mut session).await;
    let reply = send(
        &mut session,
        call("memory_brief", json!({"project":project.as_str()})),
    )
    .await;
    assert_eq!(reply["result"]["isError"], false);
    let brief = body(&reply);
    assert_eq!(brief["mode"], "tree");
    assert_eq!(brief["truncated"], false);
    let labels: Vec<_> = brief["roots"]
        .as_array()
        .unwrap()
        .iter()
        .map(|root| root["root"].as_str().unwrap())
        .collect();
    assert_eq!(
        labels,
        vec!["Preferences", "Working rules", "General lessons"]
    );
    for root in brief["roots"].as_array().unwrap() {
        assert_eq!(root["nodes"].as_array().unwrap().len(), 1);
        assert_eq!(root["nodes"][0]["title"], root["root"]);
    }
}

#[tokio::test]
async fn brief_lists_depth_one_nodes_by_recency() {
    use lambda_memory::domain_models::MemoryId;
    use lambda_memory::domain_models::memory_tree::{ParentRef, TreeRoot};
    let (store, _, mut session) = brief_fixture().await;
    // Insert the roots and titles out of their required output order.
    let pref = brief_node(
        &store,
        "pref",
        ParentRef::Root(TreeRoot::Preferences),
        "Preference",
        400,
    )
    .await;
    let older = brief_node(
        &store,
        "older",
        ParentRef::Root(TreeRoot::Decisions),
        "A older",
        100,
    )
    .await;
    let z = brief_node(
        &store,
        "z",
        ParentRef::Root(TreeRoot::Decisions),
        "Z tied",
        200,
    )
    .await;
    let a = brief_node(
        &store,
        "a",
        ParentRef::Root(TreeRoot::Decisions),
        "A tied",
        200,
    )
    .await;
    brief_node(
        &store,
        "deep",
        ParentRef::Node(MemoryId::new(&a).unwrap()),
        "Nested",
        300,
    )
    .await;
    let before = body(
        &send(
            &mut session,
            call("memory_activity", json!({"project":"project"})),
        )
        .await,
    );
    let reply = send(
        &mut session,
        call("memory_brief", json!({"project":"project"})),
    )
    .await;
    assert_eq!(reply["result"]["isError"], false);
    assert_eq!(
        body(&reply),
        json!({"project":"project","mode":"tree","roots":[
        {"root":"Decisions","nodes":[{"node":a,"title":"A tied","children":1,"basis":"observed"},{"node":z,"title":"Z tied","children":0,"basis":"observed"},{"node":older,"title":"A older","children":0,"basis":"observed"}]},
        {"root":"Preferences","nodes":[{"node":pref,"title":"Preference","children":0,"basis":"observed"}]}
    ],"truncated":false,"skill_drafts_pending":0})
    );
    let after = body(
        &send(
            &mut session,
            call("memory_activity", json!({"project":"project"})),
        )
        .await,
    );
    assert_eq!(after["entries"], before["entries"]);
    assert_eq!(
        after["history"]["instrumentation_started_at_ms"],
        before["history"]["instrumentation_started_at_ms"]
    );
}

#[tokio::test]
async fn brief_truncates_at_twelve() {
    use lambda_memory::domain_models::memory_tree::{ParentRef, TreeRoot};
    let (store, _, mut session) = brief_fixture().await;
    let mut expected = Vec::new();
    for i in 0..13 {
        expected.push(
            brief_node(
                &store,
                &format!("s-{i}"),
                ParentRef::Root(TreeRoot::OpenItems),
                &format!("Item {i:02}"),
                100 + i,
            )
            .await,
        );
    }
    let reply = send(
        &mut session,
        call("memory_brief", json!({"project":"project"})),
    )
    .await;
    assert_eq!(reply["result"]["isError"], false);
    let brief = body(&reply);
    assert_eq!(brief["truncated"], true);
    assert_eq!(brief["roots"].as_array().unwrap().len(), 1);
    assert_eq!(brief["roots"][0]["root"], "Open items");
    let nodes = brief["roots"][0]["nodes"].as_array().unwrap();
    assert_eq!(nodes.len(), 12);
    assert_eq!(
        nodes
            .iter()
            .map(|node| node["node"].as_str().unwrap())
            .collect::<Vec<_>>(),
        expected[1..]
            .iter()
            .rev()
            .map(String::as_str)
            .collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn brief_flat_mode_is_empty() {
    let (runtime, mut session) = setup().await;
    ready(&mut session).await;
    decision(&runtime, "legacy").await;
    let proposed = send(&mut session, call("memory_propose", json!({"project":"project","memory":"legacy","revision":"legacy-r","sources":["legacy"],"content":"Legacy flat knowledge"}))).await;
    assert_eq!(proposed["result"]["isError"], false);
    let reply = send(
        &mut session,
        call("memory_brief", json!({"project":"project"})),
    )
    .await;
    assert_eq!(reply["result"]["isError"], false);
    assert_eq!(
        body(&reply),
        json!({"project":"project","mode":"flat","roots":[],"truncated":false,"skill_drafts_pending":0})
    );
}

#[tokio::test]
async fn brief_requires_authorized_project() {
    let (_, mut session) = setup().await;
    ready(&mut session).await;
    let brief = send(
        &mut session,
        call("memory_brief", json!({"project":"foreign"})),
    )
    .await;
    let node = send(
        &mut session,
        call("memory_node", json!({"project":"foreign","node":"node"})),
    )
    .await;
    assert_eq!(brief["result"]["isError"], true);
    assert_eq!(body(&brief), body(&node));
}

#[tokio::test]
async fn brief_schema_and_arguments_are_strict() {
    let (_, mut session) = setup().await;
    ready(&mut session).await;
    for args in [
        json!({}),
        json!({"project":7}),
        json!({"project":"project","extra":true}),
        json!({"project":""}),
    ] {
        assert_eq!(
            send(&mut session, call("memory_brief", args)).await["error"]["code"],
            -32602
        );
    }
    let listed = send(
        &mut session,
        json!({"jsonrpc":"2.0","id":3,"method":"tools/list"}),
    )
    .await;
    let tool = listed["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == "memory_brief")
        .unwrap();
    assert_eq!(tool["annotations"]["readOnlyHint"], true);
    assert_eq!(
        tool["description"],
        "Read the project's memory tree index (titles of the top nodes under each root) for a session brief, without model calls or idle postponement."
    );
    assert_eq!(
        tool["inputSchema"],
        json!({"type":"object","properties":{"project":{"type":"string"}},"required":["project"],"additionalProperties":false})
    );
}

#[tokio::test]
async fn memory_node_is_read_only_annotated() {
    let (_, mut session) = setup().await;
    ready(&mut session).await;
    let reply = send(
        &mut session,
        json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
    )
    .await;
    let tool = reply["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "memory_node")
        .unwrap();
    assert_eq!(tool["annotations"]["readOnlyHint"], true);
}

#[tokio::test]
async fn memory_node_reports_live_lesson_stats() {
    use lambda_memory::domain_models::memory_tree::{
        CurationCommit, LinkOperation, ParentRef, TreeRoot,
    };
    let (store, _, mut session) = brief_fixture().await;
    let owner = OwnerId::new("owner").unwrap();
    let actor = ActorId::new("actor").unwrap();
    let project = ProjectId::new("project").unwrap();
    let mut sources = Vec::new();
    for (key, n, kind) in [
        ("0123456789abcdef", 1, "strategy"),
        ("0123456789abcdef", 2, "strategy"),
        ("fedcba9876543210", 1, "pitfall"),
    ] {
        let source = SourceId::new(format!("lesson/{key}/{n}")).unwrap();
        store.capture(CaptureEvent { owner:owner.clone(), actor:actor.clone(), project:project.clone(), source:source.clone(),
            kind:WorkEventKind::Lesson, outcome:WorkOutcome::Unknown, origin:format!("lesson://{key}/{n}"), recorded_at_ms:1,
            content:json!({"v":1,"kind":kind,"title":"Tests","applies_when":"Changing storage","guidance":"Run tests"}).to_string(),
        }, &RuntimeLimits::default()).await.unwrap();
        sources.push(source);
    }
    let item = store
        .commit_curation(
            &owner,
            &actor,
            &project,
            CurationCommit {
                links: vec![],
                promotes: vec![],
                operation_id: "lesson-node".into(),
                sources: sources.clone(),
                creates: vec![(
                    ParentRef::Root(TreeRoot::Lessons),
                    "Tests".into(),
                    "Run tests".into(),
                    sources,
                )],
                supersedes: vec![],
                merges: vec![],
                parked: vec![],
                stale: vec![],
                split: vec![],
                operation: LinkOperation::Create,
            },
            2,
        )
        .await
        .unwrap()
        .remove(0);
    store
        .review(
            &owner,
            &actor,
            &ReviewRequest {
                project: project.clone(),
                revision: item.revision.id,
                action: ReviewAction::Promote,
                expected_active: None,
                conflict_with: None,
            },
            3,
        )
        .await
        .unwrap();
    let reply = send(
        &mut session,
        call(
            "memory_node",
            json!({"project":"project","node":item.revision.memory.as_str()}),
        ),
    )
    .await;
    assert_eq!(reply["result"]["isError"], false);
    assert_eq!(
        body(&reply)["lesson"],
        json!({"reinforced_sessions":2,"strategies":2,"pitfalls":1})
    );
    let normal = brief_node(
        &store,
        "normal",
        ParentRef::Root(TreeRoot::Decisions),
        "Decision",
        4,
    )
    .await;
    let reply = send(
        &mut session,
        call("memory_node", json!({"project":"project","node":normal})),
    )
    .await;
    assert!(body(&reply).get("lesson").is_none());
}

#[tokio::test]
async fn memory_node_lists_links() {
    use lambda_memory::domain_models::MemoryId;
    use lambda_memory::domain_models::memory_tree::*;
    let (store, _, mut session) = brief_fixture().await;
    let owner = OwnerId::new("owner").unwrap();
    let actor = ActorId::new("actor").unwrap();
    let project = ProjectId::new("project").unwrap();
    let a = brief_node(&store, "a", ParentRef::Root(TreeRoot::Decisions), "A", 4).await;
    let b = brief_node(&store, "b", ParentRef::Root(TreeRoot::Architecture), "B", 5).await;
    let old = store
        .tree_nodes(&owner, &project)
        .await
        .unwrap()
        .into_iter()
        .find(|n| n.memory.as_str() == a)
        .unwrap();
    let source = SourceId::new("edge").unwrap();
    store
        .capture(
            CaptureEvent {
                owner: owner.clone(),
                actor: actor.clone(),
                project: project.clone(),
                source: source.clone(),
                kind: WorkEventKind::Decision,
                outcome: WorkOutcome::Unknown,
                origin: "fixture://links".into(),
                recorded_at_ms: 6,
                content: "A relates to B".into(),
            },
            &RuntimeLimits::default(),
        )
        .await
        .unwrap();
    let item = store
        .commit_curation(
            &owner,
            &actor,
            &project,
            CurationCommit {
                links: vec![CurationLink {
                    from: LinkFrom::Node(old.memory.clone()),
                    to: MemoryId::new(&b).unwrap(),
                    relation: LinkRelation::Related,
                    sources: vec![source.clone()],
                }],
                operation_id: "edge".into(),
                sources: vec![source.clone()],
                promotes: vec![],
                supersedes: vec![],
                merges: vec![(
                    old.memory,
                    old.revision.clone(),
                    "A".into(),
                    "A summary".into(),
                    vec![source],
                )],
                creates: vec![],
                parked: vec![],
                stale: vec![],
                split: vec![],
                operation: LinkOperation::Create,
            },
            7,
        )
        .await
        .unwrap()
        .remove(0);
    store
        .review(
            &owner,
            &actor,
            &ReviewRequest {
                project: project.clone(),
                revision: item.revision.id,
                action: ReviewAction::Promote,
                expected_active: Some(old.revision),
                conflict_with: None,
            },
            8,
        )
        .await
        .unwrap();
    for (node, other, title, direction) in [(&a, &b, "B", "out"), (&b, &a, "A", "in")] {
        let reply = send(
            &mut session,
            call("memory_node", json!({"project":"project","node":node})),
        )
        .await;
        assert_eq!(
            body(&reply)["links"],
            json!([{"node":other,"title":title,"relation":"related","direction":direction}])
        );
    }
    store
        .retire_node(&owner, &actor, &project, &MemoryId::new(b).unwrap(), 9)
        .await
        .unwrap();
    let reply = send(
        &mut session,
        call("memory_node", json!({"project":"project","node":a})),
    )
    .await;
    assert_eq!(body(&reply)["links"], json!([]));
}

#[tokio::test]
async fn memory_node_unknown_node_is_missing_error() {
    let (_, mut session) = setup().await;
    ready(&mut session).await;
    let reply = send(
        &mut session,
        call("memory_node", json!({"project":"project","node":"unknown"})),
    )
    .await;
    assert_eq!(reply["result"]["isError"], true);
    assert!(body(&reply).to_string().contains("record not found"));
}

#[tokio::test]
async fn memory_node_returns_sources_and_history_and_pages() {
    use lambda_memory::domain_models::memory_tree::{
        CurationCommit, LinkOperation, ParentRef, TreeRoot,
    };
    let path = format!(
        "/tmp/lambda-node-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    );
    let store = EmbeddedRuntimeStore::new(EmbeddedCaptureStore::open(&path).await.unwrap());
    let owner = OwnerId::new("owner").unwrap();
    let actor = ActorId::new("actor").unwrap();
    let project = ProjectId::new("project").unwrap();
    let mut ids = Vec::new();
    for i in 0..33 {
        let source = SourceId::new(format!("source-{i:02}")).unwrap();
        store
            .capture(
                CaptureEvent {
                    owner: owner.clone(),
                    actor: actor.clone(),
                    project: project.clone(),
                    source: source.clone(),
                    kind: WorkEventKind::Decision,
                    outcome: WorkOutcome::Unknown,
                    origin: "fixture://test".into(),
                    recorded_at_ms: i,
                    content: format!("original {i}"),
                },
                &RuntimeLimits::default(),
            )
            .await
            .unwrap();
        ids.push(source);
    }
    let plan = |op: &str, sources: Vec<SourceId>, creates, merges| CurationCommit {
        links: vec![],
        supersedes: vec![],
        promotes: vec![],
        operation_id: op.into(),
        sources,
        creates,
        merges,
        parked: vec![],
        stale: vec![],
        split: vec![],
        operation: LinkOperation::Create,
    };
    let first = store
        .commit_curation(
            &owner,
            &actor,
            &project,
            plan(
                "first",
                ids[..32].to_vec(),
                vec![(
                    ParentRef::Root(TreeRoot::Decisions),
                    "Vault".into(),
                    "Summary".into(),
                    ids[..32].to_vec(),
                )],
                vec![],
            ),
            10,
        )
        .await
        .unwrap()
        .remove(0);
    let promote = |revision: &RuntimeRevision| ReviewRequest {
        project: project.clone(),
        revision: revision.revision.id.clone(),
        action: ReviewAction::Promote,
        expected_active: revision.revision.predecessor.clone(),
        conflict_with: None,
    };
    store
        .review(&owner, &actor, &promote(&first), 11)
        .await
        .unwrap();
    let second = store
        .commit_curation(
            &owner,
            &actor,
            &project,
            plan(
                "second",
                ids[32..].to_vec(),
                vec![],
                vec![(
                    first.revision.memory.clone(),
                    first.revision.id.clone(),
                    "Vault".into(),
                    "New summary".into(),
                    ids[32..].to_vec(),
                )],
            ),
            12,
        )
        .await
        .unwrap()
        .remove(0);
    store
        .review(&owner, &actor, &promote(&second), 13)
        .await
        .unwrap();
    let runtime = Arc::new(
        MemoryRuntime::new(
            store,
            RuntimeScope {
                owner,
                actor,
                projects: vec![project],
            },
            RuntimeLimits::default(),
        )
        .unwrap(),
    );
    let mut session = McpSession::new(runtime, TOKEN.into()).unwrap();
    ready(&mut session).await;
    let node = first.revision.memory.as_str();
    let result = body(
        &send(
            &mut session,
            call(
                "memory_node",
                json!({"project":"project","node":node,"history":true,"sources":true}),
            ),
        )
        .await,
    );
    assert_eq!(result["path"], json!(["Decisions", "Vault"]));
    assert_eq!(result["history"].as_array().unwrap().len(), 2);
    assert_eq!(result["sources"].as_array().unwrap().len(), 32);
    let next = result["next_cursor"].as_str().unwrap();
    let result = body(
        &send(
            &mut session,
            call(
                "memory_node",
                json!({"project":"project","node":node,"sources":true,"cursor":next}),
            ),
        )
        .await,
    );
    assert_eq!(result["sources"].as_array().unwrap().len(), 1);
    assert!(result["next_cursor"].is_null());
}

#[tokio::test]
async fn memory_node_shows_validity_history() {
    use lambda_memory::domain_models::memory_tree::{
        CurationCommit, LinkOperation, ParentRef, TreeRoot,
    };
    let (store, _, mut session) = brief_fixture().await;
    let owner = OwnerId::new("owner").unwrap();
    let actor = ActorId::new("actor").unwrap();
    let project = ProjectId::new("project").unwrap();
    let node = brief_node(
        &store,
        "original",
        ParentRef::Root(TreeRoot::Decisions),
        "Original",
        1,
    )
    .await;
    let old = store.tree_nodes(&owner, &project).await.unwrap().remove(0);
    let source = SourceId::new("change").unwrap();
    let start = 1_735_689_600_000_u64;
    store
        .capture(
            CaptureEvent {
                owner: owner.clone(),
                actor: actor.clone(),
                project: project.clone(),
                source: source.clone(),
                kind: WorkEventKind::Decision,
                outcome: WorkOutcome::Unknown,
                origin: "fixture://validity".into(),
                recorded_at_ms: start,
                content: "Changed position".into(),
            },
            &RuntimeLimits::default(),
        )
        .await
        .unwrap();
    let item = store
        .commit_curation(
            &owner,
            &actor,
            &project,
            CurationCommit {
                links: vec![],
                operation_id: "change".into(),
                sources: vec![source.clone()],
                promotes: vec![],
                creates: vec![],
                merges: vec![(
                    old.memory.clone(),
                    old.revision.clone(),
                    "Current".into(),
                    "Current position only".into(),
                    vec![source],
                )],
                supersedes: vec![(old.memory.clone(), start)],
                parked: vec![],
                stale: vec![],
                split: vec![],
                operation: LinkOperation::Create,
            },
            start,
        )
        .await
        .unwrap()
        .remove(0);
    store
        .review(
            &owner,
            &actor,
            &ReviewRequest {
                project,
                revision: item.revision.id.clone(),
                action: ReviewAction::Promote,
                expected_active: Some(old.revision.clone()),
                conflict_with: None,
            },
            start + 1,
        )
        .await
        .unwrap();
    let result = body(
        &send(
            &mut session,
            call(
                "memory_node",
                json!({"project":"project","node":node,"history":true,"sources":true}),
            ),
        )
        .await,
    );
    assert_eq!(result["title"], "Current");
    assert_eq!(result["content"], "Current position only");
    assert_eq!(result["valid_from_ms"], start);
    let history = result["history"].as_array().unwrap();
    assert_eq!(history.len(), 2);
    let previous = history
        .iter()
        .find(|r| r["revision"] == old.revision.as_str())
        .unwrap();
    assert!(previous["valid_from_ms"].is_null());
    assert_eq!(previous["valid_until_ms"], start);
    let current = history
        .iter()
        .find(|r| r["revision"] == item.revision.id.as_str())
        .unwrap();
    assert_eq!(current["valid_from_ms"], start);
    assert!(current["valid_until_ms"].is_null());
    assert!(
        result["sources"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["operation"] == "supersede")
    );
}
