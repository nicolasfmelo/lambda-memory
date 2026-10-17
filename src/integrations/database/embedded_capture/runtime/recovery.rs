//! Operator-only recovery: immutable admission lineage, never an automatic retry path.
use super::*;

const MAX_RECOVERY_ADMISSIONS: usize = 32;
#[derive(SurrealValue)]
struct DreamAdmission {
    operation_id: String,
    sources: Vec<String>,
}

impl EmbeddedRuntimeStore {
    pub async fn blocked_dream_sources(
        &self,
        owner: &OwnerId,
        project: &ProjectId,
        limit: usize,
    ) -> Result<DreamRecoveryBatch, RuntimeError> {
        if limit == 0 || limit > 16 {
            return Err(RuntimeError::InvalidInput("recovery batch bound"));
        }
        self.read_dream_recovery(owner, project, &[], limit).await
    }

    pub async fn dream_recovery_batch(
        &self,
        owner: &OwnerId,
        project: &ProjectId,
        sources: &[SourceId],
    ) -> Result<DreamRecoveryBatch, RuntimeError> {
        if sources.is_empty()
            || sources.len() > 16
            || sources
                .iter()
                .enumerate()
                .any(|(i, s)| sources[..i].contains(s))
        {
            return Err(RuntimeError::InvalidInput("recovery sources"));
        }
        let batch = self
            .read_dream_recovery(owner, project, sources, sources.len())
            .await?;
        if batch.sources.len() != sources.len() {
            return Err(RuntimeError::Conflict);
        }
        Ok(batch)
    }

    async fn read_dream_recovery(
        &self,
        owner: &OwnerId,
        project: &ProjectId,
        selected: &[SourceId],
        limit: usize,
    ) -> Result<DreamRecoveryBatch, RuntimeError> {
        let selected = selected.iter().map(ToString::to_string).collect::<Vec<_>>();
        let mut response = self.capture.db.query(format!(
            // Materialized once: row predicates would otherwise re-run both subqueries per source.
            "LET $consumed = (SELECT VALUE source_id FROM memory_source_consumption WHERE owner_id = $owner AND project_id = $project); LET $admitted = array::distinct(array::flatten((SELECT VALUE sources FROM memory_runtime_operation WHERE owner_id = $owner AND project_id = $project AND string::starts_with(operation_id, 'dream-job:')))); SELECT {SOURCE_PROJECTION} FROM memory_source WHERE owner_id = $owner AND project_id = $project AND ($selected = [] OR source_id IN $selected) AND source_id NOT IN $consumed AND source_id IN $admitted ORDER BY recorded_at_ms, source_id LIMIT $limit;"
        )).bind(("owner", owner.to_string())).bind(("project", project.to_string()))
            .bind(("selected", selected)).bind(("limit", limit))
            .await.map_err(storage)?.check().map_err(storage)?;
        let rows: Vec<SourceRecord> = response.take(2).map_err(storage)?;
        let sources = rows
            .into_iter()
            .map(source_event)
            .collect::<Result<Vec<_>, _>>()?;
        let ids = sources
            .iter()
            .map(|s| s.source.to_string())
            .collect::<Vec<_>>();
        let admissions = self
            .dream_admissions(&self.capture.db, owner, project, &ids)
            .await?;
        // Keep the plan executable: recovery accepts at most 32 previous admissions.
        let mut kept = std::collections::BTreeSet::new();
        let mut sources = sources;
        sources.retain(|source| {
            let touching = admissions
                .iter()
                .filter(|row| row.sources.iter().any(|id| id == source.source.as_str()))
                .map(|row| row.operation_id.as_str())
                .filter(|id| !kept.contains(*id))
                .collect::<Vec<_>>();
            if kept.len() + touching.len() > MAX_RECOVERY_ADMISSIONS {
                return false;
            }
            kept.extend(touching.into_iter().map(str::to_owned));
            true
        });
        let mut previous_operations = admissions
            .into_iter()
            .filter(|row| kept.contains(&row.operation_id))
            .map(|row| JobId::new(row.operation_id).map_err(|_| RuntimeError::StorageUnavailable))
            .collect::<Result<Vec<_>, _>>()?;
        previous_operations.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        Ok(DreamRecoveryBatch {
            sources,
            previous_operations,
        })
    }

