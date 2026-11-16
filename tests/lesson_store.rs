#![cfg(feature = "memory-runtime")]
use lambda_memory::domain_models::{
    ActorId, OwnerId, ProjectId, SourceId, capture::*, lessons::*, memory_tree::*, runtime::*,
    session::MemorySession,
};
use lambda_memory::engines::lesson_policy;
use lambda_memory::integrations::database::embedded_capture::{
    EmbeddedCaptureStore, EmbeddedRuntimeStore,
};
use std::sync::atomic::{AtomicUsize, Ordering};
static NEXT: AtomicUsize = AtomicUsize::new(0);
const KEY: &str = "0123456789abcdef";
fn ids() -> (OwnerId, ActorId, ProjectId) {
    (
        OwnerId::new("owner").unwrap(),
        ActorId::new("actor").unwrap(),
        ProjectId::new("project").unwrap(),
    )
}
async fn setup() -> (
    EmbeddedRuntimeStore,
    MemorySession,
    LessonInput,
    LessonAnswer,
) {
    let path = format!(
        "/tmp/lambda-lesson-store-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    );
    let store = EmbeddedRuntimeStore::new(EmbeddedCaptureStore::open(&path).await.unwrap());
    let (owner, actor, project) = ids();
    let event = CaptureEvent {
        owner: owner.clone(),
        actor,
        project: project.clone(),
        source: SourceId::new(format!("{KEY}/turn-1")).unwrap(),
        origin: "fixture://exchange".into(),
        recorded_at_ms: 10,
        kind: WorkEventKind::Exchange,
        outcome: WorkOutcome::Unknown,
        content: r#"{"v":1,"prompt":"Test it","reply":"Tests passed"}"#.into(),
    };
    store
        .capture(event.clone(), &RuntimeLimits::default())
        .await
        .unwrap();
    let session = store
        .sessions(&owner, &project)
        .await
        .unwrap()
        .pop()
        .unwrap();
    let input = lesson_policy::build_input(
        vec![(event, "Test it".into(), "Tests passed".into())],
        vec![],
    )
    .unwrap();
    let answer = LessonAnswer {
        tasks: vec![LessonTask {
            title: "Test storage".into(),
            outcome: TaskOutcome::Succeeded,
            evidence: vec![0],
        }],
        lessons: vec![
            Lesson {
                kind: LessonKind::Strategy,
                scope: LessonScope::Project,
                title: "Test first".into(),
                applies_when: "Changing storage".into(),
                guidance: "1. Run the tests.".into(),
                task: 0,
                evidence: vec![0, 0],
            },
            Lesson {
                kind: LessonKind::Pitfall,
                scope: LessonScope::User,
                title: "Avoid assumptions".into(),
                applies_when: "Receiving requests".into(),
                guidance: "Avoid assumptions.".into(),
                task: 0,
                evidence: vec![0],
            },
        ],
    };
    (store, session, input, answer)
}
#[tokio::test]
async fn importing_a_lesson_records_every_evidence_id_in_the_insert_transaction() {
    let (store, _, input, _) = setup().await;
    let (o, _, p) = ids();
    let mut lesson = input.sources[0].clone();
    lesson.source = SourceId::new(format!("lesson/{KEY}/1")).unwrap();
    lesson.kind = WorkEventKind::Lesson;
    lesson.origin = format!("lesson://{KEY}/1");
    lesson.content=serde_json::json!({"v":1,"kind":"strategy","scope":"project","title":"Test storage","applies_when":"Changing storage","guidance":"1. Test it.","task":"Verify","task_outcome":"succeeded","evidence":[input.sources[0].source.as_str(),"absent"]}).to_string();
    store
        .import_source(lesson.clone(), &RuntimeLimits::default())
        .await
        .unwrap();
    // Present and unresolved evidence IDs are both durable with the inserted source.
    assert_eq!(
        store
            .lesson_citations(&o, &p, &lesson.source)
            .await
            .unwrap(),
        vec![
            input.sources[0].source.clone(),
            SourceId::new("absent").unwrap()
        ]
    );
    assert_eq!(
        store
            .purge(&o, &p, &[input.sources[0].source.clone()])
            .await
            .unwrap()
            .sources,
        2
    );
}

#[tokio::test]
async fn purge_of_cited_source_purges_lesson() {
    let (store, session, input, answer) = setup().await;
    let (o, a, p) = ids();
    store
        .commit_lessons(
            &o,
            &a,
            &p,
            &session,
            &input,
            &answer,
            "lesson-job:test",
            20,
            1000,
        )
        .await
        .unwrap();
    let project_lesson = SourceId::new(format!("lesson/{KEY}/1")).unwrap();
    let user_lesson = SourceId::new(format!("lesson/{KEY}/2")).unwrap();
    assert_eq!(
        store
            .lesson_citations(&o, &p, &project_lesson)
            .await
            .unwrap()
            .len(),
        1,
        "duplicate evidence is one citation pair"
    );
    let plan = CurationCommit {
        links: vec![],
        operation_id: "curation-job:lesson".into(),
        sources: vec![project_lesson.clone()],
        promotes: vec![],
        merges: vec![],
        supersedes: vec![],
        creates: vec![(
            ParentRef::Root(TreeRoot::Decisions),
            "Test first".into(),
            "Run tests before editing".into(),
            vec![project_lesson.clone()],
        )],
        parked: vec![],
        stale: vec![],
        split: vec![],
        operation: LinkOperation::Create,
    };
    let revisions = store.commit_curation(&o, &a, &p, plan, 30).await.unwrap();
    assert_eq!(revisions.len(), 1);
    let receipt = store
        .purge(&o, &p, &[input.sources[0].source.clone()])
        .await
        .unwrap();
    assert_eq!(receipt.sources, 3);
    assert_eq!(receipt.revisions, 1);
    assert!(store.export_sources(&o, &p).await.unwrap().is_empty());
    assert!(
        store
            .lesson_citations(&o, &p, &project_lesson)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        store
            .lesson_citations(&o, &p, &user_lesson)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(store.tree_nodes(&o, &p).await.unwrap().is_empty());
    let forwards = store.forwards(&o, &p, 32).await.unwrap();
    assert_eq!(forwards.len(), 1);
    assert_eq!(forwards[0].action, "purge");
    assert_eq!(forwards[0].source_id, user_lesson);
}
#[tokio::test]
async fn purging_a_lesson_removes_citations_but_not_evidence() {
    let (store, session, input, answer) = setup().await;
    let (o, a, p) = ids();
    store
        .commit_lessons(
            &o,
            &a,
            &p,
            &session,
            &input,
            &answer,
            "lesson-job:test",
            20,
            1000,
        )
        .await
        .unwrap();
    let lesson = SourceId::new(format!("lesson/{KEY}/1")).unwrap();
    assert_eq!(
        store
            .purge(&o, &p, std::slice::from_ref(&lesson))
            .await
            .unwrap()
            .sources,
        1
    );
    assert!(
        store
            .lesson_citations(&o, &p, &lesson)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(store.export_sources(&o, &p).await.unwrap().len(), 2);
}
#[tokio::test]
async fn stale_session_or_purged_input_commits_nothing() {
    let (store, session, input, answer) = setup().await;
    let (o, a, p) = ids();
    store
        .mark_extracted(&o, &p, &session.session_key, 10)
        .await
        .unwrap();
    assert_eq!(
        store
            .commit_lessons(
                &o,
                &a,
                &p,
                &session,
                &input,
                &answer,
                "lesson-job:test",
                20,
                1000
            )
            .await,
        Err(RuntimeError::Conflict)
    );
    assert_eq!(store.export_sources(&o, &p).await.unwrap().len(), 1);
    assert!(store.forwards(&o, &p, 32).await.unwrap().is_empty());
    store
        .purge(&o, &p, &[input.sources[0].source.clone()])
        .await
        .unwrap();
    assert_eq!(
        store
            .commit_lessons(
                &o,
                &a,
                &p,
                &session,
                &input,
                &answer,
                "lesson-job:test",
                20,
                1000
            )
            .await,
        Err(RuntimeError::Missing)
    );
    assert!(store.export_sources(&o, &p).await.unwrap().is_empty());
}
#[tokio::test]
async fn occupied_ids_are_skipped_even_for_nonlesson_sources() {
    let (store, session, input, answer) = setup().await;
    let (o, a, p) = ids();
    let mut collision = input.sources[0].clone();
    collision.source = SourceId::new(format!("lesson/{KEY}/2")).unwrap();
    collision.kind = WorkEventKind::Decision;
    store
        .capture(collision, &RuntimeLimits::default())
        .await
        .unwrap();
    assert_eq!(
        store
            .commit_lessons(
                &o,
                &a,
                &p,
                &session,
                &input,
                &answer,
                "lesson-job:test",
                20,
                1000
            )
            .await
            .unwrap(),
        2
    );
    let rows = store.export_sources(&o, &p).await.unwrap();
    assert_eq!(rows.len(), 4);
    assert!(
        rows.iter()
            .any(|row| row["source"] == format!("lesson/{KEY}/3") && row["kind"] == "lesson")
    );
    assert!(
        rows.iter()
            .any(|row| row["source"] == format!("lesson/{KEY}/4") && row["kind"] == "lesson")
    );
    assert_eq!(
        store.sessions(&o, &p).await.unwrap()[0].extracted_through_ms,
        Some(10)
    );
    assert_eq!(store.forwards(&o, &p, 32).await.unwrap().len(), 1);
}

#[tokio::test]
async fn activity_during_extraction_keeps_the_new_part_ready() {
    let (store, session, input, answer) = setup().await;
    let (o, a, p) = ids();
    let mut next = input.sources[0].clone();
    next.source = SourceId::new(format!("{KEY}/turn-2")).unwrap();
    next.recorded_at_ms = 11;
    store
        .capture(next, &RuntimeLimits::default())
        .await
        .unwrap();
    store
        .commit_lessons(
            &o,
            &a,
            &p,
            &session,
            &input,
            &answer,
            "lesson-job:test",
            20,
            1000,
        )
        .await
        .unwrap();
    let updated = store.sessions(&o, &p).await.unwrap().pop().unwrap();
    assert_eq!(updated.extracted_through_ms, Some(10));
    assert_eq!(updated.last_activity_ms, 11);
    assert_eq!(
        store.session_sources(&o, &p, &updated).await.unwrap().len(),
        1
    );
}

#[tokio::test]
async fn lesson_capacity_is_rechecked_in_commit() {
    let (store, session, input, answer) = setup().await;
    let (o, a, p) = ids();
    assert_eq!(
        store
            .commit_lessons(
                &o,
                &a,
                &p,
                &session,
                &input,
                &answer,
                "lesson-job:test",
                20,
                1
            )
            .await,
        Err(RuntimeError::BacklogFull)
    );
    assert_eq!(store.export_sources(&o, &p).await.unwrap().len(), 1);
    assert!(
        store.sessions(&o, &p).await.unwrap()[0]
            .extracted_through_ms
            .is_none()
    );
    assert!(store.forwards(&o, &p, 32).await.unwrap().is_empty());
}
