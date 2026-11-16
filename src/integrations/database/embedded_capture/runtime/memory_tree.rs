//! Persistent state for curated memory trees.
mod cross_links;
mod reflection;
use super::*;
use crate::domain_models::memory_tree::TreeMode;

impl EmbeddedRuntimeStore {
    pub async fn tree_mode(
        &self,
        owner: &OwnerId,
        project: &ProjectId,
    ) -> Result<TreeMode, RuntimeError> {
        let _guard = self.capture.writes.lock().await;
        let mut response = self.capture.db.query("SELECT VALUE mode FROM memory_tree_state WHERE owner_id = $owner AND project_id = $project;")
            .bind(("owner", owner.to_string())).bind(("project", project.to_string()))
            .await.map_err(storage)?.check().map_err(storage)?;
        let modes: Vec<String> = response.take(0).map_err(storage)?;
        if let Some(mode) = modes.first() {
            return match mode.as_str() {
                "tree" => Ok(TreeMode::Tree),
                "flat" => Ok(TreeMode::Flat),
                _ => Err(RuntimeError::StorageUnavailable),
            };
        }
        let mut response = self.capture.db.query("LET $active = (SELECT VALUE revision_id FROM memory_revision_state WHERE owner_id = $owner AND project_id = $project AND state = 'active'); SELECT VALUE revision_id FROM memory_revision WHERE owner_id = $owner AND project_id = $project AND kind = 'knowledge' AND memory_id != $playbook AND revision_id IN $active LIMIT 1;")
            .bind(("owner", owner.to_string())).bind(("project", project.to_string()))
            .bind(("playbook", crate::domain_models::procedural::PLAYBOOK_MEMORY))
            .await.map_err(storage)?.check().map_err(storage)?;
        let rows: Vec<String> = response.take(1).map_err(storage)?;
        Ok(if rows.is_empty() {
            TreeMode::Tree
        } else {
            TreeMode::Flat
        })
    }

    pub async fn set_tree_mode(
        &self,
        owner: &OwnerId,
        project: &ProjectId,
        mode: TreeMode,
        now_ms: u64,
    ) -> Result<(), RuntimeError> {
        let _guard = self.capture.writes.lock().await;
        let tx = self.capture.db.clone().begin().await.map_err(storage)?;
        let mut response = tx.query("SELECT VALUE mode FROM memory_tree_state WHERE owner_id = $owner AND project_id = $project;")
            .bind(("owner", owner.to_string())).bind(("project", project.to_string()))
            .await.map_err(storage)?.check().map_err(storage)?;
        let rows: Vec<String> = response.take(0).map_err(storage)?;
        let name = match mode {
            TreeMode::Tree => "tree",
            TreeMode::Flat => "flat",
        };
        let sql = if rows.is_empty() {
            "CREATE memory_tree_state SET owner_id = $owner, project_id = $project, mode = $mode, migrated_at_ms = $at, stale = [], split_queue = [], rebuild_queue = [], dependency_queue = [], parked = [] RETURN NONE;"
        } else {
            "UPDATE memory_tree_state SET mode = $mode, migrated_at_ms = $at WHERE owner_id = $owner AND project_id = $project RETURN NONE;"
        };
        tx.query(sql)
            .bind(("owner", owner.to_string()))
            .bind(("project", project.to_string()))
            .bind(("mode", name))
            .bind((
                "at",
                if mode == TreeMode::Tree {
                    Some(now_ms)
                } else {
                    None
                },
            ))
            .await
            .map_err(storage)?
            .check()
            .map_err(storage)?;
        tx.commit().await.map_err(storage)?;
        Ok(())
    }
}

use crate::domain_models::embeddings::{Embedding, cosine};
use crate::domain_models::memory_tree::{
    CurationCommit, LinkOperation, MaintenanceCommit, ParentRef, ParkReason, StoreKind, TreeNode,
    TreeQueue, TreeRoot, roots,
};

#[derive(Clone, Debug, SurrealValue)]
struct TreeMeta {
    basis: String,
    confidence: Option<String>,
    derived_from: Vec<String>,
    owner_id: String,
    project_id: String,
    revision_id: String,
    memory_id: String,
    tree_root: String,
    parent_memory: Option<String>,
    depth: u64,
    title: String,
    valid_from_ms: Option<u64>,
    valid_until_ms: Option<u64>,
    valid_until_by: Option<String>,
}
#[derive(Clone, Debug, SurrealValue)]
struct TreeEdge {
    parent_memory: String,
    child_memory: String,
}
#[derive(Clone, Debug, SurrealValue)]
struct TreeLink {
    source_id: String,
    revision_id: String,
    operation: String,
    linked_at_ms: u64,
}
#[derive(Clone, Debug, SurrealValue)]
struct Parked {
    source_id: String,
    reason: String,
    expand_node: Option<String>,
    cycles: u64,
}
#[derive(Clone, Debug, SurrealValue)]
struct TreeState {
    owner_id: String,
    project_id: String,
    mode: String,
    migrated_at_ms: Option<u64>,
    stale: Vec<String>,
    split_queue: Vec<String>,
    rebuild_queue: Vec<String>,
    dependency_queue: Vec<String>,
    parked: Vec<Parked>,
}
const META: &str = "owner_id, project_id, revision_id, memory_id, tree_root, parent_memory, depth, title, valid_from_ms, valid_until_ms, valid_until_by, basis, confidence, derived_from";
const TREE_STATE: &str = "owner_id, project_id, mode, migrated_at_ms, stale, split_queue, rebuild_queue, dependency_queue, parked";
#[cfg(test)]
#[test]
fn root_parse_reads_all_slots() {
    for (slot, root, id, label) in [
        (
            "decisions",
            TreeRoot::Decisions,
            "tree-root:decisions",
            "Decisions",
        ),
        (
            "preferences",
            TreeRoot::Preferences,
            "tree-root:preferences",
            "Preferences",
        ),
        (
            "architecture",
            TreeRoot::Architecture,
            "tree-root:architecture",
            "Architecture",
        ),
        (
            "open_items",
            TreeRoot::OpenItems,
            "tree-root:open-items",
            "Open items",
        ),
        (
            "working_rules",
            TreeRoot::WorkingRules,
            "tree-root:working-rules",
            "Working rules",
        ),
        (
            "general_lessons",
            TreeRoot::GeneralLessons,
            "tree-root:general-lessons",
            "General lessons",
        ),
        ("lessons", TreeRoot::Lessons, "tree-root:lessons", "Lessons"),
    ] {
        assert_eq!(root_parse(slot), Ok(root));
        assert_eq!(root.as_str(), slot);
        assert_eq!(root.memory_id().as_str(), id);
        assert_eq!(root.label(), label);
    }
    assert_eq!(root_parse("unknown"), Err(RuntimeError::StorageUnavailable));
}

fn root_parse(s: &str) -> Result<TreeRoot, RuntimeError> {
    match s {
        "decisions" => Ok(TreeRoot::Decisions),
        "preferences" => Ok(TreeRoot::Preferences),
        "architecture" => Ok(TreeRoot::Architecture),
        "open_items" => Ok(TreeRoot::OpenItems),
        "working_rules" => Ok(TreeRoot::WorkingRules),
        "general_lessons" => Ok(TreeRoot::GeneralLessons),
        "lessons" => Ok(TreeRoot::Lessons),
        _ => Err(RuntimeError::StorageUnavailable),
    }
}
fn operation_name(op: LinkOperation) -> &'static str {
    match op {
        LinkOperation::Reflect => "reflect",
        LinkOperation::Create => "create",
        LinkOperation::Merge => "merge",
        LinkOperation::Supersede => "supersede",
        LinkOperation::Split => "split",
        LinkOperation::Rebuild => "rebuild",
        LinkOperation::Migrate => "migrate",
    }
}
fn parse_operation(s: &str) -> Result<LinkOperation, RuntimeError> {
    match s {
        "reflect" => Ok(LinkOperation::Reflect),
        "create" => Ok(LinkOperation::Create),
        "merge" => Ok(LinkOperation::Merge),
        "supersede" => Ok(LinkOperation::Supersede),
        "split" => Ok(LinkOperation::Split),
        "rebuild" => Ok(LinkOperation::Rebuild),
        "migrate" => Ok(LinkOperation::Migrate),
        _ => Err(RuntimeError::StorageUnavailable),
    }
}
fn park_reason(p: &Parked) -> Result<ParkReason, RuntimeError> {
    match p.reason.as_str() {
        "split" => Ok(ParkReason::Split),
        "expand" => Ok(ParkReason::Expand(
            MemoryId::new(
                p.expand_node
                    .clone()
                    .ok_or(RuntimeError::StorageUnavailable)?,
            )
            .map_err(storage)?,
        )),
        _ => Err(RuntimeError::StorageUnavailable),
    }
}
fn stable(parts: &[&str]) -> String {
    let mut h = Sha256::new();
    for p in parts {
        h.update((p.len() as u64).to_be_bytes());
        h.update(p.as_bytes());
    }
    format!("{:x}", h.finalize())
}
fn add_unique(queue: &mut Vec<String>, value: String) {
    if !queue.contains(&value) {
        queue.push(value);
    }
}

