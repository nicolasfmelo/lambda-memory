//! Scoped read-only dashboard projections. No preparation, provider calls or writes.
use super::*;
use crate::domain_models::memory_tree::{Basis, StoreKind, TreeMode, TreeNode, roots};
use crate::domain_models::skills::{SkillCandidate, SkillStatus};
use crate::domain_models::ui::*;
use std::collections::HashMap;

#[derive(SurrealValue)]
struct TreeParent {
    child_memory: String,
    parent_memory: String,
}

pub(super) fn skill_ref(candidates: &[SkillCandidate], memory: &MemoryId) -> Option<SkillDraftRef> {
    candidates
        .iter()
        .filter(|c| &c.memory_id == memory)
        .max_by(|a, b| {
            let pending = |c: &SkillCandidate| {
                matches!(c.status, SkillStatus::Drafted | SkillStatus::Approved)
            };
            pending(a)
                .cmp(&pending(b))
                .then(a.created_at_ms.cmp(&b.created_at_ms))
                .then(a.candidate_id.cmp(&b.candidate_id))
        })
        .map(|c| SkillDraftRef {
            candidate: c.candidate_id.clone(),
            status: c.status,
        })
}
fn memory_ref(node: &TreeNode) -> MemoryRef {
    MemoryRef {
        node: node.memory.clone(),
        title: node.title.clone(),
    }
}
/// Structural ancestry survives inactive or purged heads. Only a complete,
/// bounded chain to this root can assign a topic.
fn lineage<'a>(
    memory: &'a str,
    root: &str,
    parents: &'a HashMap<String, String>,
) -> Option<Vec<&'a str>> {
    let mut current = memory;
    let mut path = Vec::new();
    let mut seen = HashSet::new();
    for _ in 0..64 {
        if !seen.insert(current) {
            return None;
        }
        path.push(current);
        let parent = parents.get(current)?;
        if parent == root {
            path.reverse();
            return Some(path);
        }
        current = parent;
    }
    None
}

