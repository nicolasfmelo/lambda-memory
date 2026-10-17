use lambda_memory::delivery::recall_handler::{RecallCommand, handle};
use lambda_memory::domain_models::*;
use lambda_memory::engines::recall_policy::{pack, scope_candidates, select};
use lambda_memory::integrations::database::mock_memory_repository::{
    MockMemoryRepository, fixture,
};
use lambda_memory::integrations::external_apis::{
    mock_luna_decisions_client::MockLunaDecisionsClient, mock_query_planner::MockQueryPlanner,
};
use lambda_memory::integrations::memory_ports::{
    MemoryQueryPlanner, MemoryRepository, MemorySelector,
};
use lambda_memory::integrations::provider_config::ProviderConfig;
use lambda_memory::services::recall_service::RecallService;
mod support;
use support::ready_mock;
fn input(task: &str) -> RecallRequest {
    RecallRequest {
        task: task.into(),
        owner: OwnerId::new("fixture-owner").unwrap(),
        actor: ActorId::new("fixture-actor").unwrap(),
        project: ProjectId::new("demo-project").unwrap(),
        budget_tokens: 500,
    }
}
fn service() -> RecallService<MockMemoryRepository, MockLunaDecisionsClient, MockQueryPlanner> {
    RecallService::new(
        MockMemoryRepository::new(fixture()),
        MockLunaDecisionsClient::new(ProviderConfig::default()),
        MockQueryPlanner::new(false),
        RecallPolicy::default(),
    )
}
#[test]
fn scoped_recall_with_sources() {
    let out = ready_mock(service().recall(input("memory storage"))).unwrap();
    assert!(out.evidence.selected.iter().any(|m| m.id.as_str() == "db"));
    assert!(
        !out.candidate_ids
            .iter()
            .any(|id| ["other", "old", "restricted"].contains(&id.as_str()))
    );
    assert!(out.evidence.context.contains("fixture://db"));
    assert!(out.evidence.context.contains("Project: demo-project"));
    assert!(out.evidence.context.len() <= 500);
}
#[test]
fn owner_isolation() {
    let mut request = input("memory");
    request.owner = OwnerId::new("different-owner").unwrap();
    assert!(
        ready_mock(service().recall(request))
            .unwrap()
            .evidence
            .context
            .is_empty()
    );
}
#[test]
fn actor_provenance_is_not_the_owner_boundary() {
    let mut request = input("memory");
    request.actor = ActorId::new("another-authorized-actor").unwrap();
    assert!(
        !ready_mock(service().recall(request))
            .unwrap()
            .evidence
            .context
            .is_empty()
    );
    // Authentication/actor grants are not implemented by these offline fixtures.
}
#[test]
fn budget_and_negative() {
    let mut request = input("memory");
    request.budget_tokens = 0;
    assert!(
        ready_mock(service().recall(request))
            .unwrap()
            .evidence
            .context
            .is_empty()
    );
    assert!(
        ready_mock(service().recall(input("banana recipe")))
            .unwrap()
            .evidence
            .context
            .is_empty()
    );
}
#[test]
fn refusals_and_nonfinite_confidence() {
    let request = input("memory");
    let candidates = fixture();
    assert!(
        select(
            &request,
            &candidates,
            &SelectionOutcome {
                best: None,
                confidence: 1.0,
                candidates: vec![]
            }
        )
        .is_empty()
    );
    assert!(
        select(
            &request,
            &candidates,
            &SelectionOutcome {
                best: Some(MemoryId::new("db").unwrap()),
                confidence: f64::NAN,
                candidates: vec![]
            }
        )
        .is_empty()
    );
}
#[test]
fn paid_calls_and_invalid_input() {
    let engine = RecallService::new(
        MockMemoryRepository::new(fixture()),
        MockLunaDecisionsClient::new(ProviderConfig::default()),
        MockQueryPlanner::new(false),
        RecallPolicy {
            paid_calls_enabled: true,
            ..RecallPolicy::default()
        },
    );
    assert!(matches!(
        ready_mock(engine.recall(input("memory"))),
        Err(MemoryError::PaidCallsDisabled)
    ));
    assert!(matches!(
        ready_mock(service().recall(input(""))),
        Err(MemoryError::InvalidInput(_))
    ));
}
struct FailedSelector;
impl MemorySelector for FailedSelector {
    async fn evaluate(&self, _: &SelectionRequest) -> Result<SelectionOutcome, MemoryError> {
        Err(MemoryError::SelectionUnavailable)
    }
}
#[test]
fn selector_failure_is_domain_error() {
    let engine = RecallService::new(
        MockMemoryRepository::new(fixture()),
        FailedSelector,
        MockQueryPlanner::new(false),
        RecallPolicy::default(),
    );
    let out = ready_mock(engine.recall(input("memory"))).unwrap();
    assert!(out.evidence.context.is_empty());
    assert_eq!(out.failure, Some(MemoryError::SelectionUnavailable));
}
struct FailedRepository;
impl MemoryRepository for FailedRepository {
    async fn search(&self, _: &MemoryQueryPlan, _: usize) -> Result<Vec<Memory>, MemoryError> {
        Err(MemoryError::StorageUnavailable)
    }
}
#[test]
fn storage_failure_is_domain_error() {
    let engine = RecallService::new(
        FailedRepository,
        MockLunaDecisionsClient::new(ProviderConfig::default()),
        MockQueryPlanner::new(false),
        RecallPolicy::default(),
    );
    assert!(matches!(
        ready_mock(engine.recall(input("memory"))),
        Err(MemoryError::StorageUnavailable)
    ));
}
#[test]
fn typed_ids_reject_empty_and_control_characters() {
    assert!(MemoryId::new("").is_err());
    assert!(ActorId::new("\n").is_err());
    assert!(ProjectId::new(" ").is_err());
    assert!(SourceId::new("").is_err());
}
#[test]
fn dedup_and_unicode_budget() {
    let request = input("memory");
    let memory = fixture()[0].clone();
    assert_eq!(
        scope_candidates(&request, vec![memory.clone(), memory.clone()], 4).len(),
        1
    );
    assert_eq!(pack(vec![memory.clone(), memory], 500).selected.len(), 1);
    let mut unicode = fixture()[0].clone();
    unicode.text = "\u{1f9e0}".repeat(100);
    assert!(pack(vec![unicode], 100).context.is_empty());
}
#[test]
fn delivery_dresses_ids() {
    let command = RecallCommand {
        task: "memory".into(),
        owner: "fixture-owner".into(),
        actor: "".into(),
        project: "demo-project".into(),
        budget_tokens: 300,
    };
    assert!(matches!(
        ready_mock(handle(&service(), command)),
        Err(MemoryError::InvalidIdentity("actor ID"))
    ));
}
struct WrongScopePlanner;
impl MemoryQueryPlanner for WrongScopePlanner {
    async fn plan(&self, request: &RecallRequest) -> Result<MemoryQueryPlan, MemoryError> {
        Ok(MemoryQueryPlan {
            terms: vec!["memory".into()],
            owner: request.owner.clone(),
            actor: request.actor.clone(),
            project: ProjectId::new("other").unwrap(),
            relations: vec![],
        })
    }
}
#[test]
fn planner_cannot_broaden_authoritative_scope() {
    let engine = RecallService::new(
        MockMemoryRepository::new(fixture()),
        MockLunaDecisionsClient::new(ProviderConfig::default()),
        WrongScopePlanner,
        RecallPolicy::default(),
    );
    assert!(matches!(
        ready_mock(engine.recall(input("memory"))),
        Err(MemoryError::InvalidInput(_))
    ));
}
struct LeakyRepository;
impl MemoryRepository for LeakyRepository {
    async fn search(&self, _: &MemoryQueryPlan, _: usize) -> Result<Vec<Memory>, MemoryError> {
        let mut corpus = fixture();
        let mut foreign = corpus[0].clone();
        foreign.owner = OwnerId::new("foreign-owner").unwrap();
        corpus.push(foreign);
        corpus.push(corpus[0].clone());
        Ok(corpus)
    }
}
#[test]
fn service_rechecks_untrusted_backend_scope_and_dedup() {
    let engine = RecallService::new(
        LeakyRepository,
        MockLunaDecisionsClient::new(ProviderConfig::default()),
        MockQueryPlanner::new(false),
        RecallPolicy::default(),
    );
    let out = ready_mock(engine.recall(input("memory"))).unwrap();
    assert_eq!(
        out.candidate_ids
            .iter()
            .filter(|id| id.as_str() == "db")
            .count(),
        1
    );
    assert!(
        !out.candidate_ids
            .iter()
            .any(|id| ["other", "old", "restricted"].contains(&id.as_str()))
    );
}
#[test]
fn exact_paraphrase_and_mock_reformulation_are_separate() {
    let task = "Which database holds remembered knowledge?";
    let literal = ready_mock(service().recall(input(task))).unwrap();
    assert!(literal.candidate_ids.is_empty());
    let expanded = RecallService::new(
        MockMemoryRepository::new(fixture()),
        MockLunaDecisionsClient::new(ProviderConfig::default()),
        MockQueryPlanner::new(true),
        RecallPolicy::default(),
    );
    let out = ready_mock(expanded.recall(input(task))).unwrap();
    assert!(out.query_plan.terms.contains(&"storage".into()));
    assert!(out.candidate_ids.iter().any(|id| id.as_str() == "db"));
    assert!(out.evidence.selected.iter().any(|m| m.id.as_str() == "db"));
    // Word expansion may also retrieve background memories: improvement is not semantic quality proof.
}
