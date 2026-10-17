use crate::domain_models::{agent_models::*, consolidation::ConsolidationError};
pub trait ConsolidationAgent {
    fn derive(
        &self,
        jobs: &[AgentDerivationJob],
    ) -> impl std::future::Future<Output = Result<Vec<AgentProposal>, ConsolidationError>>;
}
