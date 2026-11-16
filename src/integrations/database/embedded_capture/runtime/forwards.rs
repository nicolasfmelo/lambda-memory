//! Transactional outbox for the service's single-writer cross-store transport.
use super::*;
use crate::domain_models::memory_tree::Forward;
use serde_json::json;

#[derive(Debug, SurrealValue)]
struct ForwardRow {
    forward_id: String,
    action: String,
    source_id: String,
}

impl EmbeddedRuntimeStore {
    pub async fn forwards(
        &self,
        owner: &OwnerId,
        project: &ProjectId,
        limit: usize,
    ) -> Result<Vec<Forward>, RuntimeError> {
        self.validate_activity_scope(owner, project)?;
        if !(1..=64).contains(&limit) {
            return Err(RuntimeError::InvalidInput("forward limit"));
        }
        let tx = self.capture.db.clone().begin().await.map_err(storage)?;
        let result = async {
            let mut response = tx.query("SELECT forward_id, action, source_id FROM memory_forward WHERE owner_id=$owner AND project_id=$project AND status='pending' ORDER BY created_at_ms, forward_id LIMIT $limit;")
                .bind(("owner", owner.to_string())).bind(("project", project.to_string())).bind(("limit", limit))
                .await.map_err(storage)?.check().map_err(storage)?;
            let rows: Vec<ForwardRow> = response.take(0).map_err(storage)?;
            let mut forwards = Vec::new();
            let mut bytes = 2usize; // serialized array brackets
            for row in rows {
                // A purge carries only identity, even after a later re-capture of this ID.
                let source = if row.action == "purge" { None } else {
                    self.source_row(&tx, owner.as_str(), &row.source_id).await?.into_iter().find(|s| s.project_id == project.as_str())
                };
                let source_json = source.as_ref().map(|s| json!({"source":s.source_id,"origin":s.origin,"recorded_at_ms":s.recorded_at_ms,"kind":s.kind,"outcome":s.outcome,"content":s.content}));
                let size = serde_json::to_vec(&json!({"forward_id":row.forward_id,"action":row.action,"source_id":row.source_id,"source":source_json})).map_err(storage)?.len() + usize::from(!forwards.is_empty());
                if !forwards.is_empty() && bytes.saturating_add(size) > 256 * 1024 { break; }
                bytes = bytes.saturating_add(size);
                forwards.push(Forward {
                    forward_id: row.forward_id,
                    action: match row.action.as_str() { "capture" => "capture", "purge" => "purge", _ => return Err(RuntimeError::StorageUnavailable) },
                    source_id: SourceId::new(row.source_id).map_err(storage)?,
                    source: source.map(source_event).transpose()?,
                });
            }
            Ok(forwards)
        }.await;
        let _ = tx.cancel().await;
        result
    }

    pub async fn forward_done(
        &self,
        owner: &OwnerId,
        project: &ProjectId,
        done: &[String],
        failed: &[String],
        now: u64,
    ) -> Result<(usize, usize), RuntimeError> {
        self.validate_activity_scope(owner, project)?;
        if !(1..=64).contains(&(done.len() + failed.len()))
            || done
                .iter()
                .chain(failed)
                .any(|s| s.is_empty() || s.len() > 2048)
        {
            return Err(RuntimeError::InvalidInput("forward acknowledgement bounds"));
        }
        let _guard = self.capture.writes.lock().await;
        let tx = self.capture.db.clone().begin().await.map_err(storage)?;
        let result = async {
            let mut counts = [0; 2];
            for (i, (status, ids)) in [("done", done), ("failed", failed)].into_iter().enumerate() {
                let mut response = tx.query("UPDATE memory_forward SET status=$status, done_at_ms=$now WHERE owner_id=$owner AND project_id=$project AND status='pending' AND forward_id IN $ids RETURN AFTER;")
                    .bind(("owner", owner.to_string())).bind(("project", project.to_string())).bind(("status", status.to_owned())).bind(("now", now)).bind(("ids", ids.to_vec()))
                    .await.map_err(storage)?.check().map_err(storage)?;
                counts[i] = response.take::<Vec<ForwardRow>>(0).map_err(storage)?.len();
            }
            Ok((counts[0], counts[1]))
        }.await;
        match result {
            Ok(counts) => {
                tx.commit().await.map_err(storage)?;
                Ok(counts)
            }
            Err(e) => {
                let _ = tx.cancel().await;
                Err(e)
            }
        }
    }

    pub(super) async fn capture_forward_tx(
        &self,
        tx: &Transaction<Db>,
        owner: &OwnerId,
        project: &ProjectId,
        source: &SourceId,
        now: u64,
    ) -> Result<(), RuntimeError> {
        let mut response = tx.query("SELECT VALUE forward_id FROM memory_forward WHERE owner_id=$owner AND project_id=$project AND action='purge' AND source_id=$source LIMIT 1;")
            .bind(("owner", owner.to_string())).bind(("project", project.to_string())).bind(("source", source.to_string()))
            .await.map_err(storage)?.check().map_err(storage)?;
        let purges: Vec<String> = response.take(0).map_err(storage)?;
        let id = if purges.is_empty() {
            format!("capture:{source}")
        } else {
            format!("recapture:{now}:{source}")
        };
        tx.query("LET $existing = (SELECT VALUE forward_id FROM memory_forward WHERE owner_id=$owner AND project_id=$project AND action='capture' AND source_id=$source); IF array::len($existing) = 0 { CREATE memory_forward CONTENT {owner_id:$owner, project_id:$project, forward_id:$id, action:'capture', source_id:$source, target:'user', status:'pending', created_at_ms:$now}; };")
            .bind(("owner", owner.to_string())).bind(("project", project.to_string())).bind(("id", id)).bind(("source", source.to_string())).bind(("now", now))
            .await.map_err(storage)?.check().map_err(storage)?;
        Ok(())
    }

    pub(super) async fn purge_forwards_tx(
        &self,
        tx: &Transaction<Db>,
        owner: &OwnerId,
        project: &ProjectId,
        sources: &[String],
    ) -> Result<(), RuntimeError> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(storage)?
            .as_millis() as u64;
        let mut response = tx.query("DELETE memory_forward WHERE owner_id=$owner AND project_id=$project AND action='capture' AND source_id IN $sources RETURN BEFORE;")
            .bind(("owner", owner.to_string())).bind(("project", project.to_string())).bind(("sources", sources.to_vec()))
            .await.map_err(storage)?.check().map_err(storage)?;
        let rows: Vec<ForwardRow> = response.take(0).map_err(storage)?;
        for row in rows {
            tx.query("CREATE memory_forward CONTENT {owner_id:$owner, project_id:$project, forward_id:$id, action:'purge', source_id:$source, target:'user', status:'pending', created_at_ms:$now};")
                .bind(("owner", owner.to_string())).bind(("project", project.to_string())).bind(("id", format!("purge:{}:{now}", row.source_id))).bind(("source", row.source_id)).bind(("now", now))
                .await.map_err(storage)?.check().map_err(storage)?;
        }
        Ok(())
    }
}
