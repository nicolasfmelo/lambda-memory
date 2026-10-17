//! Curated decision timeline: immutable versions, a mutable head per entry for ordering
//! and coverage, keyset pages and uncovered decision sources. Input bounds are pure policy
//! (`engines::timeline_policy`); this store re-checks link bounds, existence and versioning.
use super::*;
use crate::domain_models::timeline::*;

const VERSION_FIELDS: &str = "owner_id, project_id, entry_id, actor_id, title, summary, status, version, occurred_at_ms, recorded_at_ms, sources, revisions, memories, tags, supersedes";
const TIMELINE_HEAD_FIELDS: &str =
    "owner_id, project_id, entry_id, version, occurred_at_ms, status, sources";

#[derive(Clone, Debug, PartialEq, Eq, SurrealValue)]
struct TimelineVersionRecord {
    owner_id: String,
    project_id: String,
    entry_id: String,
    actor_id: String,
    title: String,
    summary: String,
    status: String,
    version: u64,
    occurred_at_ms: u64,
    recorded_at_ms: u64,
    sources: Vec<String>,
    revisions: Vec<String>,
    memories: Vec<String>,
    tags: Vec<String>,
    supersedes: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, SurrealValue)]
struct TimelineHeadRecord {
    owner_id: String,
    project_id: String,
    entry_id: String,
    version: u64,
    occurred_at_ms: u64,
    status: String,
    sources: Vec<String>,
}

fn strings<T: std::fmt::Display>(values: &[T]) -> Vec<String> {
    values.iter().map(ToString::to_string).collect()
}
fn decode_ids<T>(
    values: Vec<String>,
    parse: impl Fn(String) -> Result<T, crate::domain_models::MemoryError>,
) -> Result<Vec<T>, RuntimeError> {
    values
        .into_iter()
        .map(|value| parse(value).map_err(storage))
        .collect()
}
fn decode_entry(record: TimelineVersionRecord) -> Result<TimelineEntry, RuntimeError> {
    Ok(TimelineEntry {
        project: ProjectId::new(record.project_id).map_err(storage)?,
        entry: TimelineEntryId::new(record.entry_id).map_err(storage)?,
        version: u32::try_from(record.version).map_err(storage)?,
        title: record.title,
        summary: record.summary,
        occurred_at_ms: record.occurred_at_ms,
        recorded_at_ms: record.recorded_at_ms,
        status: TimelineStatus::parse(&record.status).ok_or(RuntimeError::StorageUnavailable)?,
        sources: decode_ids(record.sources, SourceId::new)?,
        revisions: decode_ids(record.revisions, RevisionId::new)?,
        memories: decode_ids(record.memories, MemoryId::new)?,
        supersedes: record
            .supersedes
            .map(TimelineEntryId::new)
            .transpose()
            .map_err(storage)?,
        tags: record.tags,
    })
}

impl EmbeddedRuntimeStore {
    /// Applies the spec versioning table in one serialized transaction. Links are only
    /// checked when a new version is written, so a safe replay never fails on links that
    /// were valid when first recorded.
    pub async fn timeline_write(
        &self,
        owner: &OwnerId,
        actor: &ActorId,
        draft: TimelineDraft,
        now_ms: u64,
    ) -> Result<TimelineReceipt, RuntimeError> {
        if draft.sources.is_empty()
            || draft.sources.len() > TIMELINE_MAX_LINKS
            || draft.revisions.len() > TIMELINE_MAX_LINKS
            || draft.memories.len() > TIMELINE_MAX_LINKS
        {
            return Err(RuntimeError::InvalidInput("timeline link bounds"));
        }
        self.validate_activity_scope(owner, &draft.project)?;
        let _guard = self.capture.writes.lock().await;
        let tx = self.capture.db.clone().begin().await.map_err(storage)?;
        match self
            .timeline_write_tx(&tx, owner, actor, &draft, now_ms)
            .await
        {
            Ok(receipt) => {
                self.finish_attached_activity(&tx, &draft.sources, &draft.revisions, None)
                    .await?;
                tx.commit().await.map_err(storage)?;
                Ok(receipt)
            }
            Err(error) => {
                let _ = tx.cancel().await;
                Err(error)
            }
        }
    }