impl EmbeddedRuntimeStore {
    async fn state_in(
        &self,
        db: &impl QueryConnection,
        owner: &OwnerId,
        project: &ProjectId,
    ) -> Result<Option<TreeState>, RuntimeError> {
        let mut r = db.query(format!("SELECT {TREE_STATE} FROM memory_tree_state WHERE owner_id = $owner AND project_id = $project;"))
            .bind(("owner", owner.to_string())).bind(("project", project.to_string())).await.map_err(storage)?.check().map_err(storage)?;
        let rows: Vec<TreeState> = r.take(0).map_err(storage)?;
        Ok(rows.into_iter().next())
    }
    async fn ensure_tree_state(
        &self,
        tx: &Transaction<Db>,
        owner: &OwnerId,
        project: &ProjectId,
    ) -> Result<TreeState, RuntimeError> {
        if let Some(s) = self.state_in(tx, owner, project).await? {
            return Ok(s);
        }
        let mut r = tx.query("LET $active = (SELECT VALUE revision_id FROM memory_revision_state WHERE owner_id = $owner AND project_id = $project AND state = 'active'); SELECT VALUE revision_id FROM memory_revision WHERE owner_id = $owner AND project_id = $project AND kind = 'knowledge' AND memory_id != $playbook AND revision_id IN $active LIMIT 1;")
            .bind(("owner", owner.to_string())).bind(("project", project.to_string())).bind(("playbook", crate::domain_models::procedural::PLAYBOOK_MEMORY)).await.map_err(storage)?.check().map_err(storage)?;
        let rows: Vec<String> = r.take(1).map_err(storage)?;
        let state = TreeState {
            owner_id: owner.to_string(),
            project_id: project.to_string(),
            mode: if rows.is_empty() { "tree" } else { "flat" }.into(),
            migrated_at_ms: None,
            stale: vec![],
            split_queue: vec![],
            rebuild_queue: vec![],
            dependency_queue: vec![],
            parked: vec![],
        };
        self.create_row(
            tx,
            "CREATE memory_tree_state CONTENT $record RETURN NONE;",
            state.clone(),
        )
        .await?;
        Ok(state)
    }
    async fn save_tree_state(
        &self,
        tx: &Transaction<Db>,
        state: &TreeState,
    ) -> Result<(), RuntimeError> {
        tx.query("UPDATE memory_tree_state SET stale = $stale, split_queue = $split, rebuild_queue = $rebuild, dependency_queue = $dependency, parked = $parked WHERE owner_id = $owner AND project_id = $project RETURN NONE;")
            .bind(("owner", state.owner_id.clone())).bind(("project", state.project_id.clone())).bind(("stale", state.stale.clone())).bind(("split", state.split_queue.clone())).bind(("rebuild", state.rebuild_queue.clone())).bind(("dependency", state.dependency_queue.clone())).bind(("parked", state.parked.clone())).await.map_err(storage)?.check().map_err(storage)?;
        Ok(())
    }
    async fn meta_for(
        &self,
        db: &impl QueryConnection,
        owner: &OwnerId,
        project: &ProjectId,
        revision: &str,
    ) -> Result<Option<TreeMeta>, RuntimeError> {
        let mut r = db.query(format!("SELECT {META} FROM memory_tree_revision WHERE owner_id = $owner AND project_id = $project AND revision_id = $revision;"))
            .bind(("owner", owner.to_string())).bind(("project", project.to_string())).bind(("revision", revision.to_owned())).await.map_err(storage)?.check().map_err(storage)?;
        let rows: Vec<TreeMeta> = r.take(0).map_err(storage)?;
        Ok(rows.into_iter().next())
    }
    async fn tree_edges(
        &self,
        db: &impl QueryConnection,
        owner: &OwnerId,
        project: &ProjectId,
    ) -> Result<Vec<TreeEdge>, RuntimeError> {
        let mut r = db.query("SELECT parent_memory, child_memory FROM memory_tree_edge WHERE owner_id = $owner AND project_id = $project;").bind(("owner", owner.to_string())).bind(("project", project.to_string())).await.map_err(storage)?.check().map_err(storage)?;
        r.take(0).map_err(storage)
    }
    pub async fn tree_nodes(
        &self,
        owner: &OwnerId,
        project: &ProjectId,
    ) -> Result<Vec<TreeNode>, RuntimeError> {
        let _guard = self.capture.writes.lock().await;
        self.tree_nodes_in(&self.capture.db, owner, project).await
    }
    async fn tree_nodes_in(
        &self,
        db: &impl QueryConnection,
        owner: &OwnerId,
        project: &ProjectId,
    ) -> Result<Vec<TreeNode>, RuntimeError> {
        let mut r = db.query(format!("LET $active = (SELECT VALUE revision_id FROM memory_revision_state WHERE owner_id = $owner AND project_id = $project AND state = 'active'); SELECT {META} FROM memory_tree_revision WHERE owner_id = $owner AND project_id = $project AND revision_id IN $active; SELECT VALUE memory_id FROM memory_tree_revision WHERE owner_id=$owner AND project_id=$project AND basis='concluded';"))
            .bind(("owner", owner.to_string())).bind(("project", project.to_string())).await.map_err(storage)?.check().map_err(storage)?;
        let rows: Vec<TreeMeta> = r.take(1).map_err(storage)?;
        let edges = self.tree_edges(db, owner, project).await?;
        let concluded: HashSet<_> = r
            .take::<Vec<String>>(2)
            .map_err(storage)?
            .into_iter()
            .collect();
        let mut nodes = Vec::new();
        for row in rows {
            let revision = self
                .revision_row(db, owner.as_str(), &row.revision_id)
                .await?
                .ok_or(RuntimeError::StorageUnavailable)?;
            let mut r = db.query("SELECT source_id, revision_id, operation, linked_at_ms FROM memory_node_source WHERE owner_id = $owner AND project_id = $project AND memory_id = $memory;")
                .bind(("owner", owner.to_string())).bind(("project", project.to_string())).bind(("memory", row.memory_id.clone())).await.map_err(storage)?.check().map_err(storage)?;
            let links: Vec<TreeLink> = r.take(0).map_err(storage)?;
            let count = links
                .iter()
                .map(|x| &x.source_id)
                .collect::<HashSet<_>>()
                .len();
            let parent = edges
                .iter()
                .find(|e| e.child_memory == row.memory_id)
                .map(|e| e.parent_memory.clone())
                .or(row.parent_memory);
            nodes.push(TreeNode {
                basis: reflection::basis(&row.basis)?,
                confidence: row
                    .confidence
                    .as_deref()
                    .map(reflection::confidence)
                    .transpose()?,
                derived_from: row
                    .derived_from
                    .into_iter()
                    .map(|id| RevisionId::new(id).map_err(storage))
                    .collect::<Result<_, _>>()?,
                memory: MemoryId::new(row.memory_id.clone()).map_err(storage)?,
                revision: RevisionId::new(row.revision_id).map_err(storage)?,
                root: root_parse(&row.tree_root)?,
                parent: parent.map(MemoryId::new).transpose().map_err(storage)?,
                depth: u8::try_from(row.depth).map_err(storage)?,
                title: row.title,
                valid_from_ms: row.valid_from_ms,
                content: revision.content,
                direct_sources: u32::try_from(count).map_err(storage)?,
                children: u32::try_from(
                    edges
                        .iter()
                        .filter(|e| {
                            e.parent_memory == row.memory_id && !concluded.contains(&e.child_memory)
                        })
                        .count(),
                )
                .map_err(storage)?,
                last_linked_ms: links.iter().map(|x| x.linked_at_ms).max().unwrap_or(0),
            });
        }
        Ok(nodes)
    }
    /// Free operator backfill. Eligibility and writes share the writer lock and transaction.
    pub async fn backfill_timeline(
        &self,
        owner: &OwnerId,
        actor: &ActorId,
        project: &ProjectId,
        apply: bool,
        now_ms: u64,
        summary_limit: usize,
    ) -> Result<(usize, usize), RuntimeError> {
        if self.timeline_auto_policy.is_none() {
            return Err(RuntimeError::InvalidInput("timeline policy not configured"));
        }
        let _guard = self.capture.writes.lock().await;
        let tx = self.capture.db.clone().begin().await.map_err(storage)?;
        let result = async {
            let mut eligible = 0;
            let mut created = 0;
            for node in self.tree_nodes_in(&tx, owner, project).await? {
                if node.basis != crate::domain_models::memory_tree::Basis::Observed || node.root != TreeRoot::Decisions || node.children != 0 { continue; }
                let mut response = tx.query("RETURN array::len((SELECT entry_id FROM memory_timeline_version WHERE owner_id=$owner AND project_id=$project AND $memory IN memories));")
                    .bind(("owner", owner.to_string())).bind(("project", project.to_string())).bind(("memory", node.memory.to_string()))
                    .await.map_err(storage)?.check().map_err(storage)?;
                let covered: Option<usize> = response.take(0).map_err(storage)?;
                if covered.unwrap_or(0) != 0 { continue; }
                eligible += 1;
                if !apply { continue; }
                let mut response = tx.query("RETURN array::distinct((SELECT VALUE source_id FROM memory_node_source WHERE owner_id=$owner AND project_id=$project AND memory_id=$memory));")
                    .bind(("owner", owner.to_string())).bind(("project", project.to_string())).bind(("memory", node.memory.to_string()))
                    .await.map_err(storage)?.check().map_err(storage)?;
                let ids: Vec<String> = response.take(0).map_err(storage)?;
                let mut sources = Vec::new();
                for id in ids {
                    if let Some(row) = self.source_row(&tx, owner.as_str(), &id).await?.into_iter().next() { sources.push(source_event(row)?); }
                }
                if self.auto_timeline_commit_tx(&tx, owner, actor, project, &node, LinkOperation::Create, &sources, now_ms, summary_limit).await? == 0 { created += 1; }
            }
            Ok((eligible, created))
        }.await;
        match result {
            Ok(counts) => {
                tx.commit().await.map_err(storage)?;
                Ok(counts)
            }
            Err(error) => {
                let _ = tx.cancel().await;
                Err(error)
            }
        }
    }

    pub async fn tree_root_children(
        &self,
        owner: &OwnerId,
        project: &ProjectId,
    ) -> Result<Vec<(TreeRoot, u32)>, RuntimeError> {
        let _guard = self.capture.writes.lock().await;
        let edges = self.tree_edges(&self.capture.db, owner, project).await?;
        roots(StoreKind::of(project))
            .iter()
            .copied()
            .map(|r| {
                Ok((
                    r,
                    u32::try_from(
                        edges
                            .iter()
                            .filter(|e| e.parent_memory == r.memory_id().as_str())
                            .count(),
                    )
                    .map_err(storage)?,
                ))
            })
            .collect()
    }
    pub async fn tree_children(
        &self,
        owner: &OwnerId,
        project: &ProjectId,
        parent: &MemoryId,
    ) -> Result<Vec<TreeNode>, RuntimeError> {
        Ok(self
            .tree_nodes(owner, project)
            .await?
            .into_iter()
            .filter(|n| n.parent.as_ref() == Some(parent))
            .collect())
    }
    pub async fn tree_ancestors(
        &self,
        owner: &OwnerId,
        project: &ProjectId,
        memory: &MemoryId,
    ) -> Result<Vec<TreeNode>, RuntimeError> {
        let nodes = self.tree_nodes(owner, project).await?;
        let mut current = memory.clone();
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        while seen.insert(current.clone()) {
            let Some(node) = nodes.iter().find(|n| n.memory == current) else {
                break;
            };
            let Some(parent) = node.parent.as_ref() else {
                break;
            };
            if let Some(parent_node) = nodes.iter().find(|n| &n.memory == parent) {
                out.push(parent_node.clone());
                current = parent.clone();
            } else {
                break;
            }
        }
        out.reverse();
        Ok(out)
    }
    pub async fn tree_lexical_candidates(
        &self,
        owner: &OwnerId,
        project: &ProjectId,
        terms: &[String],
        limit: usize,
    ) -> Result<Vec<TreeNode>, RuntimeError> {
        if limit == 0 || limit > 32 || terms.len() > 12 || terms.iter().any(|s| s.len() > 256) {
            return Err(RuntimeError::InvalidInput("lexical query bounds"));
        }
        let mut nodes = self.tree_nodes(owner, project).await?;
        let mut unique = HashSet::new();
        let terms: Vec<_> = terms
            .iter()
            .map(|x| x.trim().to_lowercase())
            .filter(|x| !x.is_empty() && unique.insert(x.clone()))
            .collect();
        let mut scores = std::collections::HashMap::<String, usize>::new();
        for term in terms {
            let mut response = self.capture.db.query("SELECT VALUE revision_id FROM memory_revision WHERE owner_id=$owner AND project_id=$project AND string::lowercase(content) CONTAINS $term; SELECT VALUE revision_id FROM memory_tree_revision WHERE owner_id=$owner AND project_id=$project AND string::lowercase(title) CONTAINS $term;")
                .bind(("owner", owner.to_string())).bind(("project", project.to_string())).bind(("term", term)).await.map_err(storage)?.check().map_err(storage)?;
            let mut found: HashSet<String> = response
                .take::<Vec<String>>(0)
                .map_err(storage)?
                .into_iter()
                .collect();
            found.extend(response.take::<Vec<String>>(1).map_err(storage)?);
            for id in found {
                *scores.entry(id).or_default() += 1;
            }
        }
        nodes.retain(|node| scores.contains_key(node.revision.as_str()));
        nodes.sort_by(|a, b| {
            scores
                .get(b.revision.as_str())
                .cmp(&scores.get(a.revision.as_str()))
                .then_with(|| a.memory.as_str().cmp(b.memory.as_str()))
                .then_with(|| a.revision.as_str().cmp(b.revision.as_str()))
        });
        nodes.truncate(limit);
        Ok(nodes)
    }
}
impl EmbeddedRuntimeStore {
    pub async fn commit_curation(
        &self,
        owner: &OwnerId,
        actor: &ActorId,
        project: &ProjectId,
        plan: CurationCommit,
        now_ms: u64,
    ) -> Result<Vec<RuntimeRevision>, RuntimeError> {
        self.commit_curation_with_timeline(
            owner,
            actor,
            project,
            plan,
            now_ms,
            RuntimeLimits::default().max_content_bytes,
        )
        .await
        .map(|(items, _)| items)
    }

