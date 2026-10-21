use lambda_memory::domain_models::*;
use lambda_memory::engines::recall_policy::{select, select_supported};
fn fixture() -> (RecallRequest, Memory, SupportedSelectionOutcome) {
    let owner = OwnerId::new("owner").unwrap();
    let actor = ActorId::new("actor").unwrap();
    let project = ProjectId::new("project").unwrap();
    let request = RecallRequest {
        task: "Atlas production retention?".into(),
        owner: owner.clone(),
        actor: actor.clone(),
        project: project.clone(),
        budget_bytes: 1000,
    };
    let memory = Memory {
        id: MemoryId::new("memory").unwrap(),
        owner,
        actor,
        project,
        text: "Atlas production retention is 14 days.".into(),
        source: SourceId::new("source").unwrap(),
        active: true,
        restricted: false,
    };
    let outcome = SelectionOutcome {
        best: Some(memory.id.clone()),
        confidence: 0.9,
        candidates: vec![CandidateRelevance {
            memory_id: memory.id.clone(),
            relevance_probability: 0.99,
            utility_score: 1.8,
            confidence: 0.63,
        }],
    };
    let support = std::collections::BTreeMap::from([(memory.id.clone(), 0.95)]);
    (
        request,
        memory,
        SupportedSelectionOutcome {
            outcome,
            direct_support: support,
        },
    )
}
#[test]
fn auxiliary_score_confidence_cannot_veto_strong_explicit_support() {
    let (r, m, o) = fixture();
    assert!(select(&r, std::slice::from_ref(&m), &o.outcome).is_empty());
    assert_eq!(select_supported(&r, &[m], &o).len(), 1);
}
#[test]
fn hard_negative_support_and_existing_cuts_still_reject() {
    let (r, m, o) = fixture();
    for p in [0.0, 0.89, f64::NAN] {
        let mut changed = o.clone();
        changed.direct_support.insert(m.id.clone(), p);
        assert!(select_supported(&r, std::slice::from_ref(&m), &changed).is_empty());
    }
    let mut low = o.clone();
    low.outcome.confidence = 0.69;
    assert!(select_supported(&r, std::slice::from_ref(&m), &low).is_empty());
    low = o.clone();
    low.outcome.candidates[0].relevance_probability = 0.79;
    assert!(select_supported(&r, std::slice::from_ref(&m), &low).is_empty());
    low = o.clone();
    low.outcome.candidates[0].utility_score = 1.49;
    assert!(select_supported(&r, std::slice::from_ref(&m), &low).is_empty());
    let mut wrong = m.clone();
    wrong.project = ProjectId::new("foreign").unwrap();
    assert!(select_supported(&r, &[wrong], &o).is_empty());
}
#[test]
fn abstention_duplicate_and_missing_support_fail_closed() {
    let (r, m, o) = fixture();
    let mut none = o.clone();
    none.outcome.best = None;
    assert!(select_supported(&r, std::slice::from_ref(&m), &none).is_empty());
    let mut absent = o.clone();
    absent.direct_support.clear();
    assert!(select_supported(&r, std::slice::from_ref(&m), &absent).is_empty());
    assert!(select_supported(&r, &[m.clone(), m.clone()], &o).is_empty());
}
#[test]
fn valid_capture_can_be_expired_at_recall() {
    use lambda_memory::domain_models::lifecycle::*;
    use lambda_memory::engines::lifecycle_policy::*;
    let p = SourceLifecycle {
        owner: OwnerId::new("owner").unwrap(),
        project: ProjectId::new("project").unwrap(),
        source: SourceId::new("source").unwrap(),
        authoritative: true,
        expires_at_ms: Some(50),
    };
    assert_eq!(admissible_at_capture(&p, 10, 20, None), Ok(()));
    assert!(unexpired(&p, 30));
    assert!(!unexpired(&p, 50));
    assert_eq!(
        admissible_at_capture(&p, 10, 50, None),
        Err(LifecycleRejection::Expired)
    );
}

#[test]
fn verified_context_rejects_time_scope_and_cardinality_mismatches() {
    use lambda_memory::domain_models::selection_context::*;
    use lambda_memory::engines::recall_policy::validate_selection_context;
    let (r, m, _) = fixture();
    let request = SelectionRequest {
        task: r.task,
        retrieval_terms: vec![],
        candidates: vec![m.clone()],
    };
    let context = SelectionContext {
        intent: SelectionIntent::FactualQuestion,
        owner: m.owner.clone(),
        project: m.project.clone(),
        as_of_ms: 30,
        validity: vec![CandidateValidity {
            memory: m.id.clone(),
            source: m.source.clone(),
            owner: m.owner.clone(),
            project: m.project.clone(),
            valid_from_ms: 10,
            expires_at_ms: Some(50),
        }],
    };
    assert!(validate_selection_context(&request, &context).is_ok());
    for time in [5, 50, 100] {
        let mut changed = context.clone();
        changed.as_of_ms = time;
        assert!(validate_selection_context(&request, &changed).is_err());
    }
    let mut changed = context.clone();
    changed.owner = OwnerId::new("other").unwrap();
    assert!(validate_selection_context(&request, &changed).is_err());
    changed = context.clone();
    changed.validity[0].source = SourceId::new("other").unwrap();
    assert!(validate_selection_context(&request, &changed).is_err());
    changed = context.clone();
    changed.validity.push(changed.validity[0].clone());
    assert!(validate_selection_context(&request, &changed).is_err());
    changed = context;
    changed.validity.clear();
    assert!(validate_selection_context(&request, &changed).is_err());
}

#[test]
fn complementary_evidence_is_selected_without_widening_project_or_support() {
    let (request, best, mut answer) = fixture();
    let mut second = best.clone();
    second.id = MemoryId::new("second").unwrap();
    second.source = SourceId::new("second-source").unwrap();
    second.text = "Atlas production backup retention is 30 days.".into();
    let mut foreign = second.clone();
    foreign.id = MemoryId::new("foreign").unwrap();
    foreign.project = ProjectId::new("global").unwrap();
    let mut unsupported = second.clone();
    unsupported.id = MemoryId::new("unsupported").unwrap();
    for memory in [&second, &foreign, &unsupported] {
        answer.outcome.candidates.push(CandidateRelevance {
            memory_id: memory.id.clone(),
            relevance_probability: 0.99,
            utility_score: 1.9,
            confidence: 0.8,
        });
        answer.direct_support.insert(memory.id.clone(), 0.95);
    }
    answer.direct_support.insert(unsupported.id.clone(), 0.89);
    let selected = select_supported(
        &request,
        &[second.clone(), foreign, best.clone(), unsupported],
        &answer,
    );
    assert_eq!(
        selected.iter().map(|memory| &memory.id).collect::<Vec<_>>(),
        vec![&best.id, &second.id]
    );
    assert!(
        selected
            .iter()
            .all(|memory| memory.project == request.project)
    );
}