    async fn timeline_write_tx(
        &self,
        tx: &Transaction<Db>,
        owner: &OwnerId,
        actor: &ActorId,
        draft: &TimelineDraft,
        now_ms: u64,
    ) -> Result<TimelineReceipt, RuntimeError> {
        let project = draft.project.as_str();
        let head = self
            .timeline_head(tx, owner.as_str(), project, draft.entry.as_str())
            .await?;
        let mut previous: Option<TimelineEntry> = None;
        let version = match (&head, draft.expected_version) {
            (None, None) => 1,
            (None, Some(_)) => return Err(RuntimeError::Conflict),
            (Some(head), expected) => {
                let current = self
                    .timeline_version(
                        tx,
                        owner.as_str(),
                        project,
                        draft.entry.as_str(),
                        head.version,
                    )
                    .await?
                    .ok_or(RuntimeError::StorageUnavailable)?;
                let current = decode_entry(current)?;
                let replay =
                    expected.is_none() || expected.map(u64::from) == head.version.checked_sub(1);
                if replay && current.same_content(draft) {
                    return Ok(TimelineReceipt {
                        entry: current,
                        write: TimelineWrite::AlreadyRecorded,
                    });
                }
                if expected.map(u64::from) != Some(head.version) {
                    return Err(RuntimeError::Conflict);
                }
                previous = Some(current);
                u32::try_from(head.version)
                    .ok()
                    .and_then(|version| version.checked_add(1))
                    .ok_or(RuntimeError::Conflict)?
            }
        };
        self.require_timeline_links(tx, owner, draft, previous.as_ref())
            .await?;
        let record = TimelineVersionRecord {
            owner_id: owner.to_string(),
            project_id: project.to_owned(),
            entry_id: draft.entry.to_string(),
            actor_id: actor.to_string(),
            title: draft.title.clone(),
            summary: draft.summary.clone(),
            status: draft.status.as_str().to_owned(),
            version: u64::from(version),
            occurred_at_ms: draft.occurred_at_ms,
            recorded_at_ms: now_ms,
            sources: strings(&draft.sources),
            revisions: strings(&draft.revisions),
            memories: strings(&draft.memories),
            tags: draft.tags.clone(),
            supersedes: draft.supersedes.as_ref().map(ToString::to_string),
        };
        self.create_row(
            tx,
            "CREATE memory_timeline_version CONTENT $record RETURN NONE;",
            record.clone(),
        )
        .await?;
        if head.is_none() {
            self.create_row(
                tx,
                "CREATE memory_timeline_head CONTENT $record RETURN NONE;",
                TimelineHeadRecord {
                    owner_id: record.owner_id.clone(),
                    project_id: record.project_id.clone(),
                    entry_id: record.entry_id.clone(),
                    version: record.version,
                    occurred_at_ms: record.occurred_at_ms,
                    status: record.status.clone(),
                    sources: record.sources.clone(),
                },
            )
            .await?;
        } else if tx
            .query("UPDATE memory_timeline_head SET version = $version, occurred_at_ms = $occurred, status = $status, sources = $sources WHERE owner_id = $owner AND project_id = $project AND entry_id = $entry RETURN NONE;")
            .bind(("version", record.version)).bind(("occurred", record.occurred_at_ms))
            .bind(("status", record.status.clone())).bind(("sources", record.sources.clone()))
            .bind(("owner", record.owner_id.clone())).bind(("project", record.project_id.clone()))
            .bind(("entry", record.entry_id.clone()))
            .await
            .and_then(|response| response.check())
            .is_err()
        {
            return Err(RuntimeError::StorageUnavailable);
        }
        Ok(TimelineReceipt {
            entry: decode_entry(record)?,
            write: TimelineWrite::Recorded,
        })
    }

