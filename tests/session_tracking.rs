#![cfg(feature = "memory-runtime")]
use lambda_memory::domain_models::{ActorId, OwnerId, ProjectId, SourceId};
use lambda_memory::domain_models::{capture::*, runtime::*, session::*};
use lambda_memory::engines::session_policy;
use lambda_memory::integrations::database::embedded_capture::{
    EmbeddedCaptureStore, EmbeddedRuntimeStore,
};
use sha2::{Digest, Sha256};
use std::sync::atomic::{AtomicUsize, Ordering};
static NEXT: AtomicUsize = AtomicUsize::new(0);
const KEY: &str = "0123456789abcdef";
const OTHER: &str = "fedcba9876543210";
fn path() -> String {
    format!(
        "/tmp/lambda-session-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}
fn owner() -> OwnerId {
    OwnerId::new("owner").unwrap()
}
fn project() -> ProjectId {
    ProjectId::new("project").unwrap()
}
async fn store() -> EmbeddedRuntimeStore {
    EmbeddedRuntimeStore::new(EmbeddedCaptureStore::open(&path()).await.unwrap())
}
fn event(key: &str, suffix: &str, at: u64, kind: WorkEventKind) -> CaptureEvent {
    CaptureEvent {
        owner: owner(),
        project: project(),
        actor: ActorId::new("actor").unwrap(),
        source: SourceId::new(format!("{key}/{suffix}")).unwrap(),
        origin: "fixture://session".into(),
        recorded_at_ms: at,
        kind,
        outcome: WorkOutcome::Unknown,
        content: "session evidence".into(),
    }
}
async fn capture(
    store: &EmbeddedRuntimeStore,
    key: &str,
    suffix: &str,
    at: u64,
    kind: WorkEventKind,
) {
    store
        .capture(event(key, suffix, at, kind), &RuntimeLimits::default())
        .await
        .unwrap();
}
async fn ready(store: &EmbeddedRuntimeStore, now: u64, limit: usize) -> Vec<MemorySession> {
    let rows = store
        .ready_sessions(&owner(), &project(), now, limit, 60_000)
        .await
        .unwrap();
    for row in &rows {
        assert!(session_policy::ready(row, now, 60_000));
    }
    rows
}
#[test]
fn session_key_parsing() {
    for source in [
        format!("{KEY}/turn-0123456789abcdef"),
        format!("{KEY}/0123456789abcdef"),
    ] {
        assert_eq!(session_policy::session_key(&source).unwrap().as_str(), KEY);
    }
    for source in [
        "promote/0123456789abcdef/x",
        "lesson://0123456789abcdef/1",
        "0123456789abcdeF/turn-x",
        "0123456789abcde/x",
        "0123456789abcdef",
        "éééééééé/x",
    ] {
        assert!(session_policy::session_key(source).is_none(), "{source}");
    }
}
#[tokio::test]
async fn capture_updates_session_activity() {
    let store = store().await;
    capture(&store, KEY, "turn-a", 200, WorkEventKind::Exchange).await;
    capture(&store, KEY, "turn-b", 100, WorkEventKind::Exchange).await;
    capture(&store, KEY, "action", 300, WorkEventKind::Action).await;
    capture(&store, KEY, "turn-a", 900, WorkEventKind::Exchange).await;
    capture(&store, OTHER, "decision", 1000, WorkEventKind::Decision).await;
    let mut conflict = event(KEY, "turn-a", 1000, WorkEventKind::Exchange);
    conflict.content = "different".into();
    assert_eq!(
        store.capture(conflict, &RuntimeLimits::default()).await,
        Err(RuntimeError::Conflict)
    );
    let rows = store.sessions(&owner(), &project()).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(
        (
            rows[0].first_activity_ms,
            rows[0].last_activity_ms,
            rows[0].exchanges
        ),
        (100, 300, 2)
    );
    assert!(!rows[0].historical);
    assert_eq!(rows[0].live_activity_ms, Some(300));
    assert_eq!(rows[0].extracted_through_ms, None);
    assert!(
        store
            .sessions(&OwnerId::new("foreign").unwrap(), &project())
            .await
            .unwrap()
            .is_empty()
    );
}
#[tokio::test]
async fn import_updates_session_with_historical_time() {
    let store = store().await;
    store
        .import_source(
            event(KEY, "turn-a", 200, WorkEventKind::Exchange),
            &RuntimeLimits::default(),
        )
        .await
        .unwrap();
    store
        .import_source(
            event(KEY, "action", 100, WorkEventKind::Action),
            &RuntimeLimits::default(),
        )
        .await
        .unwrap();
    store
        .import_source(
            event(KEY, "turn-b", 300, WorkEventKind::Exchange),
            &RuntimeLimits::default(),
        )
        .await
        .unwrap();
    let rows = store.sessions(&owner(), &project()).await.unwrap();
    assert_eq!(
        (
            rows[0].first_activity_ms,
            rows[0].last_activity_ms,
            rows[0].exchanges
        ),
        (100, 300, 2)
    );
    assert!(rows[0].historical);
    assert_eq!(rows[0].live_activity_ms, None);
    assert_eq!(rows[0].extracted_through_ms, Some(300));
}
#[tokio::test]
async fn session_end_marks_existing_session() {
    let store = store().await;
    let raw = "raw harness session / ç";
    let hash = format!("{:x}", Sha256::digest(raw.as_bytes()));
    let key = &hash[..16];
    capture(&store, key, "turn-a", 100, WorkEventKind::Exchange).await;
    let result = store
        .session_end(&owner(), &project(), raw, 200)
        .await
        .unwrap();
    assert!(result.ended);
    assert_eq!(result.session_key.as_str(), key);
    assert!(
        store
            .session_end(&owner(), &project(), raw, 300)
            .await
            .unwrap()
            .ended
    );
    assert_eq!(
        store.sessions(&owner(), &project()).await.unwrap()[0].ended_at_ms,
        Some(200)
    );
    capture(&store, key, "turn-b", 400, WorkEventKind::Exchange).await;
    assert!(ready(&store, 401, 10).await.is_empty());
    store
        .session_end(&owner(), &project(), raw, 500)
        .await
        .unwrap();
    assert_eq!(ready(&store, 500, 10).await.len(), 1);
    assert!(
        !store
            .session_end(&owner(), &project(), "unknown", 600)
            .await
            .unwrap()
            .ended
    );
    assert_eq!(store.sessions(&owner(), &project()).await.unwrap().len(), 1);
    for invalid in ["".into(), "x\n".into(), "a".repeat(513)] {
        assert!(
            store
                .session_end(&owner(), &project(), &invalid, 600)
                .await
                .is_err()
        );
    }
}
#[tokio::test]
async fn historical_sessions_are_not_ready() {
    let store = store().await;
    let mut imported = event(KEY, "turn-a", 100, WorkEventKind::Exchange);
    imported.origin = "import://claude-code".into();
    store
        .capture(imported, &RuntimeLimits::default())
        .await
        .unwrap();
    store
        .import_source(
            event(OTHER, "turn-a", 200, WorkEventKind::Exchange),
            &RuntimeLimits::default(),
        )
        .await
        .unwrap();
    assert!(ready(&store, 100_000, 10).await.is_empty());
    capture(&store, KEY, "turn-live", 300, WorkEventKind::Exchange).await;
    let rows = ready(&store, 100_000, 10).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].session_key.as_str(), KEY);
    assert_eq!(rows[0].extracted_through_ms, Some(100));
    assert!(!rows[0].historical);
    // A subsequent operator import must not swallow this unextracted live part.
    store
        .import_source(
            event(KEY, "older-action", 150, WorkEventKind::Action),
            &RuntimeLimits::default(),
        )
        .await
        .unwrap();
    assert_eq!(
        ready(&store, 100_000, 10).await[0].extracted_through_ms,
        Some(100)
    );
}
#[tokio::test]
async fn historical_inserts_do_not_wake_extracted_live_sessions() {
    let store = store().await;
    capture(&store, KEY, "turn-live", 100, WorkEventKind::Exchange).await;
    store
        .mark_extracted(&owner(), &project(), &SessionKey::new(KEY).unwrap(), 100)
        .await
        .unwrap();
    store
        .import_source(
            event(KEY, "turn-imported", 200, WorkEventKind::Exchange),
            &RuntimeLimits::default(),
        )
        .await
        .unwrap();
    let mut imported = event(KEY, "imported-action", 300, WorkEventKind::Action);
    imported.origin = "import://claude-code".into();
    store
        .capture(imported, &RuntimeLimits::default())
        .await
        .unwrap();
    let row = store
        .sessions(&owner(), &project())
        .await
        .unwrap()
        .remove(0);
    assert_eq!(
        (row.first_activity_ms, row.last_activity_ms, row.exchanges),
        (100, 300, 2)
    );
    assert_eq!(row.live_activity_ms, Some(100));
    assert!(!session_policy::ready(&row, 100_000, 60_000));
    assert!(ready(&store, 100_000, 10).await.is_empty());
    assert!(
        store
            .inspect_lesson_backfill(&owner(), &project())
            .await
            .unwrap()
            .sessions
            .is_empty()
    );
    capture(&store, KEY, "new-live-action", 400, WorkEventKind::Action).await;
    assert_eq!(
        ready(&store, 100_000, 10).await[0].live_activity_ms,
        Some(400)
    );
}

