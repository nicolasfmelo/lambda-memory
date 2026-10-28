//! Runtime-only vectors and at-most-once background admission, separate from campaign accounting.
use super::*;
use crate::domain_models::embeddings::{Embedding, EmbeddingSpace, cosine};

#[derive(Debug, SurrealValue)]
struct VectorRank {
    revision_id: String,
    score: f64,
}

impl EmbeddedRuntimeStore {
    pub async fn has_active_runtime(
        &self,
        owner: &OwnerId,
        project: &ProjectId,
    ) -> Result<bool, RuntimeError> {
        let mut response = self.capture.db.query("SELECT VALUE revision_id FROM memory_revision_state WHERE owner_id = $owner AND project_id = $project AND state = 'active' AND memory_id != $playbook LIMIT 1;")
            .bind(("owner", owner.to_string())).bind(("project", project.to_string()))
            .bind(("playbook", crate::domain_models::procedural::PLAYBOOK_MEMORY)).await.map_err(storage)?.check().map_err(storage)?;
        let ids: Vec<String> = response.take(0).map_err(storage)?;
        Ok(!ids.is_empty())
    }
    pub async fn runtime_revision(
        &self,
        owner: &OwnerId,
        project: &ProjectId,
        id: &RevisionId,
    ) -> Result<RuntimeRevision, RuntimeError> {
        let _guard = self.capture.writes.lock().await;
        let record = self
            .revision_row(&self.capture.db, owner.as_str(), id.as_str())
            .await?
            .ok_or(RuntimeError::Missing)?;
        if record.project_id != project.as_str() {
            return Err(RuntimeError::Unauthorized);
        }
        let (states, jobs) = self
            .selected_metadata(owner, std::iter::once(&record))
            .await?;
        self.assemble(record, &states, &jobs)
    }
    pub async fn admit_runtime_operation(
        &self,
        owner: &OwnerId,
        project: &ProjectId,
        operation: &str,
        events: &[CaptureEvent],
    ) -> Result<(), RuntimeError> {
        if operation.is_empty() || operation.len() > 512 || events.is_empty() || events.len() > 32 {
            return Err(RuntimeError::InvalidInput("runtime operation bounds"));
        }
        if events
            .iter()
            .any(|event| &event.owner != owner || &event.project != project)
        {
            return Err(RuntimeError::Unauthorized);
        }
        let sources = events
            .iter()
            .map(|event| event.source.to_string())
            .collect::<Vec<_>>();
        let _guard = self.capture.writes.lock().await;
        let tx = self.capture.db.clone().begin().await.map_err(storage)?;
        self.require_live_originals(
            &tx,
            owner.as_str(),
            &events
                .iter()
                .cloned()
                .map(SourceRecord::from)
                .collect::<Vec<_>>(),
        )
        .await?;
        let mut response = tx.query("SELECT VALUE operation_id FROM memory_runtime_operation WHERE owner_id = $owner AND project_id = $project AND (operation_id = $operation OR ($dream AND string::starts_with(operation_id, 'dream-job:') AND array::intersect(sources, $sources) != []));")
            .bind(("owner", owner.to_string())).bind(("project", project.to_string())).bind(("operation", operation.to_owned()))
            .bind(("dream", operation.starts_with("dream-job:"))).bind(("sources", sources.clone()))
            .await.map_err(storage)?.check().map_err(storage)?;
        let existing: Vec<String> = response.take(0).map_err(storage)?;
        if !existing.is_empty() {
            return cancel_err(tx, RuntimeError::UncertainAdmission).await;
        }
        tx.query("CREATE memory_runtime_operation SET owner_id = $owner, project_id = $project, operation_id = $operation, sources = $sources RETURN NONE;")
            .bind(("owner", owner.to_string())).bind(("project", project.to_string())).bind(("operation", operation.to_owned()))
            .bind(("sources", sources))
            .await.map_err(storage)?.check().map_err(storage)?;
        tx.commit().await.map(|_| ()).map_err(storage)
    }
    /// Removes an admission only when its provider call provably did no billable work.
    pub async fn release_runtime_operation(
        &self,
        owner: &OwnerId,
        project: &ProjectId,
        operation: &str,
    ) -> Result<(), RuntimeError> {
        let _guard = self.capture.writes.lock().await;
        self.capture.db.query("DELETE memory_runtime_operation WHERE owner_id = $owner AND project_id = $project AND operation_id = $operation RETURN NONE;")
            .bind(("owner", owner.to_string())).bind(("project", project.to_string())).bind(("operation", operation.to_owned()))
            .await.map_err(storage)?.check().map_err(storage)?;
        Ok(())
    }
    pub async fn unindexed_active(
        &self,
        owner: &OwnerId,
        project: &ProjectId,
        space: &EmbeddingSpace,
        limit: usize,
    ) -> Result<Vec<RuntimeRevision>, RuntimeError> {
        if limit == 0 || limit > 32 {
            return Err(RuntimeError::InvalidInput("index batch bounds"));
        }
        let _guard = self.capture.writes.lock().await;
        let mut response = self.capture.db.query(format!("LET $active = (SELECT VALUE revision_id FROM memory_revision_state WHERE owner_id = $owner AND project_id = $project AND state = 'active' AND memory_id != $playbook); LET $indexed = (SELECT VALUE revision_id FROM memory_runtime_embedding WHERE owner_id = $owner AND project_id = $project AND provider = $provider AND model = $model AND index_revision = $index AND dimensions = $dimensions); SELECT {REV_FIELDS} FROM memory_revision WHERE owner_id = $owner AND project_id = $project AND revision_id IN $active AND revision_id NOT IN $indexed ORDER BY revision_id LIMIT $limit;"))
            .bind(("owner", owner.to_string())).bind(("project", project.to_string())).bind(("provider", space.provider.clone()))
            .bind(("model", space.model.clone())).bind(("index", space.revision.clone())).bind(("dimensions", space.dimensions))
            .bind(("limit", limit)).bind(("playbook", crate::domain_models::procedural::PLAYBOOK_MEMORY))
            .await.map_err(storage)?.check().map_err(storage)?;
        let rows: Vec<RevisionRecord> = response.take(2).map_err(storage)?;
        let (states, jobs) = self.selected_metadata(owner, rows.iter()).await?;
        rows.into_iter()
            .map(|row| self.assemble(row, &states, &jobs))
            .collect()
    }
    pub async fn has_runtime_embedding(
        &self,
        item: &RuntimeRevision,
        space: &EmbeddingSpace,
    ) -> Result<bool, RuntimeError> {
        let revision = &item.revision;
        let mut response = self.capture.db.query("SELECT VALUE revision_id FROM memory_runtime_embedding WHERE owner_id = $owner AND project_id = $project AND revision_id = $revision AND provider = $provider AND model = $model AND index_revision = $index AND dimensions = $dimensions LIMIT 1;")
            .bind(("owner", revision.owner.to_string())).bind(("project", revision.project.to_string())).bind(("revision", revision.id.to_string()))
            .bind(("provider", space.provider.clone())).bind(("model", space.model.clone())).bind(("index", space.revision.clone())).bind(("dimensions", space.dimensions))
            .await.map_err(storage)?.check().map_err(storage)?;
        let rows: Vec<String> = response.take(0).map_err(storage)?;
        Ok(!rows.is_empty())
    }
    pub async fn save_runtime_embedding(
        &self,
        wanted: &RuntimeRevision,
        embedding: &Embedding,
    ) -> Result<(), RuntimeError> {
        if embedding.values.len() != embedding.space.dimensions
            || cosine(&embedding.values, &embedding.values).is_none()
        {
            return Err(RuntimeError::InvalidInput("invalid runtime vector"));
        }
        let revision = &wanted.revision;
        self.validate_activity_scope(&revision.owner, &revision.project)?;
        let _guard = self.capture.writes.lock().await;
        let record = self
            .revision_row(
                &self.capture.db,
                revision.owner.as_str(),
                revision.id.as_str(),
            )
            .await?
            .ok_or(RuntimeError::Missing)?;
        let (states, jobs) = self
            .selected_metadata(&revision.owner, std::iter::once(&record))
            .await?;
        let current = self.assemble(record, &states, &jobs)?;
        if current.revision.project != revision.project
            || current.revision.content != revision.content
            || current.sources != wanted.sources
            || current.revision.sources != revision.sources
        {
            return Err(RuntimeError::Conflict);
        }
        // Purge/re-capture cannot resurrect a vector from an awaited provider response.
        self.require_live_originals(
            &self.capture.db,
            revision.owner.as_str(),
            &current
                .sources
                .iter()
                .cloned()
                .map(SourceRecord::from)
                .collect::<Vec<_>>(),
        )
        .await?;
        if self
            .has_runtime_embedding(&current, &embedding.space)
            .await?
        {
            return Ok(());
        }
        let tx = self.capture.db.clone().begin().await.map_err(storage)?;
        tx.query("CREATE memory_runtime_embedding SET owner_id = $owner, project_id = $project, revision_id = $revision, provider = $provider, model = $model, index_revision = $index, dimensions = $dimensions, embedding = $embedding RETURN NONE;")
            .bind(("owner", revision.owner.to_string())).bind(("project", revision.project.to_string())).bind(("revision", revision.id.to_string()))
            .bind(("provider", embedding.space.provider.clone())).bind(("model", embedding.space.model.clone())).bind(("index", embedding.space.revision.clone()))
            .bind(("dimensions", embedding.space.dimensions)).bind(("embedding", embedding.values.clone()))
            .await.map_err(storage)?.check().map_err(storage)?;
        self.finish_attached_activity(
            &tx,
            &[],
            std::slice::from_ref(&revision.id),
            Some(crate::domain_models::runtime_diagnostics::ActivityCounts {
                revisions_indexed: Some(1),
                ..Default::default()
            }),
        )
        .await?;
        tx.commit().await.map_err(storage)?;
        Ok(())
    }
    pub async fn active_runtime_hits(
        &self,
        owner: &OwnerId,
        projects: &[ProjectId],
        ids: &[RevisionId],
    ) -> Result<Vec<RuntimeHit>, RuntimeError> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        if ids.len() > 32 || projects.is_empty() || projects.len() > 16 {
            return Err(RuntimeError::InvalidInput("active candidate bounds"));
        }
        let _guard = self.capture.writes.lock().await;
        let mut response = self.capture.db.query(format!("LET $active = (SELECT VALUE revision_id FROM memory_revision_state WHERE owner_id = $owner AND project_id IN $projects AND state = 'active'); SELECT {REV_FIELDS} FROM memory_revision WHERE owner_id = $owner AND project_id IN $projects AND revision_id IN $ids AND revision_id IN $active;"))
            .bind(("owner", owner.to_string())).bind(("projects", projects.iter().map(ToString::to_string).collect::<Vec<_>>()))
            .bind(("ids", ids.iter().map(ToString::to_string).collect::<Vec<_>>())).await.map_err(storage)?.check().map_err(storage)?;
        let rows: Vec<RevisionRecord> = response.take(1).map_err(storage)?;
        let (states, jobs) = self.selected_metadata(owner, rows.iter()).await?;
        let mut hits = rows
            .into_iter()
            .map(|row| {
                self.assemble(row, &states, &jobs).map(|item| RuntimeHit {
                    item,
                    lexical_score: 0,
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        hits.sort_by_key(|hit| ids.iter().position(|id| id == &hit.item.revision.id));
        Ok(hits)
    }
    pub async fn runtime_vector_hits(
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
        let mut response = self.capture.db.query("LET $tree = (SELECT VALUE revision_id FROM memory_tree_revision WHERE owner_id = $owner AND project_id = $project); LET $active = (SELECT VALUE revision_id FROM memory_revision_state WHERE owner_id = $owner AND project_id = $project AND state = 'active' AND memory_id != $playbook AND revision_id NOT IN $tree); SELECT revision_id, vector::similarity::cosine(embedding, $vector) AS score FROM memory_runtime_embedding WHERE owner_id = $owner AND project_id = $project AND provider = $provider AND model = $model AND index_revision = $index AND dimensions = $dimensions AND revision_id IN $active ORDER BY score DESC, revision_id LIMIT $limit;")
            .bind(("owner", owner.to_string())).bind(("project", project.to_string())).bind(("provider", query.space.provider.clone()))
            .bind(("model", query.space.model.clone())).bind(("index", query.space.revision.clone())).bind(("dimensions", query.space.dimensions))
            .bind(("vector", query.values.clone())).bind(("limit", limit))
            .bind(("playbook", crate::domain_models::procedural::PLAYBOOK_MEMORY)).await.map_err(storage)?.check().map_err(storage)?;
        let rows: Vec<VectorRank> = response.take(2).map_err(storage)?;
        let ids = rows
            .into_iter()
            .map(|row| {
                if !row.score.is_finite() {
                    return Err(RuntimeError::StorageUnavailable);
                }
                RevisionId::new(row.revision_id).map_err(|_| RuntimeError::StorageUnavailable)
            })
            .collect::<Result<Vec<_>, _>>()?;
        self.active_runtime_hits(owner, std::slice::from_ref(project), &ids)
            .await
    }
}
