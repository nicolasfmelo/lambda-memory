//! Durable reflection scheduling and atomic source-bound conclusion commits.
use super::*;
use crate::domain_models::reflection as reflection_policy;
use crate::domain_models::{
    memory_tree::{Basis, Confidence},
    reflection::*,
};
#[derive(Clone, SurrealValue)]
struct ReflectionState {
    owner_id: String,
    project_id: String,
    branch_memory: String,
    observed_revisions: u64,
    baseline: i64,
    last_run_ms: u64,
    mature_lessons_seen: Vec<String>,
}
const STATE: &str = "owner_id, project_id, branch_memory, observed_revisions, baseline, last_run_ms, mature_lessons_seen";
pub(super) fn basis(s: &str) -> Result<Basis, RuntimeError> {
    match s {
        "observed" => Ok(Basis::Observed),
        "concluded" => Ok(Basis::Concluded),
        _ => Err(RuntimeError::StorageUnavailable),
    }
}
pub(super) fn confidence(s: &str) -> Result<Confidence, RuntimeError> {
    match s {
        "low" => Ok(Confidence::Low),
        "medium" => Ok(Confidence::Medium),
        "high" => Ok(Confidence::High),
        _ => Err(RuntimeError::StorageUnavailable),
    }
}
impl EmbeddedRuntimeStore {
    async fn reflection_states(
        &self,
        db: &impl QueryConnection,
        owner: &OwnerId,
        project: &ProjectId,
    ) -> Result<Vec<ReflectionState>, RuntimeError> {
        let mut r = db.query(format!("SELECT {STATE} FROM memory_reflection_state WHERE owner_id=$owner AND project_id=$project ORDER BY last_run_ms, branch_memory;"))
            .bind(("owner",owner.to_string())).bind(("project",project.to_string())).await.map_err(storage)?.check().map_err(storage)?;
        r.take(0).map_err(storage)
    }
    async fn mature_in(
        &self,
        db: &impl QueryConnection,
        owner: &OwnerId,
        project: &ProjectId,
        nodes: &[TreeNode],
        branch: &MemoryId,
    ) -> Result<Vec<MemoryId>, RuntimeError> {
        let mut related = HashSet::new();
        for n in nodes
            .iter()
            .filter(|n| reflection_policy::in_branch(nodes, &n.memory, branch))
        {
            related.insert(n.memory.clone());
            for link in self.cross_links_in(db, owner, project, &n.memory).await? {
                related.insert(
                    MemoryId::new(if link.from_memory == n.memory.as_str() {
                        link.to_memory
                    } else {
                        link.from_memory
                    })
                    .map_err(storage)?,
                );
            }
        }
        let mut mature = Vec::new();
        for n in nodes.iter().filter(|n| {
            n.basis == Basis::Observed && n.root.is_lesson() && related.contains(&n.memory)
        }) {
            if self
                .lesson_stats_in(db, owner, project, &n.memory)
                .await?
                .sessions
                .len()
                >= crate::domain_models::skills::SKILL_MIN_SESSIONS
            {
                mature.push(n.memory.clone());
            }
        }
        mature.sort();
        Ok(mature)
    }
    async fn initial_reflection_state(
        &self,
        tx: &Transaction<Db>,
        owner: &OwnerId,
        project: &ProjectId,
        branch: &MemoryId,
        now: u64,
    ) -> Result<ReflectionState, RuntimeError> {
        // Include the just-inserted processing revision and its edge, not only active heads.
        let edges = self.tree_edges(tx, owner, project).await?;
        let mut ids = vec![branch.to_string()];
        let mut cursor = 0;
        while cursor < ids.len() {
            let parent = ids[cursor].clone();
            for edge in edges.iter().filter(|e| e.parent_memory == parent) {
                if !ids.contains(&edge.child_memory) {
                    ids.push(edge.child_memory.clone());
                }
            }
            cursor += 1;
        }
        let mut related = ids.clone();
        for id in &ids {
            for link in self
                .cross_links_in(tx, owner, project, &MemoryId::new(id).map_err(storage)?)
                .await?
            {
                let other = if link.from_memory == *id {
                    link.to_memory
                } else {
                    link.from_memory
                };
                if !related.contains(&other) {
                    related.push(other);
                }
            }
        }
        let mut r=tx.query("RETURN array::distinct((SELECT VALUE memory_id FROM memory_tree_revision WHERE owner_id=$owner AND project_id=$project AND basis='observed' AND tree_root IN ['lessons','general_lessons'] AND memory_id IN $ids));").bind(("owner",owner.to_string())).bind(("project",project.to_string())).bind(("ids",related)).await.map_err(storage)?.check().map_err(storage)?;
        let lessons: Vec<String> = r.take(0).map_err(storage)?;
        let mut mature_lessons_seen = Vec::new();
        for id in lessons {
            if self
                .lesson_stats_in(tx, owner, project, &MemoryId::new(&id).map_err(storage)?)
                .await?
                .sessions
                .len()
                >= crate::domain_models::skills::SKILL_MIN_SESSIONS
            {
                mature_lessons_seen.push(id);
            }
        }
        mature_lessons_seen.sort();
        let mut r = tx.query("RETURN array::len((SELECT revision_id FROM memory_tree_revision WHERE owner_id=$owner AND project_id=$project AND basis='observed' AND memory_id IN $ids));")
            .bind(("owner",owner.to_string())).bind(("project",project.to_string())).bind(("ids",ids)).await.map_err(storage)?.check().map_err(storage)?;
        let count: Option<u64> = r.take(0).map_err(storage)?;
        let count = count.unwrap_or(0);
        let state = ReflectionState {
            owner_id: owner.to_string(),
            project_id: project.to_string(),
            branch_memory: branch.to_string(),
            observed_revisions: count,
            baseline: i64::try_from(count).map_err(storage)?,
            last_run_ms: now,
            mature_lessons_seen,
        };
        self.create_row(
            tx,
            "CREATE memory_reflection_state CONTENT $record RETURN NONE;",
            state.clone(),
        )
        .await?;
        Ok(state)
    }
    /// Count actual output revisions, once per memory, after all edges and links are installed.
    pub(super) async fn count_observed_revisions_tx(
        &self,
        tx: &Transaction<Db>,
        owner: &OwnerId,
        project: &ProjectId,
        items: &[RuntimeRevision],
        now: u64,
    ) -> Result<(), RuntimeError> {
        let edges = self.tree_edges(tx, owner, project).await?;
        let mut branches = std::collections::BTreeMap::<String, u64>::new();
        for item in items {
            let mut current = item.revision.memory.to_string();
            for _ in 0..32 {
                let edge = edges
                    .iter()
                    .find(|e| e.child_memory == current)
                    .ok_or(RuntimeError::Conflict)?;
                if roots(StoreKind::of(project))
                    .iter()
                    .any(|r| r.memory_id().as_str() == edge.parent_memory)
                {
                    break;
                }
                current = edge.parent_memory.clone();
            }
            *branches.entry(current).or_default() += 1;
        }
        let existing = self.reflection_states(tx, owner, project).await?;
        for (branch, count) in branches {
            if existing.iter().any(|s| s.branch_memory == branch) {
                tx.query("UPDATE memory_reflection_state SET observed_revisions += $count WHERE owner_id=$owner AND project_id=$project AND branch_memory=$branch RETURN NONE;").bind(("owner",owner.to_string())).bind(("project",project.to_string())).bind(("branch",branch)).bind(("count",count)).await.map_err(storage)?.check().map_err(storage)?;
            } else {
                self.initial_reflection_state(
                    tx,
                    owner,
                    project,
                    &MemoryId::new(branch).map_err(storage)?,
                    now,
                )
                .await?;
            }
        }
        Ok(())
    }
    #[cfg(test)]
    pub async fn test_forget_reflection_state(
        &self,
        owner: &OwnerId,
        project: &ProjectId,
    ) -> Result<(), RuntimeError> {
        let _guard = self.capture.writes.lock().await;
        self.capture.db.query("DELETE memory_reflection_state WHERE owner_id=$owner AND project_id=$project RETURN NONE;").bind(("owner",owner.to_string())).bind(("project",project.to_string())).await.map_err(storage)?.check().map_err(storage)?;
        Ok(())
    }
    pub async fn reflection_work(
        &self,
        owner: &OwnerId,
        project: &ProjectId,
        now: u64,
        policy: ReflectionPolicy,
    ) -> Result<Option<ReflectionWork>, RuntimeError> {
        let _guard = self.capture.writes.lock().await;
        let tx = self.capture.db.clone().begin().await.map_err(storage)?;
        let result = async {
            let nodes = self.tree_nodes_in(&tx, owner, project).await?;
            let mut states = self.reflection_states(&tx, owner, project).await?;
            for branch in nodes
                .iter()
                .filter(|n| n.depth == 1 && n.basis == Basis::Observed)
            {
                if !states
                    .iter()
                    .any(|s| s.branch_memory == branch.memory.as_str())
                {
                    states.push(
                        self.initial_reflection_state(&tx, owner, project, &branch.memory, now)
                            .await?,
                    );
                }
            }
            states.sort_by(|a, b| {
                a.last_run_ms
                    .cmp(&b.last_run_ms)
                    .then(a.branch_memory.cmp(&b.branch_memory))
            });
            for state in states {
                let branch = MemoryId::new(state.branch_memory).map_err(storage)?;
                if !nodes.iter().any(|n| n.memory == branch && n.depth == 1) {
                    continue;
                }
                let mature = self.mature_in(&tx, owner, project, &nodes, &branch).await?;
                let seen = state
                    .mature_lessons_seen
                    .into_iter()
                    .map(|id| MemoryId::new(id).map_err(storage))
                    .collect::<Result<Vec<_>, _>>()?;
                if !(policy.due)(state.observed_revisions, state.baseline, &mature, &seen) {
                    continue;
                }
                let mut related = HashSet::new();
                for n in nodes
                    .iter()
                    .filter(|n| reflection_policy::in_branch(&nodes, &n.memory, &branch))
                {
                    for link in self.cross_links_in(&tx, owner, project, &n.memory).await? {
                        related.insert(if link.from_memory == n.memory.as_str() {
                            link.to_memory
                        } else {
                            link.from_memory
                        });
                    }
                }
                let mut slotted = Vec::new();
                let mut lessons = 0;
                for n in &nodes {
                    let inside = reflection_policy::in_branch(&nodes, &n.memory, &branch);
                    if !inside && !related.contains(n.memory.as_str()) {
                        continue;
                    }
                    if !inside && n.root.is_lesson() {
                        if lessons == 10 {
                            continue;
                        }
                        lessons += 1;
                    }
                    let mut path = vec![n.root.label().to_owned()];
                    let mut ancestors = Vec::new();
                    let mut parent = n.parent.as_ref();
                    for _ in 0..32 {
                        let Some(p) = parent.and_then(|p| nodes.iter().find(|a| &a.memory == p))
                        else {
                            break;
                        };
                        ancestors.push(p.title.clone());
                        parent = p.parent.as_ref();
                    }
                    ancestors.reverse();
                    path.extend(ancestors);
                    path.push(n.title.clone());
                    slotted.push(ReflectionNode {
                        node: n.clone(),
                        path,
                        inside,
                    });
                }
                slotted.sort_by(|a, b| a.node.memory.cmp(&b.node.memory));
                return Ok(Some(ReflectionWork {
                    input: (policy.bound)(ReflectionInput {
                        branch,
                        nodes: slotted,
                    }),
                    mature_lessons: mature,
                }));
            }
            Ok(None)
        }
        .await;
        match result {
            Ok(work) => {
                tx.commit().await.map_err(storage)?;
                Ok(work)
            }
            Err(e) => {
                let _ = tx.cancel().await;
                Err(e)
            }
        }
    }
    pub async fn reflection_finish(
        &self,
        owner: &OwnerId,
        project: &ProjectId,
        work: &ReflectionWork,
        now: u64,
    ) -> Result<(), RuntimeError> {
        let _guard = self.capture.writes.lock().await;
        self.capture.db.query("UPDATE memory_reflection_state SET baseline=observed_revisions, last_run_ms=$now, mature_lessons_seen=array::union(mature_lessons_seen,$seen) WHERE owner_id=$owner AND project_id=$project AND branch_memory=$branch RETURN NONE;")
            .bind(("owner",owner.to_string())).bind(("project",project.to_string())).bind(("branch",work.input.branch.to_string())).bind(("now",now)).bind(("seen",work.mature_lessons.iter().map(ToString::to_string).collect::<Vec<_>>())).await.map_err(storage)?.check().map_err(storage)?;
        Ok(())
    }
    pub async fn reflection_contradictions(
        &self,
        owner: &OwnerId,
        project: &ProjectId,
    ) -> Result<Vec<(MemoryId, MemoryId)>, RuntimeError> {
        #[derive(SurrealValue)]
        struct Pair {
            concluded_memory: String,
            observed_memory: String,
        }
        let mut r=self.capture.db.query("SELECT concluded_memory, observed_memory FROM memory_contradiction WHERE owner_id=$owner AND project_id=$project;").bind(("owner",owner.to_string())).bind(("project",project.to_string())).await.map_err(storage)?.check().map_err(storage)?;
        let rows: Vec<Pair> = r.take(0).map_err(storage)?;
        rows.into_iter()
            .map(|r| {
                Ok((
                    MemoryId::new(r.concluded_memory).map_err(storage)?,
                    MemoryId::new(r.observed_memory).map_err(storage)?,
                ))
            })
            .collect()
    }
    #[allow(clippy::too_many_arguments)]
    pub async fn commit_reflection(
        &self,
        owner: &OwnerId,
        actor: &ActorId,
        project: &ProjectId,
        input: &ReflectionInput,
        answer: ReflectionAnswer,
        operation: &str,
        now: u64,
        policy: ReflectionPolicy,
    ) -> Result<Vec<RuntimeRevision>, RuntimeError> {
        if !(policy.validate)(input, &answer) {
            return Err(RuntimeError::ProviderInvalidOutput);
        }
        let answer = (policy.normalize)(input, answer);
        let _guard = self.capture.writes.lock().await;
        let tx = self.capture.db.clone().begin().await.map_err(storage)?;
        let result=async {
            let nodes=self.tree_nodes_in(&tx,owner,project).await?;
            for n in &input.nodes {
                if !nodes.iter().any(|live| live == &n.node) || n.inside != reflection_policy::in_branch(&nodes,&n.node.memory,&input.branch) { return Err(RuntimeError::Conflict); }
            }
            let mut plan=CurationCommit { links:vec![],operation_id:operation.into(),sources:vec![],promotes:vec![],merges:vec![],supersedes:vec![],creates:vec![],parked:vec![],stale:vec![],split:vec![],operation:LinkOperation::Reflect };
            let mut reasoning=std::collections::HashMap::new();
            let mut changes=answer.confidence_updates.clone();
            let mut contradicted=HashSet::new();
            let mut retired_revisions=Vec::new();
            // One confidence step (lower once, or retire when already low) per concluded node...
            for (c,_) in &answer.contradictions {
                let cnode=&input.nodes[*c].node;
                if !contradicted.insert(*c) { continue; }
                if let Some(lower)=cnode.confidence.ok_or(RuntimeError::Conflict)?.lower() { changes.push((*c,lower)); }
                else {
                    retired_revisions.extend(self.node_revision_ids(&tx,owner,project,&cnode.memory).await?);
                    self.reflection_retire_tx(&tx,owner,actor,project,&cnode.memory,now).await?;
                }
            }
            self.cascade_conclusions_tx(&tx,owner,actor,project,&retired_revisions,now).await?;
            let surviving=self.tree_edges(&tx,owner,project).await?;
            // ...but every distinct observed conflict of a surviving conclusion is recorded, so recall
            // drops it next to any of those observed nodes; retired conclusions leave no rows.
            let mut pairs=HashSet::new();
            for (c,o) in &answer.contradictions {
                let cnode=&input.nodes[*c].node; let onode=&input.nodes[*o].node;
                if !pairs.insert((*c,*o)) || !surviving.iter().any(|e| e.child_memory==cnode.memory.as_str()) { continue; }
                tx.query("IF array::len((SELECT id FROM memory_contradiction WHERE owner_id=$owner AND project_id=$project AND concluded_memory=$c AND observed_memory=$o))=0 { CREATE memory_contradiction SET owner_id=$owner,project_id=$project,concluded_memory=$c,observed_memory=$o,created_at_ms=$now; };")
                    .bind(("owner",owner.to_string())).bind(("project",project.to_string())).bind(("c",cnode.memory.to_string())).bind(("o",onode.memory.to_string())).bind(("now",now)).await.map_err(storage)?.check().map_err(storage)?;
            }
            for (i,confidence) in changes {
                if !surviving.iter().any(|e|e.child_memory==input.nodes[i].node.memory.as_str()) { continue; }
                let n=&input.nodes[i].node;
                let row=self.revision_row(&tx,owner.as_str(),n.revision.as_str()).await?.ok_or(RuntimeError::Conflict)?;
                let sources=row.sources.iter().map(|s| SourceId::new(s).map_err(storage)).collect::<Result<Vec<_>,_>>()?;
                plan.merges.push((n.memory.clone(),n.revision.clone(),n.title.clone(),n.content.clone(),sources));
                reasoning.insert(n.memory.to_string(),(confidence.as_str().into(),n.derived_from.iter().map(ToString::to_string).collect()));
            }
            for c in answer.conclusions {
                if !surviving.iter().any(|e| e.child_memory==input.nodes[c.parent].node.memory.as_str()) || c.derived_from.iter().any(|i| !surviving.iter().any(|e|e.child_memory==input.nodes[*i].node.memory.as_str())) { continue; }
                let mut events=Vec::new();
                let mut derived=Vec::new();
                for i in c.derived_from {
                    let n=&input.nodes[i].node;
                    if !derived.contains(&n.revision.to_string()) { derived.push(n.revision.to_string()); }
                    let row=self.revision_row(&tx,owner.as_str(),n.revision.as_str()).await?.ok_or(RuntimeError::Conflict)?;
                    for s in row.sources { if !events.iter().any(|e: &SourceRecord| e.source_id == s) { events.extend(self.source_row(&tx,owner.as_str(),&s).await?); } }
                }
                events.sort_by(|a,b| b.recorded_at_ms.cmp(&a.recorded_at_ms).then(a.source_id.cmp(&b.source_id))); events.truncate(32);
                let sources=events.iter().map(|e| SourceId::new(&e.source_id).map_err(storage)).collect::<Result<Vec<_>,_>>()?;
                let id=format!("tree-node:{}",stable(&[operation,&plan.creates.len().to_string()]));
                reasoning.insert(id,(c.confidence.as_str().into(),derived));
                plan.creates.push((ParentRef::Node(input.nodes[c.parent].node.memory.clone()),c.title,c.content,sources));
            }
            for s in plan.merges.iter().flat_map(|m| &m.4).chain(plan.creates.iter().flat_map(|c| &c.3)) { if !plan.sources.contains(s) { plan.sources.push(s.clone()); } }
            self.commit_curation_tx(&tx,owner,actor,project,&plan,now,false,RuntimeLimits::default().max_content_bytes,Some(&reasoning)).await.map(|(items,_)| items)
        }.await;
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
    pub(super) async fn reflection_retire_tx(
        &self,
        tx: &Transaction<Db>,
        owner: &OwnerId,
        actor: &ActorId,
        project: &ProjectId,
        memory: &MemoryId,
        now: u64,
    ) -> Result<(), RuntimeError> {
        // Publication may still be pending, including a successor of an active conclusion.
        // Reject every live revision before preparation can publish any of those heads.
        let mut response=tx.query(format!("SELECT {STATE_FIELDS} FROM memory_revision_state WHERE owner_id=$owner AND project_id=$project AND memory_id=$memory AND state IN ['processing','active','conflicted'];"))
            .bind(("owner",owner.to_string())).bind(("project",project.to_string())).bind(("memory",memory.to_string())).await.map_err(storage)?.check().map_err(storage)?;
        let states: Vec<StateRecord> = response.take(0).map_err(storage)?;
        if states.is_empty() {
            return Ok(());
        }
        if self
            .tree_edges(tx, owner, project)
            .await?
            .iter()
            .any(|e| e.child_memory == memory.as_str())
        {
            self.retire_tree_only_tx(tx, owner, actor, project, memory, now)
                .await?;
        }
        for state in states {
            self.write_state(
                tx,
                owner.as_str(),
                project.as_str(),
                memory.as_str(),
                &state.revision_id,
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
                    revision_id: state.revision_id.clone(),
                    actor_id: actor.to_string(),
                    action: "reflection_retire".into(),
                    expected_active: (state.state == "active").then_some(state.revision_id),
                    from_state: state.state,
                    to_state: "rejected".into(),
                    conflict_with: None,
                    at_ms: now,
                },
            )
            .await?;
        }
        self.delete_node_links_tx(tx, owner, project, &[memory.to_string()])
            .await?;
        Ok(())
    }
    /// Follow revision lineage, not supporting source overlap. Already retired nodes never re-enter.
    pub(in super::super) async fn cascade_conclusions_tx(
        &self,
        tx: &Transaction<Db>,
        owner: &OwnerId,
        actor: &ActorId,
        project: &ProjectId,
        revisions: &[String],
        now: u64,
    ) -> Result<(), RuntimeError> {
        let mut lost = revisions.to_vec();
        for _ in 0..4 {
            let mut response=tx.query(format!("LET $live=(SELECT VALUE revision_id FROM memory_revision_state WHERE owner_id=$owner AND project_id=$project AND state IN ['processing','active','conflicted']); SELECT {META} FROM memory_tree_revision WHERE owner_id=$owner AND project_id=$project AND basis='concluded' AND revision_id IN $live;"))
                .bind(("owner",owner.to_string())).bind(("project",project.to_string())).await.map_err(storage)?.check().map_err(storage)?;
            let nodes: Vec<TreeMeta> = response.take(1).map_err(storage)?;
            let edges = self.tree_edges(tx, owner, project).await?;
            let mut next = Vec::new();
            let mut retired = HashSet::new();
            for n in nodes.iter().filter(|n| {
                lost.contains(&n.revision_id) || n.derived_from.iter().any(|id| lost.contains(id))
            }) {
                if !edges.iter().any(|e| e.child_memory == n.memory_id)
                    || !retired.insert(n.memory_id.clone())
                {
                    continue;
                }
                let memory = MemoryId::new(&n.memory_id).map_err(storage)?;
                next.extend(self.node_revision_ids(tx, owner, project, &memory).await?);
                self.reflection_retire_tx(tx, owner, actor, project, &memory, now)
                    .await?;
            }
            if next.is_empty() {
                break;
            }
            lost = next;
        }
        Ok(())
    }
    async fn node_revision_ids(
        &self,
        db: &impl QueryConnection,
        owner: &OwnerId,
        project: &ProjectId,
        memory: &MemoryId,
    ) -> Result<Vec<String>, RuntimeError> {
        let mut r=db.query("SELECT VALUE revision_id FROM memory_tree_revision WHERE owner_id=$owner AND project_id=$project AND memory_id=$memory;").bind(("owner",owner.to_string())).bind(("project",project.to_string())).bind(("memory",memory.to_string())).await.map_err(storage)?.check().map_err(storage)?;
        r.take(0).map_err(storage)
    }
    pub async fn inspect_reflection_backfill(
        &self,
        owner: &OwnerId,
        project: &ProjectId,
    ) -> Result<ReflectionBackfillPlan, RuntimeError> {
        let nodes = self.tree_nodes(owner, project).await?;
        let mut branches = Vec::new();
        for b in nodes
            .iter()
            .filter(|n| n.depth == 1 && n.basis == Basis::Observed)
        {
            let ids = nodes
                .iter()
                .filter(|n| {
                    n.basis == Basis::Observed
                        && reflection_policy::in_branch(&nodes, &n.memory, &b.memory)
                })
                .map(|n| n.memory.to_string())
                .collect::<Vec<_>>();
            let mut r=self.capture.db.query("RETURN array::len((SELECT revision_id FROM memory_tree_revision WHERE owner_id=$owner AND project_id=$project AND basis='observed' AND memory_id IN $ids));").bind(("owner",owner.to_string())).bind(("project",project.to_string())).bind(("ids",ids)).await.map_err(storage)?.check().map_err(storage)?;
            if r.take::<Option<u64>>(0).map_err(storage)?.unwrap_or(0) >= 5 {
                branches.push(b.memory.clone());
            }
        }
        branches.sort();
        Ok(ReflectionBackfillPlan { branches })
    }
    pub async fn reflection_backfill(
        &self,
        owner: &OwnerId,
        project: &ProjectId,
        plan: &ReflectionBackfillPlan,
        now: u64,
    ) -> Result<usize, RuntimeError> {
        if &self.inspect_reflection_backfill(owner, project).await? != plan {
            return Err(RuntimeError::Conflict);
        }
        let _guard = self.capture.writes.lock().await;
        let tx = self.capture.db.clone().begin().await.map_err(storage)?;
        let result=async {
            for branch in &plan.branches {
                if !self.reflection_states(&tx,owner,project).await?.iter().any(|s|s.branch_memory==branch.as_str()) { self.initial_reflection_state(&tx,owner,project,branch,now).await?; }
                tx.query("UPDATE memory_reflection_state SET baseline=observed_revisions-5 WHERE owner_id=$owner AND project_id=$project AND branch_memory=$branch RETURN NONE;").bind(("owner",owner.to_string())).bind(("project",project.to_string())).bind(("branch",branch.to_string())).await.map_err(storage)?.check().map_err(storage)?;
            } Ok(plan.branches.len())
        }.await;
        match result {
            Ok(n) => {
                tx.commit().await.map_err(storage)?;
                Ok(n)
            }
            Err(e) => {
                let _ = tx.cancel().await;
                Err(e)
            }
        }
    }
}
