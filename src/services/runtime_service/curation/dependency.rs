use super::*;

impl MemoryRuntime {
    /// Called only after rebuild, split and stale queues are empty; one edge per tick.
    pub(super) async fn tree_dependency_tick(
        &self,
        project: &ProjectId,
        trace: &mut OperationTrace<'_>,
    ) -> Result<Option<DreamReceipt>, RuntimeError> {
        let owner = &self.scope.owner;
        let Some((memory, dependency_id)) = self.store.tree_dependency_peek(owner, project).await?
        else {
            return Ok(None);
        };
        let result = self
            .tree_dependency_step(project, &memory, &dependency_id, trace)
            .await;
        match result {
            Err(
                error @ (RuntimeError::ProviderInvalidOutput
                | RuntimeError::UncertainAdmission
                | RuntimeError::InvalidInput(MAINTENANCE_BOUNDS)),
            ) => {
                self.store
                    .tree_dependency_pop(owner, project, &memory, &dependency_id)
                    .await?;
                trace.observed_failure(match error {
                    RuntimeError::ProviderInvalidOutput => ActivityReason::ProviderInvalidOutput,
                    RuntimeError::UncertainAdmission => ActivityReason::UncertainAdmission,
                    _ => ActivityReason::TreeBounds,
                });
                Ok(Some(DreamReceipt::default()))
            }
            other => other,
        }
    }
    async fn tree_dependency_step(
        &self,
        project: &ProjectId,
        memory: &MemoryId,
        dependency_id: &MemoryId,
        trace: &mut OperationTrace<'_>,
    ) -> Result<Option<DreamReceipt>, RuntimeError> {
        let owner = &self.scope.owner;
        let providers = self
            .providers
            .as_ref()
            .ok_or(RuntimeError::ProviderUnavailable)?;
        let nodes = self.store.tree_nodes(owner, project).await?;
        let pair = nodes
            .iter()
            .find(|node| &node.memory == memory && node.basis == Basis::Observed)
            .zip(nodes.iter().find(|node| &node.memory == dependency_id));
        let Some((node, dependency)) = pair else {
            self.store
                .tree_dependency_pop(owner, project, memory, dependency_id)
                .await?;
            return Ok(Some(DreamReceipt::default()));
        };
        let sources = self
            .store
            .tree_recent_sources(owner, project, memory)
            .await?;
        if sources.is_empty() {
            return Err(RuntimeError::InvalidInput(MAINTENANCE_BOUNDS));
        }
        let ids: Vec<_> = sources.iter().map(|source| source.source.clone()).collect();
        let operation = format!(
            "tree-dependency:{}",
            stable_id(&[
                DEPENDENCY_PROMPT_VERSION,
                owner.as_str(),
                project.as_str(),
                node.revision.as_str(),
                dependency.revision.as_str()
            ])
        );
        // Purge may have removed the evidence-backed edge since this item was queued.
        // Never pay for a refresh justified only by a stale queue entry.
        if !self
            .store
            .tree_dependency_exists(owner, project, memory, dependency_id)
            .await?
        {
            self.store
                .tree_dependency_pop(owner, project, memory, dependency_id)
                .await?;
            return Ok(Some(DreamReceipt::default()));
        }
        let stage = trace
            .stage(
                project,
                ActivityPhase::TreeRefresh,
                &ids,
                &[],
                Some(&operation),
            )
            .await?;
        let output = async {
            self.store
                .admit_runtime_operation(owner, project, &operation, &sources)
                .await?;
            stage.admitted(&operation)?;
            match providers.dependency_node(node, &sources, dependency).await {
                Err(RuntimeError::ProviderRejected) => {
                    self.store
                        .release_runtime_operation(owner, project, &operation)
                        .await?;
                    Err(RuntimeError::ProviderRejected)
                }
                other => other,
            }
        }
        .await;
        trace
            .finish_stage(stage, &output, ActivityCounts::default())
            .await;
        let (title, content) = output?;
        tree_policy::validate_title(&title)
            .and_then(|_| tree_policy::validate_content(&content))
            .map_err(|_| RuntimeError::ProviderInvalidOutput)?;
        let plan = MaintenanceCommit {
            operation_id: operation,
            operation: LinkOperation::Rebuild,
            rewrites: vec![(memory.clone(), node.revision.clone(), title, content, ids)],
            creates: vec![],
            reparent: vec![],
            expected_reparent: vec![],
            move_sources: vec![],
            stale: node
                .parent
                .iter()
                .filter(|parent| nodes.iter().any(|node| &node.memory == *parent))
                .cloned()
                .collect(),
            queue_item: None,
        };
        let items = self
            .store
            .commit_dependency_refresh(
                owner,
                &self.scope.actor,
                project,
                plan,
                dependency,
                now_ms()?,
            )
            .await?;
        for item in items {
            prepared(self.prepare_revision(item, trace).await)?;
        }
        Ok(Some(DreamReceipt {
            batches: 1,
            sources: 0,
        }))
    }
}