    async fn dream_admissions(
        &self,
        connection: &impl QueryConnection,
        owner: &OwnerId,
        project: &ProjectId,
        sources: &[String],
    ) -> Result<Vec<DreamAdmission>, RuntimeError> {
        let mut response = connection.query("SELECT operation_id, sources FROM memory_runtime_operation WHERE owner_id = $owner AND project_id = $project AND string::starts_with(operation_id, 'dream-job:') AND array::intersect(sources, $sources) != [];")
            .bind(("owner", owner.to_string())).bind(("project", project.to_string()))
            .bind(("sources", sources.to_vec())).await.map_err(storage)?.check().map_err(storage)?;
        response.take(0).map_err(storage)
    }

    // Source snapshots, operator provenance and expected admission identities are separate guards.
    #[allow(clippy::too_many_arguments)]
    pub async fn admit_runtime_recovery_operation(
        &self,
        owner: &OwnerId,
        actor: &ActorId,
        project: &ProjectId,
        operation: &str,
        events: &[CaptureEvent],
        previous_operations: &[JobId],
        authorized_at_ms: u64,
    ) -> Result<(), RuntimeError> {
        if !operation.starts_with("dream-job:recovery:")
            || operation.len() > 512
            || events.is_empty()
            || events.len() > 16
            || previous_operations.is_empty()
            || previous_operations.len() > 32
            || authorized_at_ms == 0
            || previous_operations.iter().enumerate().any(|(i, id)| {
                !id.as_str().starts_with("dream-job:") || previous_operations[..i].contains(id)
            })
            || events
                .iter()
                .enumerate()
                .any(|(i, event)| events[..i].iter().any(|old| old.source == event.source))
        {
            return Err(RuntimeError::InvalidInput("recovery authorization"));
        }
        if events
            .iter()
            .any(|event| &event.owner != owner || &event.project != project)
        {
            return Err(RuntimeError::Unauthorized);
        }
        let ids = events
            .iter()
            .map(|e| e.source.to_string())
            .collect::<Vec<_>>();
        let mut expected = previous_operations
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        expected.sort();
        let _guard = self.capture.writes.lock().await;
        let tx = self.capture.db.clone().begin().await.map_err(storage)?;
        let result = async {
            self.require_live_originals(&tx, owner.as_str(),
                &events.iter().cloned().map(SourceRecord::from).collect::<Vec<_>>()).await?;
            let mut response = tx.query("SELECT VALUE source_id FROM memory_source_consumption WHERE owner_id = $owner AND project_id = $project AND source_id IN $sources;")
                .bind(("owner", owner.to_string())).bind(("project", project.to_string()))
                .bind(("sources", ids.clone())).await.map_err(storage)?.check().map_err(storage)?;
            let consumed: Vec<String> = response.take(0).map_err(storage)?;
            if !consumed.is_empty() { return Err(RuntimeError::Conflict); }
            let admissions = self.dream_admissions(&tx, owner, project, &ids).await?;
            let mut actual = admissions.iter().map(|row| row.operation_id.clone()).collect::<Vec<_>>();
            actual.sort();
            if actual != expected || actual.iter().any(|id| id == operation)
                || ids.iter().any(|id| !admissions.iter().any(|row| row.sources.contains(id)))
            {
                return Err(RuntimeError::UncertainAdmission);
            }
            let mut response = tx.query("SELECT VALUE operation_id FROM memory_runtime_operation WHERE owner_id = $owner AND project_id = $project AND operation_id = $operation;")
                .bind(("owner", owner.to_string())).bind(("project", project.to_string()))
                .bind(("operation", operation.to_owned())).await.map_err(storage)?.check().map_err(storage)?;
            let existing: Vec<String> = response.take(0).map_err(storage)?;
            if !existing.is_empty() { return Err(RuntimeError::UncertainAdmission); }
            tx.query("CREATE memory_runtime_operation SET owner_id = $owner, project_id = $project, operation_id = $operation, sources = $sources, recovery_of = $previous, authorized_by_actor = $actor, authorized_at_ms = $at RETURN NONE;")
                .bind(("owner", owner.to_string())).bind(("project", project.to_string()))
                .bind(("operation", operation.to_owned())).bind(("sources", ids))
                .bind(("previous", expected)).bind(("actor", actor.to_string())).bind(("at", authorized_at_ms))
                .await.map_err(storage)?.check().map_err(storage)?;
            Ok(())
        }.await;
        match result {
            Ok(()) => tx.commit().await.map(|_| ()).map_err(storage),
            Err(error) => cancel_err(tx, error).await,
        }
    }
}