    pub async fn commit_curation_with_timeline(
        &self,
        owner: &OwnerId,
        actor: &ActorId,
        project: &ProjectId,
        plan: CurationCommit,
        now_ms: u64,
        summary_limit: usize,
    ) -> Result<(Vec<RuntimeRevision>, usize), RuntimeError> {
        if plan.operation == LinkOperation::Reflect
            || plan.operation_id.is_empty()
            || plan.sources.len() > 32
            || plan.sources.iter().collect::<HashSet<_>>().len() != plan.sources.len()
        {
            return Err(RuntimeError::InvalidInput("curation bounds"));
        }
        self.validate_activity_scope(owner, project)?;
        let _guard = self.capture.writes.lock().await;
        let tx = self.capture.db.clone().begin().await.map_err(storage)?;
        let result = self
            .commit_curation_tx(
                &tx,
                owner,
                actor,
                project,
                &plan,
                now_ms,
                true,
                summary_limit,
                None,
            )
            .await;
        match result {
            Ok(items) => {
                tx.commit().await.map_err(storage)?;
                Ok(items)
            }
            Err(e) => {
                let _ = tx.cancel().await;
                Err(e)
            }
        }
    }
    #[allow(clippy::too_many_arguments)]
    async fn commit_curation_tx(
        &self,
        tx: &Transaction<Db>,
        owner: &OwnerId,
        actor: &ActorId,
        project: &ProjectId,
        plan: &CurationCommit,
        now_ms: u64,
        consume_sources: bool,
        summary_limit: usize,
        reasoning: Option<&std::collections::HashMap<String, (String, Vec<String>)>>,
    ) -> Result<(Vec<RuntimeRevision>, usize), RuntimeError> {
        let mut events = Vec::new();
        for source in &plan.sources {
            let row = self
                .source_row(tx, owner.as_str(), source.as_str())
                .await?
                .into_iter()
                .next()
                .ok_or(RuntimeError::Missing)?;
            if row.project_id != project.as_str() {
                return Err(RuntimeError::Unauthorized);
            }
            events.push(source_event(row)?);
        }
        let parked: HashSet<_> = plan.parked.iter().map(|(s, _)| s.clone()).collect();
        if parked.len() != plan.parked.len() || parked.iter().any(|s| !plan.sources.contains(s)) {
            return Err(RuntimeError::InvalidInput("parked source bounds"));
        }
        if plan
            .promotes
            .iter()
            .any(|s| !plan.sources.contains(s) || parked.contains(s))
            || (!plan.promotes.is_empty() && StoreKind::of(project) != StoreKind::Project)
        {
            return Err(RuntimeError::InvalidInput("promotion bounds"));
        }
        for source in &plan.promotes {
            self.capture_forward_tx(tx, owner, project, source, now_ms)
                .await?;
        }
        if plan
            .supersedes
            .iter()
            .any(|(memory, _)| !plan.merges.iter().any(|m| &m.0 == memory))
            || plan
                .supersedes
                .iter()
                .map(|s| &s.0)
                .collect::<HashSet<_>>()
                .len()
                != plan.supersedes.len()
        {
            return Err(RuntimeError::InvalidInput("supersede bounds"));
        }
        let mut metas = Vec::new();
        for (memory, expected, title, content, sources) in &plan.merges {
            let active = self
                .current_active(tx, owner.as_str(), project.as_str(), memory.as_str())
                .await?;
            let (predecessor, root, parent, depth) = if active.as_deref() == Some(expected.as_str())
            {
                let meta = self
                    .meta_for(tx, owner, project, expected.as_str())
                    .await?
                    .ok_or(RuntimeError::Conflict)?;
                if meta.memory_id != memory.as_str() {
                    return Err(RuntimeError::Conflict);
                }
                if consume_sources && meta.basis != "observed" {
                    return Err(RuntimeError::InvalidInput("curator concluded target"));
                }
                if plan.operation == LinkOperation::Reflect && meta.basis != "concluded" {
                    return Err(RuntimeError::InvalidInput("reflection observed target"));
                }
                if matches!(
                    plan.operation,
                    LinkOperation::Split | LinkOperation::Rebuild
                ) && meta.basis == "concluded"
                {
                    return Err(RuntimeError::InvalidInput("maintenance concluded target"));
                }
                (
                    Some(expected.clone()),
                    meta.tree_root,
                    meta.parent_memory,
                    meta.depth,
                )
            } else if plan.operation == LinkOperation::Rebuild
                && active.is_none()
                && expected.as_str() == "tree-missing-head"
            {
                let edges = self.tree_edges(tx, owner, project).await?;
                let (root, parent, depth) =
                    orphan_geometry(StoreKind::of(project), &edges, memory)?;
                (
                    None,
                    root.as_str().to_owned(),
                    Some(parent.to_string()),
                    u64::from(depth),
                )
            } else {
                return Err(RuntimeError::Conflict);
            };
            metas.push((
                memory.clone(),
                predecessor,
                root,
                parent,
                depth,
                title.clone(),
                content.clone(),
                sources.clone(),
                false,
            ));
        }
        for (index, (parent, title, content, sources)) in plan.creates.iter().enumerate() {
            let (parent_id, root, depth) = match parent {
                ParentRef::Root(root) => (root.memory_id(), *root, 1),
                ParentRef::Node(node) => {
                    let active = self
                        .current_active(tx, owner.as_str(), project.as_str(), node.as_str())
                        .await?
                        .ok_or(RuntimeError::Conflict)?;
                    let meta = self
                        .meta_for(tx, owner, project, &active)
                        .await?
                        .ok_or(RuntimeError::Conflict)?;
                    if consume_sources && meta.basis != "observed" {
                        return Err(RuntimeError::InvalidInput("curator concluded parent"));
                    }
                    (node.clone(), root_parse(&meta.tree_root)?, meta.depth + 1)
                }
            };
            let memory = MemoryId::new(format!(
                "tree-node:{}",
                stable(&[&plan.operation_id, &index.to_string()])
            ))
            .map_err(storage)?;
            metas.push((
                memory,
                None,
                root.as_str().into(),
                Some(parent_id.to_string()),
                depth,
                title.clone(),
                content.clone(),
                sources.clone(),
                true,
            ));
        }
        let mut state = self.ensure_tree_state(tx, owner, project).await?;
        let mut result = Vec::new();
        let mut timeline_skipped = 0;
        let mut memories = HashSet::new();
        for (memory, predecessor, root, parent, depth, title, content, sources, is_create) in metas
        {
            if !memories.insert(memory.clone())
                || sources.is_empty()
                || sources.len() > 32
                || sources.iter().collect::<HashSet<_>>().len() != sources.len()
                || sources
                    .iter()
                    .any(|s| !plan.sources.contains(s) || parked.contains(s))
                || title.trim().is_empty()
                || content.trim().is_empty()
            {
                return Err(RuntimeError::InvalidInput("curation revision bounds"));
            }
            let revision_id = RevisionId::new(format!(
                "tree-revision:{}",
                stable(&[&plan.operation_id, memory.as_str()])
            ))
            .map_err(storage)?;
            let job_id = format!(
                "tree-job:{}",
                stable(&[&plan.operation_id, memory.as_str()])
            );
            let source_events: Vec<_> = sources
                .iter()
                .map(|s| {
                    events
                        .iter()
                        .find(|e| &e.source == s)
                        .cloned()
                        .ok_or(RuntimeError::Missing)
                })
                .collect::<Result<_, _>>()?;
            let watermark = now_ms.max(
                source_events
                    .iter()
                    .map(|e| e.recorded_at_ms)
                    .max()
                    .unwrap_or(0),
            );
            let prior = if let Some(id) = &predecessor {
                self.meta_for(tx, owner, project, id.as_str()).await?
            } else {
                None
            };
            let supersede = plan.supersedes.iter().find(|s| s.0 == memory).map(|s| s.1);
            let valid_from_ms = supersede.or_else(|| prior.as_ref().and_then(|m| m.valid_from_ms));
            if let Some(start) = supersede {
                let prior = prior.as_ref().ok_or(RuntimeError::Conflict)?;
                if prior.valid_from_ms.is_some_and(|old| start < old) {
                    return Err(RuntimeError::InvalidInput("supersede validity"));
                }
            }
            let reasoning_fields = reasoning.and_then(|r| r.get(memory.as_str()));
            let (basis, confidence, derived_from) =
                if let Some((confidence, derived)) = reasoning_fields {
                    (
                        "concluded".to_owned(),
                        Some(confidence.clone()),
                        derived.clone(),
                    )
                } else if !consume_sources && let Some(prior) = &prior {
                    (
                        prior.basis.clone(),
                        prior.confidence.clone(),
                        prior.derived_from.clone(),
                    )
                } else {
                    ("observed".to_owned(), None, vec![])
                };
            let revision = Revision {
                id: revision_id.clone(),
                memory: memory.clone(),
                owner: owner.clone(),
                project: project.clone(),
                kind: KnowledgeKind::Knowledge,
                status: RevisionStatus::PendingReview,
                content,
                prerequisites: vec![],
                sources: sources.clone(),
                predecessor,
                model: "anthropic/claude-haiku-5.5".into(),
                prompt_version: "tree-curation-v6".into(),
            };
            let output = encode_revision(&revision, &job_id, watermark);
            let job = JobRecord {
                owner_id: owner.to_string(),
                project_id: project.to_string(),
                job_id: job_id.clone(),
                watermark_ms: watermark,
                sources: source_events
                    .iter()
                    .cloned()
                    .map(SourceRecord::from)
                    .collect(),
                status: "completed".into(),
                revisions: vec![output.clone()],
            };
            self.create_row(tx, "CREATE memory_job CONTENT $record RETURN NONE;", job)
                .await?;
            self.create_row(
                tx,
                "CREATE memory_revision CONTENT $record RETURN NONE;",
                output,
            )
            .await?;
            self.ensure_state(
                tx,
                &StateRecord {
                    owner_id: owner.to_string(),
                    project_id: project.to_string(),
                    revision_id: revision_id.to_string(),
                    memory_id: memory.to_string(),
                    state: "processing".into(),
                    reviewed_at_ms: None,
                    reviewed_by: None,
                },
                false,
            )
            .await?;
            self.create_row(
                tx,
                "CREATE memory_tree_revision CONTENT $record RETURN NONE;",
                TreeMeta {
                    basis: basis.clone(),
                    confidence: confidence.clone(),
                    derived_from: derived_from.clone(),
                    owner_id: owner.to_string(),
                    project_id: project.to_string(),
                    revision_id: revision_id.to_string(),
                    memory_id: memory.to_string(),
                    tree_root: root.clone(),
                    parent_memory: parent.clone(),
                    depth,
                    title: title.clone(),
                    valid_from_ms,
                    valid_until_ms: None,
                    valid_until_by: None,
                },
            )
            .await?;
            tx.query("UPDATE skill_candidate SET status='stale' WHERE owner_id=$owner AND project_id=$project AND memory_id=$memory AND revision_id!=$revision AND status IN ['drafted','approved','installed'] RETURN NONE;")
                .bind(("owner", owner.to_string())).bind(("project", project.to_string())).bind(("memory", memory.to_string())).bind(("revision", revision_id.to_string())).await.map_err(storage)?.check().map_err(storage)?;
            if is_create {
                tx.query("CREATE memory_tree_edge SET owner_id=$owner, project_id=$project, parent_memory=$parent, child_memory=$child, created_at_ms=$at RETURN NONE;")
                .bind(("owner", owner.to_string())).bind(("project", project.to_string())).bind(("parent", parent.clone())).bind(("child", memory.to_string())).bind(("at", now_ms)).await.map_err(storage)?.check().map_err(storage)?;
            }
            for source in &sources {
                tx.query("CREATE memory_node_source SET owner_id=$owner, project_id=$project, memory_id=$memory, source_id=$source, revision_id=$revision, operation=$operation, linked_at_ms=$at RETURN NONE;")
                .bind(("owner", owner.to_string())).bind(("project", project.to_string())).bind(("memory", memory.to_string())).bind(("source", source.to_string())).bind(("revision", revision_id.to_string())).bind(("operation", operation_name(if supersede.is_some() { LinkOperation::Supersede } else if plan.operation == LinkOperation::Create && !is_create { LinkOperation::Merge } else { plan.operation }))).bind(("at", now_ms)).await.map_err(storage)?.check().map_err(storage)?;
            }
            if consume_sources
                && matches!(
                    plan.operation,
                    LinkOperation::Create | LinkOperation::Migrate
                )
            {
                let node = TreeNode {
                    basis: reflection::basis(&basis)?,
                    confidence: confidence
                        .as_deref()
                        .map(reflection::confidence)
                        .transpose()?,
                    derived_from: vec![],
                    memory: memory.clone(),
                    revision: revision_id.clone(),
                    root: root_parse(&root)?,
                    parent: parent
                        .clone()
                        .map(MemoryId::new)
                        .transpose()
                        .map_err(storage)?,
                    depth: u8::try_from(depth).map_err(storage)?,
                    title: title.clone(),
                    content: revision.content.clone(),
                    direct_sources: sources.len() as u32,
                    children: self
                        .tree_edges(tx, owner, project)
                        .await?
                        .iter()
                        .filter(|e| e.parent_memory == memory.as_str())
                        .count() as u32,
                    last_linked_ms: now_ms,
                    valid_from_ms,
                };
                let operation = if supersede.is_some() {
                    LinkOperation::Supersede
                } else if is_create {
                    plan.operation
                } else {
                    LinkOperation::Merge
                };
                timeline_skipped += self
                    .auto_timeline_commit_tx(
                        tx,
                        owner,
                        actor,
                        project,
                        &node,
                        operation,
                        &source_events,
                        now_ms,
                        summary_limit,
                    )
                    .await?;
            }
            result.push(RuntimeRevision {
                revision,
                state: LifecycleState::Processing,
                sources: source_events,
                reviewed_at_ms: None,
                watermark_ms: watermark,
            });
        }
        self.insert_cross_links_tx(tx, owner, project, plan, &result, now_ms)
            .await?;
        if consume_sources
            && matches!(
                plan.operation,
                LinkOperation::Create | LinkOperation::Migrate
            )
        {
            self.count_observed_revisions_tx(tx, owner, project, &result, now_ms)
                .await?;
        }
        if consume_sources {
            for (memory, _, _, _, _) in &plan.merges {
                self.queue_dependents_tx(tx, owner, project, memory, &mut state)
                    .await?;
            }
        }
        if consume_sources && plan.operation != LinkOperation::Migrate {
            let ids: Vec<String> = plan
                .sources
                .iter()
                .filter(|s| !parked.contains(*s))
                .map(ToString::to_string)
                .collect();
            let existing = self.consumption_for(tx, owner.as_str(), &ids).await?;
            if !existing.is_empty() {
                return Err(RuntimeError::Conflict);
            }
            for source in ids {
                self.create_row(
                    tx,
                    "CREATE memory_source_consumption CONTENT $record RETURN NONE;",
                    ConsumptionRecord {
                        owner_id: owner.to_string(),
                        project_id: project.to_string(),
                        source_id: source,
                        job_id: format!("tree-consume:{}", stable(&[&plan.operation_id])),
                    },
                )
                .await?;
            }
        }
        if plan.operation == LinkOperation::Migrate {
            // Migration does not consume sources, so it records every source it has handled,
            // discarded ones included, and a rerun never curates them again.
            for source in plan.sources.iter().filter(|s| !parked.contains(*s)) {
                tx.query("UPSERT memory_tree_migrated SET owner_id=$owner, project_id=$project, source_id=$source, operation_id=$operation WHERE owner_id=$owner AND project_id=$project AND source_id=$source RETURN NONE;")
                    .bind(("owner", owner.to_string())).bind(("project", project.to_string())).bind(("source", source.to_string())).bind(("operation", plan.operation_id.clone()))
                    .await.map_err(storage)?.check().map_err(storage)?;
            }
        }
        if plan.operation != LinkOperation::Reflect {
            let previous_parked = state.parked.clone();
            state
                .parked
                .retain(|p| !plan.sources.iter().any(|s| s.as_str() == p.source_id));
            for (source, reason) in &plan.parked {
                let (reason, node) = match reason {
                    ParkReason::Split => ("split", None),
                    ParkReason::Expand(n) => ("expand", Some(n.to_string())),
                };
                state.parked.push(Parked {
                    source_id: source.to_string(),
                    reason: reason.into(),
                    expand_node: node,
                    cycles: previous_parked
                        .iter()
                        .find(|p| p.source_id == source.as_str())
                        .map_or(1, |p| p.cycles.saturating_add(1)),
                });
            }
        }
        for id in &plan.stale {
            add_unique(&mut state.stale, id.to_string());
        }
        for id in &plan.split {
            add_unique(&mut state.split_queue, id.to_string());
        }
        self.save_tree_state(tx, &state).await?;
        Ok((result, timeline_skipped))
    }
}
impl EmbeddedRuntimeStore {
    pub async fn tree_queue_pop(
        &self,
        owner: &OwnerId,
        project: &ProjectId,
        queue: TreeQueue,
    ) -> Result<Option<MemoryId>, RuntimeError> {
        let _guard = self.capture.writes.lock().await;
        let tx = self.capture.db.clone().begin().await.map_err(storage)?;
        let Some(mut state) = self.state_in(&tx, owner, project).await? else {
            tx.cancel().await.map_err(storage)?;
            return Ok(None);
        };
        let list = match queue {
            TreeQueue::Stale => &mut state.stale,
            TreeQueue::Split => &mut state.split_queue,
            TreeQueue::Rebuild => &mut state.rebuild_queue,
        };
        let mut value = None;
        while !list.is_empty() {
            let id = list.remove(0);
            let mut r=tx.query("SELECT VALUE basis FROM memory_tree_revision WHERE owner_id=$owner AND project_id=$project AND memory_id=$memory AND basis='concluded' LIMIT 1;").bind(("owner",owner.to_string())).bind(("project",project.to_string())).bind(("memory",id.clone())).await.map_err(storage)?.check().map_err(storage)?;
            if r.take::<Vec<String>>(0).map_err(storage)?.is_empty() {
                value = Some(MemoryId::new(id).map_err(storage)?);
                break;
            }
        }
        self.save_tree_state(&tx, &state).await?;
        tx.commit().await.map_err(storage)?;
        Ok(value)
    }
    pub async fn parked_sources(
        &self,
        owner: &OwnerId,
        project: &ProjectId,
    ) -> Result<Vec<(CaptureEvent, ParkReason)>, RuntimeError> {
        let _guard = self.capture.writes.lock().await;
        let Some(state) = self.state_in(&self.capture.db, owner, project).await? else {
            return Ok(vec![]);
        };
        let mut result = Vec::new();
        for parked in state.parked {
            let rows = self
                .source_row(&self.capture.db, owner.as_str(), &parked.source_id)
                .await?;
            if let Some(row) = rows.into_iter().find(|r| r.project_id == project.as_str()) {
                result.push((source_event(row)?, park_reason(&parked)?));
            }
        }
        Ok(result)
    }
    pub async fn node_sources(
        &self,
        owner: &OwnerId,
        project: &ProjectId,
        memory: &MemoryId,
        cursor: Option<String>,
        limit: usize,
    ) -> Result<
        (
            Vec<(CaptureEvent, LinkOperation, RevisionId, u64)>,
            Option<String>,
        ),
        RuntimeError,
    > {
        if limit == 0 || limit > 32 {
            return Err(RuntimeError::InvalidInput("node source bounds"));
        }
        let after = cursor.unwrap_or_default();
        let _guard = self.capture.writes.lock().await;
        let mut r = self.capture.db.query("SELECT source_id, revision_id, operation, linked_at_ms FROM memory_node_source WHERE owner_id=$owner AND project_id=$project AND memory_id=$memory ORDER BY source_id, revision_id, linked_at_ms;")
            .bind(("owner", owner.to_string())).bind(("project", project.to_string())).bind(("memory", memory.to_string())).await.map_err(storage)?.check().map_err(storage)?;
        let rows: Vec<TreeLink> = r.take(0).map_err(storage)?;
        let mut out = Vec::new();
        let mut next = None;
        let mut more = false;
        for row in rows {
            let key = format!(
                "{}\u{1f}{}\u{1f}{:020}",
                row.source_id, row.revision_id, row.linked_at_ms
            );
            if key <= after {
                continue;
            }
            let source = self
                .source_row(&self.capture.db, owner.as_str(), &row.source_id)
                .await?
                .into_iter()
                .find(|r| r.project_id == project.as_str());
            if let Some(source) = source {
                if out.len() == limit {
                    more = true;
                    break;
                }
                next = Some(key);
                out.push((
                    source_event(source)?,
                    parse_operation(&row.operation)?,
                    RevisionId::new(row.revision_id).map_err(storage)?,
                    row.linked_at_ms,
                ));
            }
        }
        if !more {
            next = None;
        }
        Ok((out, next))
    }
    pub async fn node_history(
        &self,
        owner: &OwnerId,
        project: &ProjectId,
        memory: &MemoryId,
        limit: usize,
    ) -> Result<Vec<RuntimeRevision>, RuntimeError> {
        if limit == 0 || limit > 32 {
            return Err(RuntimeError::InvalidInput("node history bounds"));
        }
        let _guard = self.capture.writes.lock().await;
        let mut r = self.capture.db.query(format!("LET $ids = (SELECT VALUE revision_id FROM memory_tree_revision WHERE owner_id=$owner AND project_id=$project AND memory_id=$memory); SELECT {REV_FIELDS} FROM memory_revision WHERE owner_id=$owner AND project_id=$project AND revision_id IN $ids ORDER BY watermark_ms DESC, revision_id DESC LIMIT $limit;"))
            .bind(("owner", owner.to_string())).bind(("project", project.to_string())).bind(("memory", memory.to_string())).bind(("limit", limit)).await.map_err(storage)?.check().map_err(storage)?;
        let rows: Vec<RevisionRecord> = r.take(1).map_err(storage)?;
        let (states, jobs) = self.selected_metadata(owner, rows.iter()).await?;
        rows.into_iter()
            .map(|r| self.assemble(r, &states, &jobs))
            .collect()
    }
    pub async fn tree_vector_hits(
        &self,
        owner: &OwnerId,
        project: &ProjectId,
        query: &Embedding,
        limit: usize,
    ) -> Result<Vec<RuntimeHit>, RuntimeError> {
        if limit == 0
            || limit > 32
            || query.values.len() != query.space.dimensions
            || cosine(&query.values, &query.values).is_none()
        {
            return Err(RuntimeError::InvalidInput("vector query bounds"));
        }
        #[derive(SurrealValue)]
        struct Rank {
            revision_id: String,
            score: f64,
        }
        let mut r = self.capture.db.query("LET $active = (SELECT VALUE revision_id FROM memory_revision_state WHERE owner_id=$owner AND project_id=$project AND state='active'); LET $nodes = (SELECT VALUE revision_id FROM memory_tree_revision WHERE owner_id=$owner AND project_id=$project); SELECT revision_id, vector::similarity::cosine(embedding, $vector) AS score FROM memory_runtime_embedding WHERE owner_id=$owner AND project_id=$project AND provider=$provider AND model=$model AND index_revision=$index AND dimensions=$dimensions AND revision_id IN $active AND revision_id IN $nodes ORDER BY score DESC, revision_id LIMIT $limit;")
            .bind(("owner",owner.to_string())).bind(("project",project.to_string())).bind(("provider",query.space.provider.clone())).bind(("model",query.space.model.clone())).bind(("index",query.space.revision.clone())).bind(("dimensions",query.space.dimensions)).bind(("vector",query.values.clone())).bind(("limit",limit)).await.map_err(storage)?.check().map_err(storage)?;
        let rows: Vec<Rank> = r.take(2).map_err(storage)?;
        let ids = rows
            .into_iter()
            .map(|r| {
                if r.score.is_finite() {
                    RevisionId::new(r.revision_id).map_err(storage)
                } else {
                    Err(RuntimeError::StorageUnavailable)
                }
            })
            .collect::<Result<Vec<_>, _>>()?;
        self.active_runtime_hits(owner, std::slice::from_ref(project), &ids)
            .await
    }
    pub async fn retire_node(
        &self,
        owner: &OwnerId,
        actor: &ActorId,
        project: &ProjectId,
        memory: &MemoryId,
        now_ms: u64,
    ) -> Result<(), RuntimeError> {
        self.retire_node_with_timeline_skips(owner, actor, project, memory, now_ms)
            .await
            .map(|_| ())
    }

