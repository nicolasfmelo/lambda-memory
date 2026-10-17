//! Resident Haiku/Gemini/Luna orchestration; the native lifecycle is authoritative.
use super::*;
use crate::domain_models::RecallRequest;
use crate::engines::recall_policy;
use crate::integrations::external_apis::router_runtime::RouterRuntimeProviders;

impl MemoryRuntime {
    pub fn with_openrouter(
        store: EmbeddedRuntimeStore,
        scope: RuntimeScope,
        limits: RuntimeLimits,
        providers: RouterRuntimeProviders,
    ) -> Result<Self, RuntimeError> {
        if limits.max_candidates > 8 {
            return Err(RuntimeError::InvalidInput(
                "Luna supports at most eight runtime candidates",
            ));
        }
        let mut runtime = Self::new(store, scope, limits)?;
        runtime.providers = Some(providers);
        Ok(runtime)
    }
    pub(super) async fn index_revision(
        &self,
        providers: &RouterRuntimeProviders,
        item: &RuntimeRevision,
        trace: &mut OperationTrace<'_>,
    ) -> Result<(), RuntimeError> {
        let _indexing = self.indexing.lock().await;
        let space = providers.space()?;
        if self.store.has_runtime_embedding(item, &space).await? {
            return Ok(());
        }
        let operation = format!(
            "embedding:{}",
            stable_id(&[
                item.revision.id.as_str(),
                &space.provider,
                &space.model,
                &space.revision
            ])
        );
        let stage = trace
            .stage(
                &item.revision.project,
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
            self.store
                .admit_runtime_operation(
                    &item.revision.owner,
                    &item.revision.project,
                    &operation,
                    &item.sources,
                )
                .await?;
            stage.admitted(&operation)?;
            let embedding = match providers.embed(&item.revision.content).await {
                Ok(embedding) => embedding,
                Err(error) => {
                    // Embeddings are cheap and idempotent: release rather than wedge recall forever.
                    self.store
                        .release_runtime_operation(
                            &item.revision.owner,
                            &item.revision.project,
                            &operation,
                        )
                        .await?;
                    return Err(error);
                }
            };
            stage
                .store(counts.clone())?
                .save_runtime_embedding(item, &embedding)
                .await
        }
        .await;
        trace.finish_stage(stage, &result, counts).await;
        result
    }
    pub(super) async fn recall_openrouter(
        &self,
        providers: &RouterRuntimeProviders,
        task: &str,
        projects: &[ProjectId],
        budget_tokens: usize,
        trace: &mut OperationTrace<'_>,
    ) -> Result<RuntimeRecall, RuntimeError> {
        let mut selected = Vec::new();
        if budget_tokens == 0 {
            return Ok(RuntimeRecall {
                context: String::new(),
                hits: selected,
                mode: "openrouter_semantic",
                truncated: false,
            });
        }
        for project in projects {
            if !self
                .store
                .has_active_runtime(&self.scope.owner, project)
                .await?
            {
                continue;
            }
            // Bounded indexing of already-active revisions; never hide an incomplete index.
            let space = providers.space()?;
            for item in self
                .store
                .unindexed_active(&self.scope.owner, project, &space, 4)
                .await?
            {
                self.index_revision(providers, &item, trace).await?;
            }
            if !self
                .store
                .unindexed_active(&self.scope.owner, project, &space, 1)
                .await?
                .is_empty()
            {
                return Err(RuntimeError::InvalidInput(
                    "semantic index catch-up incomplete; no partial recall returned",
                ));
            }
            let request = RecallRequest {
                task: task.to_owned(),
                owner: self.scope.owner.clone(),
                actor: self.scope.actor.clone(),
                project: project.clone(),
                budget_tokens,
            };
            let stage = trace
                .stage(project, ActivityPhase::QueryPlanning, &[], &[], None)
                .await?;
            let planned = providers.plan(&request).await;
            trace
                .finish_stage(stage, &planned, ActivityCounts::default())
                .await;
            let terms = planned?;
            let stage = trace
                .stage(project, ActivityPhase::QueryEmbedding, &[], &[], None)
                .await?;
            let embedded = providers.embed(task).await;
            trace
                .finish_stage(stage, &embedded, ActivityCounts::default())
                .await;
            let embedding = embedded?;
            let stage = trace
                .stage(project, ActivityPhase::Retrieval, &[], &[], None)
                .await?;
            let retrieved = async {
                let lexical = self
                    .store
                    .recall(
                        &self.scope.owner,
                        std::slice::from_ref(project),
                        &terms,
                        self.limits.max_candidates,
                    )
                    .await?;
                let vector = self
                    .store
                    .runtime_vector_hits(
                        &self.scope.owner,
                        project,
                        &embedding,
                        self.limits.max_candidates,
                    )
                    .await?;
                Ok::<_, RuntimeError>(runtime_policy::fuse_hits(
                    lexical,
                    vector,
                    self.limits.max_candidates,
                ))
            }
            .await;
            trace
                .finish_stage(stage, &retrieved, ActivityCounts::default())
                .await;
            let hits = retrieved?;
            if hits.is_empty() {
                continue;
            }
            let sources = hits
                .iter()
                .flat_map(|h| h.item.revision.sources.iter().cloned())
                .collect::<std::collections::BTreeSet<_>>()
                .into_iter()
                .collect::<Vec<_>>();
            let revisions = hits
                .iter()
                .map(|h| h.item.revision.id.clone())
                .collect::<Vec<_>>();
            let stage = trace
                .stage(
                    project,
                    ActivityPhase::Selection,
                    &sources,
                    &revisions,
                    None,
                )
                .await?;
            let selected_answer = providers.select(&request, &terms, &hits, now_ms()?).await;
            trace
                .finish_stage(stage, &selected_answer, ActivityCounts::default())
                .await;
            let (candidates, answer) = selected_answer?;
            let ids = recall_policy::select_supported(&request, &candidates, &answer)
                .into_iter()
                .map(|memory| {
                    RevisionId::new(memory.id.as_str())
                        .map_err(|_| RuntimeError::ProviderUnavailable)
                })
                .collect::<Result<Vec<_>, _>>()?;
            let current = self
                .store
                .active_runtime_hits(&self.scope.owner, std::slice::from_ref(project), &ids)
                .await?;
            selected.extend(current.into_iter().filter(|hit| {
                hits.iter().any(|old| {
                    old.item.revision.id == hit.item.revision.id
                        && old.item.revision.content == hit.item.revision.content
                        && old.item.sources == hit.item.sources
                })
            }));
        }
        // Revalidate every selected project after all awaited model calls, not only the last project.
        let ids = selected
            .iter()
            .map(|hit| hit.item.revision.id.clone())
            .collect::<Vec<_>>();
        let current = self
            .store
            .active_runtime_hits(&self.scope.owner, projects, &ids)
            .await?;
        let mut result = runtime_policy::pack(
            current
                .into_iter()
                .filter(|hit| {
                    selected.iter().any(|old| {
                        old.item.revision.id == hit.item.revision.id
                            && old.item.revision.content == hit.item.revision.content
                            && old.item.sources == hit.item.sources
                    })
                })
                .collect(),
            budget_tokens,
        );
        result.mode = "openrouter_semantic";
        for project in projects {
            let stage = trace
                .stage(project, ActivityPhase::Packing, &[], &[], None)
                .await?;
            let count = result
                .hits
                .iter()
                .filter(|hit| &hit.item.revision.project == project)
                .count() as u64;
            trace
                .finish_stage(
                    stage,
                    &Ok::<(), RuntimeError>(()),
                    ActivityCounts {
                        hits_returned: Some(count),
                        ..Default::default()
                    },
                )
                .await;
        }
        Ok(result)
    }
}