#[tokio::test]
async fn purge_and_retention_recompute_session_activity() {
    let store = store().await;
    capture(&store, KEY, "turn-first", 100, WorkEventKind::Exchange).await;
    capture(&store, KEY, "middle-action", 200, WorkEventKind::Action).await;
    capture(&store, KEY, "turn-last", 300, WorkEventKind::Exchange).await;
    capture(&store, KEY, "last-action", 400, WorkEventKind::Action).await;
    // Non-activity sources do not keep an otherwise empty session alive.
    capture(&store, KEY, "decision", 500, WorkEventKind::Decision).await;
    let purge = |suffix: &str| SourceId::new(format!("{KEY}/{suffix}")).unwrap();
    store
        .purge(
            &owner(),
            &project(),
            &[purge("turn-first"), purge("turn-last")],
        )
        .await
        .unwrap();
    let row = store
        .sessions(&owner(), &project())
        .await
        .unwrap()
        .remove(0);
    assert_eq!(
        (row.first_activity_ms, row.last_activity_ms, row.exchanges),
        (200, 400, 0)
    );
    assert!(ready(&store, 100_000, 10).await.is_empty());
    store
        .purge(&owner(), &project(), &[purge("last-action")])
        .await
        .unwrap();
    let row = store
        .sessions(&owner(), &project())
        .await
        .unwrap()
        .remove(0);
    assert_eq!(
        (row.first_activity_ms, row.last_activity_ms, row.exchanges),
        (200, 200, 0)
    );
    store
        .purge(&owner(), &project(), &[purge("middle-action")])
        .await
        .unwrap();
    assert!(
        store
            .sessions(&owner(), &project())
            .await
            .unwrap()
            .is_empty()
    );

    // Retention uses the same transactional repair, including historical backfill candidates.
    store
        .import_source(
            event(OTHER, "turn-old", 100, WorkEventKind::Exchange),
            &RuntimeLimits::default(),
        )
        .await
        .unwrap();
    store
        .import_source(
            event(OTHER, "remaining-action", 300, WorkEventKind::Action),
            &RuntimeLimits::default(),
        )
        .await
        .unwrap();
    assert_eq!(
        store
            .inspect_lesson_backfill(&owner(), &project())
            .await
            .unwrap()
            .sessions
            .len(),
        1
    );
    assert_eq!(
        store
            .retain(&owner(), &project(), 200, 10)
            .await
            .unwrap()
            .sources,
        1
    );
    let row = store
        .sessions(&owner(), &project())
        .await
        .unwrap()
        .remove(0);
    assert_eq!(
        (row.first_activity_ms, row.last_activity_ms, row.exchanges),
        (300, 300, 0)
    );
    assert!(
        store
            .inspect_lesson_backfill(&owner(), &project())
            .await
            .unwrap()
            .sessions
            .is_empty()
    );
    assert!(ready(&store, 100_000, 10).await.is_empty());
    assert_eq!(
        store
            .retain(&owner(), &project(), 400, 10)
            .await
            .unwrap()
            .sources,
        1
    );
    assert!(
        store
            .sessions(&owner(), &project())
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn ready_sessions_rules() {
    let store = store().await;
    capture(&store, KEY, "turn-a", 100, WorkEventKind::Exchange).await;
    capture(&store, OTHER, "turn-a", 200, WorkEventKind::Exchange).await;
    capture(
        &store,
        "aaaaaaaaaaaaaaaa",
        "action",
        1,
        WorkEventKind::Action,
    )
    .await;
    assert!(ready(&store, 60_099, 10).await.is_empty());
    assert_eq!(ready(&store, 60_100, 10).await.len(), 1);
    let rows = ready(&store, 60_200, 10).await;
    assert_eq!(
        rows.iter()
            .map(|r| r.session_key.as_str())
            .collect::<Vec<_>>(),
        [KEY, OTHER]
    );
    assert_eq!(ready(&store, 60_200, 1).await[0].session_key.as_str(), KEY);
    assert!(ready(&store, 60_200, 0).await.is_empty());
    store
        .mark_extracted(&owner(), &project(), &SessionKey::new(KEY).unwrap(), 100)
        .await
        .unwrap();
    assert_eq!(ready(&store, 60_200, 10).await.len(), 1);
    capture(&store, KEY, "action-b", 300, WorkEventKind::Action).await;
    assert_eq!(ready(&store, 60_200, 10).await.len(), 1);
    assert_eq!(ready(&store, 60_300, 10).await.len(), 2);
    // Pure rule: future activity is not idle, stale end is not a signal, zero exchanges never ready.
    let mut row = store
        .sessions(&owner(), &project())
        .await
        .unwrap()
        .into_iter()
        .find(|r| r.session_key.as_str() == KEY)
        .unwrap();
    assert!(!session_policy::ready(&row, 0, 60_000));
    row.ended_at_ms = Some(299);
    assert!(!session_policy::ready(&row, 301, 60_000));
    row.ended_at_ms = Some(300);
    assert!(session_policy::ready(&row, 301, 60_000));
    row.exchanges = 0;
    assert!(!session_policy::ready(&row, 100_000, 60_000));
}

#[tokio::test]
async fn lesson_backfill_requires_consent_and_makes_sessions_ready() {
    use std::io::Write;
    use std::process::{Command, Stdio};
    use surrealdb::{Surreal, engine::local::SurrealKv};
    let path = path();
    let store = EmbeddedRuntimeStore::new(EmbeddedCaptureStore::open(&path).await.unwrap());
    store
        .import_source(
            event(KEY, "turn-a", 100, WorkEventKind::Exchange),
            &RuntimeLimits::default(),
        )
        .await
        .unwrap();
    capture(
        &store,
        "aaaaaaaaaaaaaaaa",
        "turn-live",
        400,
        WorkEventKind::Exchange,
    )
    .await;
    capture(
        &store,
        "bbbbbbbbbbbbbbbb",
        "turn-done",
        400,
        WorkEventKind::Exchange,
    )
    .await;
    store
        .mark_extracted(
            &owner(),
            &project(),
            &SessionKey::new("bbbbbbbbbbbbbbbb").unwrap(),
            400,
        )
        .await
        .unwrap();
    let mut stale = store
        .inspect_lesson_backfill(&owner(), &project())
        .await
        .unwrap();
    stale.sessions.clear();
    assert!(
        store
            .lesson_backfill(&owner(), &project(), &stale)
            .await
            .is_err()
    );
    drop(store);
    // Simulate pre-migration sources with no bookkeeping row, in a separate scoped writer.
    let mut db = None;
    for _ in 0..30 {
        match Surreal::new::<SurrealKv>(&path).await {
            Ok(open) => {
                db = Some(open);
                break;
            }
            Err(_) => tokio::time::sleep(std::time::Duration::from_millis(50)).await,
        }
    }
    let db = db.unwrap();
    db.use_ns("lambda_memory")
        .use_db("experiment")
        .await
        .unwrap();
    db.query("CREATE memory_source SET owner_id='owner', project_id='project', actor_id='actor', source_id='fedcba9876543210/turn-old', origin='fixture://old', recorded_at_ms=50, kind='exchange', outcome='unknown', content='historical exchange';").await.unwrap().check().unwrap();
    drop(db);
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    let run = |flag: &str, input: Option<&serde_json::Value>| {
        let mut child = Command::new(env!("CARGO_BIN_EXE_lambda-memory"))
            .arg(flag)
            .env("LAMBDA_MEMORY_DATA_DIR", &path)
            .env("LAMBDA_MEMORY_OWNER", "owner")
            .env("LAMBDA_MEMORY_ACTOR", "actor")
            .env("LAMBDA_MEMORY_PROJECT", "project")
            .env("LAMBDA_MEMORY_TOKEN", "operator-token-32-bytes-minimum!!")
            .env_remove("LAMBDA_MEMORY_ALLOWED_PROJECTS")
            .env("LAMBDA_MEMORY_MODE", "local")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        if let Some(input) = input {
            child
                .stdin
                .take()
                .unwrap()
                .write_all(input.to_string().as_bytes())
                .unwrap();
        }
        drop(child.stdin.take());
        child.wait_with_output().unwrap()
    };
    let output = run("--inspect-lesson-backfill", None);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let mut plan: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        plan,
        serde_json::json!({"project":"project","sessions":[KEY, OTHER],"estimated_calls":2,"accept_charges":false})
    );
    assert!(!run("--lesson-backfill", Some(&plan)).status.success());
    plan["accept_charges"] = serde_json::json!(true);
    let mut wrong = plan.clone();
    wrong["estimated_calls"] = serde_json::json!(1);
    let output = run("--lesson-backfill", Some(&wrong));
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("exact lesson backfill consent required")
    );
    let output = run("--lesson-backfill", Some(&plan));
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap(),
        serde_json::json!({"project":"project","sessions":2})
    );
    assert!(!run("--lesson-backfill", Some(&plan)).status.success());
    let store = EmbeddedRuntimeStore::new(EmbeddedCaptureStore::open(&path).await.unwrap());
    let rows = ready(&store, 500, 10).await;
    assert_eq!(
        rows.iter()
            .map(|r| r.session_key.as_str())
            .collect::<Vec<_>>(),
        [OTHER, KEY]
    );
    assert!(rows.iter().all(|r| !r.historical
        && r.live_activity_ms == Some(r.last_activity_ms)
        && r.extracted_through_ms.is_none()
        && r.ended_at_ms == Some(r.last_activity_ms)));
    assert!(
        store
            .inspect_lesson_backfill(&owner(), &project())
            .await
            .unwrap()
            .sessions
            .is_empty()
    );
}