    pub async fn retire_node_with_timeline_skips(
        &self,
        owner: &OwnerId,
        actor: &ActorId,
        project: &ProjectId,
        memory: &MemoryId,
        now_ms: u64,
    ) -> Result<usize, RuntimeError> {
        self.validate_activity_scope(owner, project)?;
        let _guard = self.capture.writes.lock().await;
        let tx = self.capture.db.clone().begin().await.map_err(storage)?;
        let result = self
            .retire_tree_tx(&tx, owner, actor, project, memory, now_ms)
            .await;
        match result {
            Ok(skipped) => {
                self.finish_attached_timeline_activity(&tx, &[], &[], None, skipped)
                    .await?;
                tx.commit().await.map_err(storage)?;
                Ok(skipped)
            }
            Err(e) => {
                let _ = tx.cancel().await;
                Err(e)
            }
        }
    }
    pub(super) async fn retire_tree_tx(
        &self,
        tx: &Transaction<Db>,
        owner: &OwnerId,
        actor: &ActorId,
        project: &ProjectId,
        memory: &MemoryId,
        now_ms: u64,
    ) -> Result<usize, RuntimeError> {
        let mut r=tx.query("SELECT VALUE revision_id FROM memory_tree_revision WHERE owner_id=$owner AND project_id=$project AND memory_id=$memory;").bind(("owner",owner.to_string())).bind(("project",project.to_string())).bind(("memory",memory.to_string())).await.map_err(storage)?.check().map_err(storage)?;
        let ids: Vec<String> = r.take(0).map_err(storage)?;
        let skipped = self
            .retire_tree_only_tx(tx, owner, actor, project, memory, now_ms)
            .await?;
        self.cascade_conclusions_tx(tx, owner, actor, project, &ids, now_ms)
            .await?;
        Ok(skipped)
    }
    /// Shared audited retirement: keep immutable evidence, remove the edge, reject the head.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn retire_audited_tree_tx(
        &self,
        tx: &Transaction<Db>,
        owner: &OwnerId,
        actor: &ActorId,
        project: &ProjectId,
        memory: &MemoryId,
        now: u64,
        action: &str,
    ) -> Result<usize, RuntimeError> {
        let active = self
            .current_active(tx, owner.as_str(), project.as_str(), memory.as_str())
            .await?
            .ok_or(RuntimeError::Missing)?;
        let skipped = self
            .retire_tree_only_tx(tx, owner, actor, project, memory, now)
            .await?;
        self.write_state(
            tx,
            owner.as_str(),
            project.as_str(),
            memory.as_str(),
            &active,
            LifecycleState::Rejected,
            Some(now),
            Some(actor.as_str()),
        )
        .await?;
        self.create_row(
            tx,
            "CREATE memory_lifecycle_audit CONTENT $record RETURN NONE;",
            AuditRecord {
                owner_id: owner.to_string(),
                project_id: project.to_string(),
                revision_id: active.clone(),
                actor_id: actor.to_string(),
                action: action.into(),
                from_state: "active".into(),
                to_state: "rejected".into(),
                expected_active: Some(active),
                conflict_with: None,
                at_ms: now,
            },
        )
        .await?;
        Ok(skipped)
    }
    async fn retire_tree_only_tx(
        &self,
        tx: &Transaction<Db>,
        owner: &OwnerId,
        actor: &ActorId,
        project: &ProjectId,
        memory: &MemoryId,
        now_ms: u64,
    ) -> Result<usize, RuntimeError> {
        let skipped = self
            .auto_timeline_retire_tx(tx, owner, actor, project, memory, now_ms)
            .await?;
        let edges = self.tree_edges(tx, owner, project).await?;
        let edge = edges
            .iter()
            .find(|e| e.child_memory == memory.as_str())
            .ok_or(RuntimeError::Missing)?;
        if let Some(active) = self
            .current_active(tx, owner.as_str(), project.as_str(), memory.as_str())
            .await?
        {
            tx.query("UPDATE memory_revision_state SET state='superseded', reviewed_at_ms=$at, reviewed_by=$actor WHERE owner_id=$owner AND project_id=$project AND revision_id=$revision RETURN NONE; DELETE memory_active_head WHERE owner_id=$owner AND project_id=$project AND memory_id=$memory RETURN NONE;")
                .bind(("owner",owner.to_string())).bind(("project",project.to_string())).bind(("memory",memory.to_string())).bind(("revision",active)).bind(("actor",actor.to_string())).bind(("at",now_ms)).await.map_err(storage)?.check().map_err(storage)?;
        }
        tx.query("UPDATE memory_tree_edge SET parent_memory=$parent WHERE owner_id=$owner AND project_id=$project AND parent_memory=$memory RETURN NONE; DELETE memory_tree_edge WHERE owner_id=$owner AND project_id=$project AND child_memory=$memory RETURN NONE; UPDATE memory_tree_revision SET parent_memory=$parent WHERE owner_id=$owner AND project_id=$project AND parent_memory=$memory RETURN NONE;")
            .bind(("owner",owner.to_string())).bind(("project",project.to_string())).bind(("memory",memory.to_string())).bind(("parent",edge.parent_memory.clone())).await.map_err(storage)?.check().map_err(storage)?;
        let mut descendants = vec![memory.to_string()];
        let mut cursor = 0;
        while cursor < descendants.len() {
            let parent_id = descendants[cursor].clone();
            for e in edges.iter().filter(|e| e.parent_memory == parent_id) {
                if !descendants.contains(&e.child_memory) {
                    descendants.push(e.child_memory.clone());
                }
            }
            cursor += 1;
        }
        descendants.remove(0);
        tx.query("UPDATE memory_tree_revision SET depth = depth - 1 WHERE owner_id=$owner AND project_id=$project AND memory_id IN $ids RETURN NONE;")
            .bind(("owner",owner.to_string())).bind(("project",project.to_string())).bind(("ids",descendants)).await.map_err(storage)?.check().map_err(storage)?;
        let mut state = self.ensure_tree_state(tx, owner, project).await?;
        let mut r=tx.query("SELECT VALUE basis FROM memory_tree_revision WHERE owner_id=$owner AND project_id=$project AND memory_id=$memory AND basis='concluded' LIMIT 1;").bind(("owner",owner.to_string())).bind(("project",project.to_string())).bind(("memory",memory.to_string())).await.map_err(storage)?.check().map_err(storage)?;
        if r.take::<Vec<String>>(0).map_err(storage)?.is_empty() {
            add_unique(&mut state.stale, edge.parent_memory.clone());
        }
        state.stale.retain(|id| id != memory.as_str());
        state.split_queue.retain(|id| id != memory.as_str());
        state.rebuild_queue.retain(|id| id != memory.as_str());
        Self::drop_dependency_entries(&mut state, &[memory.to_string()]);
        self.delete_node_links_tx(tx, owner, project, &[memory.to_string()])
            .await?;
        self.save_tree_state(tx, &state).await?;
        Ok(skipped)
    }
    pub(super) async fn purge_tree_tx(
        &self,
        tx: &Transaction<Db>,
        owner: &OwnerId,
        project: &ProjectId,
        sources: &[String],
        revisions: &[String],
    ) -> Result<(), RuntimeError> {
        let actor = ActorId::new("system:reflection-purge").map_err(storage)?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(storage)?
            .as_millis()
            .min(u64::MAX as u128) as u64;
        self.cascade_conclusions_tx(tx, owner, &actor, project, revisions, now)
            .await?;
        let mut r = tx.query("LET $deleted = (SELECT VALUE revision_id FROM memory_revision_state WHERE owner_id=$owner AND project_id=$project AND state='active' AND revision_id IN $ids); SELECT VALUE memory_id FROM memory_tree_revision WHERE owner_id=$owner AND project_id=$project AND revision_id IN $deleted;")
            .bind(("owner",owner.to_string())).bind(("project",project.to_string())).bind(("ids",revisions.to_vec())).await.map_err(storage)?.check().map_err(storage)?;
        let nodes: Vec<String> = r.take(1).map_err(storage)?;
        if !nodes.is_empty() || self.state_in(tx, owner, project).await?.is_some() {
            let mut state = self.ensure_tree_state(tx, owner, project).await?;
            state
                .parked
                .retain(|parked| !sources.contains(&parked.source_id));
            for node in &nodes {
                add_unique(&mut state.rebuild_queue, node.clone());
            }
            Self::drop_dependency_entries(&mut state, &nodes);
            self.save_tree_state(tx, &state).await?;
        }
        self.delete_node_links_tx(tx, owner, project, &nodes)
            .await?;
        tx.query("UPDATE memory_link SET sources = array::difference(sources, $sources) WHERE owner_id=$owner AND project_id=$project; DELETE memory_link WHERE owner_id=$owner AND project_id=$project AND array::len(sources)=0 RETURN NONE;")
            .bind(("owner", owner.to_string())).bind(("project", project.to_string())).bind(("sources", sources.to_vec())).await.map_err(storage)?.check().map_err(storage)?;
        self.prune_dependency_queue_tx(tx, owner, project).await?;
        self.delete_in(tx, "DELETE memory_node_source WHERE owner_id=$owner AND project_id=$project AND source_id IN $ids RETURN NONE;", owner, project, sources).await?;
        self.delete_in(tx, "DELETE memory_tree_migrated WHERE owner_id=$owner AND project_id=$project AND source_id IN $ids RETURN NONE;", owner, project, sources).await?;
        // Reviews belong to these exact revisions, including when retention purges them.
        // Retirement keeps revisions and their reviews; only physical deletion cascades here.
        self.delete_in(tx, "DELETE memory_promotion_review WHERE owner_id=$owner AND project_id=$project AND revision_id IN $ids RETURN NONE;", owner, project, revisions).await?;
        self.delete_in(tx, "DELETE memory_tree_revision WHERE owner_id=$owner AND project_id=$project AND revision_id IN $ids RETURN NONE;", owner, project, revisions).await?;
        Ok(())
    }
}

