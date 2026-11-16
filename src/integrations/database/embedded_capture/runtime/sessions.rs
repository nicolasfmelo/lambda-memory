//! Session state shares the immutable source insert transaction and writer lock.
use super::*;
use crate::domain_models::session::*;
use std::collections::BTreeMap;

const FIELDS: &str = "owner_id, project_id, session_key, first_activity_ms, last_activity_ms, live_activity_ms, exchanges, ended_at_ms, extracted_through_ms, historical, lesson_count";
#[derive(Clone, Debug, SurrealValue)]
struct SessionRecord {
    owner_id: String,
    project_id: String,
    session_key: String,
    first_activity_ms: u64,
    last_activity_ms: u64,
    live_activity_ms: Option<u64>,
    exchanges: u64,
    ended_at_ms: Option<u64>,
    extracted_through_ms: Option<u64>,
    historical: bool,
    lesson_count: u64,
}
impl SessionRecord {
    fn domain(self) -> Result<MemorySession, RuntimeError> {
        Ok(MemorySession {
            session_key: SessionKey::new(self.session_key)
                .ok_or(RuntimeError::StorageUnavailable)?,
            first_activity_ms: self.first_activity_ms,
            last_activity_ms: self.last_activity_ms,
            live_activity_ms: self.live_activity_ms,
            exchanges: self.exchanges,
            ended_at_ms: self.ended_at_ms,
            extracted_through_ms: self.extracted_through_ms,
            historical: self.historical,
        })
    }
}
async fn rows(
    db: &impl QueryConnection,
    owner: &str,
    project: &str,
) -> Result<Vec<SessionRecord>, RuntimeError> {
    let mut response = db
        .query(format!(
            "SELECT {FIELDS} FROM memory_session WHERE owner_id = $owner AND project_id = $project;"
        ))
        .bind(("owner", owner.to_owned()))
        .bind(("project", project.to_owned()))
        .await
        .map_err(storage)?
        .check()
        .map_err(storage)?;
    response.take(0).map_err(storage)
}
async fn save(tx: &Transaction<Db>, row: SessionRecord) -> Result<(), RuntimeError> {
    // Identity-index lookup, no datastore record IDs escape this module.
    tx.query("UPSERT memory_session CONTENT $row WHERE owner_id = $owner AND project_id = $project AND session_key = $key RETURN NONE;")
        .bind(("owner", row.owner_id.clone())).bind(("project", row.project_id.clone()))
        .bind(("key", row.session_key.clone())).bind(("row", row))
        .await.map_err(storage)?.check().map_err(storage)?;
    Ok(())
}
pub(super) async fn record_activity(
    tx: &Transaction<Db>,
    source: &SourceRecord,
    importing: bool,
) -> Result<(), RuntimeError> {
    if !matches!(source.kind.as_str(), "exchange" | "action") {
        return Ok(());
    }
    let Some(key) = SessionKey::from_source(&source.source_id) else {
        return Ok(());
    };
    let mut response = tx.query(format!("SELECT {FIELDS} FROM memory_session WHERE owner_id = $owner AND project_id = $project AND session_key = $key;"))
        .bind(("owner", source.owner_id.clone())).bind(("project", source.project_id.clone())).bind(("key", key.as_str().to_owned()))
        .await.map_err(storage)?.check().map_err(storage)?;
    let mut existing: Vec<SessionRecord> = response.take(0).map_err(storage)?;
    let historical = importing || source.origin.starts_with("import://");
    let mut row = existing.pop().unwrap_or(SessionRecord {
        owner_id: source.owner_id.clone(),
        project_id: source.project_id.clone(),
        session_key: key.as_str().into(),
        first_activity_ms: source.recorded_at_ms,
        last_activity_ms: source.recorded_at_ms,
        live_activity_ms: None,
        exchanges: 0,
        ended_at_ms: None,
        extracted_through_ms: None,
        historical,
        lesson_count: 0,
    });
    row.first_activity_ms = row.first_activity_ms.min(source.recorded_at_ms);
    row.last_activity_ms = row.last_activity_ms.max(source.recorded_at_ms);
    row.exchanges += u64::from(source.kind == "exchange");
    if !historical {
        row.historical = false;
        row.live_activity_ms = Some(row.live_activity_ms.map_or(source.recorded_at_ms, |live| {
            live.max(source.recorded_at_ms)
        }));
    }
    if row.historical {
        row.extracted_through_ms = Some(row.last_activity_ms);
    }
    save(tx, row).await
}

