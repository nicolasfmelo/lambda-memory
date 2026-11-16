//! Source-bound cross-branch edges and the dedicated dependency refresh queue.
#[cfg(test)]
mod tests;
use super::*;
use crate::domain_models::memory_tree::{CurationLink, LinkFrom, LinkRelation, NodeLink};

#[derive(SurrealValue)]
pub(super) struct CrossLink {
    pub(super) from_memory: String,
    pub(super) to_memory: String,
    relation: String,
    sources: Vec<String>,
    created_at_ms: u64,
}
fn relation(value: &str) -> Result<LinkRelation, RuntimeError> {
    match value {
        "depends_on" => Ok(LinkRelation::DependsOn),
        "replaces" => Ok(LinkRelation::Replaces),
        "related" => Ok(LinkRelation::Related),
        _ => Err(RuntimeError::StorageUnavailable),
    }
}
impl EmbeddedRuntimeStore {
    pub(super) async fn cross_links_in(
        &self,
        db: &impl QueryConnection,
        owner: &OwnerId,
        project: &ProjectId,
        memory: &MemoryId,
    ) -> Result<Vec<CrossLink>, RuntimeError> {
        let mut response = db.query("SELECT from_memory, to_memory, relation, sources, created_at_ms FROM memory_link WHERE owner_id=$owner AND project_id=$project AND (from_memory=$memory OR to_memory=$memory) ORDER BY created_at_ms DESC, from_memory, to_memory, relation;")
            .bind(("owner", owner.to_string())).bind(("project", project.to_string())).bind(("memory", memory.to_string())).await.map_err(storage)?.check().map_err(storage)?;
        response.take(0).map_err(storage)
    }
    pub async fn tree_links(
        &self,
        owner: &OwnerId,
        project: &ProjectId,
        memory: &MemoryId,
    ) -> Result<Vec<NodeLink>, RuntimeError> {
        let _guard = self.capture.writes.lock().await;
        let nodes = self.tree_nodes_in(&self.capture.db, owner, project).await?;
        if !nodes.iter().any(|node| &node.memory == memory) {
            return Ok(vec![]);
        }
        let mut result = Vec::new();
        for link in self
            .cross_links_in(&self.capture.db, owner, project, memory)
            .await?
        {
            let outgoing = link.from_memory == memory.as_str();
            let other = if outgoing {
                &link.to_memory
            } else {
                &link.from_memory
            };
            if let Some(node) = nodes.iter().find(|node| node.memory.as_str() == other) {
                result.push(NodeLink {
                    node: node.memory.clone(),
                    title: node.title.clone(),
                    relation: relation(&link.relation)?,
                    outgoing,
                });
            }
        }
        Ok(result)
    }
    pub async fn tree_linked_scores(
        &self,
        owner: &OwnerId,
        project: &ProjectId,
        memory: &MemoryId,
        query: &Embedding,
    ) -> Result<Vec<(RuntimeHit, f32)>, RuntimeError> {
        let links = self.tree_links(owner, project, memory).await?;
        let nodes = self.tree_nodes(owner, project).await?;
        let ids: Vec<_> = nodes
            .into_iter()
            .filter(|node| links.iter().any(|link| link.node == node.memory))
            .map(|node| node.revision)
            .collect();
        let mut result = Vec::new();
        for ids in ids.chunks(32) {
            result.extend(self.tree_scores(owner, project, ids, query).await?);
        }
        Ok(result)
    }
    pub(super) async fn insert_cross_links_tx(
        &self,
        tx: &Transaction<Db>,
        owner: &OwnerId,
        project: &ProjectId,
        plan: &CurationCommit,
        revisions: &[RuntimeRevision],
        now_ms: u64,
    ) -> Result<(), RuntimeError> {
        for CurationLink {
            from,
            to,
            relation,
            sources,
        } in &plan.links
        {
            let from = match from {
                LinkFrom::Node(memory) => memory.clone(),
                LinkFrom::Created(index) => {
                    if *index >= plan.creates.len() {
                        return Err(RuntimeError::InvalidInput("link create index"));
                    }
                    MemoryId::new(format!(
                        "tree-node:{}",
                        stable(&[&plan.operation_id, &index.to_string()])
                    ))
                    .map_err(storage)?
                }
            };
            let revision = revisions
                .iter()
                .find(|item| item.revision.memory == from)
                .ok_or(RuntimeError::InvalidInput("link origin"))?;
            if from == *to
                || sources.is_empty()
                || sources
                    .iter()
                    .any(|source| !revision.revision.sources.contains(source))
                || (*relation == LinkRelation::Replaces
                    && !plan.supersedes.iter().any(|(id, _)| id == &from))
            {
                return Err(RuntimeError::InvalidInput("link bounds"));
            }
            let active = self
                .current_active(tx, owner.as_str(), project.as_str(), to.as_str())
                .await?
                .ok_or(RuntimeError::Conflict)?;
            let meta = self
                .meta_for(tx, owner, project, &active)
                .await?
                .ok_or(RuntimeError::Conflict)?;
            if meta.basis != "observed" {
                return Err(RuntimeError::InvalidInput("curator concluded link"));
            }
            // Existing edges keep their original evidence; never let the unique index roll back curation.
            tx.query("IF array::len((SELECT id FROM memory_link WHERE owner_id=$owner AND project_id=$project AND from_memory=$from AND to_memory=$to AND relation=$relation)) = 0 { CREATE memory_link SET owner_id=$owner, project_id=$project, from_memory=$from, to_memory=$to, relation=$relation, sources=$sources, created_at_ms=$at; };")
                .bind(("owner", owner.to_string())).bind(("project", project.to_string())).bind(("from", from.to_string())).bind(("to", to.to_string())).bind(("relation", relation.as_str())).bind(("sources", sources.iter().map(ToString::to_string).collect::<Vec<_>>())).bind(("at", now_ms)).await.map_err(storage)?.check().map_err(storage)?;
        }
        Ok(())
    }
    pub(super) async fn queue_dependents_tx(
        &self,
        tx: &Transaction<Db>,
        owner: &OwnerId,
        project: &ProjectId,
        dependency: &MemoryId,
        state: &mut TreeState,
    ) -> Result<(), RuntimeError> {
        let mut count = 0;
        for link in self.cross_links_in(tx, owner, project, dependency).await? {
            if link.to_memory != dependency.as_str() || link.relation != "depends_on" {
                continue;
            }
            let nodes = self.tree_nodes_in(tx, owner, project).await?;
            if !nodes.iter().any(|n| {
                n.memory.as_str() == link.from_memory
                    && n.basis == crate::domain_models::memory_tree::Basis::Observed
            }) {
                continue;
            }
            add_unique(
                &mut state.dependency_queue,
                format!("{}|{}", link.from_memory, link.to_memory),
            );
            count += 1;
            if count == 8 {
                break;
            }
        }
        Ok(())
    }
    pub(super) fn drop_dependency_entries(state: &mut TreeState, removed: &[String]) {
        state.dependency_queue.retain(|entry| {
            entry
                .split_once('|')
                .is_some_and(|(a, b)| !removed.iter().any(|id| id == a || id == b))
        });
    }
    pub(super) async fn delete_node_links_tx(
        &self,
        tx: &Transaction<Db>,
        owner: &OwnerId,
        project: &ProjectId,
        nodes: &[String],
    ) -> Result<(), RuntimeError> {
        tx.query("DELETE memory_link WHERE owner_id=$owner AND project_id=$project AND (from_memory IN $nodes OR to_memory IN $nodes) RETURN NONE; DELETE memory_contradiction WHERE owner_id=$owner AND project_id=$project AND (concluded_memory IN $nodes OR observed_memory IN $nodes) RETURN NONE;")
            .bind(("owner", owner.to_string())).bind(("project", project.to_string())).bind(("nodes", nodes.to_vec())).await.map_err(storage)?.check().map_err(storage)?;
        Ok(())
    }
    /// Queue entries are valid only while their exact directed depends_on edge survives.
    pub(super) async fn prune_dependency_queue_tx(
        &self,
        tx: &Transaction<Db>,
        owner: &OwnerId,
        project: &ProjectId,
    ) -> Result<(), RuntimeError> {
        let Some(mut state) = self.state_in(tx, owner, project).await? else {
            return Ok(());
        };
        let mut response = tx.query("SELECT from_memory, to_memory, relation, sources, created_at_ms FROM memory_link WHERE owner_id=$owner AND project_id=$project AND relation='depends_on';")
            .bind(("owner", owner.to_string())).bind(("project", project.to_string())).await.map_err(storage)?.check().map_err(storage)?;
        let links: Vec<CrossLink> = response.take(0).map_err(storage)?;
        let entries: HashSet<_> = links
            .iter()
            .map(|link| format!("{}|{}", link.from_memory, link.to_memory))
            .collect();
        state
            .dependency_queue
            .retain(|entry| entries.contains(entry));
        self.save_tree_state(tx, &state).await
    }
    pub async fn tree_dependency_exists(
        &self,
        owner: &OwnerId,
        project: &ProjectId,
        node: &MemoryId,
        dependency: &MemoryId,
    ) -> Result<bool, RuntimeError> {
        let mut response = self.capture.db.query("SELECT VALUE from_memory FROM memory_link WHERE owner_id=$owner AND project_id=$project AND from_memory=$node AND to_memory=$dependency AND relation='depends_on' LIMIT 1;")
            .bind(("owner", owner.to_string())).bind(("project", project.to_string())).bind(("node", node.to_string())).bind(("dependency", dependency.to_string())).await.map_err(storage)?.check().map_err(storage)?;
        let links: Vec<String> = response.take(0).map_err(storage)?;
        Ok(!links.is_empty())
    }
    #[cfg(test)]
    pub async fn test_queue_dependency(
        &self,
        owner: &OwnerId,
        project: &ProjectId,
        node: &MemoryId,
        dependency: &MemoryId,
    ) -> Result<(), RuntimeError> {
        let _guard = self.capture.writes.lock().await;
        let tx = self.capture.db.clone().begin().await.map_err(storage)?;
        let mut state = self.ensure_tree_state(&tx, owner, project).await?;
        add_unique(&mut state.dependency_queue, format!("{node}|{dependency}"));
        self.save_tree_state(&tx, &state).await?;
        tx.commit().await.map(|_| ()).map_err(storage)
    }
    pub async fn tree_dependency_peek(
        &self,
        owner: &OwnerId,
        project: &ProjectId,
    ) -> Result<Option<(MemoryId, MemoryId)>, RuntimeError> {
        let Some(state) = self.state_in(&self.capture.db, owner, project).await? else {
            return Ok(None);
        };
        let nodes = self.tree_nodes(owner, project).await?;
        state
            .dependency_queue
            .iter()
            .find(|entry| {
                entry.split_once('|').is_none_or(|(a, b)| {
                    !nodes.iter().any(|n| {
                        n.basis == crate::domain_models::memory_tree::Basis::Concluded
                            && (n.memory.as_str() == a || n.memory.as_str() == b)
                    })
                })
            })
            .map(|entry| {
                let (a, b) = entry
                    .split_once('|')
                    .ok_or(RuntimeError::StorageUnavailable)?;
                Ok((
                    MemoryId::new(a).map_err(storage)?,
                    MemoryId::new(b).map_err(storage)?,
                ))
            })
            .transpose()
    }
    pub async fn tree_dependency_pop(
        &self,
        owner: &OwnerId,
        project: &ProjectId,
        node: &MemoryId,
        dependency: &MemoryId,
    ) -> Result<(), RuntimeError> {
        let _guard = self.capture.writes.lock().await;
        let tx = self.capture.db.clone().begin().await.map_err(storage)?;
        self.pop_dependency_tx(&tx, owner, project, node, dependency)
            .await?;
        tx.commit().await.map(|_| ()).map_err(storage)
    }
    async fn pop_dependency_tx(
        &self,
        tx: &Transaction<Db>,
        owner: &OwnerId,
        project: &ProjectId,
        node: &MemoryId,
        dependency: &MemoryId,
    ) -> Result<(), RuntimeError> {
        let mut state = self.ensure_tree_state(tx, owner, project).await?;
        let entry = format!("{node}|{dependency}");
        state.dependency_queue.retain(|item| item != &entry);
        self.save_tree_state(tx, &state).await
    }
    /// The rewrite and queue acknowledgement are one transaction. Both heads are CAS guarded.
    pub async fn commit_dependency_refresh(
        &self,
        owner: &OwnerId,
        actor: &ActorId,
        project: &ProjectId,
        plan: MaintenanceCommit,
        dependency: &TreeNode,
        now_ms: u64,
    ) -> Result<Vec<RuntimeRevision>, RuntimeError> {
        self.validate_activity_scope(owner, project)?;
        let node = plan
            .rewrites
            .first()
            .ok_or(RuntimeError::InvalidInput("dependency rewrite"))?
            .0
            .clone();
        let _guard = self.capture.writes.lock().await;
        let tx = self.capture.db.clone().begin().await.map_err(storage)?;
        let result = async {
            if self
                .current_active(
                    &tx,
                    owner.as_str(),
                    project.as_str(),
                    dependency.memory.as_str(),
                )
                .await?
                .as_deref()
                != Some(dependency.revision.as_str())
            {
                return Err(RuntimeError::Conflict);
            }
            let items = self
                .commit_tree_maintenance_tx(&tx, owner, actor, project, &plan, now_ms)
                .await?;
            self.pop_dependency_tx(&tx, owner, project, &node, &dependency.memory)
                .await?;
            Ok(items)
        }
        .await;
        match result {
            Ok(items) => {
                tx.commit().await.map_err(storage)?;
                Ok(items)
            }
            Err(error) => {
                let _ = tx.cancel().await;
                Err(error)
            }
        }
    }
}