impl EmbeddedRuntimeStore {
    pub async fn tree_history_entry(
        &self,
        item: RuntimeRevision,
    ) -> Result<crate::domain_models::memory_tree::NodeHistory, RuntimeError> {
        let meta = self
            .meta_for(
                &self.capture.db,
                &item.revision.owner,
                &item.revision.project,
                item.revision.id.as_str(),
            )
            .await?
            .ok_or(RuntimeError::Missing)?;
        Ok(crate::domain_models::memory_tree::NodeHistory {
            basis: reflection::basis(&meta.basis)?,
            confidence: meta
                .confidence
                .as_deref()
                .map(reflection::confidence)
                .transpose()?,
            derived_from: meta
                .derived_from
                .into_iter()
                .map(|id| RevisionId::new(id).map_err(storage))
                .collect::<Result<_, _>>()?,
            item,
            valid_from_ms: meta.valid_from_ms,
            valid_until_ms: meta.valid_until_ms,
            valid_until_by: meta
                .valid_until_by
                .map(RevisionId::new)
                .transpose()
                .map_err(storage)?,
        })
    }

    /// The nearest closed predecessor is the latest position replaced, even after refinements.
    pub async fn tree_replaced(
        &self,
        item: &RuntimeRevision,
    ) -> Result<Option<crate::domain_models::memory_tree::ReplacedPosition>, RuntimeError> {
        let mut predecessor = item.revision.predecessor.as_ref().map(ToString::to_string);
        for _ in 0..16 {
            let Some(id) = predecessor else { break };
            let Some(meta) = self
                .meta_for(
                    &self.capture.db,
                    &item.revision.owner,
                    &item.revision.project,
                    &id,
                )
                .await?
            else {
                break;
            };
            if let Some(until) = meta.valid_until_ms {
                return Ok(Some(crate::domain_models::memory_tree::ReplacedPosition {
                    title: meta.title,
                    valid_until_ms: until,
                }));
            }
            predecessor = self
                .revision_row(&self.capture.db, item.revision.owner.as_str(), &id)
                .await?
                .and_then(|r| r.predecessor);
        }
        Ok(None)
    }