/// Rebuild source-derived aggregates after deletion, within the caller's purge transaction.
/// Live/extraction watermarks remain bookkeeping history, not counts of retained sources.
pub(super) async fn recompute_after_purge(
    tx: &Transaction<Db>,
    owner: &OwnerId,
    project: &ProjectId,
    deleted_sources: &[String],
) -> Result<(), RuntimeError> {
    let keys: Vec<_> = deleted_sources
        .iter()
        .filter_map(|source| SessionKey::from_source(source))
        .map(|key| key.as_str().to_owned())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    if keys.is_empty() {
        return Ok(());
    }
    let mut response = tx.query(format!("SELECT {FIELDS} FROM memory_session WHERE owner_id = $owner AND project_id = $project AND session_key IN $keys;"))
        .bind(("owner", owner.to_string())).bind(("project", project.to_string())).bind(("keys", keys))
        .await.map_err(storage)?.check().map_err(storage)?;
    let affected: Vec<SessionRecord> = response.take(0).map_err(storage)?;
    for mut row in affected {
        let mut response = tx.query("SELECT source_id, recorded_at_ms, kind, origin FROM memory_source WHERE owner_id = $owner AND project_id = $project AND kind IN ['exchange', 'action'] AND string::starts_with(source_id, $prefix);")
            .bind(("owner", owner.to_string())).bind(("project", project.to_string()))
            .bind(("prefix", format!("{}/", row.session_key)))
            .await.map_err(storage)?.check().map_err(storage)?;
        let sources: Vec<ActivitySource> = response.take(0).map_err(storage)?;
        if sources.is_empty() {
            tx.query("DELETE memory_session WHERE owner_id = $owner AND project_id = $project AND session_key = $key RETURN NONE;")
                .bind(("owner", owner.to_string())).bind(("project", project.to_string())).bind(("key", row.session_key))
                .await.map_err(storage)?.check().map_err(storage)?;
        } else {
            row.first_activity_ms = sources
                .iter()
                .map(|source| source.recorded_at_ms)
                .min()
                .ok_or(RuntimeError::StorageUnavailable)?;
            row.last_activity_ms = sources
                .iter()
                .map(|source| source.recorded_at_ms)
                .max()
                .ok_or(RuntimeError::StorageUnavailable)?;
            row.exchanges = sources
                .iter()
                .filter(|source| source.kind == "exchange")
                .count() as u64;
            if row
                .live_activity_ms
                .is_some_and(|live| live > row.last_activity_ms)
            {
                // Sources merged by identity consolidation retain their original origin and count as live.
                let remaining_live = sources
                    .iter()
                    .filter(|source| {
                        !source.origin.starts_with("import://")
                            && row
                                .extracted_through_ms
                                .is_none_or(|through| source.recorded_at_ms > through)
                    })
                    .map(|source| source.recorded_at_ms)
                    .max();
                row.live_activity_ms = remaining_live.or(row.extracted_through_ms);
                if remaining_live.is_none()
                    && row.extracted_through_ms.is_none()
                    && !sources
                        .iter()
                        .any(|source| !source.origin.starts_with("import://"))
                {
                    row.historical = true;
                    row.extracted_through_ms = Some(row.last_activity_ms);
                }
            }
            save(tx, row).await?;
        }
    }
    Ok(())
}

