//! Transport DTO validation + service dispatch only. MCP intentionally deferred.
use crate::domain_models::{ActorId, MemoryError, OwnerId, ProjectId, RecallOutput, RecallRequest};
use crate::integrations::memory_ports::{MemoryQueryPlanner, MemoryRepository, MemorySelector};
use crate::services::recall_service::RecallService;

pub struct RecallCommand {
    pub task: String,
    pub owner: String,
    pub actor: String,
    pub project: String,
    pub budget_bytes: usize,
}
impl RecallCommand {
    pub fn into_domain(self) -> Result<RecallRequest, MemoryError> {
        Ok(RecallRequest {
            task: self.task,
            owner: OwnerId::new(self.owner)?,
            actor: ActorId::new(self.actor)?,
            project: ProjectId::new(self.project)?,
            budget_bytes: self.budget_bytes,
        })
    }
}
pub async fn handle<R: MemoryRepository, S: MemorySelector, P: MemoryQueryPlanner>(
    service: &RecallService<R, S, P>,
    command: RecallCommand,
) -> Result<RecallOutput, MemoryError> {
    service.recall(command.into_domain()?).await
}
