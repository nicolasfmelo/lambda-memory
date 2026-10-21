//! Explicit operator paths, never exposed through MCP or automatic retry.
use super::*;
use crate::domain_models::embeddings::EmbeddingSpace;
use crate::domain_models::runtime_diagnostics::{ActivityOperation, RecordedOperation};
use crate::domain_models::runtime_embedding::EmbeddingRecoveryPlan;

impl MemoryRuntime {
    fn maintenance_space(&self, project: &ProjectId) -> Result<EmbeddingSpace, RuntimeError> {
        self.authorize(project)?;
        if self.scope.projects.len() != 1 {
            return Err(RuntimeError::InvalidInput(
                "embedding maintenance requires one project",
            ));
        }
        self.providers
            .as_ref()
            .ok_or(RuntimeError::InvalidInput(
                "embedding maintenance requires openrouter mode",
            ))?
            .space()
    }
    pub async fn embedding_recovery_plan(
        &self,
        project: &ProjectId,
    ) -> Result<EmbeddingRecoveryPlan, RuntimeError> {
        let space = self.maintenance_space(project)?;
        self.store
            .embedding_recovery_plan(&self.scope.owner, project, &space, 16)
            .await
    }
    pub fn embedding_space(&self, project: &ProjectId) -> Result<EmbeddingSpace, RuntimeError> {
        self.maintenance_space(project)
    }
    pub async fn recover_embeddings(
        &self,
        project: &ProjectId,
        plan: &EmbeddingRecoveryPlan,
    ) -> RecordedOperation<usize> {
        let _foreground = self.enter();
        let _background = self.background.lock().await;
        let result = async {
            if self.maintenance_space(project)? != plan.space {
                return Err(RuntimeError::Conflict);
            }
            let providers = self
                .providers
                .as_ref()
                .ok_or(RuntimeError::InvalidInput("embedding provider required"))?;
            let now = now_ms()?;
            // Validate retention before committing any paid admission.
            for item in &plan.items {
                let revision = self
                    .store
                    .runtime_revision(&self.scope.owner, project, &item.revision)
                    .await?;
                if revision.sources.iter().any(|s| {
                    s.recorded_at_ms > now
                        || self
                            .limits
                            .retention_ms
                            .is_some_and(|r| s.recorded_at_ms < now.saturating_sub(r))
                }) {
                    return Err(RuntimeError::Conflict);
                }
            }
            let mut trace = self
                .begin_trace(
                    std::slice::from_ref(project),
                    ActivityOperation::Dream,
                    ActivityPhase::Indexing,
                    false,
                )
                .await?;
            let result = async {
                let indexing = self.indexing.lock().await;
                let admitted = self
                    .store
                    .admit_embedding_recovery(
                        &self.scope.owner,
                        &self.scope.actor,
                        project,
                        plan,
                        now,
                    )
                    .await?;
                let mut completed = Vec::with_capacity(admitted.len());
                for (item, operation) in admitted {
                    let stage = trace
                        .stage(
                            project,
                            ActivityPhase::Indexing,
                            &item.revision.sources,
                            std::slice::from_ref(&item.revision.id),
                            Some(&operation),
                        )
                        .await?;
                    let counts = ActivityCounts {
                        revisions_indexed: Some(1),
                        ..Default::default()
                    };
                    let result = async {
                        stage.admitted(&operation)?;
                        let embedding = providers.embed_document(&item.revision.content).await?;
                        stage
                            .store(counts.clone())?
                            .save_runtime_embedding(&item, &embedding)
                            .await
                    }
                    .await;
                    trace.finish_stage(stage, &result, counts).await;
                    result?;
                    completed.push(item);
                }
                drop(indexing);
                let count = completed.len();
                for item in completed {
                    prepared(self.prepare_revision(item, &mut trace).await)?;
                }
                Ok(count)
            }
            .await;
            Ok(self.finish_trace(trace, result).await)
        }
        .await;
        result.unwrap_or_else(|error| RecordedOperation {
            result: Err(error),
            diagnostics_warning: None,
        })
    }
    /// Bounded, explicitly invoked target-space backfill; an uncertain admission stops the batch.
    pub async fn prepare_embedding_index(
        &self,
        project: &ProjectId,
        expected_space: &EmbeddingSpace,
        limit: usize,
    ) -> RecordedOperation<usize> {
        let _foreground = self.enter();
        let _background = self.background.lock().await;
        let result = async {
            let space = self.maintenance_space(project)?;
            if &space != expected_space || limit == 0 || limit > 16 {
                return Err(RuntimeError::InvalidInput(
                    "index preparation space or bound",
                ));
            }
            let providers = self
                .providers
                .as_ref()
                .ok_or(RuntimeError::InvalidInput("embedding provider required"))?;
            let items = self
                .store
                .unindexed_active(&self.scope.owner, project, &space, limit)
                .await?;
            let mut trace = self
                .begin_trace(
                    std::slice::from_ref(project),
                    ActivityOperation::Dream,
                    ActivityPhase::Indexing,
                    false,
                )
                .await?;
            let result = async {
                let count = items.len();
                for item in items {
                    self.index_revision(providers, &item, &mut trace).await?;
                }
                Ok(count)
            }
            .await;
            Ok(self.finish_trace(trace, result).await)
        }
        .await;
        result.unwrap_or_else(|error| RecordedOperation {
            result: Err(error),
            diagnostics_warning: None,
        })
    }
}