impl EmbeddedRuntimeStore {
    pub async fn sessions(
        &self,
        owner: &OwnerId,
        project: &ProjectId,
    ) -> Result<Vec<MemorySession>, RuntimeError> {
        rows(&self.capture.db, owner.as_str(), project.as_str())
            .await?
            .into_iter()
            .map(SessionRecord::domain)
            .collect()
    }
    pub async fn ready_sessions(
        &self,
        owner: &OwnerId,
        project: &ProjectId,
        now: u64,
        limit: usize,
        session_idle_ms: u64,
    ) -> Result<Vec<MemorySession>, RuntimeError> {
        let mut response = self.capture.db.query(format!("SELECT {FIELDS} FROM memory_session WHERE owner_id = $owner AND project_id = $project AND exchanges >= 1 AND live_activity_ms != NONE AND (extracted_through_ms = NONE OR live_activity_ms > extracted_through_ms) AND ((ended_at_ms != NONE AND ended_at_ms >= last_activity_ms) OR (last_activity_ms <= $now AND $now - last_activity_ms >= $idle)) ORDER BY last_activity_ms, session_key LIMIT $limit;"))
            .bind(("owner", owner.to_string())).bind(("project", project.to_string()))
            .bind(("now", now)).bind(("idle", session_idle_ms)).bind(("limit", limit))
            .await.map_err(storage)?.check().map_err(storage)?;
        let rows: Vec<SessionRecord> = response.take(0).map_err(storage)?;
        rows.into_iter().map(SessionRecord::domain).collect()
    }
    pub async fn session_end(
        &self,
        owner: &OwnerId,
        project: &ProjectId,
        raw: &str,
        now: u64,
    ) -> Result<SessionEnd, RuntimeError> {
        if raw.is_empty() || raw.chars().count() > 512 || raw.chars().any(char::is_control) {
            return Err(RuntimeError::InvalidInput("invalid session"));
        }
        let hash = format!("{:x}", Sha256::digest(raw.as_bytes()));
        let key = SessionKey::new(&hash[..16]).ok_or(RuntimeError::StorageUnavailable)?;
        let _guard = self.capture.writes.lock().await;
        let tx = self.capture.db.clone().begin().await.map_err(storage)?;
        let mut response = tx.query(format!("SELECT {FIELDS} FROM memory_session WHERE owner_id = $owner AND project_id = $project AND session_key = $key;"))
            .bind(("owner", owner.to_string())).bind(("project", project.to_string())).bind(("key", key.as_str().to_owned()))
            .await.map_err(storage)?.check().map_err(storage)?;
        let mut existing: Vec<SessionRecord> = response.take(0).map_err(storage)?;
        let ended = if let Some(mut row) = existing.pop() {
            if row.ended_at_ms.is_none_or(|end| end < row.last_activity_ms) {
                row.ended_at_ms = Some(now);
                save(&tx, row).await?;
            }
            true
        } else {
            false
        };
        tx.commit().await.map_err(storage)?;
        Ok(SessionEnd {
            session_key: key,
            ended,
        })
    }
    pub async fn mark_extracted(
        &self,
        owner: &OwnerId,
        project: &ProjectId,
        key: &SessionKey,
        through_ms: u64,
    ) -> Result<(), RuntimeError> {
        let _guard = self.capture.writes.lock().await;
        self.capture.db.query("UPDATE memory_session SET extracted_through_ms = $through WHERE owner_id = $owner AND project_id = $project AND session_key = $key RETURN NONE;")
            .bind(("owner", owner.to_string())).bind(("project", project.to_string()))
            .bind(("key", key.as_str().to_owned())).bind(("through", through_ms))
            .await.map_err(storage)?.check().map_err(storage)?;
        Ok(())
    }
    pub async fn inspect_lesson_backfill(
        &self,
        owner: &OwnerId,
        project: &ProjectId,
    ) -> Result<LessonBackfillPlan, RuntimeError> {
        let tx = self.capture.db.clone().begin().await.map_err(storage)?;
        let result = backfill_state(&tx, owner, project)
            .await
            .map(|(_, _, plan)| plan);
        tx.cancel().await.map_err(storage)?;
        result
    }
    pub async fn lesson_backfill(
        &self,
        owner: &OwnerId,
        project: &ProjectId,
        plan: &LessonBackfillPlan,
    ) -> Result<usize, RuntimeError> {
        let _guard = self.capture.writes.lock().await;
        let tx = self.capture.db.clone().begin().await.map_err(storage)?;
        let (mut existing, missing, expected) = backfill_state(&tx, owner, project).await?;
        if &expected != plan {
            return cancel_err(
                tx,
                RuntimeError::InvalidInput("exact lesson backfill consent required"),
            )
            .await;
        }
        for row in missing {
            save(&tx, row.clone()).await?;
            existing.insert(row.session_key.clone(), row);
        }
        for key in &plan.sessions {
            let row = existing
                .get_mut(key.as_str())
                .ok_or(RuntimeError::StorageUnavailable)?;
            row.historical = false;
            row.live_activity_ms = Some(row.last_activity_ms);
            row.extracted_through_ms = None;
            row.ended_at_ms = Some(row.last_activity_ms);
            save(&tx, row.clone()).await?;
        }
        tx.commit().await.map_err(storage)?;
        Ok(plan.sessions.len())
    }
}

