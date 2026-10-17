//! Scoped operational metadata; evidence and paid-admission lifetime stay independent.
use super::*;
use crate::domain_models::embeddings::EmbeddingSpace;
use crate::domain_models::runtime_diagnostics::*;
use crate::domain_models::{ActivityId, WorkerInstanceId};
use serde_json::{Value, json};

const ENTRY_FIELDS: &str = "entry_id, parent_id, instance_id, entry_kind, operation, phase, started_at_ms, finished_at_ms, classified_at_ms, duration_ms, outcome, reason, counts";
const RETAINED_ROOTS: &str = "LET $retained_roots = (SELECT VALUE entry_id FROM memory_runtime_activity WHERE owner_id = $owner AND project_id = $project AND entry_kind = 'operation' AND (outcome = 'running' OR finished_at_ms > $cutoff OR classified_at_ms > $cutoff));";
const LIVE_TREES: &str = "root_id IN $retained_roots";
const EFFECTIVE_OUTCOME: &str = "IF outcome = 'running' AND (instance_id != $instance OR entry_id NOT IN $active) THEN 'indeterminate' ELSE outcome END";
const REVISION_STATE_SETS: &str = "LET $known_revisions = (SELECT VALUE revision_id FROM memory_revision_state WHERE owner_id = $owner AND project_id = $project); LET $processing_revisions = (SELECT VALUE revision_id FROM memory_revision_state WHERE owner_id = $owner AND project_id = $project AND state = 'processing');";
const PENDING_REVISIONS: &str =
    "(revision_id NOT IN $known_revisions OR revision_id IN $processing_revisions)";
const CONSUMED_SOURCE_SET: &str = "LET $consumed_sources = (SELECT VALUE source_id FROM memory_source_consumption WHERE owner_id = $owner AND project_id = $project);";
const INDEXED_REVISION_SET: &str = "LET $indexed_revisions = (SELECT VALUE revision_id FROM memory_runtime_embedding WHERE owner_id = $owner AND project_id = $project AND provider = $provider AND model = $model AND index_revision = $index AND dimensions = $dimensions);";

#[derive(Clone, Debug, Default, SurrealValue)]
struct CountsRow {
    sources_created: Option<u64>,
    sources_consumed: Option<u64>,
    revisions_created: Option<u64>,
    revisions_indexed: Option<u64>,
    revisions_published: Option<u64>,
    hits_returned: Option<u64>,
    sources_removed: Option<u64>,
    revisions_removed: Option<u64>,
    jobs_removed: Option<u64>,
}
impl From<&ActivityCounts> for CountsRow {
    fn from(c: &ActivityCounts) -> Self {
        Self {
            sources_created: c.sources_created,
            sources_consumed: c.sources_consumed,
            revisions_created: c.revisions_created,
            revisions_indexed: c.revisions_indexed,
            revisions_published: c.revisions_published,
            hits_returned: c.hits_returned,
            sources_removed: c.sources_removed,
            revisions_removed: c.revisions_removed,
            jobs_removed: c.jobs_removed,
        }
    }
}
impl From<CountsRow> for ActivityCounts {
    fn from(c: CountsRow) -> Self {
        Self {
            sources_created: c.sources_created,
            sources_consumed: c.sources_consumed,
            revisions_created: c.revisions_created,
            revisions_indexed: c.revisions_indexed,
            revisions_published: c.revisions_published,
            hits_returned: c.hits_returned,
            sources_removed: c.sources_removed,
            revisions_removed: c.revisions_removed,
            jobs_removed: c.jobs_removed,
        }
    }
}
#[derive(Debug, SurrealValue)]
struct EntryRow {
    entry_id: String,
    parent_id: Option<String>,
    instance_id: String,
    entry_kind: String,
    operation: String,
    phase: String,
    started_at_ms: u64,
    finished_at_ms: Option<u64>,
    classified_at_ms: Option<u64>,
    duration_ms: Option<u64>,
    outcome: String,
    reason: Option<String>,
    counts: CountsRow,
}
impl EntryRow {
    fn domain(
        self,
        project: &ProjectId,
        context: &ActivityReadContext,
    ) -> Result<ActivityEntry, RuntimeError> {
        let mut outcome =
            ActivityOutcome::parse(&self.outcome).ok_or(RuntimeError::StorageUnavailable)?;
        let mut reason = self
            .reason
            .as_deref()
            .map(|s| ActivityReason::parse(s).ok_or(RuntimeError::StorageUnavailable))
            .transpose()?;
        let id = ActivityId::new(self.entry_id).map_err(storage)?;
        let instance = WorkerInstanceId::new(self.instance_id).map_err(storage)?;
        if outcome == ActivityOutcome::Running
            && (instance != context.instance || !context.active_entries.contains(&id))
        {
            outcome = ActivityOutcome::Indeterminate;
            reason = Some(ActivityReason::Interrupted);
        }
        Ok(ActivityEntry {
            id,
            parent: self
                .parent_id
                .map(ActivityId::new)
                .transpose()
                .map_err(storage)?,
            project: project.clone(),
            entry_kind: ActivityEntryKind::parse(&self.entry_kind)
                .ok_or(RuntimeError::StorageUnavailable)?,
            operation: ActivityOperation::parse(&self.operation)
                .ok_or(RuntimeError::StorageUnavailable)?,
            phase: ActivityPhase::parse(&self.phase).ok_or(RuntimeError::StorageUnavailable)?,
            instance,
            started_at_ms: self.started_at_ms,
            finished_at_ms: self.finished_at_ms,
            classified_at_ms: self.classified_at_ms,
            duration_ms: self.duration_ms,
            outcome,
            reason,
            counts: self.counts.into(),
        })
    }
}
#[derive(Debug, SurrealValue)]
struct ParentRow {
    root_id: String,
    outcome: String,
}
#[derive(Debug, SurrealValue)]
struct TotalRow {
    total: u64,
}
#[derive(Debug, SurrealValue)]
struct StateTotal {
    state: String,
    total: u64,
}