    /// Sources mirror `propose_tx`: absent for this owner is `Missing`, another project is
    /// `Unauthorized`. Revisions, memories and `supersedes` must exist in this project.
    /// When versioning an entry, `memories` and `supersedes` values carried over unchanged
    /// from `previous` (the head) are not re-checked: purge may leave them dangling by
    /// design. Sources and revisions cited by a surviving entry cannot dangle, since purging
    /// them cascades the entry, so they are always checked.
    async fn require_timeline_links(
        &self,
        tx: &Transaction<Db>,
        owner: &OwnerId,
        draft: &TimelineDraft,
        previous: Option<&TimelineEntry>,
    ) -> Result<(), RuntimeError> {
        let project = draft.project.as_str();
        for source in &draft.sources {
            let rows = self.source_row(tx, owner.as_str(), source.as_str()).await?;
            let row = rows.first().ok_or(RuntimeError::Missing)?;
            if row.project_id != project {
                return Err(RuntimeError::Unauthorized);
            }
        }
        let new_memories: Vec<MemoryId> = draft
            .memories
            .iter()
            .filter(|memory| previous.is_none_or(|head| !head.memories.contains(memory)))
            .cloned()
            .collect();
        for (field, ids) in [
            ("revision_id", strings(&draft.revisions)),
            ("memory_id", strings(&new_memories)),
        ] {
            if ids.is_empty() {
                continue;
            }
            let mut response = tx
                .query(format!("RETURN array::len(array::distinct((SELECT VALUE {field} FROM memory_revision WHERE owner_id = $owner AND project_id = $project AND {field} IN $ids)));"))
                .bind(("owner", owner.to_string())).bind(("project", project.to_owned())).bind(("ids", ids.clone()))
                .await.map_err(storage)?.check().map_err(storage)?;
            let found: Option<usize> = response.take(0).map_err(storage)?;
            if found != Some(ids.len()) {
                return Err(RuntimeError::Missing);
            }
        }
        let carried = previous.is_some_and(|head| head.supersedes == draft.supersedes);
        if let Some(target) = draft.supersedes.as_ref().filter(|_| !carried) {
            self.timeline_head(tx, owner.as_str(), project, target.as_str())
                .await?
                .ok_or(RuntimeError::Missing)?;
        }
        Ok(())
    }

    pub async fn timeline_page(
        &self,
        owner: &OwnerId,
        query: &TimelineQuery,
    ) -> Result<TimelinePage, RuntimeError> {
        let history = query.entry.is_some();
        let key_matches = match &query.after {
            None => true,
            Some(TimelineKey::Version(_)) => history,
            Some(TimelineKey::Position { .. }) => !history,
        };
        if !(1..=TIMELINE_MAX_PAGE).contains(&query.limit)
            || query.statuses.len() > TIMELINE_MAX_STATUSES
            || query
                .statuses
                .iter()
                .enumerate()
                .any(|(i, status)| query.statuses[..i].contains(status))
            || (history && (!query.statuses.is_empty() || query.descending))
            || !key_matches
        {
            return Err(RuntimeError::InvalidInput("timeline query bounds"));
        }
        let _guard = self.capture.writes.lock().await;
        let db = &self.capture.db;
        let fetch = query.limit + 1;
        let mut records = match &query.entry {
            Some(entry) => {
                let after = match query.after {
                    Some(TimelineKey::Version(version)) => u64::from(version),
                    _ => 0,
                };
                let mut response = db
                    .query(format!("SELECT {VERSION_FIELDS} FROM memory_timeline_version WHERE owner_id = $owner AND project_id = $project AND entry_id = $entry AND version > $after ORDER BY version LIMIT $fetch;"))
                    .bind(("owner", owner.to_string())).bind(("project", query.project.to_string()))
                    .bind(("entry", entry.to_string())).bind(("after", after)).bind(("fetch", fetch))
                    .await.map_err(storage)?.check().map_err(storage)?;
                response
                    .take::<Vec<TimelineVersionRecord>>(0)
                    .map_err(storage)?
            }
            None => {
                let heads = self.timeline_heads(owner, query, fetch).await?;
                let mut records = Vec::with_capacity(heads.len());
                for head in heads {
                    let record = self
                        .timeline_version(
                            db,
                            owner.as_str(),
                            &head.project_id,
                            &head.entry_id,
                            head.version,
                        )
                        .await?
                        .ok_or(RuntimeError::StorageUnavailable)?;
                    records.push(record);
                }
                records
            }
        };
        let more = records.len() > query.limit;
        records.truncate(query.limit);
        let entries = records
            .into_iter()
            .map(decode_entry)
            .collect::<Result<Vec<_>, _>>()?;
        let (uncovered, uncovered_more) = if !history && query.after.is_none() {
            self.uncovered_decisions(owner, &query.project).await?
        } else {
            (Vec::new(), false)
        };
        Ok(TimelinePage {
            entries,
            more,
            uncovered,
            uncovered_more,
        })
    }