#[tokio::test]
async fn purge_newest_live_does_not_wake_imported_content() {
    let store = store().await;
    capture(&store, KEY, "live", 100, WorkEventKind::Exchange).await;
    store
        .mark_extracted(&owner(), &project(), &SessionKey::new(KEY).unwrap(), 100)
        .await
        .unwrap();
    let mut imported = event(KEY, "imported", 150, WorkEventKind::Exchange);
    imported.origin = "import://history".into();
    store
        .capture(imported, &RuntimeLimits::default())
        .await
        .unwrap();
    capture(&store, KEY, "action", 200, WorkEventKind::Action).await;
    store
        .purge(
            &owner(),
            &project(),
            &[SourceId::new(format!("{KEY}/action")).unwrap()],
        )
        .await
        .unwrap();
    assert!(ready(&store, 100_000, 10).await.is_empty());
    capture(&store, KEY, "new", 300, WorkEventKind::Exchange).await;
    assert_eq!(
        ready(&store, 100_000, 10).await[0].live_activity_ms,
        Some(300)
    );
}

#[tokio::test]
async fn purge_newest_live_keeps_remaining_unextracted_work_ready() {
    let store = store().await;
    let raw = "session for purge";
    let hash = format!("{:x}", Sha256::digest(raw.as_bytes()));
    let key = &hash[..16];
    capture(&store, key, "one", 100, WorkEventKind::Exchange).await;
    capture(&store, key, "two", 190, WorkEventKind::Exchange).await;
    capture(&store, key, "action", 200, WorkEventKind::Action).await;
    store
        .session_end(&owner(), &project(), raw, 250)
        .await
        .unwrap();
    store
        .purge(
            &owner(),
            &project(),
            &[SourceId::new(format!("{key}/action")).unwrap()],
        )
        .await
        .unwrap();
    assert_eq!(ready(&store, 250, 10).await[0].live_activity_ms, Some(190));
}

