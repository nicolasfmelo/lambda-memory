//! Idle admission and actual source-bound work; empty scheduler ticks have no history.
use super::*;
use crate::domain_models::runtime_diagnostics::*;

impl MemoryRuntime {
    fn paid_dreaming(&self) -> bool {
        #[cfg(feature = "router-http")]
        {
            self.providers.is_some()
        }
        #[cfg(not(feature = "router-http"))]
        {
            false
        }
    }

    /// A read-only maintenance plan; it neither admits work nor calls providers.
    pub async fn dream_recovery_plan(
        &self,
        project: &ProjectId,
    ) -> Result<DreamRecoveryBatch, RuntimeError> {
        self.authorize(project)?;
        self.store
            .blocked_dream_sources(&self.scope.owner, project, self.limits.dream_batch)
            .await
    }

    /// Operator-only, exact-snapshot recovery. Normal MCP and idle work cannot enter this path.
    pub async fn recover_dream_sources(
        &self,
        project: &ProjectId,
        sources: &[SourceId],
        previous_operations: &[JobId],
    ) -> RecordedOperation<DreamReceipt> {
        let _foreground = self.enter();
        let _background = self.background.lock().await;
        let result = async {
            self.authorize(project)?;
            if !self.paid_dreaming()
                || sources.is_empty()
                || sources.len() > self.limits.dream_batch
                || previous_operations.is_empty()
                || previous_operations.len() > 32
                || previous_operations
                    .iter()
                    .enumerate()
                    .any(|(i, id)| previous_operations[..i].contains(id))
            {
                return Err(RuntimeError::InvalidInput("explicit paid recovery bounds"));
            }
            let batch = self
                .store
                .dream_recovery_batch(&self.scope.owner, project, sources)
                .await?;
            if batch.previous_operations.len() != previous_operations.len()
                || batch
                    .previous_operations
                    .iter()
                    .any(|id| !previous_operations.contains(id))
            {
                return Err(RuntimeError::UncertainAdmission);
            }
            let now = now_ms()?;
            if batch.sources.iter().any(|source| {
                source.recorded_at_ms > now
                    || self.limits.retention_ms.is_some_and(|retention| {
                        source.recorded_at_ms < now.saturating_sub(retention)
                    })
            }) {
                return Err(RuntimeError::Conflict);
            }
            let mut trace = self
                .begin_trace(
                    std::slice::from_ref(project),
                    ActivityOperation::Dream,
                    ActivityPhase::Derivation,
                    false,
                )
                .await?;
            let result = self
                .derive_batch(
                    project,
                    batch.sources,
                    &batch.previous_operations,
                    &mut trace,
                )
                .await;
            Ok(self.finish_trace(trace, result).await)
        }
        .await;
        result.unwrap_or_else(|error| RecordedOperation {
            result: Err(error),
            diagnostics_warning: None,
        })
    }