    /// Keyset over `(occurred_at_ms, entry_id)`; descending reverses both the order and
    /// the comparison so the cursor stays exclusive.
    async fn timeline_heads(
        &self,
        owner: &OwnerId,
        query: &TimelineQuery,
        fetch: usize,
    ) -> Result<Vec<TimelineHeadRecord>, RuntimeError> {
        let (cmp, dir) = if query.descending {
            ("<", "DESC")
        } else {
            (">", "ASC")
        };
        let (at, after_entry) = match &query.after {
            Some(TimelineKey::Position {
                occurred_at_ms,
                entry,
            }) => (Some(*occurred_at_ms), Some(entry.to_string())),
            _ => (None, None),
        };
        let keyset = if at.is_some() {
            format!(
                " AND (occurred_at_ms {cmp} $at OR (occurred_at_ms = $at AND entry_id {cmp} $after_entry))"
            )
        } else {
            String::new()
        };
        let filter = if query.statuses.is_empty() {
            ""
        } else {
            " AND status IN $statuses"
        };
        let mut response = self.capture.db
            .query(format!("SELECT {TIMELINE_HEAD_FIELDS} FROM memory_timeline_head WHERE owner_id = $owner AND project_id = $project{filter}{keyset} ORDER BY occurred_at_ms {dir}, entry_id {dir} LIMIT $fetch;"))
            .bind(("owner", owner.to_string())).bind(("project", query.project.to_string()))
            .bind(("statuses", query.statuses.iter().map(|s| s.as_str().to_owned()).collect::<Vec<_>>()))
            .bind(("at", at)).bind(("after_entry", after_entry)).bind(("fetch", fetch))
            .await.map_err(storage)?.check().map_err(storage)?;
        response.take(0).map_err(storage)
    }

