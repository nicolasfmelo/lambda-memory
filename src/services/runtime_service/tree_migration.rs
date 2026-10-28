use super::*;
use crate::domain_models::memory_tree::{LinkOperation, TreeMode, TreeQueue};
use crate::domain_models::runtime_diagnostics::ActivityOperation;

pub struct TreeMigrationPlan {
    pub mode: TreeMode,
    pub flat_memories: usize,
    pub sources: Vec<SourceId>,
    pub batches: usize,
}
pub struct CurateExistingReport {
    pub batches: usize,
    pub sources: usize,
}

impl MemoryRuntime {
    pub async fn inspect_tree(
        &self,
        project: &ProjectId,
    ) -> Result<TreeMigrationPlan, RuntimeError> {
        self.authorize(project)?;
        let mode = self.store.tree_mode(&self.scope.owner, project).await?;
        // A migrated project keeps its old flat revisions out of recall; nothing is left to curate.
        if mode == TreeMode::Tree {
            return Ok(TreeMigrationPlan {
                mode,
                flat_memories: 0,
                sources: Vec::new(),
                batches: 0,
            });
        }
        let (flat_memories, events) = self
            .store
            .tree_migration_sources(&self.scope.owner, project)
            .await?;
        let sources: Vec<_> = events.into_iter().map(|e| e.source).collect();
        let batches = sources.len().div_ceil(self.limits.curation_batch);
        Ok(TreeMigrationPlan {
            mode,
            flat_memories,
            sources,
            batches,
        })
    }

    #[cfg(feature = "router-http")]
    pub async fn curate_existing(
        &self,
        project: &ProjectId,
        sources: Vec<SourceId>,
    ) -> Result<CurateExistingReport, RuntimeError> {
        self.authorize(project)?;
        let _foreground = self.enter();
        let _background = self.background.lock().await;
        let plan = self.inspect_tree(project).await?;
        if plan.sources != sources {
            return Err(RuntimeError::Conflict);
        }
        if plan.mode != TreeMode::Flat && !sources.is_empty() {
            return Err(RuntimeError::Conflict);
        }
        let (_, events) = self
            .store
            .tree_migration_sources(&self.scope.owner, project)
            .await?;
        let linked = self
            .store
            .tree_migrated_sources(&self.scope.owner, project)
            .await?;
        let mut remaining: Vec<_> = events
            .into_iter()
            .filter(|e| !linked.contains(&e.source))
            .collect();
        let mut trace = self
            .begin_trace(
                std::slice::from_ref(project),
                ActivityOperation::Dream,
                ActivityPhase::Curation,
                false,
            )
            .await?;
        let result = async {
            let mut batches = 0;
            loop {
                let pending = self
                    .store
                    .list(
                        &self.scope.owner,
                        project,
                        &[LifecycleState::Processing],
                        self.limits.dream_batch,
                    )
                    .await?;
                if pending.is_empty() {
                    break;
                }
                for item in pending {
                    self.prepare_revision(item, &mut trace).await?;
                }
            }
            let mut ticks = 0;
            while !remaining.is_empty() {
                let count = remaining.len().min(self.limits.curation_batch);
                let batch: Vec<_> = remaining.drain(..count).collect();
                self.curate_batch(project, batch.clone(), LinkOperation::Migrate, &mut trace)
                    .await?;
                batches += 1;
                let parked = self
                    .store
                    .parked_sources(&self.scope.owner, project)
                    .await?;
                for (event, _) in parked {
                    if batch.iter().any(|source| source.source == event.source) {
                        remaining.push(event);
                    }
                }
                if !remaining.is_empty()
                    && self
                        .store
                        .tree_queue_peek(&self.scope.owner, project, TreeQueue::Split)
                        .await?
                        .is_some()
                {
                    if ticks >= 64 {
                        return Err(RuntimeError::Conflict);
                    }
                    self.tree_maintenance_tick(project, &mut trace).await?;
                    ticks += 1;
                }
                if batches > sources.len().saturating_mul(3).saturating_add(64) {
                    return Err(RuntimeError::Conflict);
                }
            }
            for _ in ticks..64 {
                if self
                    .tree_maintenance_tick(project, &mut trace)
                    .await?
                    .is_none()
                {
                    break;
                }
            }
            for queue in [TreeQueue::Rebuild, TreeQueue::Split, TreeQueue::Stale] {
                if self
                    .store
                    .tree_queue_peek(&self.scope.owner, project, queue)
                    .await?
                    .is_some()
                {
                    return Err(RuntimeError::Conflict);
                }
            }
            if !self
                .store
                .parked_sources(&self.scope.owner, project)
                .await?
                .is_empty()
            {
                return Err(RuntimeError::Conflict);
            }
            self.store
                .set_tree_mode(&self.scope.owner, project, TreeMode::Tree, now_ms()?)
                .await?;
            Ok(CurateExistingReport {
                batches,
                sources: sources.len(),
            })
        }
        .await;
        self.finish_trace(trace, result).await.result
    }
}
