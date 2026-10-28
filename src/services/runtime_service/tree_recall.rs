//! Tree-only retrieval and bounded descent for one authorized project.
use super::*;
use crate::domain_models::RecallRequest;
use crate::domain_models::embeddings::Embedding;
use crate::domain_models::memory_tree::{TreeHit, TreeRoot};
use crate::engines::recall_policy;
use crate::integrations::external_apis::router_runtime::RouterRuntimeProviders;

impl MemoryRuntime {
    async fn select_tree_hits(
        &self,
        providers: &RouterRuntimeProviders,
        request: &RecallRequest,
        terms: &[String],
        hits: &[RuntimeHit],
        trace: &mut OperationTrace<'_>,
    ) -> Result<Vec<RuntimeHit>, RuntimeError> {
        if hits.is_empty() {
            return Ok(Vec::new());
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
                &request.project,
                ActivityPhase::Selection,
                &sources,
                &revisions,
                None,
            )
            .await?;
        let answer = providers.select(request, terms, hits, now_ms()?).await;
        trace
            .finish_stage(stage, &answer, ActivityCounts::default())
            .await;
        let (candidates, answer) = answer?;
        let ids = recall_policy::select_supported(request, &candidates, &answer)
            .into_iter()
            .map(|memory| {
                RevisionId::new(memory.id.as_str()).map_err(|_| RuntimeError::ProviderUnavailable)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let current = self
            .store
            .active_runtime_hits(
                &self.scope.owner,
                std::slice::from_ref(&request.project),
                &ids,
            )
            .await?;
        Ok(current
            .into_iter()
            .filter(|hit| {
                hits.iter().any(|old| {
                    old.item.revision.id == hit.item.revision.id
                        && old.item.revision.content == hit.item.revision.content
                        && old.item.sources == hit.item.sources
                })
            })
            .collect())
    }
    pub(super) async fn recall_tree(
        &self,
        providers: &RouterRuntimeProviders,
        request: &RecallRequest,
        terms: &[String],
        embedding: &Embedding,
        trace: &mut OperationTrace<'_>,
    ) -> Result<Vec<TreeHit>, RuntimeError> {
        let owner = &self.scope.owner;
        let project = &request.project;
        let stage = trace
            .stage(project, ActivityPhase::Retrieval, &[], &[], None)
            .await?;
        let retrieved = async {
            let lexical = if terms.is_empty() {
                Vec::new()
            } else {
                let nodes = self
                    .store
                    .tree_lexical_candidates(owner, project, terms, self.limits.max_candidates)
                    .await?;
                let ids = nodes
                    .into_iter()
                    .map(|node| node.revision)
                    .collect::<Vec<_>>();
                self.store
                    .active_runtime_hits(owner, std::slice::from_ref(project), &ids)
                    .await?
            };
            let vector = self
                .store
                .tree_vector_hits(owner, project, embedding, self.limits.max_candidates)
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
        let parents = self
            .select_tree_hits(providers, request, terms, &retrieved?, trace)
            .await?;
        let mut children = Vec::new();
        for parent in &parents {
            let memory = &parent.item.revision.memory;
            let Some(score) = self
                .store
                .tree_node_score(owner, project, memory, embedding)
                .await?
            else {
                continue;
            };
            for (child, child_score) in self
                .store
                .tree_child_scores(owner, project, memory, embedding)
                .await?
            {
                if child_score > score
                    && !children.iter().any(|(old, _): &(RuntimeHit, MemoryId)| {
                        old.item.revision.id == child.item.revision.id
                    })
                {
                    children.push((child, memory.clone()));
                }
            }
        }
        children.truncate(8);
        let candidates = children
            .iter()
            .map(|(hit, _)| hit.clone())
            .collect::<Vec<_>>();
        let selected_children = self
            .select_tree_hits(providers, request, terms, &candidates, trace)
            .await?;
        let nodes = self.store.tree_nodes(owner, project).await?;
        let mut endpoints = Vec::new();
        for parent in parents {
            let replacements = selected_children
                .iter()
                .filter(|child| {
                    children.iter().any(|(candidate, owner)| {
                        candidate.item.revision.id == child.item.revision.id
                            && owner == &parent.item.revision.memory
                    })
                })
                .cloned()
                .collect::<Vec<_>>();
            if replacements.is_empty() {
                endpoints.push(parent);
            } else {
                endpoints.extend(replacements);
            }
        }
        let mut result: Vec<TreeHit> = Vec::new();
        for hit in endpoints {
            if result
                .iter()
                .any(|old| old.hit.item.revision.id == hit.item.revision.id)
            {
                continue;
            }
            let Some(node) = nodes
                .iter()
                .find(|node| node.revision == hit.item.revision.id)
            else {
                continue;
            };
            let root = match node.root {
                TreeRoot::Decisions => "Decisions",
                TreeRoot::Preferences => "Preferences",
                TreeRoot::Architecture => "Architecture",
                TreeRoot::OpenItems => "Open items",
            };
            let mut path = vec![root.to_owned()];
            path.extend(
                self.store
                    .tree_ancestors(owner, project, &node.memory)
                    .await?
                    .into_iter()
                    .map(|ancestor| ancestor.title),
            );
            path.push(node.title.clone());
            result.push(TreeHit {
                hit,
                node: node.memory.clone(),
                path,
                source_count: node.direct_sources,
            });
        }
        Ok(result)
    }
}
