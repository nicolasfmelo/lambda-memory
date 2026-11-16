#![cfg(feature = "memory-runtime")]

use lambda_memory::domain_models::capture::*;
use lambda_memory::domain_models::runtime::{RevisionProposal, RuntimeLimits};
use lambda_memory::domain_models::{ActorId, MemoryId, OwnerId, ProjectId, RevisionId, SourceId};
use lambda_memory::integrations::database::embedded_capture::{
    EmbeddedCaptureStore, EmbeddedRuntimeStore,
};
use serde_json::{Value, json};
use std::io::Write;
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT: AtomicUsize = AtomicUsize::new(0);
struct Fixture {
    path: String,
    project: &'static str,
}
impl Fixture {
    fn new(project: &'static str) -> Self {
        let root = std::env::var("HOME").unwrap();
        let path = format!(
            "{root}/.cache/lambda-memory/identity-merge-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        );
        std::fs::create_dir_all(&path).unwrap();
        Self { path, project }
    }
    async fn open(&self) -> EmbeddedRuntimeStore {
        for _ in 0..30 {
            if let Ok(capture) = EmbeddedCaptureStore::open(&self.path).await {
                return EmbeddedRuntimeStore::new(capture);
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        panic!("datastore lock did not release");
    }
    async fn command(&self, flag: &str, input: &[u8], max_pending: usize) -> Output {
        for _ in 0..30 {
            let mut child = Command::new(env!("CARGO_BIN_EXE_lambda-memory"))
                .arg(flag)
                .env("LAMBDA_MEMORY_DATA_DIR", &self.path)
                .env("LAMBDA_MEMORY_TOKEN", "t".repeat(40))
                .env("LAMBDA_MEMORY_OWNER", "owner")
                .env("LAMBDA_MEMORY_ACTOR", "operator")
                .env("LAMBDA_MEMORY_PROJECT", self.project)
                .env("LAMBDA_MEMORY_MODE", "local")
                .env("LAMBDA_MEMORY_MAX_PENDING", max_pending.to_string())
                .env_remove("LAMBDA_MEMORY_ALLOWED_PROJECTS")
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            // A failed open can close stdin before the writer finishes; its storage error is
            // safe to retry, as in runtime_recovery's operator helper.
            let _ = child.stdin.take().unwrap().write_all(input);
            let output = child.wait_with_output().unwrap();
            if !String::from_utf8_lossy(&output.stderr).contains("storage unavailable") {
                return output;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        panic!("operator could not open store");
    }
    async fn json(&self, flag: &str, input: &[u8], max_pending: usize) -> Value {
        let output = self.command(flag, input, max_pending).await;
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }
    async fn export(&self) -> Vec<Value> {
        let output = self.command("--export-sources", b"", 1000).await;
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .map(|s| serde_json::from_str(s).unwrap())
            .collect()
    }
    async fn inspect(&self, max: usize) -> Value {
        self.json("--inspect-sources", b"", max).await
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}
fn owner() -> OwnerId {
    OwnerId::new("owner").unwrap()
}
fn line(id: &str, kind: &str, content: &str) -> Value {
    json!({"source":id, "origin":"fixture://original", "recorded_at_ms":123,
        "kind":kind, "outcome":"succeeded", "content":content})
}
fn action() -> String {
    json!({"v":1, "tool":"Bash", "intent":"Run tests", "summary":"Tests passed",
        "error_head":null, "session":"0123456789abcdef", "seq":1})
    .to_string()
}
fn exchange(reply: &str) -> String {
    json!({"v":1,"prompt":"What changed?","reply":reply}).to_string()
}
fn jsonl(lines: &[Value]) -> Vec<u8> {
    lines
        .iter()
        .map(|line| format!("{line}\n"))
        .collect::<String>()
        .into_bytes()
}
fn event(project: &str, line: &Value) -> CaptureEvent {
    CaptureEvent {
        source: SourceId::new(line["source"].as_str().unwrap()).unwrap(),
        owner: owner(),
        actor: ActorId::new("original-actor").unwrap(),
        project: ProjectId::new(project).unwrap(),
        origin: line["origin"].as_str().unwrap().into(),
        recorded_at_ms: line["recorded_at_ms"].as_u64().unwrap(),
        kind: match line["kind"].as_str().unwrap() {
            "action" => WorkEventKind::Action,
            "exchange" => WorkEventKind::Exchange,
            "result" => WorkEventKind::Result,
            _ => WorkEventKind::Decision,
        },
        outcome: WorkOutcome::Succeeded,
        content: line["content"].as_str().unwrap().into(),
    }
}
async fn seed(fixture: &Fixture, rows: &[Value], consume_first: bool) {
    let store = fixture.open().await;
    for row in rows {
        store
            .capture(event(fixture.project, row), &RuntimeLimits::default())
            .await
            .unwrap();
    }
    if consume_first {
        store
            .propose(
                &owner(),
                &ActorId::new("original-actor").unwrap(),
                RevisionProposal {
                    project: ProjectId::new(fixture.project).unwrap(),
                    memory: MemoryId::new("memory").unwrap(),
                    revision: RevisionId::new("revision").unwrap(),
                    predecessor: None,
                    sources: vec![SourceId::new(rows[0]["source"].as_str().unwrap()).unwrap()],
                    content: "A sourced memory".into(),
                },
                1000,
            )
            .await
            .unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn imported_lessons_restore_purge_citations_in_either_order() {
    let key = "0123456789abcdef";
    let exchange_id = format!("{key}/turn-1");
    let lesson_id = format!("lesson/{key}/1");
    for origin in [format!("lesson://{key}/1"), "promote://other/lesson".into()] {
        let a = Fixture::new("project-a");
        let exchange = line(&exchange_id, "exchange", &exchange("Tests passed"));
        let mut lesson=line(&lesson_id,"lesson",&json!({"v":1,"kind":"strategy","scope":"project","title":"Test the change","applies_when":"Changing storage","guidance":"1. Run tests.","task":"Verify storage","task_outcome":"succeeded","evidence":[exchange_id,"not-in-this-store"]}).to_string());
        lesson["origin"] = json!(origin);
        assert_eq!(
            a.json("--import-sources", &jsonl(&[exchange, lesson]), 1000)
                .await["inserted"],
            2
        );
        let exported = a.export().await;
        for reversed in [false, true] {
            let b = Fixture::new("project-b");
            let mut input = exported.clone();
            if reversed {
                input.reverse();
            }
            assert_eq!(
                b.json("--import-sources", &jsonl(&input), 1000).await["inserted"],
                2
            );
            // Duplicate imports leave the eagerly created citations unchanged.
            assert_eq!(
                b.json("--import-sources", &jsonl(&input), 1000).await["duplicates"],
                2
            );
            let store = b.open().await;
            let project = ProjectId::new("project-b").unwrap();
            let evidence = SourceId::new(&exchange_id).unwrap();
            let lesson = SourceId::new(&lesson_id).unwrap();
            assert_eq!(
                store
                    .lesson_citations(&owner(), &project, &lesson)
                    .await
                    .unwrap(),
                vec![
                    evidence.clone(),
                    SourceId::new("not-in-this-store").unwrap()
                ]
            );
            assert_eq!(
                store
                    .purge(&owner(), &project, &[evidence])
                    .await
                    .unwrap()
                    .sources,
                2
            );
            assert!(
                store
                    .export_sources(&owner(), &project)
                    .await
                    .unwrap()
                    .is_empty()
            );
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn lesson_citation_survives_until_evidence_arrives_in_a_later_import() {
    let fixture = Fixture::new("project-b");
    let evidence_id = "0123456789abcdef/turn-later";
    let lesson_id = "lesson/0123456789abcdef/1";
    let mut lesson=line(lesson_id,"lesson",&json!({"v":1,"kind":"strategy","scope":"project","title":"Test storage","applies_when":"Changing storage","guidance":"1. Test it.","task":"Verify","task_outcome":"succeeded","evidence":[evidence_id]}).to_string());
    lesson["origin"] = json!("lesson://0123456789abcdef/1");
    assert_eq!(
        fixture
            .json("--import-sources", &jsonl(&[lesson]), 1000)
            .await["inserted"],
        1
    );
    let evidence = line(evidence_id, "exchange", &exchange("Tests passed"));
    assert_eq!(
        fixture
            .json("--import-sources", &jsonl(&[evidence]), 1000)
            .await["inserted"],
        1
    );
    let store = fixture.open().await;
    let project = ProjectId::new("project-b").unwrap();
    assert_eq!(
        store
            .purge(&owner(), &project, &[SourceId::new(evidence_id).unwrap()])
            .await
            .unwrap()
            .sources,
        2
    );
    assert!(
        store
            .export_sources(&owner(), &project)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn imported_lessons_reject_extra_missing_and_secret_evidence_fields() {
    let fixture = Fixture::new("project-b");
    let valid = json!({"v":1,"kind":"strategy","scope":"project","title":"Test storage","applies_when":"Changing storage","guidance":"1. Test it.","task":"Verify","task_outcome":"succeeded","evidence":["source"]});
    let mut extra = valid.clone();
    extra["extra"] = json!(format!("ghp_{}", "a".repeat(36)));
    let mut missing = valid.clone();
    missing.as_object_mut().unwrap().remove("task_outcome");
    let mut secret_evidence = valid.clone();
    secret_evidence["evidence"] = json!([format!("ghp_{}", "a".repeat(36))]);
    for origin in ["lesson://session/1", "promote://other/lesson"] {
        for (i, content) in [&extra, &missing, &secret_evidence].into_iter().enumerate() {
            let mut row = line(&format!("invalid-{i}"), "lesson", &content.to_string());
            row["origin"] = json!(origin);
            let result = fixture.json("--import-sources", &jsonl(&[row]), 1000).await;
            assert_eq!(result["inserted"], 0);
            assert_eq!(result["rejected"], json!([format!("invalid-{i}")]));
        }
    }
    assert!(fixture.export().await.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn export_then_import_preserves_fields() {
    let a = Fixture::new("project-a");
    let b = Fixture::new("project-b");
    let mut decision = line("z-decision", "decision", "Use the embedded store");
    decision["recorded_at_ms"] = json!(100);
    let mut exchange = line("a-exchange", "exchange", &exchange("The store is local"));
    exchange["recorded_at_ms"] = json!(100);
    let action = line("action", "action", &action());
    seed(
        &a,
        &[decision.clone(), action.clone(), exchange.clone()],
        true,
    )
    .await;
    assert_eq!(a.inspect(1000).await["pending"], 1);
    let exported = a.export().await;
    assert_eq!(exported, [exchange, decision, action]);
    let report = b.json("--import-sources", &jsonl(&exported), 1000).await;
    assert_eq!(report["inserted"], 3);
    assert_eq!(b.export().await, exported);
    let counts = b.inspect(1000).await;
    assert_eq!(counts["curatable"], 2);
    assert_eq!(counts["pending"], 2, "consumption must not be copied");
    let store = b.open().await;
    let pending = store
        .eligible_sources(
            &owner(),
            &ProjectId::new("project-b").unwrap(),
            32,
            false,
            None,
            None,
            true,
        )
        .await
        .unwrap();
    assert_eq!(pending.len(), 2);
    assert!(pending.iter().all(|e| e.actor.as_str() == "operator"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn import_is_idempotent() {
    let fixture = Fixture::new("project-b");
    let rows = [
        line("decision", "decision", "Keep sources immutable"),
        line("action", "action", &action()),
    ];
    let input = jsonl(&rows);
    assert_eq!(
        fixture.json("--import-sources", &input, 140).await["inserted"],
        2
    );
    let mut retry = rows.to_vec();
    retry[0]["recorded_at_ms"] = json!(456);
    retry[0]["outcome"] = json!("failed");
    let report = fixture.json("--import-sources", &jsonl(&retry), 100).await;
    assert_eq!(
        report,
        json!({"project":"project-b", "inserted":0,"duplicates":2,"conflicts":[],"rejected":[],"remaining":0,
            "inserted_ids":[],"duplicate_ids":["decision","action"],"remaining_ids":[]})
    );
    // An existing actor different from the operator also does not change source identity.
    let original = Fixture::new("project-a");
    seed(&original, &rows, true).await;
    assert_eq!(
        original.json("--import-sources", &input, 100).await["duplicates"],
        2
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn import_reports_conflicts() {
    let fixture = Fixture::new("project-b");
    let original = line("same-id", "decision", "Original evidence");
    seed(&fixture, std::slice::from_ref(&original), false).await;
    for changed in [
        line("same-id", "decision", "Changed evidence"),
        {
            let mut row = original.clone();
            row["origin"] = json!("other://origin");
            row
        },
        {
            let mut row = original.clone();
            row["kind"] = json!("result");
            row
        },
    ] {
        let report = fixture
            .json("--import-sources", &jsonl(&[changed]), 1000)
            .await;
        assert_eq!(report["conflicts"], json!(["same-id"]));
        assert_eq!(report["inserted"], 0);
    }
    assert_eq!(fixture.export().await, [original]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn import_rejects_secrets() {
    let fixture = Fixture::new("project-b");
    let secret = format!("ghp_{}", "A".repeat(36));
    let mut bad_action: Value = serde_json::from_str(&action()).unwrap();
    bad_action["error_head"] = json!(secret);
    let rows = [
        line("decision-secret", "decision", &secret),
        line("exchange-secret", "exchange", &exchange(&secret)),
        line("action-secret", "action", &bad_action.to_string()),
        line("clean", "decision", "A clean decision"),
    ];
    let report = fixture.json("--import-sources", &jsonl(&rows), 1000).await;
    assert_eq!(
        report["rejected"],
        json!(["decision-secret", "exchange-secret", "action-secret"])
    );
    assert_eq!(report["inserted"], 1);
    assert_eq!(fixture.export().await, [rows[3].clone()]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn import_respects_pending_headroom() {
    let fixture = Fixture::new("project-b");
    let pending: Vec<_> = (0..10)
        .map(|n| {
            line(
                &format!("pending-{n}"),
                "decision",
                "Existing pending decision",
            )
        })
        .collect();
    seed(&fixture, &pending, false).await;
    let mut rows: Vec<_> = (0..5)
        .map(|n| line(&format!("new-{n}"), "decision", "Imported decision"))
        .collect();
    rows.extend((0..3).map(|n| line(&format!("action-{n}"), "action", &action())));
    let mut legacy = line("legacy", "result", "Old tool result");
    legacy["origin"] = json!("omp://tool_result/old");
    rows.push(legacy);
    let input = jsonl(&rows);
    let report = fixture.json("--import-sources", &input, 140).await;
    assert_eq!(report["inserted"], 6);
    assert_eq!(report["inserted_ids"].as_array().unwrap().len(), 6);
    assert_eq!(report["remaining"], 3);
    assert_eq!(report["remaining_ids"], json!(["new-2", "new-3", "new-4"]));
    assert_eq!(fixture.inspect(140).await["pending"], 12);
    // Repeating at the limit still recognizes existing lines before applying headroom.
    let report = fixture.json("--import-sources", &input, 140).await;
    assert_eq!(report["duplicates"], 6);
    assert_eq!(report["remaining"], 3);
    let small = Fixture::new("small-project");
    let report = small.json("--import-sources", &input, 100).await;
    assert_eq!(report["inserted"], 4);
    assert_eq!(report["remaining"], 5);
    assert_eq!(small.inspect(100).await["pending"], 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn import_keeps_old_sources() {
    let fixture = Fixture::new("project-b");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    let mut old = line("old", "decision", "Historical source");
    old["recorded_at_ms"] = json!(now - 500 * 86_400_000);
    assert_eq!(
        fixture
            .json("--import-sources", &jsonl(&[old.clone()]), 1000)
            .await["inserted"],
        1
    );
    assert_eq!(fixture.export().await, [old]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn inspect_counts_sources() {
    let fixture = Fixture::new("project-a");
    let mut legacy = line("legacy", "result", "Tool output");
    legacy["origin"] = json!("omp://tool_result");
    seed(
        &fixture,
        &[
            line("consumed", "decision", "Consumed decision"),
            line("pending", "decision", "Pending decision"),
            line("exchange", "exchange", &exchange("Reply")),
            line("action", "action", &action()),
            legacy,
        ],
        true,
    )
    .await;
    assert_eq!(
        fixture.inspect(140).await,
        json!({"project":"project-a", "sources":5,"curatable":3,"pending":2,"max_pending":140})
    );
    let empty = Fixture::new("empty");
    assert_eq!(
        empty.inspect(100).await,
        json!({"project":"empty", "sources":0,"curatable":0,"pending":0,"max_pending":100})
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn malformed_input_is_rejected_as_a_whole() {
    let fixture = Fixture::new("project-b");
    let clean = line("clean", "decision", "Valid evidence");
    let mut extra = clean.clone();
    extra["unknown"] = json!(true);
    let mut missing = clean.clone();
    missing.as_object_mut().unwrap().remove("origin");
    let mut wrong_type = clean.clone();
    wrong_type["recorded_at_ms"] = json!("123");
    let mut malformed = jsonl(std::slice::from_ref(&clean));
    malformed.extend_from_slice(b"not-json\n");
    for input in [
        malformed,
        jsonl(&[clean.clone(), extra]),
        jsonl(&[clean.clone(), missing]),
        jsonl(&[clean.clone(), wrong_type]),
        [jsonl(&[clean]), b"\n".to_vec()].concat(),
    ] {
        let output = fixture.command("--import-sources", &input, 1000).await;
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        assert_eq!(fixture.inspect(1000).await["sources"], 0);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn import_rejects_out_of_range_timestamp() {
    let fixture = Fixture::new("project-b");
    let clean = line("clean", "decision", "Valid evidence");
    let mut out_of_range = line("out-of-range", "decision", "Invalid timestamp");
    out_of_range["recorded_at_ms"] = json!(9223372036854775808_u64);
    let output = fixture
        .command("--import-sources", &jsonl(&[clean, out_of_range]), 1000)
        .await;
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("invalid runtime input"));
    assert_eq!(fixture.inspect(1000).await["sources"], 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn oversized_input_is_rejected_as_a_whole() {
    let fixture = Fixture::new("project-b");
    let mut bytes = jsonl(&[line("clean", "decision", "Valid evidence")]);
    bytes.resize(64 * 1024 * 1024 + 1, b' ');
    let output = fixture.command("--import-sources", &bytes, 1000).await;
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(fixture.inspect(1000).await["sources"], 0);
    drop(bytes);
    let row = jsonl(&[line("clean", "decision", "Valid evidence")]);
    let output = fixture
        .command("--import-sources", &row.repeat(100_001), 1000)
        .await;
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(fixture.inspect(1000).await["sources"], 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn invalid_evidence_is_rejected_line_by_line() {
    let fixture = Fixture::new("project-b");
    let mut bad_origin = line("origin", "decision", "Decision");
    bad_origin["origin"] = json!("bad\norigin");
    let rows = [
        line("action", "action", "{}"),
        line("exchange", "exchange", "not JSON"),
        line("unknown-kind", "unknown", "Evidence"),
        line("empty", "decision", " "),
        bad_origin,
        line("large", "decision", &"x".repeat(8193)),
        line("clean", "decision", "Valid"),
    ];
    let report = fixture.json("--import-sources", &jsonl(&rows), 1000).await;
    assert_eq!(report["inserted"], 1);
    assert_eq!(
        report["rejected"],
        json!([
            "action",
            "exchange",
            "unknown-kind",
            "empty",
            "origin",
            "large"
        ])
    );
}