#[tokio::test]
async fn purge_backfilled_import_restores_consent_requirement() {
    let store = store().await;
    for (suffix, at) in [("one", 100), ("two", 200)] {
        let mut imported = event(KEY, suffix, at, WorkEventKind::Exchange);
        imported.origin = "import://history".into();
        store
            .capture(imported, &RuntimeLimits::default())
            .await
            .unwrap();
    }
    let plan = store
        .inspect_lesson_backfill(&owner(), &project())
        .await
        .unwrap();
    store
        .lesson_backfill(&owner(), &project(), &plan)
        .await
        .unwrap();
    assert_eq!(ready(&store, 100_000, 10).await.len(), 1);
    store
        .purge(
            &owner(),
            &project(),
            &[SourceId::new(format!("{KEY}/two")).unwrap()],
        )
        .await
        .unwrap();
    assert!(ready(&store, 100_000, 10).await.is_empty());
    assert_eq!(
        store
            .inspect_lesson_backfill(&owner(), &project())
            .await
            .unwrap()
            .sessions,
        plan.sessions
    );
}

#[tokio::test]
async fn partial_purge_does_not_wake_an_extracted_session() {
    const KEY: &str = "1111111111111111";
    let store = store().await;
    capture(&store, KEY, "turn-1", 100, WorkEventKind::Exchange).await;
    store
        .mark_extracted(&owner(), &project(), &SessionKey::new(KEY).unwrap(), 100)
        .await
        .unwrap();
    capture(&store, KEY, "late-action", 200, WorkEventKind::Action).await;
    store
        .purge(
            &owner(),
            &project(),
            &[SourceId::new(format!("{KEY}/late-action")).unwrap()],
        )
        .await
        .unwrap();
    assert!(ready(&store, 10_000_000, 10).await.is_empty());
}