    /// Publication owns the closure: pending, rejected and retired revisions have no effect.
    /// The supersede being published is the active successor, so it always defines the
    /// predecessor's closure (date and owner), also when a conflict resolution replaces another one.
    pub(super) async fn close_predecessor_tx(
        &self,
        tx: &Transaction<Db>,
        owner: &OwnerId,
        record: &RevisionRecord,
    ) -> Result<(), RuntimeError> {
        let Some(predecessor) = &record.predecessor else {
            return Ok(());
        };
        let project = ProjectId::new(record.project_id.clone()).map_err(storage)?;
        let Some(meta) = self
            .meta_for(tx, owner, &project, &record.revision_id)
            .await?
        else {
            return Ok(());
        };
        let Some(start) = meta.valid_from_ms else {
            return Ok(());
        };
        tx.query("LET $supersede = (SELECT VALUE revision_id FROM memory_node_source WHERE owner_id=$owner AND project_id=$project AND revision_id=$revision AND operation='supersede'); IF array::len($supersede) > 0 { UPDATE memory_tree_revision SET valid_until_ms=$until, valid_until_by=$revision WHERE owner_id=$owner AND project_id=$project AND revision_id=$predecessor; };")
            .bind(("owner", owner.to_string())).bind(("project", record.project_id.clone())).bind(("revision", record.revision_id.clone())).bind(("predecessor", predecessor.clone())).bind(("until", start)).await.map_err(storage)?.check().map_err(storage)?;
        Ok(())
    }

    /// Text for indexing a tree revision, including its title.
    pub async fn tree_text(&self, item: &RuntimeRevision) -> Result<Option<String>, RuntimeError> {
        Ok(self
            .meta_for(
                &self.capture.db,
                &item.revision.owner,
                &item.revision.project,
                item.revision.id.as_str(),
            )
            .await?
            .map(|meta| format!("{}\n{}", meta.title, item.revision.content)))
    }

    /// Parked sources eligible for their second curation cycle.
    pub async fn tree_parked_cycles(
        &self,
        owner: &OwnerId,
        project: &ProjectId,
    ) -> Result<Vec<(CaptureEvent, ParkReason, u64)>, RuntimeError> {
        let _guard = self.capture.writes.lock().await;
        let Some(state) = self.state_in(&self.capture.db, owner, project).await? else {
            return Ok(vec![]);
        };
        let mut out = Vec::new();
        for parked in state.parked {
            let rows = self
                .source_row(&self.capture.db, owner.as_str(), &parked.source_id)
                .await?;
            if let Some(row) = rows.into_iter().find(|r| r.project_id == project.as_str()) {
                out.push((source_event(row)?, park_reason(&parked)?, parked.cycles));
            }
        }
        Ok(out)
    }
}

impl EmbeddedRuntimeStore {
    pub async fn tree_status(
        &self,
        owner: &OwnerId,
        project: &ProjectId,
    ) -> Result<crate::domain_models::runtime_diagnostics::TreeStatus, RuntimeError> {
        let mode = self.tree_mode(owner, project).await?;
        let nodes_active = self.tree_nodes(owner, project).await?.len() as u64;
        let state = self.state_in(&self.capture.db, owner, project).await?;
        Ok(crate::domain_models::runtime_diagnostics::TreeStatus {
            mode: match mode {
                TreeMode::Flat => "flat",
                TreeMode::Tree => "tree",
            },
            nodes_active,
            stale: state.as_ref().map_or(0, |s| s.stale.len() as u64),
            split_queue: state.as_ref().map_or(0, |s| s.split_queue.len() as u64),
            parked_sources: state.as_ref().map_or(0, |s| s.parked.len() as u64),
        })
    }
}

impl EmbeddedRuntimeStore {
    pub async fn tree_admitted_sources(
        &self,
        owner: &OwnerId,
        project: &ProjectId,
    ) -> Result<Vec<SourceId>, RuntimeError> {
        let mut r = self.capture.db.query("SELECT VALUE sources FROM memory_runtime_operation WHERE owner_id=$owner AND project_id=$project AND string::starts_with(operation_id, 'curation-job:');")
            .bind(("owner", owner.to_string())).bind(("project", project.to_string())).await.map_err(storage)?.check().map_err(storage)?;
        let rows: Vec<Vec<String>> = r.take(0).map_err(storage)?;
        rows.into_iter()
            .flatten()
            .map(|s| SourceId::new(s).map_err(storage))
            .collect()
    }
}

