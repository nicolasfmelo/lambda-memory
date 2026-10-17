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
    assert_eq!(tools.len(), 10);
    for tool in tools {
        let name = tool["name"].as_str().unwrap();
        let read = matches!(
            name,
            "memory_list"
                | "memory_recall"
                | "memory_status"
                | "memory_activity"
                | "memory_timeline"
        );
        assert_eq!(tool["annotations"]["readOnlyHint"], read, "{name}");
        assert_eq!(
            tool["annotations"]["destructiveHint"],
            name == "memory_purge",
            "{name}"
        );
    }
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
