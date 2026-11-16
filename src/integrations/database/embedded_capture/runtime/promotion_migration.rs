//! Offline promotion reads and atomic review/retirement transitions.
use super::*;
use crate::domain_models::memory_tree::{Forward, StoreKind};
use crate::domain_models::promotion::*;

#[derive(SurrealValue)]
struct ReviewRow {
    memory_id: String,
    revision_id: String,
    verdict: String,
    decided_at_ms: u64,
    retired_at_ms: Option<u64>,
}
#[derive(SurrealValue)]
struct SourceLink {
    source_id: String,
    linked_at_ms: u64,
}
#[derive(SurrealValue)]
struct ForwardStatus {
    source_id: String,
    status: String,
}
#[derive(SurrealValue)]
struct CaptureForward {
    forward_id: String,
    source_id: String,
}

impl EmbeddedRuntimeStore {
    pub async fn promotion_reviews(
        &self,
        owner: &OwnerId,
        project: &ProjectId,
    ) -> Result<Vec<PromotionReview>, RuntimeError> {
        self.validate_activity_scope(owner, project)?;
        let mut r = self.capture.db.query("SELECT memory_id, revision_id, verdict, decided_at_ms, retired_at_ms FROM memory_promotion_review WHERE owner_id=$owner AND project_id=$project ORDER BY decided_at_ms DESC, revision_id DESC;")
            .bind(("owner", owner.to_string())).bind(("project", project.to_string())).await.map_err(storage)?.check().map_err(storage)?;
        let rows: Vec<ReviewRow> = r.take(0).map_err(storage)?;
        rows.into_iter()
            .map(|r| {
                Ok(PromotionReview {
                    memory: MemoryId::new(r.memory_id).map_err(storage)?,
                    revision: RevisionId::new(r.revision_id).map_err(storage)?,
                    verdict: match r.verdict.as_str() {
                        "user" => PromotionVerdict::User,
                        "project" => PromotionVerdict::Project,
                        _ => return Err(RuntimeError::StorageUnavailable),
                    },
                    decided_at_ms: r.decided_at_ms,
                    retired_at_ms: r.retired_at_ms,
                })
            })
            .collect()
    }