    pub async fn dream_once(&self) -> RecordedOperation<DreamReceipt> {
        match self.dream_tick().await {
            Ok(Some(recorded)) => recorded,
            Ok(None) => RecordedOperation {
                result: Ok(DreamReceipt::default()),
                diagnostics_warning: None,
            },
            Err(error) => RecordedOperation {
                result: Err(error),
                diagnostics_warning: None,
            },
        }
    }
    async fn dream_tick(&self) -> Result<Option<RecordedOperation<DreamReceipt>>, RuntimeError> {
        self.initialize_diagnostics().await?;
        let Ok(_background) = self.background.try_lock() else {
            return Ok(None);
        };
        if self.background_stopped()? || !self.idle() {
            return Ok(None);
        }
        let now = now_ms()?;
        for project in &self.scope.projects {
            if !self.idle() {
                break;
            }
            if let Some(retention) = self.limits.retention_ms
                && let Some(cutoff) = now.checked_sub(retention)
            {
                let expired = self
                    .store
                    .retention_source_ids(
                        &self.scope.owner,
                        project,
                        cutoff,
                        self.limits.dream_batch,
                    )
                    .await?;
                if !expired.is_empty() {
                    let mut trace = self
                        .begin_trace(
                            std::slice::from_ref(project),
                            ActivityOperation::Purge,
                            ActivityPhase::Purge,
                            true,
                        )
                        .await?;
                    let result = self.purge_effect(project, &expired, &mut trace).await;
                    if let Ok(receipt) = &result {
                        trace.counts(
                            project,
                            &ActivityCounts {
                                sources_removed: Some(receipt.sources as u64),
                                revisions_removed: Some(receipt.revisions as u64),
                                jobs_removed: Some(receipt.jobs as u64),
                                ..Default::default()
                            },
                        );
                    }
                    let recorded = self.finish_trace(trace, result).await;
                    if recorded.result.is_err() || recorded.diagnostics_warning.is_some() {
                        return Ok(Some(RecordedOperation {
                            result: recorded.result.map(|_| DreamReceipt::default()),
                            diagnostics_warning: recorded.diagnostics_warning,
                        }));
                    }
                }
            }
            if !self.idle() {
                break;
            }
            let pending = self
                .store
                .list(
                    &self.scope.owner,
                    project,
                    &[LifecycleState::Processing],
                    self.limits.dream_batch,
                )
                .await?;
            if !pending.is_empty() {
                let mut trace = self
                    .begin_trace(
                        std::slice::from_ref(project),
                        ActivityOperation::Dream,
                        ActivityPhase::Preparation,
                        true,
                    )
                    .await?;
                let result = async {
                    for item in pending {
                        super::prepared(self.prepare_revision(item, &mut trace).await)?;
                    }
                    Ok(DreamReceipt::default())
                }
                .await;
                return Ok(Some(self.finish_dream(trace, result).await));
            }
            let sources = self
                .store
                .eligible_sources(
                    &self.scope.owner,
                    project,
                    self.limits.dream_batch,
                    self.paid_dreaming(),
                    self.limits
                        .retention_ms
                        .map(|retention| now.saturating_sub(retention)),
                    Some(now),
                )
                .await?;
            if sources.is_empty() {
                continue;
            }
            let mut trace = self
                .begin_trace(
                    std::slice::from_ref(project),
                    ActivityOperation::Dream,
                    ActivityPhase::Derivation,
                    true,
                )
                .await?;
            let result = self.derive_batch(project, sources, &[], &mut trace).await;
            return Ok(Some(self.finish_dream(trace, result).await));
        }
        Ok(None)
    }
    async fn finish_dream(
        &self,
        trace: OperationTrace<'_>,
        result: Result<DreamReceipt, RuntimeError>,
    ) -> RecordedOperation<DreamReceipt> {
        let stopped = result
            .as_ref()
            .err()
            .filter(|e| {
                matches!(
                    e,
                    RuntimeError::ProviderUnavailable | RuntimeError::UncertainAdmission
                )
            })
            .map(ActivityReason::from_error);
        let mut recorded = self.finish_trace(trace, result).await;
        if let Some(reason) = stopped
            && let Err(error) = self.stop_background(reason)
        {
            recorded.result = Err(error);
        }
        recorded
    }
    async fn derive_batch(
        &self,
        project: &ProjectId,
        sources: Vec<CaptureEvent>,
        recovery_of: &[JobId],
        trace: &mut OperationTrace<'_>,
    ) -> Result<DreamReceipt, RuntimeError> {
        let mut key_parts = Vec::with_capacity(2 + sources.len() + recovery_of.len());
        key_parts.extend([self.scope.owner.as_str(), project.as_str()]);
        key_parts.extend(sources.iter().map(|s| s.source.as_str()));
        key_parts.extend(recovery_of.iter().map(JobId::as_str));
        let batch_key = stable_id(&key_parts);
        let watermark_ms = sources
            .iter()
            .map(|s| s.recorded_at_ms)
            .max()
            .ok_or(RuntimeError::StorageUnavailable)?;
        let snapshot = JobSnapshot {
            id: JobId::new(if recovery_of.is_empty() {
                format!("dream-job:{batch_key}")
            } else {
                format!("dream-job:recovery:{batch_key}")
            })
            .map_err(|_| RuntimeError::InvalidInput("dream job identity"))?,
            owner: self.scope.owner.clone(),
            project: project.clone(),
            watermark_ms,
            sources,
        };
        consolidation_policy::validate_snapshot(&snapshot)
            .map_err(|_| RuntimeError::InvalidInput("invalid dream snapshot"))?;
        let source_ids = snapshot
            .sources
            .iter()
            .map(|s| s.source.clone())
            .collect::<Vec<_>>();
        #[cfg(feature = "router-http")]
        let paid = self.providers.as_ref().map(|_| snapshot.id.as_str());
        #[cfg(not(feature = "router-http"))]
        let paid: Option<&str> = None;
        let stage = trace
            .stage(project, ActivityPhase::Derivation, &source_ids, &[], paid)
            .await?;
        #[cfg(feature = "router-http")]
        let derived = async {
            if let Some(providers) = &self.providers {
                if recovery_of.is_empty() {
                    self.store
                        .admit_runtime_operation(
                            &snapshot.owner,
                            &snapshot.project,
                            snapshot.id.as_str(),
                            &snapshot.sources,
                        )
                        .await?;
                } else {
                    self.store
                        .admit_runtime_recovery_operation(
                            &snapshot.owner,
                            &self.scope.actor,
                            &snapshot.project,
                            snapshot.id.as_str(),
                            &snapshot.sources,
                            recovery_of,
                            now_ms()?,
                        )
                        .await?;
                }
                stage.admitted(snapshot.id.as_str())?;
                match providers.derive(&snapshot).await {
                    Err(RuntimeError::ProviderRejected) => {
                        // Refused before billable work: the sources return to pending, not blocked.
                        self.store
                            .release_runtime_operation(
                                &snapshot.owner,
                                &snapshot.project,
                                snapshot.id.as_str(),
                            )
                            .await?;
                        Err(RuntimeError::ProviderRejected)
                    }
                    derived => Ok(Some(derived?)),
                }
            } else {
                Ok(None)
            }
        }
        .await;
        #[cfg(not(feature = "router-http"))]
        let derived: Result<Option<Vec<(SourceId, String)>>, RuntimeError> = Ok(None);
        trace
            .finish_stage(stage, &derived, ActivityCounts::default())
            .await;
        let derived = derived?;
        let mut revisions = Vec::with_capacity(snapshot.sources.len());
        for event in &snapshot.sources {
            let key = stable_id(&[
                event.owner.as_str(),
                event.project.as_str(),
                event.source.as_str(),
            ]);
            let content = match &derived {
                Some(claims) => claims
                    .iter()
                    .find(|(source, _)| source == &event.source)
                    .map(|(_, content)| content.clone())
                    .ok_or(RuntimeError::ProviderUnavailable)?,
                None => event.content.clone(),
            };
            let remote = derived.is_some();
            let revision = Revision {
                id: RevisionId::new(format!("dream-revision:{key}"))
                    .map_err(|_| RuntimeError::StorageUnavailable)?,
                memory: MemoryId::new(format!("dream-memory:{key}"))
                    .map_err(|_| RuntimeError::StorageUnavailable)?,
                owner: event.owner.clone(),
                project: event.project.clone(),
                kind: KnowledgeKind::Knowledge,
                status: RevisionStatus::PendingReview,
                content,
                prerequisites: Vec::new(),
                sources: vec![event.source.clone()],
                predecessor: None,
                model: if remote {
                    "anthropic/claude-haiku-5.5"
                } else {
                    "local-extractive-v1"
                }
                .into(),
                prompt_version: if remote {
                    "runtime-source-bound-v2"
                } else {
                    "outcome-preserving-v1"
                }
                .into(),
            };
            consolidation_policy::validate_revision(&snapshot, &revision)
                .map_err(|_| RuntimeError::InvalidInput("invalid dream revision"))?;
            revisions.push(revision);
        }
        if !self.idle() && derived.is_none() {
            trace.observed_failure(ActivityReason::Interrupted);
            return Ok(DreamReceipt::default());
        }
        let count = snapshot.sources.len();
        let counts = ActivityCounts {
            sources_consumed: Some(count as u64),
            revisions_created: Some(revisions.len() as u64),
            ..Default::default()
        };
        let stage = trace
            .stage(project, ActivityPhase::Preparation, &source_ids, &[], None)
            .await?;
        let result = stage
            .store(counts.clone())?
            .commit_batch(snapshot, revisions)
            .await;
        trace.finish_stage(stage, &result, counts).await;
        if result? {
            self.prepare_project(project, self.limits.dream_batch, trace)
                .await?;
            Ok(DreamReceipt {
                batches: 1,
                sources: count,
            })
        } else {
            trace.observed_failure(ActivityReason::Interrupted);
            Ok(DreamReceipt::default())
        }
    }
}