fn ids<T: std::fmt::Display>(values: &[T]) -> Vec<String> {
    values.iter().map(ToString::to_string).collect()
}
fn digest_parts(parts: impl IntoIterator<Item = impl AsRef<str>>) -> String {
    let mut digest = Sha256::new();
    for part in parts {
        let part = part.as_ref();
        digest.update((part.len() as u64).to_be_bytes());
        digest.update(part.as_bytes());
    }
    format!("{:x}", digest.finalize())
}
fn cursor_encode(value: &Value) -> Result<String, RuntimeError> {
    let bytes = serde_json::to_vec(value).map_err(storage)?;
    let mut result = String::with_capacity(bytes.len() * 2);
    use std::fmt::Write;
    for byte in bytes {
        write!(&mut result, "{byte:02x}").map_err(storage)?;
    }
    if result.len() > ACTIVITY_MAX_CURSOR_BYTES {
        return Err(RuntimeError::StorageUnavailable);
    }
    Ok(result)
}
fn cursor_decode(value: &str) -> Result<Value, RuntimeError> {
    if value.is_empty() || value.len() > ACTIVITY_MAX_CURSOR_BYTES || !value.len().is_multiple_of(2)
    {
        return Err(RuntimeError::InvalidInput("invalid activity cursor"));
    }
    let mut bytes = Vec::with_capacity(value.len() / 2);
    for pair in value.as_bytes().as_chunks::<2>().0 {
        let digit = |c: u8| match c {
            b'0'..=b'9' => Some(c - b'0'),
            b'a'..=b'f' => Some(c - b'a' + 10),
            _ => None,
        };
        bytes.push(
            digit(pair[0])
                .zip(digit(pair[1]))
                .map(|(a, b)| a * 16 + b)
                .ok_or(RuntimeError::InvalidInput("invalid activity cursor"))?,
        );
    }
    serde_json::from_slice(&bytes)
        .map_err(|_| RuntimeError::InvalidInput("invalid activity cursor"))
}

impl EmbeddedRuntimeStore {
    pub fn with_activity_commit(&self, commit: ActivityCommit) -> Self {
        Self {
            capture: self.capture.clone(),
            activity_commit: Some(commit),
        }
    }

