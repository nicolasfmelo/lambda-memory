use crate::domain_models::{agent_models::*, consolidation::ConsolidationError};
use crate::integrations::derivation_ports::ConsolidationAgent;
pub struct AgentDerivationService<A> {
    agent: A,
}
impl<A: ConsolidationAgent> AgentDerivationService<A> {
    pub fn new(agent: A) -> Self {
        Self { agent }
    }
    pub async fn derive(
        &self,
        jobs: &[AgentDerivationJob],
    ) -> Result<Vec<AgentProposal>, ConsolidationError> {
        if jobs.is_empty()
            || jobs.len() > 32
            || jobs.iter().map(|j| j.sources.len()).sum::<usize>() > 64
        {
            return Err(ConsolidationError::InvalidSnapshot);
        }
        for job in jobs {
            if job.id.is_empty()
                || job.sources.is_empty()
                || job.sources.iter().any(|s| {
                    s.event.content.len() > 8192
                        || s.policy.owner != s.event.owner
                        || s.policy.source != s.event.source
                        || s.policy.project != s.event.project
                })
            {
                return Err(ConsolidationError::InvalidSnapshot);
            }
        }
        let proposals = self.agent.derive(jobs).await?;
        Self::validate_proposals(jobs, &proposals)?;
        Ok(proposals)
    }
}

impl<A: ConsolidationAgent> AgentDerivationService<A> {
    pub fn validate_proposals(
        jobs: &[AgentDerivationJob],
        proposals: &[AgentProposal],
    ) -> Result<(), ConsolidationError> {
        if proposals.len() != jobs.iter().map(|j| j.sources.len()).sum::<usize>() {
            return Err(ConsolidationError::InvalidRevision);
        }
        for (i, p) in proposals.iter().enumerate() {
            let job = jobs
                .iter()
                .find(|j| j.id == p.job)
                .ok_or(ConsolidationError::InvalidRevision)?;
            if !job.sources.iter().any(|s| s.event.source == p.source)
                || proposals[..i]
                    .iter()
                    .any(|old| old.job == p.job && old.source == p.source)
                || p.content.trim().is_empty()
                || p.content.len() > 8192
            {
                return Err(ConsolidationError::InvalidRevision);
            }
        }

        Ok(())
    }
}