    /// Every distinct linked source, newest link first, without the curator's 32-source cap.
    pub async fn promotion_sources(
        &self,
        owner: &OwnerId,
        project: &ProjectId,
        memory: &MemoryId,
    ) -> Result<Vec<CaptureEvent>, RuntimeError> {
        self.validate_activity_scope(owner, project)?;
        self.promotion_sources_in(&self.capture.db, owner, project, memory)
            .await
    }
    async fn promotion_sources_in(
        &self,
        db: &impl QueryConnection,
        owner: &OwnerId,
        project: &ProjectId,
        memory: &MemoryId,
    ) -> Result<Vec<CaptureEvent>, RuntimeError> {
        let ids = self
            .promotion_source_ids(db, owner, project, memory)
            .await?;
        let mut events = Vec::new();
        for id in ids {
            if let Some(row) = self
                .source_row(db, owner.as_str(), &id)
                .await?
                .into_iter()
                .find(|r| r.project_id == project.as_str())
            {
                events.push(source_event(row)?);
            }
        }
        Ok(events)
    }
    async fn promotion_source_ids(
        &self,
        db: &impl QueryConnection,
        owner: &OwnerId,
        project: &ProjectId,
        memory: &MemoryId,
    ) -> Result<Vec<String>, RuntimeError> {
        let mut r = db.query("SELECT source_id, linked_at_ms FROM memory_node_source WHERE owner_id=$owner AND project_id=$project AND memory_id=$memory;")
            .bind(("owner", owner.to_string())).bind(("project", project.to_string())).bind(("memory", memory.to_string())).await.map_err(storage)?.check().map_err(storage)?;
        let mut links: Vec<SourceLink> = r.take(0).map_err(storage)?;
        links.sort_by(|a, b| {
            b.linked_at_ms
                .cmp(&a.linked_at_ms)
                .then_with(|| a.source_id.cmp(&b.source_id))
        });
        let mut seen = HashSet::new();
        Ok(links
            .into_iter()
            .filter_map(|link| {
                seen.insert(link.source_id.clone())
                    .then_some(link.source_id)
            })
            .collect())
    }
    pub async fn promotion_admitted_sources(
        &self,
        owner: &OwnerId,
        project: &ProjectId,
    ) -> Result<Vec<SourceId>, RuntimeError> {
        self.validate_activity_scope(owner, project)?;
        let mut r = self.capture.db.query("SELECT VALUE sources FROM memory_runtime_operation WHERE owner_id=$owner AND project_id=$project AND string::starts_with(operation_id, 'promote-migration:');")
            .bind(("owner", owner.to_string())).bind(("project", project.to_string())).await.map_err(storage)?.check().map_err(storage)?;
        let rows: Vec<Vec<String>> = r.take(0).map_err(storage)?;
        rows.into_iter()
            // Several nodes can use the same event. Count admissions, not duplicate slots.
            .flat_map(|row| row.into_iter().collect::<BTreeSet<_>>())
            .map(|s| SourceId::new(s).map_err(storage))
            .collect()
    }
    /// Reviews and outbox writes commit together, after checking the reviewed snapshots.
    pub async fn commit_promotion_reviews(
        &self,
        owner: &OwnerId,
        project: &ProjectId,
        batch: &[(PromotionCandidate, PromotionVerdict)],
        now: u64,
    ) -> Result<usize, RuntimeError> {
        self.validate_activity_scope(owner, project)?;
        if StoreKind::of(project) != StoreKind::Project || batch.is_empty() || batch.len() > 16 {
            return Err(RuntimeError::InvalidInput("promotion batch"));
        }
        let _guard = self.capture.writes.lock().await;
        let tx = self.capture.db.clone().begin().await.map_err(storage)?;
        let result = async {
            let mut forwards = 0;
            for (candidate, verdict) in batch {
                let n = &candidate.node;
                if self.current_active(&tx, owner.as_str(), project.as_str(), n.memory.as_str()).await?.as_deref() != Some(n.revision.as_str()) { return Err(RuntimeError::Conflict); }
                let mut check = tx.query("SELECT VALUE child_memory FROM memory_tree_edge WHERE owner_id=$owner AND project_id=$project AND parent_memory=$memory LIMIT 1; SELECT VALUE revision_id FROM memory_promotion_review WHERE owner_id=$owner AND project_id=$project AND memory_id=$memory AND revision_id=$revision;")
                    .bind(("owner", owner.to_string())).bind(("project", project.to_string())).bind(("memory", n.memory.to_string())).bind(("revision", n.revision.to_string())).await.map_err(storage)?.check().map_err(storage)?;
                if !check.take::<Vec<String>>(0).map_err(storage)?.is_empty() || !check.take::<Vec<String>>(1).map_err(storage)?.is_empty() { return Err(RuntimeError::Conflict); }
                let events = self.promotion_sources_in(&tx, owner, project, &n.memory).await?;
                if events != candidate.sources { return Err(RuntimeError::Conflict); }
                tx.query("CREATE memory_promotion_review SET owner_id=$owner, project_id=$project, memory_id=$memory, revision_id=$revision, verdict=$verdict, decided_at_ms=$now RETURN NONE;")
                    .bind(("owner", owner.to_string())).bind(("project", project.to_string())).bind(("memory", n.memory.to_string())).bind(("revision", n.revision.to_string())).bind(("verdict", verdict.as_str())).bind(("now", now)).await.map_err(storage)?.check().map_err(storage)?;
                if *verdict == PromotionVerdict::User {
                    if events.is_empty() { return Err(RuntimeError::InvalidInput("promotion requires sources")); }
                    for event in events {
                        let mut r = tx.query("SELECT VALUE forward_id FROM memory_forward WHERE owner_id=$owner AND project_id=$project AND action='capture' AND source_id=$source;")
                            .bind(("owner", owner.to_string())).bind(("project", project.to_string())).bind(("source", event.source.to_string())).await.map_err(storage)?.check().map_err(storage)?;
                        if r.take::<Vec<String>>(0).map_err(storage)?.is_empty() { forwards += 1; }
                        self.capture_forward_tx(&tx, owner, project, &event.source, now).await?;
                    }
                }
            }
            Ok(forwards)
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

    pub async fn export_capture_forwards(
        &self,
        owner: &OwnerId,
        project: &ProjectId,
    ) -> Result<Vec<Forward>, RuntimeError> {
        self.validate_activity_scope(owner, project)?;
        let tx = self.capture.db.clone().begin().await.map_err(storage)?;
        let result = async {
            // Offline transfer cannot apply purges. Leave captures behind any pending purge
            // of the same source for the service's ordered drain instead of overtaking it.
            let mut r = tx.query("LET $purging = (SELECT VALUE source_id FROM memory_forward WHERE owner_id=$owner AND project_id=$project AND action='purge' AND status='pending'); SELECT forward_id, source_id FROM memory_forward WHERE owner_id=$owner AND project_id=$project AND action='capture' AND status='pending' AND source_id NOT IN $purging ORDER BY created_at_ms, forward_id;")
                .bind(("owner", owner.to_string())).bind(("project", project.to_string())).await.map_err(storage)?.check().map_err(storage)?;
            let rows: Vec<CaptureForward> = r.take(1).map_err(storage)?;
            let mut out = Vec::new();
            for forward in rows {
                if let Some(row) = self.source_row(&tx, owner.as_str(), &forward.source_id).await?.into_iter().find(|r| r.project_id == project.as_str()) {
                    out.push(Forward { forward_id: forward.forward_id, action: "capture", source_id: SourceId::new(forward.source_id).map_err(storage)?, source: Some(source_event(row)?) });
                }
            }
            Ok(out)
        }.await;
        let _ = tx.cancel().await;
        result
    }

    pub async fn inspect_copies(
        &self,
        owner: &OwnerId,
        project: &ProjectId,
        ids: &[SourceId],
    ) -> Result<CopyInspection, RuntimeError> {
        self.validate_activity_scope(owner, project)?;
        if StoreKind::of(project) != StoreKind::User || ids.len() > 10_000 {
            return Err(RuntimeError::InvalidInput(
                "copy inspection requires a user store and at most 10000 IDs",
            ));
        }
        let tx = self.capture.db.clone().begin().await.map_err(storage)?;
        let result = async {
            let mut out = CopyInspection::default();
            let requested: Vec<_> = ids.iter().map(ToString::to_string).collect();
            let mut r = tx.query("LET $revisions = (SELECT VALUE revision_id FROM memory_revision_state WHERE owner_id=$owner AND project_id=$project AND state='active'); LET $active = (SELECT VALUE memory_id FROM memory_tree_revision WHERE owner_id=$owner AND project_id=$project AND revision_id IN $revisions); SELECT VALUE source_id FROM memory_node_source WHERE owner_id=$owner AND project_id=$project AND memory_id IN $active AND source_id IN $ids; SELECT VALUE source_id FROM memory_source WHERE owner_id=$owner AND project_id=$project AND source_id IN $ids;")
                .bind(("owner", owner.to_string())).bind(("project", project.to_string())).bind(("ids", requested.clone())).await.map_err(storage)?.check().map_err(storage)?;
            let active: HashSet<String> = r.take::<Vec<String>>(2).map_err(storage)?.into_iter().collect();
            let present: HashSet<String> = r.take::<Vec<String>>(3).map_err(storage)?.into_iter().collect();
            let consumption = self.consumption_for(&tx, owner.as_str(), &requested).await?;
            for id in ids.iter().collect::<BTreeSet<_>>() {
                if !present.contains(id.as_str()) { out.missing.push(id.clone()); }
                else if active.contains(id.as_str()) { out.active.push(id.clone()); }
                else if consumption.iter().any(|c| c.project_id == project.as_str() && c.source_id == id.as_str()) { out.consumed.push(id.clone()); }
                else { out.pending.push(id.clone()); }
            }
            Ok(out)
        }.await;
        let _ = tx.cancel().await;
        result
    }

    /// Listing does not mutate anything; an apply re-checks each listed node in its own transaction.
    pub async fn retire_promoted(
        &self,
        owner: &OwnerId,
        actor: &ActorId,
        project: &ProjectId,
        apply: Option<&[MemoryId]>,
        now: u64,
    ) -> Result<PromotionRetirement, RuntimeError> {
        self.validate_activity_scope(owner, project)?;
        if StoreKind::of(project) != StoreKind::Project {
            return Err(RuntimeError::InvalidInput(
                "promotion requires a project store",
            ));
        }
        let _guard = self.capture.writes.lock().await;
        let reviews = self.promotion_reviews(owner, project).await?;
        let mut seen = HashSet::new();
        let mut out = PromotionRetirement::default();
        for review in reviews {
            if !seen.insert(review.memory.clone())
                || review.verdict != PromotionVerdict::User
                || review.retired_at_ms.is_some()
            {
                continue;
            }
            let tx = self.capture.db.clone().begin().await.map_err(storage)?;
            let result = async {
                if self.current_active(&tx, owner.as_str(), project.as_str(), review.memory.as_str()).await?.as_deref() != Some(review.revision.as_str()) { return Ok(false); }
                let ids = self.promotion_source_ids(&tx, owner, project, &review.memory).await?;
                let mut r = tx.query("SELECT source_id, status FROM memory_forward WHERE owner_id=$owner AND project_id=$project AND action='capture' AND source_id IN $ids;")
                    .bind(("owner", owner.to_string())).bind(("project", project.to_string())).bind(("ids", ids.clone())).await.map_err(storage)?.check().map_err(storage)?;
                let statuses: Vec<ForwardStatus> = r.take(0).map_err(storage)?;
                if statuses.iter().any(|s| s.status == "failed") { out.blocked.push(review.memory.clone()); return Ok(false); }
                if ids.is_empty() || !ids.iter().all(|id| statuses.iter().any(|s| &s.source_id == id && s.status == "done")) { return Ok(false); }
                out.eligible.push(PromotionEligible { node: review.memory.clone(), sources: ids.into_iter().map(|s| SourceId::new(s).map_err(storage)).collect::<Result<_,_>>()? });
                if !apply.is_some_and(|ids| ids.contains(&review.memory)) { return Ok(false); }
                let skipped = self.retire_audited_tree_tx(&tx, owner, actor, project, &review.memory, now, "promotion_retire").await?;
                let mut r=tx.query("SELECT VALUE revision_id FROM memory_tree_revision WHERE owner_id=$owner AND project_id=$project AND memory_id=$memory;").bind(("owner",owner.to_string())).bind(("project",project.to_string())).bind(("memory",review.memory.to_string())).await.map_err(storage)?.check().map_err(storage)?;
                let revisions:Vec<String>=r.take(0).map_err(storage)?;
                self.cascade_conclusions_tx(&tx,owner,actor,project,&revisions,now).await?;
                tx.query("UPDATE memory_promotion_review SET retired_at_ms=$now WHERE owner_id=$owner AND project_id=$project AND memory_id=$memory AND revision_id=$revision RETURN NONE;")
                    .bind(("owner", owner.to_string())).bind(("project", project.to_string())).bind(("memory", review.memory.to_string())).bind(("revision", review.revision.to_string())).bind(("now", now)).await.map_err(storage)?.check().map_err(storage)?;
                self.finish_attached_timeline_activity(&tx, &[], std::slice::from_ref(&review.revision), None, skipped).await?;
                Ok(true)
            }.await;
            match result {
                Ok(true) => {
                    tx.commit().await.map_err(storage)?;
                    out.retired += 1;
                }
                Ok(false) => {
                    tx.cancel().await.map_err(storage)?;
                }
                Err(e) => {
                    let _ = tx.cancel().await;
                    return Err(e);
                }
            }
        }
        out.eligible.sort_by(|a, b| a.node.cmp(&b.node));
        out.blocked.sort();
        Ok(out)
    }
}