    async fn activity_parent<C: QueryConnection>(
        &self,
        db: &C,
        owner: &OwnerId,
        project: &ProjectId,
        id: &ActivityId,
    ) -> Result<Option<ParentRow>, RuntimeError> {
        let mut response=db.query("SELECT root_id, outcome FROM memory_runtime_activity WHERE owner_id = $owner AND project_id = $project AND entry_id = $entry;")
            .bind(("owner",owner.to_string())).bind(("project",project.to_string())).bind(("entry",id.to_string())).await.map_err(storage)?.check().map_err(storage)?;
        let mut rows: Vec<ParentRow> = response.take(0).map_err(storage)?;
        Ok(rows.pop())
    }
    async fn activity_associations<C: QueryConnection>(
        &self,
        db: &C,
        owner: &OwnerId,
        project: &ProjectId,
        sources: &[SourceId],
        revisions: &[RevisionId],
    ) -> Result<bool, RuntimeError> {
        for (table, field, values) in [
            ("memory_source", "source_id", ids(sources)),
            ("memory_revision", "revision_id", ids(revisions)),
        ] {
            if values.is_empty() {
                continue;
            }
            let wanted = values.len();
            let mut response=db.query(format!("SELECT count() AS total FROM {table} WHERE owner_id = $owner AND project_id = $project AND {field} IN $ids GROUP ALL;"))
                .bind(("owner",owner.to_string())).bind(("project",project.to_string())).bind(("ids",values)).await.map_err(storage)?.check().map_err(storage)?;
            let rows: Vec<TotalRow> = response.take(0).map_err(storage)?;
            if rows.first().map_or(0, |r| r.total) != wanted as u64 {
                return Ok(false);
            }
        }
        Ok(true)
    }
    pub async fn initialize_activity(
        &self,
        owner: &OwnerId,
        projects: &[ProjectId],
        instance: &WorkerInstanceId,
        now_ms: u64,
    ) -> Result<(), RuntimeError> {
        let _guard = self.capture.writes.lock().await;
        let tx = self.capture.db.clone().begin().await.map_err(storage)?;
        for project in projects {
            let mut response=tx.query("SELECT VALUE started_at_ms FROM memory_runtime_activity_meta WHERE owner_id = $owner AND project_id = $project;")
                .bind(("owner",owner.to_string())).bind(("project",project.to_string())).await.map_err(storage)?.check().map_err(storage)?;
            let rows: Vec<u64> = response.take(0).map_err(storage)?;
            if rows.is_empty() {
                tx.query("CREATE memory_runtime_activity_meta SET owner_id = $owner, project_id = $project, started_at_ms = $now RETURN NONE;")
                .bind(("owner",owner.to_string())).bind(("project",project.to_string())).bind(("now",now_ms)).await.map_err(storage)?.check().map_err(storage)?;
            }
            tx.query("UPDATE memory_runtime_activity SET outcome = 'indeterminate', reason = 'interrupted', classified_at_ms = $now, finished_at_ms = NONE, duration_ms = NONE WHERE owner_id = $owner AND project_id = $project AND outcome = 'running' AND instance_id != $instance RETURN NONE;")
                .bind(("owner",owner.to_string())).bind(("project",project.to_string())).bind(("instance",instance.to_string())).bind(("now",now_ms)).await.map_err(storage)?.check().map_err(storage)?;
        }
        tx.commit().await.map(|_| ()).map_err(storage)
    }
    pub async fn start_activity(
        &self,
        owner: &OwnerId,
        record: ActivityRecord,
    ) -> Result<ActivityWrite, RuntimeError> {
        let entry = &record.entry;
        if entry.id.as_str().len() > 64
            || entry.instance.as_str().len() > 64
            || entry.outcome != ActivityOutcome::Running
            || entry.finished_at_ms.is_some()
            || entry.classified_at_ms.is_some()
        {
            return Err(RuntimeError::InvalidInput("invalid activity start"));
        }
        if (entry.entry_kind == ActivityEntryKind::Stage) != entry.parent.is_some() {
            return Err(RuntimeError::InvalidInput("invalid activity parent"));
        }
        let _guard = self.capture.writes.lock().await;
        let tx = self.capture.db.clone().begin().await.map_err(storage)?;
        let root = if let Some(parent) = &entry.parent {
            let Some(parent) = self
                .activity_parent(&tx, owner, &entry.project, parent)
                .await?
            else {
                tx.cancel().await.map_err(storage)?;
                return Ok(ActivityWrite::Removed);
            };
            if parent.outcome != "running" {
                return cancel_err(tx, RuntimeError::Conflict).await;
            }
            parent.root_id
        } else {
            entry.id.to_string()
        };
        if !self
            .activity_associations(
                &tx,
                owner,
                &entry.project,
                &record.sources,
                &record.revisions,
            )
            .await?
        {
            tx.cancel().await.map_err(storage)?;
            return Ok(ActivityWrite::Removed);
        }
        tx.query("CREATE memory_runtime_activity SET owner_id = $owner, project_id = $project, entry_id = $entry, parent_id = $parent, root_id = $root, instance_id = $instance, entry_kind = $kind, operation = $operation, phase = $phase, started_at_ms = $start, outcome = 'running', counts = $counts, source_ids = $sources, revision_ids = $revisions, paid_operation = $paid RETURN NONE;")
            .bind(("owner",owner.to_string())).bind(("project",entry.project.to_string())).bind(("entry",entry.id.to_string())).bind(("parent",entry.parent.as_ref().map(ToString::to_string)))
            .bind(("root",root.clone())).bind(("instance",entry.instance.to_string())).bind(("kind",entry.entry_kind.as_str())).bind(("operation",entry.operation.as_str())).bind(("phase",entry.phase.as_str()))
            .bind(("start",entry.started_at_ms)).bind(("counts",CountsRow::from(&entry.counts))).bind(("sources",ids(&record.sources))).bind(("revisions",ids(&record.revisions))).bind(("paid",record.paid_operation)).await.map_err(storage)?.check().map_err(storage)?;
        tx.query("UPDATE memory_runtime_activity SET phase = $phase, source_ids = array::union(source_ids, $sources), revision_ids = array::union(revision_ids, $revisions) WHERE owner_id = $owner AND project_id = $project AND entry_id = $root AND outcome = 'running' RETURN NONE;")
            .bind(("owner",owner.to_string())).bind(("project",entry.project.to_string())).bind(("root",root)).bind(("phase",entry.phase.as_str())).bind(("sources",ids(&record.sources))).bind(("revisions",ids(&record.revisions))).await.map_err(storage)?.check().map_err(storage)?;
        tx.commit().await.map_err(storage)?;
        Ok(ActivityWrite::Recorded)
    }
    async fn finish_activity_tx(
        &self,
        tx: &Transaction<Db>,
        owner: &OwnerId,
        project: &ProjectId,
        finish: &ActivityFinish,
    ) -> Result<ActivityWrite, RuntimeError> {
        let Some(parent) = self.activity_parent(tx, owner, project, &finish.id).await? else {
            return Ok(ActivityWrite::Removed);
        };
        if parent.outcome != "running" {
            if parent.outcome == finish.outcome.as_str()
                && finish.outcome == ActivityOutcome::Succeeded
            {
                tx.query("UPDATE memory_runtime_activity SET finished_at_ms = $finish, duration_ms = $duration WHERE owner_id = $owner AND project_id = $project AND entry_id = $entry RETURN NONE;")
                    .bind(("owner",owner.to_string())).bind(("project",project.to_string())).bind(("entry",finish.id.to_string()))
                    .bind(("finish",finish.finished_at_ms)).bind(("duration",finish.duration_ms)).await.map_err(storage)?.check().map_err(storage)?;
            }
            return Ok(ActivityWrite::AlreadyClosed);
        }
        if finish.outcome == ActivityOutcome::Running {
            return Err(RuntimeError::InvalidInput("invalid activity finish"));
        }
        if parent.root_id == finish.id.as_str() && finish.outcome == ActivityOutcome::Succeeded {
            let mut response=tx.query("SELECT count() AS total FROM memory_runtime_activity WHERE owner_id = $owner AND project_id = $project AND root_id = $root AND entry_kind = 'stage' AND outcome = 'running' GROUP ALL;")
                .bind(("owner",owner.to_string())).bind(("project",project.to_string())).bind(("root",parent.root_id)).await.map_err(storage)?.check().map_err(storage)?;
            let rows: Vec<TotalRow> = response.take(0).map_err(storage)?;
            if rows.first().is_some_and(|r| r.total != 0) {
                return Err(RuntimeError::Conflict);
            }
        }
        tx.query("UPDATE memory_runtime_activity SET phase = $phase, outcome = $outcome, finished_at_ms = $finish, classified_at_ms = $classified, duration_ms = $duration, reason = $reason, counts = $counts WHERE owner_id = $owner AND project_id = $project AND entry_id = $entry AND outcome = 'running' RETURN NONE;")
            .bind(("owner",owner.to_string())).bind(("project",project.to_string())).bind(("entry",finish.id.to_string())).bind(("phase",finish.phase.as_str())).bind(("outcome",finish.outcome.as_str()))
            .bind(("finish",finish.finished_at_ms)).bind(("classified",finish.classified_at_ms)).bind(("duration",finish.duration_ms)).bind(("reason",finish.reason.map(ActivityReason::as_str))).bind(("counts",CountsRow::from(&finish.counts))).await.map_err(storage)?.check().map_err(storage)?;
        Ok(ActivityWrite::Recorded)
    }
    pub async fn finish_activity(
        &self,
        owner: &OwnerId,
        project: &ProjectId,
        finish: &ActivityFinish,
    ) -> Result<ActivityWrite, RuntimeError> {
        let _guard = self.capture.writes.lock().await;
        let tx = self.capture.db.clone().begin().await.map_err(storage)?;
        let result = self.finish_activity_tx(&tx, owner, project, finish).await?;
        tx.commit().await.map_err(storage)?;
        Ok(result)
    }
    async fn link_activity_tx(
        &self,
        tx: &Transaction<Db>,
        owner: &OwnerId,
        project: &ProjectId,
        id: &ActivityId,
        associations: (&[SourceId], &[RevisionId], Option<&str>),
    ) -> Result<ActivityWrite, RuntimeError> {
        let (sources, revisions, paid) = associations;
        let Some(parent) = self.activity_parent(tx, owner, project, id).await? else {
            return Ok(ActivityWrite::Removed);
        };
        if parent.outcome != "running" {
            return Ok(ActivityWrite::AlreadyClosed);
        }
        if !self
            .activity_associations(tx, owner, project, sources, revisions)
            .await?
        {
            return Ok(ActivityWrite::Removed);
        }
        tx.query("UPDATE memory_runtime_activity SET source_ids = array::union(source_ids, $sources), revision_ids = array::union(revision_ids, $revisions) WHERE owner_id = $owner AND project_id = $project AND (entry_id = $entry OR entry_id = $root) RETURN NONE;")
            .bind(("owner",owner.to_string())).bind(("project",project.to_string())).bind(("entry",id.to_string())).bind(("root",parent.root_id)).bind(("sources",ids(sources))).bind(("revisions",ids(revisions))).await.map_err(storage)?.check().map_err(storage)?;
        if let Some(paid) = paid {
            tx.query("UPDATE memory_runtime_activity SET paid_operation = $paid WHERE owner_id = $owner AND project_id = $project AND entry_id = $entry RETURN NONE;")
            .bind(("owner",owner.to_string())).bind(("project",project.to_string())).bind(("entry",id.to_string())).bind(("paid",paid.to_owned())).await.map_err(storage)?.check().map_err(storage)?;
        }
        Ok(ActivityWrite::Recorded)
    }
    pub async fn link_activity(
        &self,
        owner: &OwnerId,
        project: &ProjectId,
        id: &ActivityId,
        sources: &[SourceId],
        revisions: &[RevisionId],
        paid: Option<&str>,
    ) -> Result<ActivityWrite, RuntimeError> {
        let _guard = self.capture.writes.lock().await;
        let tx = self.capture.db.clone().begin().await.map_err(storage)?;
        let result = self
            .link_activity_tx(&tx, owner, project, id, (sources, revisions, paid))
            .await?;
        tx.commit().await.map_err(storage)?;
        Ok(result)
    }
    pub(super) fn validate_activity_scope(
        &self,
        owner: &OwnerId,
        project: &ProjectId,
    ) -> Result<(), RuntimeError> {
        if self
            .activity_commit
            .as_ref()
            .is_some_and(|c| &c.owner != owner || &c.project != project)
        {
            return Err(RuntimeError::Unauthorized);
        }
        Ok(())
    }
    pub(super) async fn finish_attached_activity(
        &self,
        tx: &Transaction<Db>,
        sources: &[SourceId],
        revisions: &[RevisionId],
        counts: Option<ActivityCounts>,
    ) -> Result<(), RuntimeError> {
        if let Some(commit) = &self.activity_commit {
            self.link_activity_tx(
                tx,
                &commit.owner,
                &commit.project,
                &commit.finish.id,
                (sources, revisions, None),
            )
            .await?;
            let mut finish = commit.finish.clone();
            if let Some(counts) = counts {
                finish.counts = counts;
            }
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(storage)?
                .as_millis();
            finish.finished_at_ms = Some(u64::try_from(now).map_err(storage)?);
            // The service refines a committed result with its measured monotonic duration.
            finish.duration_ms = None;
            self.finish_activity_tx(tx, &commit.owner, &commit.project, &finish)
                .await?;
            if let Some(root) = &commit.root_finish {
                self.finish_activity_tx(tx, &commit.owner, &commit.project, root)
                    .await?;
            }
        }
        Ok(())
    }
    async fn activity_coverage(
        &self,
        tx: &Transaction<Db>,
        owner: &OwnerId,
        project: &ProjectId,
        now: u64,
    ) -> Result<HistoryCoverage, RuntimeError> {
        let mut response=tx.query("SELECT VALUE started_at_ms FROM memory_runtime_activity_meta WHERE owner_id = $owner AND project_id = $project;")
            .bind(("owner",owner.to_string())).bind(("project",project.to_string())).await.map_err(storage)?.check().map_err(storage)?;
        let rows: Vec<u64> = response.take(0).map_err(storage)?;
        Ok(HistoryCoverage {
            retention_days: 7,
            instrumentation_started_at_ms: rows.first().copied(),
            retained_since_ms: now.saturating_sub(ACTIVITY_RETENTION_MS),
        })
    }
    async fn scoped_total(
        &self,
        tx: &Transaction<Db>,
        owner: &OwnerId,
        project: &ProjectId,
        sql: &str,
        context: Option<&ActivityReadContext>,
    ) -> Result<u64, RuntimeError> {
        let mut query = tx
            .query(sql)
            .bind(("owner", owner.to_string()))
            .bind(("project", project.to_string()));
        if let Some(context) = context {
            query = query.bind(("paid", context.active_paid_operations.clone()));
        }
        let mut response = query.await.map_err(storage)?.check().map_err(storage)?;
        let rows: Vec<TotalRow> = response
            .take(response.num_statements() - 1)
            .map_err(storage)?;
        Ok(rows.first().map_or(0, |r| r.total))
    }
    async fn latest_activity(
        &self,
        tx: &Transaction<Db>,
        owner: &OwnerId,
        project: &ProjectId,
        context: &ActivityReadContext,
        now: u64,
        filter: (ActivityOutcome, bool),
    ) -> Result<Option<ActivityEntry>, RuntimeError> {
        let (outcome, dream) = filter;
        let mut response=tx.query(format!("SELECT {ENTRY_FIELDS} FROM memory_runtime_activity WHERE owner_id = $owner AND project_id = $project AND entry_kind = 'operation' AND outcome = $outcome AND (!$dream OR operation = 'dream') AND (finished_at_ms > $cutoff OR classified_at_ms > $cutoff) ORDER BY finished_at_ms DESC, entry_id DESC LIMIT 1;"))
            .bind(("owner",owner.to_string())).bind(("project",project.to_string())).bind(("outcome",outcome.as_str())).bind(("dream",dream)).bind(("cutoff",now.saturating_sub(ACTIVITY_RETENTION_MS))).await.map_err(storage)?.check().map_err(storage)?;
        let mut rows: Vec<EntryRow> = response.take(0).map_err(storage)?;
        rows.pop().map(|r| r.domain(project, context)).transpose()
    }
    pub async fn activity_snapshot(
        &self,
        owner: &OwnerId,
        project: &ProjectId,
        space: Option<&EmbeddingSpace>,
        context: &ActivityReadContext,
        now: u64,
    ) -> Result<StoredActivityStatus, RuntimeError> {
        let tx = self.capture.db.clone().begin().await.map_err(storage)?;
        let pending_condition = "source_id NOT IN $consumed_sources";
        let unconsumed_sources = self
            .scoped_total(
                &tx,
                owner,
                project,
                &format!("{CONSUMED_SOURCE_SET} SELECT count() AS total FROM memory_source WHERE owner_id = $owner AND project_id = $project AND {pending_condition} GROUP ALL;"),
                None,
            )
            .await?;
        let blocked_sources = if space.is_some() {
            self.scoped_total(&tx,owner,project,&format!("{CONSUMED_SOURCE_SET} LET $blocked_sources = array::flatten((SELECT VALUE sources FROM memory_runtime_operation WHERE owner_id = $owner AND project_id = $project AND string::starts_with(operation_id, 'dream-job:') AND operation_id NOT IN $paid)); SELECT count() AS total FROM memory_source WHERE owner_id = $owner AND project_id = $project AND {pending_condition} AND source_id IN $blocked_sources GROUP ALL;"),Some(context)).await?
        } else {
            0
        };
        let processing_revisions = self
            .scoped_total(
                &tx,
                owner,
                project,
                &format!("{REVISION_STATE_SETS} SELECT count() AS total FROM memory_revision WHERE owner_id = $owner AND project_id = $project AND {PENDING_REVISIONS} GROUP ALL;"),
                None,
            )
            .await?;
        let mut lifecycle = LifecycleCounts {
            processing: processing_revisions,
            ..LifecycleCounts::default()
        };
        let mut response=tx.query("LET $existing = (SELECT VALUE revision_id FROM memory_revision WHERE owner_id = $owner AND project_id = $project); SELECT state, count() AS total FROM memory_revision_state WHERE owner_id = $owner AND project_id = $project AND revision_id IN $existing GROUP BY state;")
            .bind(("owner",owner.to_string())).bind(("project",project.to_string())).await.map_err(storage)?.check().map_err(storage)?;
        let states: Vec<StateTotal> = response.take(1).map_err(storage)?;
        for row in states {
            match parse_state(&row.state)? {
                LifecycleState::Active => lifecycle.active = row.total,
                LifecycleState::Processing => {}
                LifecycleState::Conflicted => lifecycle.conflicted = row.total,
                LifecycleState::Rejected => lifecycle.rejected = row.total,
                LifecycleState::Superseded => lifecycle.superseded = row.total,
            }
        }
        let mut blocked_revisions = 0;
        let active_without_current_embedding = if let Some(space) = space {
            let missing = "revision_id NOT IN $indexed_revisions";
            let mut response=tx.query(format!("{INDEXED_REVISION_SET} LET $active = (SELECT VALUE revision_id FROM memory_revision_state WHERE owner_id = $owner AND project_id = $project AND state = 'active'); SELECT count() AS total FROM memory_revision WHERE owner_id = $owner AND project_id = $project AND revision_id IN $active AND {missing} GROUP ALL;"))
                .bind(("owner",owner.to_string())).bind(("project",project.to_string())).bind(("provider",space.provider.clone())).bind(("model",space.model.clone())).bind(("index",space.revision.clone())).bind(("dimensions",space.dimensions)).await.map_err(storage)?.check().map_err(storage)?;
            let rows: Vec<TotalRow> = response.take(2).map_err(storage)?;
            let active = rows.first().map_or(0, |r| r.total);
            // Legacy markers encode revision/space identity only in their hash. Project IDs, not contents, are sufficient to recover those exact keys.
            let mut response=tx.query(format!("{REVISION_STATE_SETS} {INDEXED_REVISION_SET} SELECT VALUE revision_id FROM memory_revision WHERE owner_id = $owner AND project_id = $project AND {PENDING_REVISIONS} AND {missing};"))
                .bind(("owner",owner.to_string())).bind(("project",project.to_string())).bind(("provider",space.provider.clone())).bind(("model",space.model.clone())).bind(("index",space.revision.clone())).bind(("dimensions",space.dimensions)).await.map_err(storage)?.check().map_err(storage)?;
            let revisions: Vec<String> = response.take(3).map_err(storage)?;
            for chunk in revisions.chunks(256) {
                let operations: Vec<String> = chunk
                    .iter()
                    .map(|id| {
                        format!(
                            "embedding:{}",
                            digest_parts([
                                id.as_str(),
                                space.provider.as_str(),
                                space.model.as_str(),
                                space.revision.as_str()
                            ])
                        )
                    })
                    .collect();
                let mut response=tx.query("SELECT count() AS total FROM memory_runtime_operation WHERE owner_id = $owner AND project_id = $project AND operation_id IN $operations AND operation_id NOT IN $paid GROUP ALL;")
                    .bind(("owner",owner.to_string())).bind(("project",project.to_string())).bind(("operations",operations)).bind(("paid",context.active_paid_operations.clone())).await.map_err(storage)?.check().map_err(storage)?;
                let rows: Vec<TotalRow> = response.take(0).map_err(storage)?;
                blocked_revisions += rows.first().map_or(0, |r| r.total);
            }
            Some(active)
        } else {
            None
        };
        let unresolved_conflicts = self
            .scoped_total(
                &tx,
                owner,
                project,
                "SELECT count() AS total FROM memory_revision_conflict WHERE owner_id = $owner AND project_id = $project AND resolved = false GROUP ALL;",
                None,
            )
            .await?;
        let last_success = self
            .latest_activity(
                &tx,
                owner,
                project,
                context,
                now,
                (ActivityOutcome::Succeeded, false),
            )
            .await?;
        let last_failure = self
            .latest_activity(
                &tx,
                owner,
                project,
                context,
                now,
                (ActivityOutcome::Failed, false),
            )
            .await?;
        let last_dream_success = self
            .latest_activity(
                &tx,
                owner,
                project,
                context,
                now,
                (ActivityOutcome::Succeeded, true),
            )
            .await?;
        let last_dream_failure = self
            .latest_activity(
                &tx,
                owner,
                project,
                context,
                now,
                (ActivityOutcome::Failed, true),
            )
            .await?;
        let history = self.activity_coverage(&tx, owner, project, now).await?;
        tx.cancel().await.map_err(storage)?;
        Ok(StoredActivityStatus {
            pending: PendingCounts {
                unconsumed_sources,
                processing_revisions,
                blocked_sources,
                blocked_revisions,
                active_without_current_embedding,
            },
            lifecycle,
            unresolved_conflicts,
            last_success,
            last_failure,
            last_dream_success,
            last_dream_failure,
            history,
        })
    }
    pub async fn activity_page(
        &self,
        owner: &OwnerId,
        request: &ActivityRequest,
        context: &ActivityReadContext,
        now: u64,
    ) -> Result<ActivityPage, RuntimeError> {
        if request.limit == 0 || request.limit > ACTIVITY_MAX_LIMIT {
            return Err(RuntimeError::InvalidInput("invalid activity limit"));
        }
        let mut operations = request.operations.clone();
        operations.sort_unstable();
        operations.dedup();
        let mut outcomes = request.outcomes.clone();
        outcomes.sort_unstable();
        outcomes.dedup();
        if operations.len() != request.operations.len() || outcomes.len() != request.outcomes.len()
        {
            return Err(RuntimeError::InvalidInput("duplicate activity filters"));
        }
        let scope = digest_parts([owner.as_str(), request.project.as_str()]);
        let filter = digest_parts(
            operations
                .iter()
                .map(|v| v.as_str())
                .chain(std::iter::once("/"))
                .chain(outcomes.iter().map(|v| v.as_str())),
        );
        let cursor = request.cursor.as_deref().map(cursor_decode).transpose()?;
        let mut before_time = u64::MAX;
        let mut before_id = String::new();
        let mut ceiling_time = u64::MAX;
        let mut ceiling_id = String::new();
        if let Some(cursor) = &cursor {
            if cursor.as_object().is_none_or(|v| v.len() != 6)
                || cursor["scope"] != scope
                || cursor["filter"] != filter
            {
                return Err(RuntimeError::InvalidInput(
                    "activity cursor scope/filter mismatch",
                ));
            }
            before_time = cursor["before_time"]
                .as_u64()
                .filter(|timestamp| i64::try_from(*timestamp).is_ok())
                .ok_or(RuntimeError::InvalidInput("invalid activity cursor"))?;
            ceiling_time = cursor["ceiling_time"]
                .as_u64()
                .filter(|timestamp| i64::try_from(*timestamp).is_ok())
                .ok_or(RuntimeError::InvalidInput("invalid activity cursor"))?;
            before_id = cursor["before_id"]
                .as_str()
                .filter(|v| !v.is_empty() && v.len() <= 64)
                .ok_or(RuntimeError::InvalidInput("invalid activity cursor"))?
                .to_owned();
            ceiling_id = cursor["ceiling_id"]
                .as_str()
                .filter(|v| !v.is_empty() && v.len() <= 64)
                .ok_or(RuntimeError::InvalidInput("invalid activity cursor"))?
                .to_owned();
            if (before_time, before_id.as_str()) > (ceiling_time, ceiling_id.as_str()) {
                return Err(RuntimeError::InvalidInput("invalid activity cursor order"));
            }
        }
        let tx = self.capture.db.clone().begin().await.map_err(storage)?;
        let filter_sql = format!(
            "owner_id = $owner AND project_id = $project AND ({LIVE_TREES}) AND (array::len($operations) = 0 OR operation IN $operations) AND (array::len($outcomes) = 0 OR ({EFFECTIVE_OUTCOME}) IN $outcomes) AND (!$paged OR started_at_ms < $before_time OR (started_at_ms = $before_time AND entry_id < $before_id)) AND (!$paged OR started_at_ms < $ceiling_time OR (started_at_ms = $ceiling_time AND entry_id <= $ceiling_id))"
        );
        let mut response=tx.query(format!("{RETAINED_ROOTS} SELECT {ENTRY_FIELDS} FROM memory_runtime_activity WHERE {filter_sql} ORDER BY started_at_ms DESC, entry_id DESC LIMIT $limit;"))
            .bind(("owner",owner.to_string())).bind(("project",request.project.to_string())).bind(("instance",context.instance.to_string())).bind(("active",ids(&context.active_entries))).bind(("cutoff",now.saturating_sub(ACTIVITY_RETENTION_MS)))
            .bind(("operations",operations.iter().map(|v|v.as_str().to_owned()).collect::<Vec<_>>())).bind(("outcomes",outcomes.iter().map(|v|v.as_str().to_owned()).collect::<Vec<_>>()))
            .bind(("paged",cursor.is_some())).bind(("before_time",if before_time==u64::MAX{0}else{before_time})).bind(("before_id",before_id)).bind(("ceiling_time",if ceiling_time==u64::MAX{0}else{ceiling_time})).bind(("ceiling_id",ceiling_id.clone())).bind(("limit",request.limit+1)).await.map_err(storage)?.check().map_err(storage)?;
        let mut rows: Vec<EntryRow> = response.take(1).map_err(storage)?;
        let more = rows.len() > request.limit;
        if more {
            rows.pop();
        }
        if cursor.is_none()
            && let Some(first) = rows.first()
        {
            ceiling_time = first.started_at_ms;
            ceiling_id = first.entry_id.clone();
        }
        let next_cursor = if more {
            let last = rows.last().ok_or(RuntimeError::StorageUnavailable)?;
            Some(cursor_encode(
                &json!({"scope":scope,"filter":filter,"ceiling_time":ceiling_time,"ceiling_id":ceiling_id,"before_time":last.started_at_ms,"before_id":last.entry_id}),
            )?)
        } else {
            None
        };
        let entries = rows
            .into_iter()
            .map(|r| r.domain(&request.project, context))
            .collect::<Result<Vec<_>, _>>()?;
        let history = self
            .activity_coverage(&tx, owner, &request.project, now)
            .await?;
        tx.cancel().await.map_err(storage)?;
        Ok(ActivityPage {
            project: request.project.clone(),
            observed_at_ms: now,
            history,
            entries,
            next_cursor,
        })
    }
    pub async fn maintain_activity(
        &self,
        owner: &OwnerId,
        project: &ProjectId,
        context: &ActivityReadContext,
        now: u64,
        limit: usize,
    ) -> Result<(), RuntimeError> {
        self.maintain_activity_with(owner, project, || Ok(context.clone()), now, limit)
            .await
    }
    /// Takes the live-entry snapshot only after acquiring the write lock. Running rows are
    /// persisted under that lock after their live entry exists, so no live operation can be
    /// missing from the snapshot and misclassified as interrupted.
    pub async fn maintain_activity_with(
        &self,
        owner: &OwnerId,
        project: &ProjectId,
        snapshot: impl FnOnce() -> Result<ActivityReadContext, RuntimeError>,
        now: u64,
        limit: usize,
    ) -> Result<(), RuntimeError> {
        if limit == 0 {
            return Ok(());
        }
        let _guard = self.capture.writes.lock().await;
        let context = &snapshot()?;
        let tx = self.capture.db.clone().begin().await.map_err(storage)?;
        tx.query("UPDATE memory_runtime_activity SET outcome = 'indeterminate', reason = 'interrupted', classified_at_ms = $now, finished_at_ms = NONE, duration_ms = NONE WHERE owner_id = $owner AND project_id = $project AND outcome = 'running' AND (instance_id != $instance OR entry_id NOT IN $active) RETURN NONE;")
            .bind(("owner",owner.to_string())).bind(("project",project.to_string())).bind(("instance",context.instance.to_string())).bind(("active",ids(&context.active_entries))).bind(("now",now)).await.map_err(storage)?.check().map_err(storage)?;
        let mut response=tx.query("SELECT VALUE entry_id FROM memory_runtime_activity WHERE owner_id = $owner AND project_id = $project AND entry_kind = 'operation' AND outcome != 'running' AND ((finished_at_ms != NONE AND finished_at_ms <= $cutoff) OR (classified_at_ms != NONE AND classified_at_ms <= $cutoff)) ORDER BY started_at_ms, entry_id LIMIT $limit;")
            .bind(("owner",owner.to_string())).bind(("project",project.to_string())).bind(("cutoff",now.saturating_sub(ACTIVITY_RETENTION_MS))).bind(("limit",limit)).await.map_err(storage)?.check().map_err(storage)?;
        let roots: Vec<String> = response.take(0).map_err(storage)?;
        if !roots.is_empty() {
            tx.query("DELETE memory_runtime_activity WHERE owner_id = $owner AND project_id = $project AND root_id IN $roots RETURN NONE;")
            .bind(("owner",owner.to_string())).bind(("project",project.to_string())).bind(("roots",roots)).await.map_err(storage)?.check().map_err(storage)?;
        }
        tx.commit().await.map(|_| ()).map_err(storage)
    }
    pub(super) async fn purge_activity_tx(
        &self,
        tx: &Transaction<Db>,
        owner: &OwnerId,
        project: &ProjectId,
        sources: &[String],
        revisions: &[String],
    ) -> Result<(), RuntimeError> {
        let mut response=tx.query("SELECT VALUE root_id FROM memory_runtime_activity WHERE owner_id = $owner AND project_id = $project AND (array::intersect(source_ids, $sources) != [] OR array::intersect(revision_ids, $revisions) != []);")
            .bind(("owner",owner.to_string())).bind(("project",project.to_string())).bind(("sources",sources.to_vec())).bind(("revisions",revisions.to_vec())).await.map_err(storage)?.check().map_err(storage)?;
        let roots: Vec<String> = response.take(0).map_err(storage)?;
        if !roots.is_empty() {
            tx.query("DELETE memory_runtime_activity WHERE owner_id = $owner AND project_id = $project AND root_id IN $roots RETURN NONE;")
            .bind(("owner",owner.to_string())).bind(("project",project.to_string())).bind(("roots",roots)).await.map_err(storage)?.check().map_err(storage)?;
        }
        Ok(())
    }
}