impl EmbeddedRuntimeStore {
    pub async fn ui_skill_draft(
        &self,
        owner: &OwnerId,
        project: &ProjectId,
        memory: &MemoryId,
    ) -> Result<Option<SkillDraftRef>, RuntimeError> {
        Ok(skill_ref(
            &self.skill_candidates(owner, project, None).await?,
            memory,
        ))
    }
    pub async fn ui_replaced(
        &self,
        owner: &OwnerId,
        node: &TreeNode,
    ) -> Result<Option<crate::domain_models::memory_tree::ReplacedPosition>, RuntimeError> {
        let row = self
            .revision_row(&self.capture.db, owner.as_str(), node.revision.as_str())
            .await?
            .ok_or(RuntimeError::Missing)?;
        let (states, jobs) = self.selected_metadata(owner, std::iter::once(&row)).await?;
        self.tree_replaced(&self.assemble(row, &states, &jobs)?)
            .await
    }
    pub async fn ui_derived_memories(
        &self,
        owner: &OwnerId,
        project: &ProjectId,
        node: &TreeNode,
    ) -> Result<Vec<MemoryRef>, RuntimeError> {
        let nodes = self.tree_nodes(owner, project).await?;
        let mut out = Vec::new();
        for revision in &node.derived_from {
            if let Some(row) = self
                .revision_row(&self.capture.db, owner.as_str(), revision.as_str())
                .await?
                && let Some(n) = nodes.iter().find(|n| n.memory.as_str() == row.memory_id)
                && !out.iter().any(|m: &MemoryRef| m.node == n.memory)
            {
                out.push(memory_ref(n));
            }
        }
        Ok(out)
    }
    pub async fn ui_overview(
        &self,
        owner: &OwnerId,
        project: &ProjectId,
        offset: i32,
        now_ms: u64,
    ) -> Result<MemoryOverview, RuntimeError> {
        let mode = self.tree_mode(owner, project).await?;
        let nodes = if mode == TreeMode::Tree {
            self.tree_nodes(owner, project).await?
        } else {
            vec![]
        };
        let nodes: Vec<_> = nodes
            .into_iter()
            .filter(|n| n.memory != n.root.memory_id())
            .collect();
        let roots = roots(StoreKind::of(project))
            .iter()
            .map(|root| {
                let branch: Vec<_> = nodes.iter().filter(|n| n.root == *root).collect();
                // Recent descending, memory id ascending, as in the nodes page.
                let latest = branch
                    .iter()
                    .min_by(|a, b| {
                        b.last_linked_ms
                            .cmp(&a.last_linked_ms)
                            .then(a.memory.cmp(&b.memory))
                    })
                    .map(|n| memory_ref(n));
                OverviewRoot {
                    root: *root,
                    nodes: branch.len(),
                    latest,
                }
            })
            .collect();
        let mut reinforced = 0;
        for n in nodes.iter().filter(|n| n.root.is_lesson()) {
            if self
                .lesson_stats(owner, project, &n.memory)
                .await?
                .sessions
                .len()
                >= 2
            {
                reinforced += 1;
            }
        }
        let candidates = self.skill_candidates(owner, project, None).await?;
        let skill_drafts = [
            SkillStatus::Drafted,
            SkillStatus::Approved,
            SkillStatus::Installed,
            SkillStatus::Stale,
            SkillStatus::Rejected,
        ]
        .into_iter()
        .map(|s| (s, candidates.iter().filter(|c| c.status == s).count()))
        .collect();
        let shift = i64::from(offset) * 60_000;
        let today = ((now_ms as i64) + shift).div_euclid(86_400_000);
        let first = today - 29;
        let start = (first * 86_400_000 - shift).max(0) as u64;
        let end = ((today + 1) * 86_400_000 - shift).max(0) as u64;
        let mut r = self.capture.db.query("LET $active = (SELECT VALUE revision_id FROM memory_revision_state WHERE owner_id=$owner AND project_id=$project AND state='active'); LET $tree = (SELECT VALUE memory_id FROM memory_tree_revision WHERE owner_id=$owner AND project_id=$project); RETURN array::len((SELECT revision_id FROM memory_revision WHERE owner_id=$owner AND project_id=$project AND revision_id IN $active AND memory_id NOT IN $tree AND memory_id!=$playbook)); SELECT VALUE recorded_at_ms FROM memory_source WHERE owner_id=$owner AND project_id=$project AND recorded_at_ms >= $start AND recorded_at_ms < $end; SELECT VALUE at_ms FROM memory_lifecycle_audit WHERE owner_id=$owner AND project_id=$project AND to_state='active' AND at_ms >= $start AND at_ms < $end;")
            .bind(("owner",owner.to_string())).bind(("project",project.to_string())).bind(("playbook",crate::domain_models::procedural::PLAYBOOK_MEMORY)).bind(("start",start)).bind(("end",end)).await.map_err(storage)?.check().map_err(storage)?;
        let flat_active = r
            .take::<Option<usize>>(2)
            .map_err(storage)?
            .ok_or(RuntimeError::StorageUnavailable)?;
        let captured: Vec<u64> = r.take(3).map_err(storage)?;
        let published: Vec<u64> = r.take(4).map_err(storage)?;
        let mut days: Vec<_> = (first..=today)
            .map(|d| OverviewDay {
                day: day_label(d),
                captured: 0,
                published: 0,
            })
            .collect();
        for (times, publication) in [(captured, false), (published, true)] {
            for ms in times {
                let index = ((ms as i64 + shift).div_euclid(86_400_000) - first) as usize;
                if let Some(day) = days.get_mut(index) {
                    if publication {
                        day.published += 1;
                    } else {
                        day.captured += 1;
                    }
                }
            }
        }
        Ok(MemoryOverview {
            project: project.clone(),
            mode,
            roots,
            flat_active,
            observed: nodes.iter().filter(|n| n.basis == Basis::Observed).count(),
            concluded: nodes.iter().filter(|n| n.basis == Basis::Concluded).count(),
            lessons: nodes.iter().filter(|n| n.root.is_lesson()).count(),
            reinforced,
            skill_drafts,
            days,
        })
    }
    pub async fn ui_nodes(
        &self,
        owner: &OwnerId,
        query: &NodesQuery,
    ) -> Result<NodesPage, RuntimeError> {
        let all = if self.tree_mode(owner, &query.project).await? == TreeMode::Tree {
            self.tree_nodes(owner, &query.project).await?
        } else {
            vec![]
        };
        let branch: Vec<_> = all
            .iter()
            .filter(|n| n.root == query.root && n.memory != query.root.memory_id())
            .collect();
        let mut response = self.capture.db.query("SELECT child_memory, parent_memory FROM memory_tree_edge WHERE owner_id=$owner AND project_id=$project;")
            .bind(("owner", owner.to_string())).bind(("project", query.project.to_string()))
            .await.map_err(storage)?.check().map_err(storage)?;
        let edges: Vec<TreeParent> = response.take(0).map_err(storage)?;
        let parents: HashMap<_, _> = edges
            .into_iter()
            .map(|e| (e.child_memory, e.parent_memory))
            .collect();
        let root = query.root.memory_id();
        let paths: HashMap<_, _> = branch
            .iter()
            .map(|n| {
                (
                    n.memory.as_str(),
                    lineage(n.memory.as_str(), root.as_str(), &parents),
                )
            })
            .collect();
        let first: Vec<_> = branch
            .iter()
            .copied()
            .filter(|n| {
                parents
                    .get(n.memory.as_str())
                    .is_some_and(|p| p == root.as_str())
            })
            .collect();
        if query
            .topic
            .as_ref()
            .is_some_and(|t| !first.iter().any(|n| &n.memory == t))
        {
            return Err(RuntimeError::InvalidInput(
                "topic must be a first-level child of root",
            ));
        }
        // Counts and filtering share the same structural assignment, even across
        // ancestors whose heads are no longer active.
        let mut topics: Vec<_> = first
            .iter()
            .map(|t| NodeTopic {
                memory: memory_ref(t),
                count: branch
                    .iter()
                    .filter(|n| {
                        paths
                            .get(n.memory.as_str())
                            .and_then(|p| p.as_ref())
                            .and_then(|p| p.first())
                            .is_some_and(|id| *id == t.memory.as_str())
                    })
                    .count(),
            })
            .collect();
        topics.sort_by(|a, b| a.memory.node.cmp(&b.memory.node));
        let needle = query.query.to_lowercase();
        let mut matched = Vec::new();
        for n in &branch {
            if query.basis.is_some_and(|b| b != n.basis)
                || (!needle.is_empty()
                    && !n.title.to_lowercase().contains(&needle)
                    && !n.content.to_lowercase().contains(&needle))
            {
                continue;
            }
            let path = paths.get(n.memory.as_str()).and_then(|p| p.as_ref());
            let topic = path
                .and_then(|p| p.first())
                .and_then(|id| branch.iter().find(|n| n.memory.as_str() == *id));
            if query
                .topic
                .as_ref()
                .is_some_and(|t| topic.is_none_or(|n| t != &n.memory))
            {
                continue;
            }
            let replaced = self.ui_replaced(owner, n).await?;
            if query.updated && replaced.is_none() {
                continue;
            }
            let mut end = n.content.len().min(280);
            while !n.content.is_char_boundary(end) {
                end -= 1;
            }
            matched.push(UiNode {
                node: (*n).clone(),
                summary: n.content[..end].to_owned(),
                path: std::iter::once(query.root.label().to_owned())
                    .chain(
                        path.cloned()
                            .unwrap_or_else(|| vec![n.memory.as_str()])
                            .into_iter()
                            .filter_map(|id| branch.iter().find(|n| n.memory.as_str() == id))
                            .map(|n| n.title.clone()),
                    )
                    .collect(),
                topic: topic.map(|n| memory_ref(n)),
                replaced,
                link_count: 0,
                lesson: None,
                skill_draft: None,
            });
        }
        let total = matched.len();
        let key = |n: &UiNode| match query.sort {
            NodesSort::Recent => n.node.last_linked_ms,
            NodesSort::Sources => u64::from(n.node.direct_sources),
        };
        matched.sort_by(|a, b| key(b).cmp(&key(a)).then(a.node.memory.cmp(&b.node.memory)));
        if let Some((at, memory)) = &query.after {
            matched.retain(|n| key(n) < *at || (key(n) == *at && n.node.memory > *memory));
        }
        let more = matched.len() > query.limit;
        matched.truncate(query.limit);
        let candidates = self.skill_candidates(owner, &query.project, None).await?;
        for n in &mut matched {
            let mut response = self.capture.db.query("RETURN array::len((SELECT id FROM memory_link WHERE owner_id=$owner AND project_id=$project AND (from_memory=$memory OR to_memory=$memory)));")
                .bind(("owner", owner.to_string())).bind(("project", query.project.to_string())).bind(("memory", n.node.memory.to_string())).await.map_err(storage)?.check().map_err(storage)?;
            n.link_count = response
                .take::<Option<usize>>(0)
                .map_err(storage)?
                .ok_or(RuntimeError::StorageUnavailable)?;
            n.lesson = if query.root.is_lesson() {
                Some(
                    self.lesson_stats(owner, &query.project, &n.node.memory)
                        .await?,
                )
            } else {
                None
            };
            n.skill_draft = skill_ref(&candidates, &n.node.memory);
        }
        Ok(NodesPage {
            total,
            topics,
            nodes: matched,
            more,
        })
    }
}
fn day_label(days: i64) -> String {
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let mut year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

#[cfg(test)]
mod tests {
    use super::lineage;
    use std::collections::HashMap;

    #[test]
    fn topic_lineage_requires_a_complete_acyclic_chain_to_its_root() {
        let mut parents: HashMap<_, _> = [("A", "tree-root:decisions"), ("B", "A"), ("C", "B")]
            .into_iter()
            .map(|(child, parent)| (child.to_owned(), parent.to_owned()))
            .collect();
        assert_eq!(
            lineage("C", "tree-root:decisions", &parents),
            Some(vec!["A", "B", "C"])
        );
        assert!(lineage("C", "tree-root:preferences", &parents).is_none());
        parents.remove("B");
        assert!(lineage("C", "tree-root:decisions", &parents).is_none());
        parents.insert("B".into(), "C".into());
        assert!(lineage("C", "tree-root:decisions", &parents).is_none());
    }

    #[test]
    fn topic_lineage_is_bounded_to_64_edges() {
        let mut parents = HashMap::from([("0".to_owned(), "tree-root:decisions".to_owned())]);
        for i in 1..=64 {
            parents.insert(i.to_string(), (i - 1).to_string());
        }
        assert_eq!(
            lineage("63", "tree-root:decisions", &parents)
                .unwrap()
                .len(),
            64
        );
        assert!(lineage("64", "tree-root:decisions", &parents).is_none());
    }
}

// Gregorian civil date from Unix days (400-year eras); no timezone or locale dependency.