    /// Decision sources cited by no entry head, newest first, one row past the limit.
    async fn uncovered_decisions(
        &self,
        owner: &OwnerId,
        project: &ProjectId,
    ) -> Result<(Vec<UncoveredDecision>, bool), RuntimeError> {
        let mut response = self.capture.db
            .query(format!("LET $covered = array::distinct(array::flatten((SELECT VALUE sources FROM memory_timeline_head WHERE owner_id = $owner AND project_id = $project))); SELECT {SOURCE_PROJECTION} FROM memory_source WHERE owner_id = $owner AND project_id = $project AND kind = 'decision' AND source_id NOT IN $covered ORDER BY recorded_at_ms DESC, source_id DESC LIMIT $fetch;"))
            .bind(("owner", owner.to_string())).bind(("project", project.to_string()))
            .bind(("fetch", TIMELINE_UNCOVERED_LIMIT + 1))
            .await.map_err(storage)?.check().map_err(storage)?;
        let mut rows: Vec<SourceRecord> = response.take(1).map_err(storage)?;
        let more = rows.len() > TIMELINE_UNCOVERED_LIMIT;
        rows.truncate(TIMELINE_UNCOVERED_LIMIT);
        let uncovered = rows
            .into_iter()
            .map(|row| {
                let event = source_event(row)?;
                let (preview, preview_truncated) = timeline_preview(&event.content);
                Ok(UncoveredDecision {
                    source: event.source,
                    origin: event.origin,
                    recorded_at_ms: event.recorded_at_ms,
                    outcome: event.outcome,
                    preview,
                    preview_truncated,
                })
            })
            .collect::<Result<Vec<_>, RuntimeError>>()?;
        Ok((uncovered, more))
    }

    /// Purge cascade: every entry with any version citing a purged source or a revision
    /// removed by the cascade loses all its versions and its head. Returns the entry count.
    pub(super) async fn purge_timeline_tx(
        &self,
        tx: &Transaction<Db>,
        owner: &OwnerId,
        project: &ProjectId,
        sources: &[String],
        revisions: &[String],
    ) -> Result<usize, RuntimeError> {
        let mut response = tx
            .query("RETURN array::distinct((SELECT VALUE entry_id FROM memory_timeline_version WHERE owner_id = $owner AND project_id = $project AND (array::intersect(sources, $sources) != [] OR array::intersect(revisions, $revisions) != [])));")
            .bind(("owner", owner.to_string())).bind(("project", project.to_string()))
            .bind(("sources", sources.to_vec())).bind(("revisions", revisions.to_vec()))
            .await.map_err(storage)?.check().map_err(storage)?;
        let entries: Vec<String> = response.take(0).map_err(storage)?;
        self.delete_in(tx, "DELETE memory_timeline_version WHERE owner_id = $owner AND project_id = $project AND entry_id IN $ids RETURN NONE;", owner, project, &entries).await?;
        self.delete_in(tx, "DELETE memory_timeline_head WHERE owner_id = $owner AND project_id = $project AND entry_id IN $ids RETURN NONE;", owner, project, &entries).await?;
        Ok(entries.len())
    }

    async fn timeline_head(
        &self,
        db: &impl QueryConnection,
        owner: &str,
        project: &str,
        entry: &str,
    ) -> Result<Option<TimelineHeadRecord>, RuntimeError> {
        let mut response = db
            .query(format!("SELECT {TIMELINE_HEAD_FIELDS} FROM memory_timeline_head WHERE owner_id = $owner AND project_id = $project AND entry_id = $entry;"))
            .bind(("owner", owner.to_owned())).bind(("project", project.to_owned())).bind(("entry", entry.to_owned()))
            .await.map_err(storage)?.check().map_err(storage)?;
        let rows: Vec<TimelineHeadRecord> = response.take(0).map_err(storage)?;
        Ok(rows.into_iter().next())
    }

    async fn timeline_version(
        &self,
        db: &impl QueryConnection,
        owner: &str,
        project: &str,
        entry: &str,
        version: u64,
    ) -> Result<Option<TimelineVersionRecord>, RuntimeError> {
        let mut response = db
            .query(format!("SELECT {VERSION_FIELDS} FROM memory_timeline_version WHERE owner_id = $owner AND project_id = $project AND entry_id = $entry AND version = $version;"))
            .bind(("owner", owner.to_owned())).bind(("project", project.to_owned()))
            .bind(("entry", entry.to_owned())).bind(("version", version))
            .await.map_err(storage)?.check().map_err(storage)?;
        let rows: Vec<TimelineVersionRecord> = response.take(0).map_err(storage)?;
        Ok(rows.into_iter().next())
    }
}
