//! Read-only source projections for single-project operator transfers.
use super::*;
use serde_json::{Value, json};

impl EmbeddedRuntimeStore {
    /// All originals, including consumed sources; strings are exported exactly as stored.
    pub async fn export_sources(
        &self,
        owner: &OwnerId,
        project: &ProjectId,
    ) -> Result<Vec<Value>, RuntimeError> {
        let mut response = self.capture.db.query(format!(
            "SELECT {SOURCE_PROJECTION} FROM memory_source WHERE owner_id = $owner AND project_id = $project ORDER BY recorded_at_ms, source_id;"
        ))
            .bind(("owner", owner.to_string()))
            .bind(("project", project.to_string()))
            .await.map_err(storage)?.check().map_err(storage)?;
        let rows: Vec<SourceRecord> = response.take(0).map_err(storage)?;
        Ok(rows.into_iter().map(|row| json!({
            "source": row.source_id, "origin": row.origin, "recorded_at_ms": row.recorded_at_ms,
            "kind": row.kind, "outcome": row.outcome, "content": row.content,
        })).collect())
    }

    /// Record every citation in the source insert transaction, even if the evidence will
    /// arrive in a later import. Unresolved IDs are harmless and need no end-of-run repair.
    pub(super) async fn restore_imported_lesson_citations_tx(
        &self,
        tx: &Transaction<Db>,
        lesson: &SourceRecord,
    ) -> Result<(), RuntimeError> {
        if lesson.kind != "lesson"
            || !(lesson.origin.starts_with("lesson://") || lesson.origin.starts_with("promote://"))
        {
            return Ok(());
        }
        let parsed = crate::integrations::lesson_content::parse_lesson(&lesson.content)
            .ok_or(RuntimeError::InvalidInput("invalid lesson"))?;
        let distinct: BTreeSet<_> = parsed
            .evidence
            .into_iter()
            .map(|id| id.to_string())
            .collect();
        for cites in distinct {
            tx.query("CREATE memory_source_citation CONTENT {owner_id:$owner,project_id:$project,source_id:$source,cites:$cites} RETURN NONE;")
                .bind(("owner", lesson.owner_id.clone())).bind(("project", lesson.project_id.clone()))
                .bind(("source", lesson.source_id.clone())).bind(("cites", cites))
                .await.map_err(storage)?.check().map_err(storage)?;
        }
        Ok(())
    }

    /// (All sources, curatable sources, unconsumed curatable sources) in one snapshot.
    pub async fn inspect_sources(
        &self,
        owner: &OwnerId,
        project: &ProjectId,
    ) -> Result<(usize, usize, usize), RuntimeError> {
        #[derive(Debug, SurrealValue)]
        struct Total {
            total: usize,
        }
        let tx = self.capture.db.clone().begin().await.map_err(storage)?;
        let mut response = tx.query(format!(
            "SELECT count() AS total FROM memory_source WHERE owner_id = $owner AND project_id = $project GROUP ALL; SELECT count() AS total FROM memory_source WHERE owner_id = $owner AND project_id = $project AND kind IN [{BACKLOG_KINDS}] AND !string::starts_with(origin, 'omp://tool_result') GROUP ALL;"
        ))
            .bind(("owner", owner.to_string()))
            .bind(("project", project.to_string()))
            .await.map_err(storage)?.check().map_err(storage)?;
        let sources: Vec<Total> = response.take(0).map_err(storage)?;
        let curatable: Vec<Total> = response.take(1).map_err(storage)?;
        let pending = self
            .pending_backlog(&tx, owner.as_str(), project.as_str())
            .await?;
        tx.cancel().await.map_err(storage)?;
        Ok((
            sources.first().map_or(0, |r| r.total),
            curatable.first().map_or(0, |r| r.total),
            pending,
        ))
    }
}
