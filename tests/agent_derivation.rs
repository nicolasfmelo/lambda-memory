use lambda_memory::domain_models::{
    agent_models::*, capture::*, consolidation::ConsolidationError, lifecycle::*, *,
};
use lambda_memory::integrations::derivation_ports::ConsolidationAgent;
use lambda_memory::services::agent_derivation_service::AgentDerivationService;
struct DuplicateAgent;
impl ConsolidationAgent for DuplicateAgent {
    async fn derive(
        &self,
        _: &[AgentDerivationJob],
    ) -> Result<Vec<AgentProposal>, ConsolidationError> {
        let p = AgentProposal {
            job: "job".into(),
            source: SourceId::new("s1").unwrap(),
            eligible: true,
            content: "Synthetic fact.".into(),
            supersedes: None,
        };
        Ok(vec![p.clone(), p])
    }
}
fn input() -> Vec<AgentDerivationJob> {
    let owner = OwnerId::new("o").unwrap();
    let project = ProjectId::new("p").unwrap();
    vec![AgentDerivationJob {
        id: "job".into(),
        sources: ["s1", "s2"]
            .iter()
            .map(|id| AgentSource {
                event: CaptureEvent {
                    source: SourceId::new(*id).unwrap(),
                    owner: owner.clone(),
                    actor: ActorId::new("a").unwrap(),
                    project: project.clone(),
                    origin: "fixture://service".into(),
                    recorded_at_ms: 1,
                    kind: WorkEventKind::Decision,
                    outcome: WorkOutcome::Succeeded,
                    content: "Synthetic fact.".into(),
                },
                policy: SourceLifecycle {
                    source: SourceId::new(*id).unwrap(),
                    owner: owner.clone(),
                    project: project.clone(),
                    authoritative: true,
                    expires_at_ms: None,
                },
                arrival_ms: 1,
                fact_key: MemoryId::new("fact").unwrap(),
            })
            .collect(),
    }]
}
#[test]
fn duplicate_derivations_and_mismatched_source_policy_fail_closed() {
    let service = AgentDerivationService::new(DuplicateAgent);
    let input = input();
    assert!(matches!(
        crate::ready(service.derive(&input)),
        Err(ConsolidationError::InvalidRevision)
    ));
    let mut invalid = input.clone();
    invalid[0].sources[0].policy.owner = OwnerId::new("other").unwrap();
    assert!(matches!(
        crate::ready(service.derive(&invalid)),
        Err(ConsolidationError::InvalidSnapshot)
    ));
}
fn ready<F: std::future::Future>(future: F) -> F::Output {
    use std::task::{Context, Poll, Waker};
    let w = Waker::noop();
    let mut c = Context::from_waker(w);
    let mut future = std::pin::pin!(future);
    match future.as_mut().poll(&mut c) {
        Poll::Ready(r) => r,
        Poll::Pending => panic!("test fixture must be immediately ready"),
    }
}