#[derive(SurrealValue)]
struct MaintenanceLink {
    source_id: String,
    memory_id: String,
}

impl EmbeddedRuntimeStore {
    /// Atomically writes maintenance revisions and moves tree edges/source links without consuming sources.
    pub async fn commit_tree_maintenance(
        &self,
        owner: &OwnerId,
        actor: &ActorId,
        project: &ProjectId,
        plan: MaintenanceCommit,
        now_ms: u64,
    ) -> Result<Vec<RuntimeRevision>, RuntimeError> {
        if !matches!(
            plan.operation,
            LinkOperation::Merge | LinkOperation::Split | LinkOperation::Rebuild
        ) || plan.operation_id.is_empty()
        {
            return Err(RuntimeError::InvalidInput("maintenance operation"));
        }
        if plan
            .reparent
            .iter()
            .map(|(child, _)| child)
            .collect::<HashSet<_>>()
            .len()
            != plan.reparent.len()
            || plan
                .move_sources
                .iter()
                .map(|(source, from, _)| (source, from))
                .collect::<HashSet<_>>()
                .len()
                != plan.move_sources.len()
        {
            return Err(RuntimeError::InvalidInput("duplicate maintenance move"));
        }
        self.validate_activity_scope(owner, project)?;
        let _guard = self.capture.writes.lock().await;
        let tx = self.capture.db.clone().begin().await.map_err(storage)?;
        let result = self
            .commit_tree_maintenance_tx(&tx, owner, actor, project, &plan, now_ms)
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
    async fn commit_tree_maintenance_tx(
        &self,
        tx: &Transaction<Db>,
        owner: &OwnerId,
        actor: &ActorId,
        project: &ProjectId,
        plan: &MaintenanceCommit,
        now_ms: u64,
    ) -> Result<Vec<RuntimeRevision>, RuntimeError> {
        let edges = self.tree_edges(tx, owner, project).await?;
        let mut r = tx.query("SELECT source_id, memory_id FROM memory_node_source WHERE owner_id=$owner AND project_id=$project;")
            .bind(("owner", owner.to_string())).bind(("project", project.to_string())).await.map_err(storage)?.check().map_err(storage)?;
        let links: Vec<MaintenanceLink> = r.take(0).map_err(storage)?;
        let belongs = |source: &SourceId, ancestor: &MemoryId| -> bool {
            links
                .iter()
                .filter(|l| l.source_id == source.as_str())
                .any(|l| {
                    let mut current = l.memory_id.as_str();
                    let mut seen = HashSet::new();
                    while seen.insert(current) {
                        if current == ancestor.as_str() {
                            return true;
                        }
                        let Some(edge) = edges.iter().find(|e| e.child_memory == current) else {
                            break;
                        };
                        current = &edge.parent_memory;
                    }
                    false
                })
        };
        for (memory, _, _, _, sources) in &plan.rewrites {
            if sources.is_empty() || sources.iter().any(|s| !belongs(s, memory)) {
                return Err(RuntimeError::InvalidInput("maintenance source scope"));
            }
        }
        for (parent, _, _, sources) in &plan.creates {
            let parent = match parent {
                ParentRef::Root(root) => root.memory_id(),
                ParentRef::Node(node) => node.clone(),
            };
            if sources.is_empty() || sources.iter().any(|s| !belongs(s, &parent)) {
                return Err(RuntimeError::InvalidInput("maintenance source scope"));
            }
        }
        for (child, expected) in &plan.expected_reparent {
            if self
                .current_active(tx, owner.as_str(), project.as_str(), child.as_str())
                .await?
                .as_deref()
                != Some(expected.as_str())
            {
                return Err(RuntimeError::Conflict);
            }
        }
        if plan
            .reparent
            .iter()
            .any(|(child, _)| !plan.expected_reparent.iter().any(|(id, _)| id == child))
        {
            return Err(RuntimeError::InvalidInput("missing reparent revision"));
        }
        for (source, from, index) in &plan.move_sources {
            if !links
                .iter()
                .any(|l| l.source_id == source.as_str() && l.memory_id == from.as_str())
                || plan
                    .creates
                    .get(*index)
                    .is_none_or(|(_, _, _, s)| !s.contains(source))
            {
                return Err(RuntimeError::InvalidInput("maintenance source move"));
            }
        }
        let mut sources = Vec::new();
        for source in plan
            .rewrites
            .iter()
            .flat_map(|x| &x.4)
            .chain(plan.creates.iter().flat_map(|x| &x.3))
        {
            if !sources.contains(source) {
                sources.push(source.clone());
            }
        }
        let commit = CurationCommit {
            links: vec![],
            operation_id: plan.operation_id.clone(),
            sources,
            promotes: vec![],
            supersedes: vec![],
            merges: plan.rewrites.clone(),
            creates: plan.creates.clone(),
            parked: vec![],
            stale: plan.stale.clone(),
            split: vec![],
            operation: plan.operation,
        };
        let (items, _) = self
            .commit_curation_tx(
                tx,
                owner,
                actor,
                project,
                &commit,
                now_ms,
                false,
                RuntimeLimits::default().max_content_bytes,
                None,
            )
            .await?;
        for (source, from, _) in &plan.move_sources {
            tx.query("DELETE memory_node_source WHERE owner_id=$owner AND project_id=$project AND memory_id=$memory AND source_id=$source RETURN NONE;")
                .bind(("owner",owner.to_string())).bind(("project",project.to_string())).bind(("memory",from.to_string())).bind(("source",source.to_string())).await.map_err(storage)?.check().map_err(storage)?;
        }
        for (child, index) in &plan.reparent {
            let (parent, _, _, _) = plan
                .creates
                .get(*index)
                .ok_or(RuntimeError::InvalidInput("maintenance parent index"))?;
            let old_parent = match parent {
                ParentRef::Root(root) => root.memory_id(),
                ParentRef::Node(node) => node.clone(),
            };
            if !edges
                .iter()
                .any(|e| e.child_memory == child.as_str() && e.parent_memory == old_parent.as_str())
            {
                return Err(RuntimeError::Conflict);
            }
            let new_parent = format!(
                "tree-node:{}",
                stable(&[&plan.operation_id, &index.to_string()])
            );
            let mut descendants = vec![child.to_string()];
            let mut i = 0;
            while i < descendants.len() {
                let current = descendants[i].clone();
                for edge in edges.iter().filter(|e| e.parent_memory == current) {
                    if !descendants.contains(&edge.child_memory) {
                        descendants.push(edge.child_memory.clone());
                    }
                }
                i += 1;
            }
            tx.query("UPDATE memory_tree_edge SET parent_memory=$parent WHERE owner_id=$owner AND project_id=$project AND child_memory=$child RETURN NONE; UPDATE memory_tree_revision SET parent_memory=$parent WHERE owner_id=$owner AND project_id=$project AND memory_id=$child RETURN NONE; UPDATE memory_tree_revision SET depth=depth+1 WHERE owner_id=$owner AND project_id=$project AND memory_id IN $descendants RETURN NONE;")
                .bind(("owner",owner.to_string())).bind(("project",project.to_string())).bind(("parent",new_parent)).bind(("child",child.to_string())).bind(("descendants",descendants)).await.map_err(storage)?.check().map_err(storage)?;
        }
        if let Some((queue, memory)) = &plan.queue_item {
            let mut state = self.ensure_tree_state(tx, owner, project).await?;
            let list = match queue {
                TreeQueue::Stale => &mut state.stale,
                TreeQueue::Split => &mut state.split_queue,
                TreeQueue::Rebuild => &mut state.rebuild_queue,
            };
            if !list.contains(&memory.to_string()) {
                return Err(RuntimeError::Conflict);
            }
            list.retain(|id| id != memory.as_str());
            self.save_tree_state(tx, &state).await?;
        }
        Ok(items)
    }
}

impl EmbeddedRuntimeStore {
    pub async fn tree_queue_peek(
        &self,
        owner: &OwnerId,
        project: &ProjectId,
        queue: TreeQueue,
    ) -> Result<Option<MemoryId>, RuntimeError> {
        let Some(state) = self.state_in(&self.capture.db, owner, project).await? else {
            return Ok(None);
        };
        let list = match queue {
            TreeQueue::Stale => state.stale,
            TreeQueue::Split => state.split_queue,
            TreeQueue::Rebuild => state.rebuild_queue,
        };
        for id in list {
            let mut r=self.capture.db.query("SELECT VALUE basis FROM memory_tree_revision WHERE owner_id=$owner AND project_id=$project AND memory_id=$memory AND basis='concluded' LIMIT 1;").bind(("owner",owner.to_string())).bind(("project",project.to_string())).bind(("memory",id.clone())).await.map_err(storage)?.check().map_err(storage)?;
            if r.take::<Vec<String>>(0).map_err(storage)?.is_empty() {
                return MemoryId::new(id).map(Some).map_err(storage);
            }
        }
        Ok(None)
    }
}

fn orphan_geometry(
    kind: StoreKind,
    edges: &[TreeEdge],
    memory: &MemoryId,
) -> Result<(TreeRoot, MemoryId, u8), RuntimeError> {
    let mut current = memory.as_str();
    let mut seen = HashSet::new();
    let mut parent = None;
    let mut depth = 0u8;
    while seen.insert(current.to_owned()) {
        let edge = edges
            .iter()
            .find(|e| e.child_memory == current)
            .ok_or(RuntimeError::Missing)?;
        if parent.is_none() {
            parent = Some(MemoryId::new(edge.parent_memory.clone()).map_err(storage)?);
        }
        depth = depth
            .checked_add(1)
            .ok_or(RuntimeError::StorageUnavailable)?;
        if let Some(root) = roots(kind)
            .iter()
            .copied()
            .find(|r| r.memory_id().as_str() == edge.parent_memory)
        {
            return Ok((root, parent.ok_or(RuntimeError::Missing)?, depth));
        }
        current = &edge.parent_memory;
    }
    Err(RuntimeError::Conflict)
}
impl EmbeddedRuntimeStore {
    pub async fn tree_rebuild_node(
        &self,
        owner: &OwnerId,
        project: &ProjectId,
        memory: &MemoryId,
    ) -> Result<TreeNode, RuntimeError> {
        if let Some(node) = self
            .tree_nodes(owner, project)
            .await?
            .into_iter()
            .find(|n| &n.memory == memory)
        {
            return Ok(node);
        }
        let edges = self.tree_edges(&self.capture.db, owner, project).await?;
        let (root, parent, depth) = orphan_geometry(StoreKind::of(project), &edges, memory)?;
        Ok(TreeNode {
            basis: Default::default(),
            confidence: None,
            derived_from: vec![],
            memory: memory.clone(),
            revision: RevisionId::new("tree-missing-head").map_err(storage)?,
            root,
            parent: Some(parent),
            depth,
            title: String::new(),
            content: String::new(),
            direct_sources: 0,
            children: edges
                .iter()
                .filter(|e| e.parent_memory == memory.as_str())
                .count() as u32,
            last_linked_ms: 0,
            valid_from_ms: None,
        })
    }
}

impl EmbeddedRuntimeStore {
    pub async fn tree_recent_sources(
        &self,
        owner: &OwnerId,
        project: &ProjectId,
        memory: &MemoryId,
    ) -> Result<Vec<CaptureEvent>, RuntimeError> {
        let mut r = self.capture.db.query(format!("LET $linked = (SELECT VALUE source_id FROM memory_node_source WHERE owner_id=$owner AND project_id=$project AND memory_id=$memory); SELECT {SOURCE_PROJECTION} FROM memory_source WHERE owner_id=$owner AND project_id=$project AND source_id IN $linked ORDER BY recorded_at_ms DESC, source_id LIMIT 32;"))
            .bind(("owner",owner.to_string())).bind(("project",project.to_string())).bind(("memory",memory.to_string())).await.map_err(storage)?.check().map_err(storage)?;
        let rows: Vec<SourceRecord> = r.take(1).map_err(storage)?;
        rows.into_iter().map(source_event).collect()
    }
}

#[cfg(test)]
impl EmbeddedRuntimeStore {
    pub async fn test_queue_split(
        &self,
        owner: &OwnerId,
        project: &ProjectId,
        memory: &MemoryId,
    ) -> Result<(), RuntimeError> {
        let _guard = self.capture.writes.lock().await;
        let tx = self.capture.db.clone().begin().await.map_err(storage)?;
        let mut state = self.ensure_tree_state(&tx, owner, project).await?;
        add_unique(&mut state.split_queue, memory.to_string());
        self.save_tree_state(&tx, &state).await?;
        tx.commit().await.map(|_| ()).map_err(storage)
    }
}

impl EmbeddedRuntimeStore {
    /// Page past parked or admitted tree sources without starving later eligible evidence.
    pub async fn tree_eligible_after_exclusions(
        &self,
        owner: &OwnerId,
        project: &ProjectId,
        limit: usize,
        from_ms: Option<u64>,
        to_ms: Option<u64>,
        excluded: &[SourceId],
    ) -> Result<Vec<CaptureEvent>, RuntimeError> {
        if limit == 0 || limit > 32 {
            return Err(RuntimeError::InvalidInput("pending batch bound"));
        }
        let excluded: Vec<String> = excluded.iter().map(ToString::to_string).collect();
        let mut r = self.capture.db.query(format!("LET $consumed = (SELECT VALUE source_id FROM memory_source_consumption WHERE owner_id = $owner AND project_id = $project); LET $admitted = array::distinct(array::flatten((SELECT VALUE sources FROM memory_runtime_operation WHERE owner_id = $owner AND project_id = $project AND string::starts_with(operation_id, 'dream-job:')))); SELECT {SOURCE_PROJECTION} FROM memory_source WHERE owner_id = $owner AND project_id = $project AND ($from = NONE OR recorded_at_ms >= $from) AND ($to = NONE OR recorded_at_ms <= $to) AND kind != 'action' AND !string::starts_with(origin, 'omp://tool_result') AND source_id NOT IN $consumed AND source_id NOT IN $admitted AND source_id NOT IN $excluded ORDER BY recorded_at_ms, source_id LIMIT $limit;"))
            .bind(("owner",owner.to_string())).bind(("project",project.to_string())).bind(("limit",limit)).bind(("from",from_ms)).bind(("to",to_ms)).bind(("excluded",excluded)).await.map_err(storage)?.check().map_err(storage)?;
        let rows: Vec<SourceRecord> = r.take(2).map_err(storage)?;
        rows.into_iter().map(source_event).collect()
    }
}

impl EmbeddedRuntimeStore {
    /// Cosine scores for active node revisions in the query's exact embedding space.
    async fn tree_scores(
        &self,
        owner: &OwnerId,
        project: &ProjectId,
        revisions: &[RevisionId],
        query: &Embedding,
    ) -> Result<Vec<(RuntimeHit, f32)>, RuntimeError> {
        if revisions.is_empty() {
            return Ok(Vec::new());
        }
        if query.values.len() != query.space.dimensions
            || cosine(&query.values, &query.values).is_none()
        {
            return Err(RuntimeError::InvalidInput("vector query bounds"));
        }
        #[derive(SurrealValue)]
        struct Score {
            revision_id: String,
            score: f64,
        }
        let mut response = self.capture.db.query("LET $active = (SELECT VALUE revision_id FROM memory_revision_state WHERE owner_id=$owner AND project_id=$project AND state='active'); LET $nodes = (SELECT VALUE revision_id FROM memory_tree_revision WHERE owner_id=$owner AND project_id=$project); SELECT revision_id, vector::similarity::cosine(embedding, $vector) AS score FROM memory_runtime_embedding WHERE owner_id=$owner AND project_id=$project AND provider=$provider AND model=$model AND index_revision=$index AND dimensions=$dimensions AND revision_id IN $active AND revision_id IN $nodes AND revision_id IN $ids ORDER BY score DESC, revision_id;")
            .bind(("owner", owner.to_string())).bind(("project", project.to_string())).bind(("provider", query.space.provider.clone())).bind(("model", query.space.model.clone())).bind(("index", query.space.revision.clone())).bind(("dimensions", query.space.dimensions)).bind(("vector", query.values.clone())).bind(("ids", revisions.iter().map(ToString::to_string).collect::<Vec<_>>())).await.map_err(storage)?.check().map_err(storage)?;
        let scores: Vec<Score> = response.take(2).map_err(storage)?;
        let ids = scores
            .iter()
            .map(|s| {
                if s.score.is_finite() {
                    RevisionId::new(&s.revision_id).map_err(storage)
                } else {
                    Err(RuntimeError::StorageUnavailable)
                }
            })
            .collect::<Result<Vec<_>, _>>()?;
        let hits = self
            .active_runtime_hits(owner, std::slice::from_ref(project), &ids)
            .await?;
        Ok(hits
            .into_iter()
            .filter_map(|hit| {
                scores
                    .iter()
                    .find(|s| s.revision_id == hit.item.revision.id.as_str())
                    .map(|s| (hit, s.score as f32))
            })
            .collect())
    }
    pub async fn tree_node_score(
        &self,
        owner: &OwnerId,
        project: &ProjectId,
        node: &MemoryId,
        query: &Embedding,
    ) -> Result<Option<f32>, RuntimeError> {
        let nodes = self.tree_nodes(owner, project).await?;
        let Some(node) = nodes.iter().find(|n| &n.memory == node) else {
            return Ok(None);
        };
        Ok(self
            .tree_scores(owner, project, std::slice::from_ref(&node.revision), query)
            .await?
            .first()
            .map(|(_, score)| *score))
    }
    pub async fn tree_child_scores(
        &self,
        owner: &OwnerId,
        project: &ProjectId,
        parent: &MemoryId,
        query: &Embedding,
    ) -> Result<Vec<(RuntimeHit, f32)>, RuntimeError> {
        let children = self.tree_children(owner, project, parent).await?;
        // Hydration is bounded to 32 revisions; tree policy bounds children to 16.
        let mut scores = Vec::new();
        for chunk in children.chunks(32) {
            let ids: Vec<_> = chunk.iter().map(|node| node.revision.clone()).collect();
            scores.extend(self.tree_scores(owner, project, &ids, query).await?);
        }
        scores.sort_by(|a, b| {
            b.1.total_cmp(&a.1)
                .then(a.0.item.revision.id.cmp(&b.0.item.revision.id))
        });
        Ok(scores)
    }
}

impl EmbeddedRuntimeStore {
    /// Snapshot of active flat knowledge and its original sources, ordered oldest first.
    pub async fn tree_migration_sources(
        &self,
        owner: &OwnerId,
        project: &ProjectId,
    ) -> Result<(usize, Vec<CaptureEvent>), RuntimeError> {
        let mut r = self.capture.db.query("LET $active = (SELECT VALUE revision_id FROM memory_revision_state WHERE owner_id=$owner AND project_id=$project AND state='active'); LET $nodes = (SELECT VALUE memory_id FROM memory_tree_revision WHERE owner_id=$owner AND project_id=$project); SELECT sources FROM memory_revision WHERE owner_id=$owner AND project_id=$project AND kind='knowledge' AND memory_id != $playbook AND memory_id NOT IN $nodes AND revision_id IN $active;")
            .bind(("owner", owner.to_string())).bind(("project", project.to_string())).bind(("playbook", crate::domain_models::procedural::PLAYBOOK_MEMORY))
            .await.map_err(storage)?.check().map_err(storage)?;
        #[derive(SurrealValue)]
        struct Sources {
            sources: Vec<String>,
        }
        let rows: Vec<Sources> = r.take(2).map_err(storage)?;
        let count = rows.len();
        let mut ids = rows.into_iter().flat_map(|r| r.sources).collect::<Vec<_>>();
        ids.sort();
        ids.dedup();
        let mut events = Vec::with_capacity(ids.len());
        for id in ids {
            let row = self
                .source_row(&self.capture.db, owner.as_str(), &id)
                .await?
                .into_iter()
                .next()
                .ok_or(RuntimeError::Missing)?;
            let event = source_event(row)?;
            if &event.project != project {
                return Err(RuntimeError::Conflict);
            }
            events.push(event);
        }
        events.sort_by(|a, b| {
            a.recorded_at_ms
                .cmp(&b.recorded_at_ms)
                .then_with(|| a.source.as_str().cmp(b.source.as_str()))
        });
        Ok((count, events))
    }

    pub async fn tree_migrated_sources(
        &self,
        owner: &OwnerId,
        project: &ProjectId,
    ) -> Result<Vec<SourceId>, RuntimeError> {
        let mut r = self.capture.db.query("SELECT VALUE source_id FROM memory_node_source WHERE owner_id=$owner AND project_id=$project AND operation='migrate'; SELECT VALUE source_id FROM memory_tree_migrated WHERE owner_id=$owner AND project_id=$project;")
            .bind(("owner", owner.to_string())).bind(("project", project.to_string()))
            .await.map_err(storage)?.check().map_err(storage)?;
        let mut rows: Vec<String> = r.take(0).map_err(storage)?;
        rows.extend(r.take::<Vec<String>>(1).map_err(storage)?);
        rows.sort();
        rows.dedup();
        rows.into_iter()
            .map(|s| SourceId::new(s).map_err(|_| RuntimeError::StorageUnavailable))
            .collect()
    }
}