#[derive(Debug, SurrealValue)]
struct EffectRow {
    outcome: String,
    counts: CountsRow,
}
impl EmbeddedRuntimeStore {
    pub async fn recorded_activity_effect(
        &self,
        owner: &OwnerId,
        project: &ProjectId,
        id: &ActivityId,
    ) -> Result<Option<(ActivityOutcome, ActivityCounts)>, RuntimeError> {
        let mut response=self.capture.db.query("SELECT outcome, counts FROM memory_runtime_activity WHERE owner_id = $owner AND project_id = $project AND entry_id = $entry;")
            .bind(("owner",owner.to_string())).bind(("project",project.to_string())).bind(("entry",id.to_string())).await.map_err(storage)?.check().map_err(storage)?;
        let mut rows: Vec<EffectRow> = response.take(0).map_err(storage)?;
        rows.pop()
            .map(|row| {
                Ok((
                    ActivityOutcome::parse(&row.outcome).ok_or(RuntimeError::StorageUnavailable)?,
                    row.counts.into(),
                ))
            })
            .transpose()
    }
    pub async fn retention_source_ids(
        &self,
        owner: &OwnerId,
        project: &ProjectId,
        before: u64,
        limit: usize,
    ) -> Result<Vec<SourceId>, RuntimeError> {
        let mut response=self.capture.db.query("SELECT VALUE source_id FROM memory_source WHERE owner_id = $owner AND project_id = $project AND recorded_at_ms < $before ORDER BY recorded_at_ms, source_id LIMIT $limit;")
            .bind(("owner",owner.to_string())).bind(("project",project.to_string())).bind(("before",before)).bind(("limit",limit)).await.map_err(storage)?.check().map_err(storage)?;
        let rows: Vec<String> = response.take(0).map_err(storage)?;
        rows.into_iter()
            .map(|id| SourceId::new(id).map_err(storage))
            .collect()
    }
}
