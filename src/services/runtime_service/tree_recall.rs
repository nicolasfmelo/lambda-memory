//! Tree-only retrieval and bounded descent for one authorized project.
use super::*;
use crate::domain_models::RecallRequest;
use crate::domain_models::embeddings::Embedding;
use crate::domain_models::memory_tree::{Basis, TreeHit};
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
        exclude: &[MemoryId],
        trace: &mut OperationTrace<'_>,
    ) -> Result<Vec<TreeHit>, RuntimeError> {
        let owner = &self.scope.owner;
        let project = &request.project;
        let stage = trace
            .stage(project, ActivityPhase::Retrieval, &[], &[], None)
            .await?;
        let retrieved = async {
            let retrieval_limit = self
                .limits
                .max_candidates
                .saturating_add(exclude.len())
                .min(32);
            let lexical = if terms.is_empty() {
                Vec::new()
            } else {
                let nodes = self
                    .store
                    .tree_lexical_candidates(owner, project, terms, retrieval_limit)
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
                .tree_vector_hits(owner, project, embedding, retrieval_limit)
                .await?;
            Ok::<_, RuntimeError>(runtime_policy::fuse_hits(
                runtime_policy::exclude_hits(lexical, exclude),
                runtime_policy::exclude_hits(vector, exclude),
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
        let mut already_selected_children = Vec::new();
        for parent in &parents {
            let memory = &parent.item.revision.memory;
            let score = self
                .store
                .tree_node_score(owner, project, memory, embedding)
                .await?;
            for (child, child_score) in self
                .store
                .tree_child_scores(owner, project, memory, embedding)
                .await?
            {
                if !score.is_some_and(|score| child_score > score)
                    || exclude.contains(&child.item.revision.memory)
                {
                    continue;
                }
                if parents
                    .iter()
                    .any(|parent| parent.item.revision.id == child.item.revision.id)
                {
                    // Luna already selected this child. Keep its replacement effect without
                    // spending another selection slot (or call) on the same revision.
                    already_selected_children.push((child, memory.clone()));
                } else if !children.iter().any(
                    |(old, _, _): &(RuntimeHit, Option<MemoryId>, f32)| {
                        old.item.revision.id == child.item.revision.id
                    },
                ) {
                    children.push((child, Some(memory.clone()), child_score));
                }
            }
        }
        // Linked endpoints are additional context, not replacements for the endpoint that
        // reached them. No parent-score floor applies, and no further hops are traversed.
        for parent in &parents {
            for (linked, score) in self
                .store
                .tree_linked_scores(owner, project, &parent.item.revision.memory, embedding)
                .await?
            {
                if !exclude.contains(&linked.item.revision.memory)
                    && !parents
                        .iter()
                        .any(|parent| parent.item.revision.id == linked.item.revision.id)
                    && !children
                        .iter()
                        .any(|(old, _, _)| old.item.revision.id == linked.item.revision.id)
                {
                    children.push((linked, None, score));
                }
            }
        }
        children.sort_by(|a, b| {
            b.2.total_cmp(&a.2)
                .then(a.0.item.revision.id.cmp(&b.0.item.revision.id))
        });
        children.truncate(8);
        let candidates = children
            .iter()
            .map(|(hit, _, _)| hit.clone())
            .collect::<Vec<_>>();
        let selected_children = self
            .select_tree_hits(providers, request, terms, &candidates, trace)
            .await?;
        let nodes = self.store.tree_nodes(owner, project).await?;
        let contradictions = self.store.reflection_contradictions(owner, project).await?;
        let selected_observed = parents
            .iter()
            .chain(already_selected_children.iter().map(|(child, _)| child))
            .chain(selected_children.iter())
            .filter(|hit| {
                nodes
                    .iter()
                    .any(|n| n.memory == hit.item.revision.memory && n.basis == Basis::Observed)
            })
            .map(|hit| hit.item.revision.memory.clone())
            .collect::<Vec<_>>();
        let mut endpoints = Vec::new();
        for parent in parents {
            let replacements = already_selected_children
                .iter()
                .filter(|(_, owner)| owner == &parent.item.revision.memory)
                .map(|(child, _)| child)
                .chain(selected_children.iter().filter(|child| {
                    children.iter().any(|(candidate, owner, _)| {
                        candidate.item.revision.id == child.item.revision.id
                            && owner.as_ref() == Some(&parent.item.revision.memory)
                    })
                }))
                .filter(|child| {
                    !crate::engines::reflection_policy::conflicts(
                        &child.item.revision.memory,
                        &selected_observed,
                        &contradictions,
                    )
                })
                .cloned()
                .collect::<Vec<_>>();
            if replacements.is_empty() {
                endpoints.push(parent);
            } else {
                endpoints.extend(replacements);
            }
        }
        endpoints.extend(selected_children.into_iter().filter(|hit| {
            children.iter().any(|(candidate, parent, _)| {
                parent.is_none() && candidate.item.revision.id == hit.item.revision.id
            })
        }));
        let observed = endpoints
            .iter()
            .filter(|hit| {
                nodes
                    .iter()
                    .any(|n| n.memory == hit.item.revision.memory && n.basis == Basis::Observed)
            })
            .map(|hit| hit.item.revision.memory.clone())
            .collect::<Vec<_>>();
        endpoints.retain(|hit| {
            !crate::engines::reflection_policy::conflicts(
                &hit.item.revision.memory,
                &observed,
                &contradictions,
            )
        });
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
            let root = node.root.label();
            let mut path = vec![root.to_owned()];
            path.extend(
                self.store
                    .tree_ancestors(owner, project, &node.memory)
                    .await?
                    .into_iter()
                    .map(|ancestor| ancestor.title),
            );
            path.push(node.title.clone());
            let replaced = self.store.tree_replaced(&hit.item).await?;
            let label = crate::engines::reflection_policy::label(node.basis, node.confidence);
            let content = format!(
                "{label}{}{}",
                hit.item.revision.content,
                crate::engines::tree_policy::validity_note(node.valid_from_ms, replaced.as_ref())
            );
            result.push(TreeHit {
                basis: node.basis,
                confidence: node.confidence,
                derived_from: node.derived_from.clone(),
                content,
                valid_from_ms: node.valid_from_ms,
                replaced,
                root: node.root,
                hit,
                node: node.memory.clone(),
                path,
                source_count: node.direct_sources,
            });
        }
        Ok(result)
    }
}
