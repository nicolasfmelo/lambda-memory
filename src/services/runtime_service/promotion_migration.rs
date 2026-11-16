//! Explicitly consented, single-project promotion classification; never retries paid work.
use super::*;
use crate::domain_models::promotion::*;
use crate::engines::promotion_policy;

impl MemoryRuntime {
    async fn promotion_candidates(
        &self,
        project: &ProjectId,
    ) -> Result<Vec<PromotionCandidate>, RuntimeError> {
        self.authorize(project)?;
        let owner = &self.scope.owner;
        let nodes = self.store.tree_nodes(owner, project).await?;
        let reviewed = self
            .store
            .promotion_reviews(owner, project)
            .await?
            .into_iter()
            .map(|r| r.revision)
            .collect::<Vec<_>>();
        let mut candidates = Vec::new();
        for node in promotion_policy::candidates(project, &nodes, &reviewed)? {
            let sources = self
                .store
                .promotion_sources(owner, project, &node.memory)
                .await?;
            let mut path = vec![node.root.label().to_string()];
            path.extend(
                self.store
                    .tree_ancestors(owner, project, &node.memory)
                    .await?
                    .into_iter()
                    .map(|n| n.title),
            );
            path.push(node.title.clone());
            candidates.push(PromotionCandidate {
                node,
                path,
                sources,
            });
        }
        Ok(candidates)
    }
    pub async fn inspect_promotions(
        &self,
        project: &ProjectId,
    ) -> Result<PromotionPlan, RuntimeError> {
        let candidates = self.promotion_candidates(project).await?;
        Ok(Self::promotion_plan(&candidates))
    }
    fn promotion_plan(candidates: &[PromotionCandidate]) -> PromotionPlan {
        PromotionPlan {
            nodes: candidates.iter().map(|c| c.node.memory.clone()).collect(),
            batches: candidates.len().div_ceil(promotion_policy::BATCH_SIZE),
            linked_sources: candidates.iter().map(|c| c.sources.len()).sum(),
        }
    }
    #[cfg(feature = "router-http")]
    pub async fn promote_existing(
        &self,
        project: &ProjectId,
        plan: &PromotionPlan,
        accept_charges: bool,
    ) -> Result<PromotionReport, RuntimeError> {
        self.authorize(project)?;
        let _foreground = self.enter();
        let _background = self.background.lock().await;
        let candidates = self.promotion_candidates(project).await?;
        if !accept_charges || &Self::promotion_plan(&candidates) != plan {
            return Err(RuntimeError::InvalidInput(
                "exact promotion consent required",
            ));
        }
        let owner = &self.scope.owner;
        let mut report = PromotionReport::default();
        for batch in candidates.chunks(promotion_policy::BATCH_SIZE) {
            let sourced: Vec<_> = batch
                .iter()
                .filter(|c| !c.sources.is_empty())
                .cloned()
                .collect();
            let mut verdicts = Vec::new();
            if !sourced.is_empty() {
                let providers = self
                    .providers
                    .as_ref()
                    .ok_or(RuntimeError::ProviderUnavailable)?;
                let events: Vec<_> = sourced.iter().map(|c| c.sources[0].clone()).collect();
                let earlier = self
                    .store
                    .promotion_admitted_sources(owner, project)
                    .await?;
                let cycles: Vec<_> = events
                    .iter()
                    .map(|e| {
                        format!(
                            "{}:{}",
                            e.source,
                            earlier.iter().filter(|id| **id == e.source).count()
                        )
                    })
                    .collect();
                let mut parts = vec![
                    promotion_policy::PROMPT_VERSION,
                    owner.as_str(),
                    project.as_str(),
                ];
                parts.extend(batch.iter().map(|c| c.node.revision.as_str()));
                parts.extend(cycles.iter().map(String::as_str));
                let operation = format!("promote-migration:{}", stable_id(&parts));
                self.store
                    .admit_runtime_operation(owner, project, &operation, &events)
                    .await?;
                let output = match providers.classify_promotions(&sourced).await {
                    Err(RuntimeError::ProviderRejected) => {
                        self.store
                            .release_runtime_operation(owner, project, &operation)
                            .await?;
                        return Err(RuntimeError::ProviderRejected);
                    }
                    other => other?,
                };
                verdicts = promotion_policy::validate_output(&output, sourced.len())?;
            }
            let mut index = 0;
            let decisions: Vec<_> = batch
                .iter()
                .map(|c| {
                    let verdict = if c.sources.is_empty() {
                        PromotionVerdict::Project
                    } else {
                        let v = verdicts[index];
                        index += 1;
                        v
                    };
                    (c.clone(), verdict)
                })
                .collect();
            report.forwards += self
                .store
                .commit_promotion_reviews(owner, project, &decisions, now_ms()?)
                .await?;
            report.classified += decisions.len();
            report.user_nodes.extend(
                decisions
                    .into_iter()
                    .filter(|(_, v)| *v == PromotionVerdict::User)
                    .map(|(c, _)| c.node.memory),
            );
        }
        Ok(report)
    }
}

#[cfg(all(test, feature = "router-http"))]
mod tests;
