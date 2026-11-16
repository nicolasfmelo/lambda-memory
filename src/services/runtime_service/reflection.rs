//! One paid branch reflection per idle tick; every attempt advances its watermark.
use super::*;
use crate::domain_models::memory_tree::Basis;
use crate::domain_models::reflection::*;
use crate::engines::reflection_policy;
impl MemoryRuntime {
    pub async fn inspect_reflection_backfill(
        &self,
        project: &ProjectId,
    ) -> Result<ReflectionBackfillPlan, RuntimeError> {
        self.authorize(project)?;
        self.store
            .inspect_reflection_backfill(&self.scope.owner, project)
            .await
    }
    pub async fn reflection_backfill(
        &self,
        project: &ProjectId,
        plan: &ReflectionBackfillPlan,
    ) -> Result<usize, RuntimeError> {
        self.authorize(project)?;
        self.store
            .reflection_backfill(&self.scope.owner, project, plan, now_ms()?)
            .await
    }
    #[cfg(feature = "router-http")]
    pub(super) async fn reflection_tick(
        &self,
        project: &ProjectId,
        work: ReflectionWork,
        trace: &mut OperationTrace<'_>,
    ) -> Result<Option<DreamReceipt>, RuntimeError> {
        let Some(providers) = &self.providers else {
            return Ok(None);
        };
        // No observed evidence means no billable run and no scheduling acknowledgement.
        if !work
            .input
            .nodes
            .iter()
            .any(|n| n.node.basis == Basis::Observed)
        {
            return Ok(None);
        }
        let owner = &self.scope.owner;
        let result = async {
            let mut parts = vec![
                reflection_policy::PROMPT_VERSION,
                owner.as_str(),
                project.as_str(),
                work.input.branch.as_str(),
            ];
            parts.extend(work.input.nodes.iter().map(|n| n.node.revision.as_str()));
            let operation = format!("reflect-job:{}", stable_id(&parts));
            let mut events = Vec::new();
            for n in work
                .input
                .nodes
                .iter()
                .filter(|n| n.node.basis == Basis::Observed)
            {
                if let Some(e) = self
                    .store
                    .tree_recent_sources(owner, project, &n.node.memory)
                    .await?
                    .into_iter()
                    .next()
                    && !events
                        .iter()
                        .any(|old: &CaptureEvent| old.source == e.source)
                {
                    events.push(e);
                }
            }
            events.sort_by(|a, b| {
                b.recorded_at_ms
                    .cmp(&a.recorded_at_ms)
                    .then(a.source.cmp(&b.source))
            });
            events.truncate(32);
            if events.is_empty() {
                return Err(RuntimeError::ProviderInvalidOutput);
            }
            let ids = events.iter().map(|e| e.source.clone()).collect::<Vec<_>>();
            let stage = trace
                .stage(
                    project,
                    ActivityPhase::Derivation,
                    &ids,
                    &[],
                    Some(&operation),
                )
                .await?;
            let answer = async {
                self.store
                    .admit_runtime_operation(owner, project, &operation, &events)
                    .await?;
                stage.admitted(&operation)?;
                match providers.reflect(&work.input).await {
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
                .finish_stage(stage, &answer, ActivityCounts::default())
                .await;
            let answer = answer?;
            if !reflection_policy::validate(&work.input, &answer) {
                return Err(RuntimeError::ProviderInvalidOutput);
            }
            let items = self
                .store
                .commit_reflection(
                    owner,
                    &self.scope.actor,
                    project,
                    &work.input,
                    answer,
                    &operation,
                    now_ms()?,
                    reflection_policy::POLICY,
                )
                .await?;
            for item in items {
                prepared(self.prepare_revision(item, trace).await)?;
            }
            Ok(DreamReceipt {
                batches: 1,
                sources: 0,
            })
        }
        .await;
        self.store
            .reflection_finish(owner, project, &work, now_ms()?)
            .await?;
        match result {
            Err(RuntimeError::ProviderUnavailable) => Err(RuntimeError::ProviderUnavailable),
            Err(error) => {
                trace.observed_failure(ActivityReason::from_error(&error));
                Ok(Some(DreamReceipt::default()))
            }
            Ok(receipt) => Ok(Some(receipt)),
        }
    }
}