#[derive(Debug, SurrealValue)]
struct ActivitySource {
    source_id: String,
    recorded_at_ms: u64,
    kind: String,
    origin: String,
}
type BackfillState = (
    BTreeMap<String, SessionRecord>,
    Vec<SessionRecord>,
    LessonBackfillPlan,
);
async fn backfill_state(
    tx: &Transaction<Db>,
    owner: &OwnerId,
    project: &ProjectId,
) -> Result<BackfillState, RuntimeError> {
    let existing: BTreeMap<_, _> = rows(tx, owner.as_str(), project.as_str())
        .await?
        .into_iter()
        .map(|row| (row.session_key.clone(), row))
        .collect();
    let mut response = tx.query("SELECT source_id, recorded_at_ms, kind, origin FROM memory_source WHERE owner_id = $owner AND project_id = $project AND kind IN ['exchange', 'action'];")
        .bind(("owner", owner.to_string())).bind(("project", project.to_string()))
        .await.map_err(storage)?.check().map_err(storage)?;
    let sources: Vec<ActivitySource> = response.take(0).map_err(storage)?;
    let mut missing = BTreeMap::new();
    for source in sources {
        let Some(key) = SessionKey::from_source(&source.source_id) else {
            continue;
        };
        if existing.contains_key(key.as_str()) {
            continue;
        }
        let row = missing
            .entry(key.as_str().to_owned())
            .or_insert(SessionRecord {
                owner_id: owner.to_string(),
                project_id: project.to_string(),
                session_key: key.as_str().into(),
                first_activity_ms: source.recorded_at_ms,
                last_activity_ms: source.recorded_at_ms,
                live_activity_ms: None,
                exchanges: 0,
                ended_at_ms: None,
                extracted_through_ms: None,
                historical: true,
                lesson_count: 0,
            });
        row.first_activity_ms = row.first_activity_ms.min(source.recorded_at_ms);
        row.last_activity_ms = row.last_activity_ms.max(source.recorded_at_ms);
        row.exchanges += u64::from(source.kind == "exchange");
        row.extracted_through_ms = Some(row.last_activity_ms);
    }
    let mut sessions: Vec<_> = existing
        .values()
        .chain(missing.values())
        .filter(|row| row.historical && row.exchanges >= 1)
        .map(|row| SessionKey::new(row.session_key.clone()).ok_or(RuntimeError::StorageUnavailable))
        .collect::<Result<_, _>>()?;
    sessions.sort();
    Ok((
        existing,
        missing.into_values().collect(),
        LessonBackfillPlan { sessions },
    ))
}
