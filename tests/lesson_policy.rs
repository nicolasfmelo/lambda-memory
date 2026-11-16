use lambda_memory::domain_models::{
    ActorId, OwnerId, ProjectId, SourceId, capture::*, lessons::*, procedural::*,
};
use lambda_memory::engines::lesson_policy::*;
fn source(n: usize, kind: WorkEventKind) -> CaptureEvent {
    CaptureEvent {
        owner: OwnerId::new("owner").unwrap(),
        actor: ActorId::new("actor").unwrap(),
        project: ProjectId::new("project").unwrap(),
        source: SourceId::new(format!("0123456789abcdef/{n}")).unwrap(),
        origin: "fixture://test".into(),
        recorded_at_ms: n as u64,
        kind,
        outcome: WorkOutcome::Unknown,
        content: String::new(),
    }
}
fn actions() -> Vec<(CaptureEvent, EpisodeRecord)> {
    [
        (WorkOutcome::Failed, "cargo test bad", Some("not found")),
        (WorkOutcome::Succeeded, "cargo test good", None),
    ]
    .into_iter()
    .enumerate()
    .map(|(i, (outcome, summary, error))| {
        (
            source(100 + i, WorkEventKind::Action),
            EpisodeRecord {
                episode: ActionEpisode {
                    tool: "Bash".into(),
                    intent: "test storage".into(),
                    summary: summary.into(),
                    error_head: error.map(str::to_owned),
                    session: "0123456789abcdef".into(),
                    seq: i as u64,
                },
                outcome,
                recorded_at_ms: 100 + i as u64,
            },
        )
    })
    .collect()
}
#[test]
fn lesson_input_bounds() {
    let exchanges = (0..100)
        .map(|i| {
            (
                source(i, WorkEventKind::Exchange),
                format!("Correction {i}: {}", "p".repeat(5000)),
                "r".repeat(10_000),
            )
        })
        .collect();
    let input = build_input(exchanges, actions()).unwrap();
    assert!(input_bytes(&input) <= INPUT_BYTES);
    assert!(input.exchanges.len() < 100);
    assert!(!input.exchanges.is_empty());
    assert_eq!(
        input.sources[input.exchanges.len() - 1].source,
        source(99, WorkEventKind::Exchange).source
    );
    let first_id = input.sources[0].recorded_at_ms as usize;
    assert_eq!(
        input.exchanges.iter().map(|e| e.slot).collect::<Vec<_>>(),
        (0..input.exchanges.len()).collect::<Vec<_>>()
    );
    for (i, e) in input.exchanges.iter().enumerate() {
        assert_eq!(input.sources[i].recorded_at_ms, (first_id + i) as u64);
        assert!(
            e.prompt
                .starts_with(&format!("Correction {}:", first_id + i))
        );
        assert_eq!(e.prompt.chars().count(), 500);
        assert_eq!(e.reply.chars().count(), 1000);
    }
    assert_eq!(input.digest.len(), 1);
    let d = &input.digest[0];
    assert_eq!(d.fixed_by.as_deref(), Some("bash cargo test good"));
    assert_eq!(
        d.evidence,
        vec![input.exchanges.len(), input.exchanges.len() + 1]
    );
    for (i, n) in d.evidence.iter().zip([100, 101]) {
        assert_eq!(input.sources[*i].recorded_at_ms, n);
    }
    // Reply truncation is exhausted before any prompt truncation; oldest first.
    let input = build_input(
        (0..3)
            .map(|i| {
                (
                    source(i, WorkEventKind::Exchange),
                    "p".repeat(1000),
                    "r".repeat(20_000),
                )
            })
            .collect(),
        vec![],
    )
    .unwrap();
    assert_eq!(input.exchanges[0].reply.len(), 1000);
    assert_eq!(input.exchanges[1].reply.len(), 20_000);
    assert!(input.exchanges.iter().all(|e| e.prompt.len() == 1000));
    // Even an oversized final exchange is kept, never discarded.
    let input = build_input(
        vec![(
            source(1, WorkEventKind::Exchange),
            "p".repeat(100_000),
            "r".repeat(100_000),
        )],
        vec![],
    )
    .unwrap();
    assert_eq!(input.exchanges.len(), 1);
    assert_eq!(input.exchanges[0].prompt.len(), 500);
    assert_eq!(input.exchanges[0].reply.len(), 1000);
    assert!(input_bytes(&input) <= INPUT_BYTES);
}
#[test]
fn lesson_validation_checks_all_fields_and_bounds() {
    let input = build_input(
        vec![(
            source(1, WorkEventKind::Exchange),
            "prompt".into(),
            "reply".into(),
        )],
        vec![],
    )
    .unwrap();
    let valid = LessonAnswer {
        tasks: vec![LessonTask {
            title: "Task".into(),
            evidence: vec![0],
            outcome: TaskOutcome::Succeeded,
        }],
        lessons: vec![Lesson {
            kind: LessonKind::Strategy,
            scope: LessonScope::Project,
            title: "Title".into(),
            applies_when: "When testing".into(),
            guidance: "1. Test".into(),
            task: 0,
            evidence: vec![0],
        }],
    };
    assert!(validate(&input, &valid));
    let changes: Vec<fn(&mut LessonAnswer)> = vec![
        |a| a.tasks[0].title = "".into(),
        |a| a.tasks[0].title = "x".repeat(81),
        |a| a.tasks[0].title = "two\nlines".into(),
        |a| a.tasks[0].title = "sk-syntheticfixturekey".into(),
        |a| a.tasks[0].evidence = vec![1],
        |a| a.tasks[0].evidence = vec![0; 17],
        |a| a.tasks = vec![a.tasks[0].clone(); 13],
        |a| a.lessons[0].evidence = vec![],
        |a| a.lessons[0].evidence = vec![0; 9],
        |a| a.lessons[0].task = 1,
        |a| a.lessons[0].title = "x\ny".into(),
        |a| a.lessons[0].title = "".into(),
        |a| a.lessons[0].applies_when = "é".repeat(301),
        |a| a.lessons[0].guidance = " ".into(),
        |a| a.lessons[0].applies_when = "sk-syntheticfixturekey".into(),
        |a| a.lessons[0].title = "sk-syntheticfixturekey".into(),
    ];
    for change in changes {
        let mut a = valid.clone();
        change(&mut a);
        assert!(!validate(&input, &a), "{a:?}");
    }
}
#[test]
fn digest_keeps_unresolved_failures_and_never_treats_unknown_as_a_fix() {
    let mut a = actions();
    a[1].1.outcome = WorkOutcome::Unknown;
    let records = a.into_iter().map(|(_, r)| r).collect::<Vec<_>>();
    let digest = lambda_memory::engines::procedural_policy::session_digest(&records);
    assert_eq!(digest.len(), 1);
    assert!(digest[0].fixed_by.is_none());
    assert_eq!(digest[0].evidence, vec![0]);
}
