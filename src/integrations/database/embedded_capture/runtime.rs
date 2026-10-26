//! Durable runtime capture, review and purge. SDK types stay private.
//! Domain/datastore only: the runtime service applies snapshot/revision policy before
//! `commit_batch`; this store re-checks immutable originals, lineage and atomic consumption.
use super::{EmbeddedCaptureStore, SOURCE_PROJECTION, SourceRecord};
use crate::domain_models::capture::*;
use crate::domain_models::consolidation::*;
use crate::domain_models::runtime::*;
use crate::domain_models::{ActorId, JobId, MemoryId, OwnerId, ProjectId, RevisionId, SourceId};
use sha2::{Digest, Sha256};
use std::borrow::Cow;
use std::collections::{BTreeSet, HashSet};
use surrealdb::{
    Surreal,
    engine::local::Db,
    method::{Query, Transaction},
    types::SurrealValue,
};

mod diagnostics;
mod embedding_recovery;
mod recovery;
mod semantic;
mod timeline;

// Shared queries must retain the transaction ID on the SDK's distinct type.
trait QueryConnection: Sync {
    fn query<'a>(&'a self, sql: impl Into<Cow<'a, str>>) -> Query<'a, Db>;
}
impl QueryConnection for Surreal<Db> {
    fn query<'a>(&'a self, sql: impl Into<Cow<'a, str>>) -> Query<'a, Db> {
        Surreal::query(self, sql)
    }
}
impl QueryConnection for Transaction<Db> {
    fn query<'a>(&'a self, sql: impl Into<Cow<'a, str>>) -> Query<'a, Db> {
        Transaction::query(self, sql)
    }
}

const JOB_FIELDS: &str = "owner_id, project_id, job_id, watermark_ms, sources, status, revisions";
const REV_FIELDS: &str = "owner_id, project_id, revision_id, memory_id, job_id, watermark_ms, kind, status, content, prerequisites, sources, predecessor, model, prompt_version";
const STATE_FIELDS: &str =
    "owner_id, project_id, revision_id, memory_id, state, reviewed_at_ms, reviewed_by";
const CONSUMPTION_FIELDS: &str = "owner_id, project_id, source_id, job_id";
/// Origin prefix of legacy per-tool-call sources, which are not derivation input.
const TOOL_RESULT_ORIGIN: &str = "omp://tool_result";
/// Every stored kind except `action`, spelled out so the `source_kind_scope` index serves the scan.
const BACKLOG_KINDS: &str = "'attempt', 'result', 'error', 'decision'";
const HEAD_FIELDS: &str = "owner_id, project_id, memory_id, revision_id";
const CONFLICT_FIELDS: &str =
    "owner_id, project_id, memory_id, left_id, right_id, resolved, selected_id";

#[derive(Clone)]
pub struct EmbeddedRuntimeStore {
    capture: EmbeddedCaptureStore,
    activity_commit: Option<crate::domain_models::runtime_diagnostics::ActivityCommit>,
}

#[derive(Clone, Debug, PartialEq, Eq, SurrealValue)]
struct RevisionRecord {
    owner_id: String,
    project_id: String,
    revision_id: String,
    memory_id: String,
    job_id: String,
    watermark_ms: u64,
    kind: String,
    status: String,
    content: String,
    prerequisites: Vec<String>,
    sources: Vec<String>,
    predecessor: Option<String>,
    model: String,
    prompt_version: String,
}
#[derive(Clone, Debug, PartialEq, Eq, SurrealValue)]
struct JobRecord {
    owner_id: String,
    project_id: String,
    job_id: String,
    watermark_ms: u64,
    sources: Vec<SourceRecord>,
    status: String,
    revisions: Vec<RevisionRecord>,
}
#[derive(Clone, Debug, PartialEq, Eq, SurrealValue)]
struct StateRecord {
    owner_id: String,
    project_id: String,
    revision_id: String,
    memory_id: String,
    state: String,
    reviewed_at_ms: Option<u64>,
    reviewed_by: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, SurrealValue)]
struct ConsumptionRecord {
    owner_id: String,
    project_id: String,
    source_id: String,
    job_id: String,
}
#[derive(Clone, Debug, PartialEq, Eq, SurrealValue)]
struct HeadRecord {
    owner_id: String,
    project_id: String,
    memory_id: String,
    revision_id: String,
}
#[derive(Clone, Debug, PartialEq, Eq, SurrealValue)]
struct ConflictRecord {
    owner_id: String,
    project_id: String,
    memory_id: String,
    left_id: String,
    right_id: String,
    resolved: bool,
    selected_id: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, SurrealValue)]
struct AuditRecord {
    owner_id: String,
    project_id: String,
    revision_id: String,
    actor_id: String,
    action: String,
    from_state: String,
    to_state: String,
    expected_active: Option<String>,
    conflict_with: Option<String>,
    at_ms: u64,
}
/// Audit projection read back for lineage pages; `actor_id` is deliberately not selected.
#[derive(Debug, SurrealValue)]
struct AuditRow {
    revision_id: String,
    action: String,
    from_state: String,
    to_state: String,
    expected_active: Option<String>,
    conflict_with: Option<String>,
    at_ms: u64,
}
#[derive(Debug, SurrealValue)]
struct RankedRecord {
    revision: RevisionRecord,
    lexical_score: u64,
    memory_key: String,
    revision_key: String,
}
#[derive(Debug, SurrealValue)]
struct PurgeJob {
    job_id: String,
    source_ids: Vec<String>,
}
#[derive(Debug, SurrealValue)]
struct PurgeRevision {
    revision_id: String,
    job_id: String,
    sources: Vec<String>,
    predecessor: Option<String>,
}

fn storage<T>(_: T) -> RuntimeError {
    RuntimeError::StorageUnavailable
}
async fn cancel_err<T>(tx: Transaction<Db>, error: RuntimeError) -> Result<T, RuntimeError> {
    tx.cancel().await.map_err(storage)?;
    Err(error)
}
fn state_name(state: LifecycleState) -> &'static str {
    match state {
        LifecycleState::Processing => "processing",
        LifecycleState::Active => "active",
        LifecycleState::Rejected => "rejected",
        LifecycleState::Conflicted => "conflicted",
        LifecycleState::Superseded => "superseded",
    }
}
fn parse_state(value: &str) -> Result<LifecycleState, RuntimeError> {
    match value {
        "processing" => Ok(LifecycleState::Processing),
        "active" => Ok(LifecycleState::Active),
        "rejected" => Ok(LifecycleState::Rejected),
        "conflicted" => Ok(LifecycleState::Conflicted),
        "superseded" => Ok(LifecycleState::Superseded),
        _ => Err(RuntimeError::StorageUnavailable),
    }
}
/// Audit action of the operator-only retirement of legacy tool-metadata revisions.
const RETIRE_ACTION: &str = "operator_retire_tool_metadata";
fn action_name(action: ReviewAction) -> &'static str {
    match action {
        ReviewAction::Promote => "promote",
        ReviewAction::Reject => "reject",
        ReviewAction::Conflict => "conflict",
    }
}
fn parse_action(value: &str) -> Result<ReviewAction, RuntimeError> {
    match value {
        "promote" => Ok(ReviewAction::Promote),
        "reject" => Ok(ReviewAction::Reject),
        "conflict" => Ok(ReviewAction::Conflict),
        // Operator retirement is a rejection in the public lifecycle vocabulary.
        RETIRE_ACTION => Ok(ReviewAction::Reject),
        _ => Err(RuntimeError::StorageUnavailable),
    }
}
fn audit_entry(row: AuditRow) -> Result<LifecycleAuditEntry, RuntimeError> {
    let id = |value: String| RevisionId::new(value).map_err(storage);
    Ok(LifecycleAuditEntry {
        revision: id(row.revision_id)?,
        action: parse_action(&row.action)?,
        from: parse_state(&row.from_state)?,
        to: parse_state(&row.to_state)?,
        expected_active: row.expected_active.map(id).transpose()?,
        conflict_with: row.conflict_with.map(id).transpose()?,
        at_ms: row.at_ms,
    })
}
fn kind_name(kind: &KnowledgeKind) -> &'static str {
    match kind {
        KnowledgeKind::Knowledge => "knowledge",
        KnowledgeKind::Procedure => "procedure",
    }
}
fn parse_kind(value: &str) -> Result<KnowledgeKind, RuntimeError> {
    match value {
        "knowledge" => Ok(KnowledgeKind::Knowledge),
        "procedure" => Ok(KnowledgeKind::Procedure),
        _ => Err(RuntimeError::StorageUnavailable),
    }
}
fn source_event(record: SourceRecord) -> Result<CaptureEvent, RuntimeError> {
    record
        .into_domain()
        .map_err(|_| RuntimeError::StorageUnavailable)
}
/// MCP capture stamps wall time on every call. Same source/content must stay idempotent.
fn same_substance(old: &SourceRecord, wanted: &SourceRecord) -> bool {
    old.owner_id == wanted.owner_id
        && old.source_id == wanted.source_id
        && old.actor_id == wanted.actor_id
        && old.project_id == wanted.project_id
        && old.origin == wanted.origin
        && old.kind == wanted.kind
        && old.outcome == wanted.outcome
        && old.content == wanted.content
}
fn validate_event(event: &CaptureEvent, limits: &RuntimeLimits) -> Result<(), RuntimeError> {
    if limits.max_pending == 0
        || limits.max_content_bytes == 0
        || event.content.trim().is_empty()
        || event.content.len() > limits.max_content_bytes
        || event.origin.trim().is_empty()
        || event.origin.len() > 2048
        || event.origin.chars().any(char::is_control)
    {
        return Err(RuntimeError::InvalidInput("invalid capture evidence"));
    }
    Ok(())
}
/// Store-local snapshot shape. Service already ran the engine validator; this file cannot import engines.
fn validate_snapshot(snapshot: &JobSnapshot) -> Result<(), RuntimeError> {
    if snapshot.sources.is_empty() || snapshot.sources.len() > 32 {
        return Err(RuntimeError::InvalidInput("invalid snapshot"));
    }
    for (i, event) in snapshot.sources.iter().enumerate() {
        if event.owner != snapshot.owner
            || event.project != snapshot.project
            || event.recorded_at_ms > snapshot.watermark_ms
            || event.content.trim().is_empty()
            || event.content.len() > 8192
            || event.origin.trim().is_empty()
            || event.origin.len() > 2048
            || event.origin.chars().any(char::is_control)
            || snapshot.sources[..i]
                .iter()
                .any(|old| old.source == event.source)
        {
            return Err(RuntimeError::InvalidInput("invalid snapshot"));
        }
    }
    Ok(())
}
/// Store-local revision shape and source membership. Lineage/originals are checked against durable rows.
fn validate_revision(snapshot: &JobSnapshot, revision: &Revision) -> Result<(), RuntimeError> {
    if revision.owner != snapshot.owner
        || revision.project != snapshot.project
        || revision.content.trim().is_empty()
        || revision.content.len() > 8192
        || revision.model.trim().is_empty()
        || revision.model.len() > 256
        || revision.prompt_version.trim().is_empty()
        || revision.prompt_version.len() > 256
        || revision.sources.is_empty()
        || revision.sources.len() > 32
        || revision.prerequisites.len() > 16
        || revision
            .prerequisites
            .iter()
            .any(|p| p.trim().is_empty() || p.len() > 1024)
    {
        return Err(RuntimeError::InvalidInput("invalid revision"));
    }
    for (i, source) in revision.sources.iter().enumerate() {
        if revision.sources[..i].contains(source)
            || !snapshot.sources.iter().any(|event| &event.source == source)
        {
            return Err(RuntimeError::InvalidInput("invalid revision"));
        }
    }
    if revision.kind == KnowledgeKind::Procedure
        && (revision.prerequisites.is_empty()
            || !snapshot.sources.iter().any(|event| {
                revision.sources.contains(&event.source) && event.outcome == WorkOutcome::Succeeded
            }))
    {
        return Err(RuntimeError::InvalidInput("invalid revision"));
    }
    Ok(())
}
fn encode_revision(revision: &Revision, job_id: &str, watermark_ms: u64) -> RevisionRecord {
    RevisionRecord {
        owner_id: revision.owner.to_string(),
        project_id: revision.project.to_string(),
        revision_id: revision.id.to_string(),
        memory_id: revision.memory.to_string(),
        job_id: job_id.into(),
        watermark_ms,
        kind: kind_name(&revision.kind).into(),
        status: "pending_review".into(),
        content: revision.content.clone(),
        prerequisites: revision.prerequisites.clone(),
        sources: revision.sources.iter().map(|id| id.to_string()).collect(),
        predecessor: revision.predecessor.as_ref().map(|id| id.to_string()),
        model: revision.model.clone(),
        prompt_version: revision.prompt_version.clone(),
    }
}
fn decode_revision(record: RevisionRecord) -> Result<Revision, RuntimeError> {
    if record.status != "pending_review" {
        return Err(RuntimeError::StorageUnavailable);
    }
    Ok(Revision {
        id: RevisionId::new(record.revision_id).map_err(|_| RuntimeError::StorageUnavailable)?,
        memory: MemoryId::new(record.memory_id).map_err(|_| RuntimeError::StorageUnavailable)?,
        owner: OwnerId::new(record.owner_id).map_err(|_| RuntimeError::StorageUnavailable)?,
        project: ProjectId::new(record.project_id).map_err(|_| RuntimeError::StorageUnavailable)?,
        kind: parse_kind(&record.kind)?,
        status: RevisionStatus::PendingReview,
        content: record.content,
        prerequisites: record.prerequisites,
        sources: record
            .sources
            .into_iter()
            .map(|id| SourceId::new(id).map_err(|_| RuntimeError::StorageUnavailable))
            .collect::<Result<_, _>>()?,
        predecessor: record
            .predecessor
            .map(|id| RevisionId::new(id).map_err(|_| RuntimeError::StorageUnavailable))
            .transpose()?,
        model: record.model,
        prompt_version: record.prompt_version,
    })
}
fn pair_ids(left: &str, right: &str) -> (String, String) {
    if left <= right {
        (left.into(), right.into())
    } else {
        (right.into(), left.into())
    }
}
fn predecessor_ok(prior: &[RevisionRecord], output: &RevisionRecord) -> Result<(), RuntimeError> {
    if prior
        .iter()
        .any(|old| old.revision_id == output.revision_id)
    {
        return Err(RuntimeError::Conflict);
    }
    if let Some(predecessor) = &output.predecessor {
        if !prior.iter().any(|old| {
            &old.revision_id == predecessor
                && old.memory_id == output.memory_id
                && old.project_id == output.project_id
                && old.kind == output.kind
                && old.watermark_ms <= output.watermark_ms
        }) {
            return Err(RuntimeError::InvalidInput("invalid predecessor"));
        }
    } else if prior
        .iter()
        .any(|old| old.memory_id == output.memory_id && old.project_id == output.project_id)
    {
        return Err(RuntimeError::InvalidInput(
            "memory lineage requires predecessor",
        ));
    }
    Ok(())
}

impl EmbeddedRuntimeStore {
    pub fn new(capture: EmbeddedCaptureStore) -> Self {
        Self {
            capture,
            activity_commit: None,
        }
    }

    pub async fn capture(
        &self,
        event: CaptureEvent,
        limits: &RuntimeLimits,
    ) -> Result<CaptureReceipt, RuntimeError> {
        validate_event(&event, limits)?;
        self.validate_activity_scope(&event.owner, &event.project)?;
        let source = event.source.clone();
        let wanted = SourceRecord::from(event);
        let _guard = self.capture.writes.lock().await;
        let tx = self.capture.db.clone().begin().await.map_err(storage)?;
        let mut response = tx.query(format!("SELECT {SOURCE_PROJECTION} FROM memory_source WHERE owner_id = $owner AND source_id = $source;"))
            .bind(("owner", wanted.owner_id.clone())).bind(("source", wanted.source_id.clone()))
            .await.map_err(storage)?.check().map_err(storage)?;
        let existing: Vec<SourceRecord> = response.take(0).map_err(storage)?;
        if let Some(old) = existing.first() {
            // Ignore incoming recorded_at; keep the first persisted timestamp.
            let error = if same_substance(old, &wanted) {
                None
            } else {
                Some(RuntimeError::Conflict)
            };
            tx.cancel().await.map_err(storage)?;
            return match error {
                None => Ok(CaptureReceipt {
                    source,
                    status: CaptureStatus::AlreadySavedPending,
                }),
                Some(error) => Err(error),
            };
        }
        // Action and legacy tool-result sources never count toward the backlog, so their capture
        // skips the count and cannot be refused by it.
        if wanted.kind != "action" && !wanted.origin.starts_with(TOOL_RESULT_ORIGIN) {
            let pending = self
                .pending_backlog(&tx, &wanted.owner_id, &wanted.project_id)
                .await?;
            if pending >= limits.max_pending {
                return cancel_err(tx, RuntimeError::BacklogFull).await;
            }
        }
        if tx
            .query("CREATE memory_source CONTENT $source RETURN NONE;")
            .bind(("source", wanted))
            .await
            .and_then(|response| response.check())
            .is_err()
        {
            let _ = tx.cancel().await;
            return Err(RuntimeError::StorageUnavailable);
        }
        self.finish_attached_activity(
            &tx,
            std::slice::from_ref(&source),
            &[],
            Some(crate::domain_models::runtime_diagnostics::ActivityCounts {
                sources_created: Some(1),
                ..Default::default()
            }),
        )
        .await?;
        tx.commit().await.map_err(storage)?;
        Ok(CaptureReceipt {
            source,
            status: CaptureStatus::SavedPending,
        })
    }

    /// Unconsumed originals of one project. The eligible source ids come from the
    /// `source_kind_scope` index (action episodes are never visited) and the consumed ids from
    /// `consumption_scope`; the anti-join is a hash set in memory, so it is O(n + m) instead of
    /// the O(n x m) `NOT IN` scan over the consumption array.
    async fn pending_backlog(
        &self,
        tx: &Transaction<Db>,
        owner: &str,
        project: &str,
    ) -> Result<usize, RuntimeError> {
        let mut response = tx.query(format!("SELECT VALUE source_id FROM memory_source WHERE owner_id = $owner AND project_id = $project AND kind IN [{BACKLOG_KINDS}] AND !string::starts_with(origin, 'omp://tool_result'); SELECT VALUE source_id FROM memory_source_consumption WHERE owner_id = $owner AND project_id = $project;"))
            .bind(("owner", owner.to_owned())).bind(("project", project.to_owned()))
            .await.map_err(storage)?.check().map_err(storage)?;
        let eligible: Vec<String> = response.take(0).map_err(storage)?;
        let consumed: Vec<String> = response.take(1).map_err(storage)?;
        let consumed: HashSet<&str> = consumed.iter().map(String::as_str).collect();
        Ok(eligible
            .iter()
            .filter(|id| !consumed.contains(id.as_str()))
            .count())
    }

    pub async fn pending_sources(
        &self,
        owner: &OwnerId,
        project: &ProjectId,
        limit: usize,
        skip_admitted: bool,
    ) -> Result<Vec<CaptureEvent>, RuntimeError> {
        self.eligible_sources(owner, project, limit, skip_admitted, None, None)
            .await
    }

    /// Retained `action` sources of a project, newest first, for procedural aggregation.
    pub async fn action_episodes(
        &self,
        owner: &OwnerId,
        project: &ProjectId,
        limit: usize,
    ) -> Result<Vec<CaptureEvent>, RuntimeError> {
        if limit == 0 || limit > 5000 {
            return Err(RuntimeError::InvalidInput("action episode bound"));
        }
        let mut response = self.capture.db.query(format!(
            "SELECT {SOURCE_PROJECTION} FROM memory_source WHERE owner_id = $owner AND project_id = $project AND kind = 'action' ORDER BY recorded_at_ms DESC, source_id DESC LIMIT $limit;"
        )).bind(("owner", owner.to_string())).bind(("project", project.to_string())).bind(("limit", limit))
            .await.map_err(storage)?.check().map_err(storage)?;
        let rows: Vec<SourceRecord> = response.take(0).map_err(storage)?;
        rows.into_iter().map(source_event).collect()
    }

    /// How many `action` sources the project retains, without reading their content.
    pub async fn action_episode_count(
        &self,
        owner: &OwnerId,
        project: &ProjectId,
    ) -> Result<u64, RuntimeError> {
        #[derive(Debug, SurrealValue)]
        struct Total {
            total: u64,
        }
        let mut response = self.capture.db.query("SELECT count() AS total FROM memory_source WHERE owner_id = $owner AND project_id = $project AND kind = 'action' GROUP ALL;")
            .bind(("owner", owner.to_string())).bind(("project", project.to_string()))
            .await.map_err(storage)?.check().map_err(storage)?;
        let rows: Vec<Total> = response.take(0).map_err(storage)?;
        Ok(rows.first().map_or(0, |row| row.total))
    }

    /// The active revision of the fixed playbook memory, if the project has one.
    pub async fn active_playbook(
        &self,
        owner: &OwnerId,
        project: &ProjectId,
    ) -> Result<Option<RuntimeRevision>, RuntimeError> {
        // Reads only: one snapshot transaction gives a consistent view without queueing behind
        // the writer lock, so a burst of captures cannot delay the session-start playbook read.
        let tx = self.capture.db.clone().begin().await.map_err(storage)?;
        let result = self.active_playbook_in(&tx, owner, project).await;
        tx.cancel().await.map_err(storage)?;
        result
    }

    async fn active_playbook_in(
        &self,
        tx: &Transaction<Db>,
        owner: &OwnerId,
        project: &ProjectId,
    ) -> Result<Option<RuntimeRevision>, RuntimeError> {
        let Some(active) = self
            .current_active(
                tx,
                owner.as_str(),
                project.as_str(),
                crate::domain_models::procedural::PLAYBOOK_MEMORY,
            )
            .await?
        else {
            return Ok(None);
        };
        let Some(record) = self.revision_row(tx, owner.as_str(), &active).await? else {
            return Ok(None);
        };
        let (states, jobs) = self
            .selected_metadata_in(tx, owner, std::iter::once(&record))
            .await?;
        let item = self.assemble(record, &states, &jobs)?;
        Ok((item.state == LifecycleState::Active).then_some(item))
    }

    /// Pending originals recorded within `[from_ms, to_ms]`. The window is applied before the
    /// limit, so expired or future-dated rows cannot starve eligible ones.
    pub async fn eligible_sources(
        &self,
        owner: &OwnerId,
        project: &ProjectId,
        limit: usize,
        skip_admitted: bool,
        from_ms: Option<u64>,
        to_ms: Option<u64>,
    ) -> Result<Vec<CaptureEvent>, RuntimeError> {
        if limit == 0 || limit > 32 {
            return Err(RuntimeError::InvalidInput("pending batch bound"));
        }
        let mut response = self.capture.db.query(format!(
            "LET $consumed = (SELECT VALUE source_id FROM memory_source_consumption WHERE owner_id = $owner AND project_id = $project); LET $admitted = IF $skip THEN array::distinct(array::flatten((SELECT VALUE sources FROM memory_runtime_operation WHERE owner_id = $owner AND project_id = $project AND string::starts_with(operation_id, 'dream-job:')))) ELSE [] END; SELECT {SOURCE_PROJECTION} FROM memory_source WHERE owner_id = $owner AND project_id = $project AND ($from = NONE OR recorded_at_ms >= $from) AND ($to = NONE OR recorded_at_ms <= $to) AND kind != 'action' AND !string::starts_with(origin, 'omp://tool_result') AND source_id NOT IN $consumed AND source_id NOT IN $admitted ORDER BY recorded_at_ms, source_id LIMIT $limit;"
        )).bind(("owner", owner.to_string())).bind(("project", project.to_string())).bind(("limit", limit)).bind(("skip", skip_admitted))
            .bind(("from", from_ms)).bind(("to", to_ms))
            .await.map_err(storage)?.check().map_err(storage)?;
        let originals: Vec<SourceRecord> = response.take(2).map_err(storage)?;
        originals.into_iter().map(source_event).collect()
    }

    pub async fn commit_batch(
        &self,
        snapshot: JobSnapshot,
        revisions: Vec<Revision>,
    ) -> Result<bool, RuntimeError> {
        validate_snapshot(&snapshot)?;
        self.validate_activity_scope(&snapshot.owner, &snapshot.project)?;
        let activity_links = self.activity_commit.as_ref().map(|_| {
            (
                snapshot
                    .sources
                    .iter()
                    .map(|s| s.source.clone())
                    .collect::<Vec<_>>(),
                revisions.iter().map(|r| r.id.clone()).collect::<Vec<_>>(),
            )
        });
        if revisions.is_empty() || revisions.len() > 16 {
            return Err(RuntimeError::InvalidInput("invalid revision"));
        }
        for (i, revision) in revisions.iter().enumerate() {
            validate_revision(&snapshot, revision)?;
            if revisions[..i]
                .iter()
                .any(|old| old.id == revision.id || old.memory == revision.memory)
            {
                return Err(RuntimeError::InvalidInput("invalid revision"));
            }
        }
        let _guard = self.capture.writes.lock().await;
        let tx = self.capture.db.clone().begin().await.map_err(storage)?;
        match self.commit_batch_tx(&tx, snapshot, revisions, true).await {
            Ok(created) => {
                if let Some((sources, revisions)) = &activity_links {
                    self.finish_attached_activity(
                        &tx,
                        sources,
                        revisions,
                        Some(crate::domain_models::runtime_diagnostics::ActivityCounts {
                            sources_consumed: Some(if created { sources.len() as u64 } else { 0 }),
                            revisions_created: Some(if created {
                                revisions.len() as u64
                            } else {
                                0
                            }),
                            ..Default::default()
                        }),
                    )
                    .await?;
                }
                tx.commit().await.map_err(storage)?;
                Ok(created)
            }
            Err(error) => {
                let _ = tx.cancel().await;
                Err(error)
            }
        }
    }

    async fn commit_batch_tx(
        &self,
        tx: &Transaction<Db>,
        snapshot: JobSnapshot,
        revisions: Vec<Revision>,
        consume: bool,
    ) -> Result<bool, RuntimeError> {
        let wanted = JobRecord {
            owner_id: snapshot.owner.to_string(),
            project_id: snapshot.project.to_string(),
            job_id: snapshot.id.to_string(),
            watermark_ms: snapshot.watermark_ms,
            sources: snapshot
                .sources
                .iter()
                .cloned()
                .map(SourceRecord::from)
                .collect(),
            status: "completed".into(),
            revisions: Vec::new(),
        };
        // Live originals, not job-embedded copies: purge + same ID recapture must fail stale replay.
        self.require_live_originals(tx, &wanted.owner_id, &wanted.sources)
            .await?;
        let source_ids: Vec<String> = wanted.sources.iter().map(|s| s.source_id.clone()).collect();
        let consumed = self
            .consumption_for(tx, &wanted.owner_id, &source_ids)
            .await?;
        let mut response = tx
            .query(format!(
                "SELECT {JOB_FIELDS} FROM memory_job WHERE owner_id = $owner AND job_id = $job;"
            ))
            .bind(("owner", wanted.owner_id.clone()))
            .bind(("job", wanted.job_id.clone()))
            .await
            .map_err(storage)?
            .check()
            .map_err(storage)?;
        let existing: Vec<JobRecord> = response.take(0).map_err(storage)?;
        let outputs: Vec<RevisionRecord> = revisions
            .iter()
            .map(|revision| encode_revision(revision, &wanted.job_id, wanted.watermark_ms))
            .collect();
        if let Some(old) = existing.first() {
            self.require_live_originals(tx, &wanted.owner_id, &old.sources)
                .await?;
            let same = old.owner_id == wanted.owner_id
                && old.project_id == wanted.project_id
                && old.job_id == wanted.job_id
                && old.watermark_ms == wanted.watermark_ms
                && old.sources == wanted.sources;
            if !same {
                return Err(RuntimeError::Conflict);
            }
            if old.status == "completed" {
                let consumption_matches = !consume
                    || (consumed.iter().all(|row| row.job_id == wanted.job_id)
                        && consumed.len() == source_ids.len());
                return if old.revisions == outputs && consumption_matches {
                    Ok(false)
                } else {
                    Err(RuntimeError::Conflict)
                };
            }
        }
        if consume && consumed.iter().any(|row| row.job_id != wanted.job_id) {
            return Err(RuntimeError::Conflict);
        }
        let ids = outputs
            .iter()
            .flat_map(|row| std::iter::once(row.revision_id.clone()).chain(row.predecessor.clone()))
            .collect::<Vec<_>>();
        let roots = outputs
            .iter()
            .filter(|row| row.predecessor.is_none())
            .map(|row| row.memory_id.clone())
            .collect::<Vec<_>>();
        let prior = self
            .lineage_rows(tx, &wanted.owner_id, &wanted.project_id, &ids, &roots)
            .await?;
        for output in &outputs {
            if existing.is_empty()
                && prior
                    .iter()
                    .any(|old| old.revision_id == output.revision_id)
            {
                return Err(RuntimeError::Conflict);
            }
            predecessor_ok(&prior, output)?;
        }
        if existing.is_empty() {
            let mut created = wanted.clone();
            created.revisions = outputs.clone();
            self.create_row(tx, "CREATE memory_job CONTENT $record RETURN NONE;", created).await?;
        } else if tx.query("UPDATE memory_job SET status = 'completed', revisions = $revisions WHERE owner_id = $owner AND job_id = $job RETURN NONE;")
            .bind(("revisions", outputs.clone())).bind(("owner", wanted.owner_id.clone())).bind(("job", wanted.job_id.clone()))
            .await.and_then(|response| response.check()).is_err() {
            return Err(RuntimeError::StorageUnavailable);
        }
        for output in &outputs {
            if !prior
                .iter()
                .any(|old| old.revision_id == output.revision_id)
            {
                self.create_row(
                    tx,
                    "CREATE memory_revision CONTENT $record RETURN NONE;",
                    output.clone(),
                )
                .await?;
            }
            self.ensure_state(
                tx,
                &StateRecord {
                    owner_id: output.owner_id.clone(),
                    project_id: output.project_id.clone(),
                    revision_id: output.revision_id.clone(),
                    memory_id: output.memory_id.clone(),
                    state: state_name(LifecycleState::Processing).into(),
                    reviewed_at_ms: None,
                    reviewed_by: None,
                },
                false,
            )
            .await?;
        }
        if consume {
            for source in &wanted.sources {
                if consumed.iter().all(|row| row.source_id != source.source_id) {
                    self.create_row(
                        tx,
                        "CREATE memory_source_consumption CONTENT $record RETURN NONE;",
                        ConsumptionRecord {
                            owner_id: wanted.owner_id.clone(),
                            project_id: wanted.project_id.clone(),
                            source_id: source.source_id.clone(),
                            job_id: wanted.job_id.clone(),
                        },
                    )
                    .await?;
                }
            }
        }
        Ok(true)
    }

    pub async fn propose(
        &self,
        owner: &OwnerId,
        actor: &ActorId,
        proposal: RevisionProposal,
        now_ms: u64,
    ) -> Result<RuntimeRevision, RuntimeError> {
        let _ = actor;
        if proposal.content.trim().is_empty()
            || proposal.sources.is_empty()
            || proposal.sources.len() > 32
            || proposal
                .sources
                .iter()
                .enumerate()
                .any(|(i, source)| proposal.sources[..i].contains(source))
        {
            return Err(RuntimeError::InvalidInput("invalid source-backed proposal"));
        }
        self.validate_activity_scope(owner, &proposal.project)?;
        let _guard = self.capture.writes.lock().await;
        let tx = self.capture.db.clone().begin().await.map_err(storage)?;
        match self.propose_tx(&tx, owner, proposal, now_ms).await {
            Ok((revision, created)) => {
                let revision_id = RevisionId::new(revision.clone()).map_err(storage)?;
                self.finish_attached_activity(
                    &tx,
                    &[],
                    std::slice::from_ref(&revision_id),
                    Some(crate::domain_models::runtime_diagnostics::ActivityCounts {
                        revisions_created: Some(u64::from(created)),
                        ..Default::default()
                    }),
                )
                .await?;
                tx.commit().await.map_err(storage)?;
                self.hydrate(&self.capture.db, owner, &revision).await
            }
            Err(error) => {
                let _ = tx.cancel().await;
                Err(error)
            }
        }
    }

    async fn propose_tx(
        &self,
        tx: &Transaction<Db>,
        owner: &OwnerId,
        proposal: RevisionProposal,
        now_ms: u64,
    ) -> Result<(String, bool), RuntimeError> {
        let mut events = Vec::new();
        for source in &proposal.sources {
            let rows = self.source_row(tx, owner.as_str(), source.as_str()).await?;
            let event = source_event(rows.into_iter().next().ok_or(RuntimeError::Missing)?)?;
            if event.owner != *owner || event.project != proposal.project {
                return Err(RuntimeError::Unauthorized);
            }
            events.push(event);
        }
        // Native snapshot is built from stored originals. now_ms keeps successor watermarks at/after the parent job.
        let watermark_ms = now_ms.max(
            events
                .iter()
                .map(|event| event.recorded_at_ms)
                .max()
                .unwrap_or(now_ms),
        );
        let ids = std::iter::once(proposal.revision.to_string())
            .chain(proposal.predecessor.as_ref().map(|id| id.to_string()))
            .collect::<Vec<_>>();
        let roots = if proposal.predecessor.is_none() {
            vec![proposal.memory.to_string()]
        } else {
            Vec::new()
        };
        let prior = self
            .lineage_rows(tx, owner.as_str(), proposal.project.as_str(), &ids, &roots)
            .await?;
        if let Some(existing) = prior
            .iter()
            .find(|old| old.revision_id == proposal.revision.as_str())
        {
            let same = existing.owner_id == owner.as_str()
                && existing.project_id == proposal.project.as_str()
                && existing.memory_id == proposal.memory.as_str()
                && existing.content == proposal.content
                && existing.sources
                    == proposal
                        .sources
                        .iter()
                        .map(|id| id.to_string())
                        .collect::<Vec<_>>()
                && existing.predecessor.as_deref()
                    == proposal.predecessor.as_ref().map(|id| id.as_str());
            return if same {
                Ok((proposal.revision.to_string(), false))
            } else {
                Err(RuntimeError::Conflict)
            };
        }
        let (kind, prerequisites) = if let Some(predecessor) = &proposal.predecessor {
            let parent = prior
                .iter()
                .find(|old| old.revision_id == predecessor.as_str())
                .ok_or(RuntimeError::Missing)?;
            if parent.owner_id != owner.as_str()
                || parent.project_id != proposal.project.as_str()
                || parent.memory_id != proposal.memory.as_str()
            {
                return Err(RuntimeError::InvalidInput("invalid predecessor"));
            }
            (parse_kind(&parent.kind)?, parent.prerequisites.clone())
        } else {
            (KnowledgeKind::Knowledge, Vec::new())
        };
        // Internal job keys must not shorten the caller's valid revision-ID bound.
        let job_id = JobId::new(format!(
            "native-job:{:x}",
            Sha256::digest(proposal.revision.as_str().as_bytes())
        ))
        .map_err(|_| RuntimeError::InvalidInput("native job identity"))?;
        let snapshot = JobSnapshot {
            id: job_id,
            owner: owner.clone(),
            project: proposal.project.clone(),
            watermark_ms,
            sources: events,
        };
        let revision = Revision {
            id: proposal.revision.clone(),
            memory: proposal.memory,
            owner: owner.clone(),
            project: proposal.project,
            kind,
            status: RevisionStatus::PendingReview,
            content: proposal.content,
            prerequisites,
            sources: proposal.sources,
            predecessor: proposal.predecessor,
            model: "native".into(),
            prompt_version: "native-v1".into(),
        };
        validate_snapshot(&snapshot)?;
        validate_revision(&snapshot, &revision)?;
        let output = encode_revision(&revision, snapshot.id.as_str(), snapshot.watermark_ms);
        predecessor_ok(&prior, &output)?;
        self.commit_batch_tx(tx, snapshot, vec![revision], false)
            .await?;
        let ids = output.sources.clone();
        let consumed = self.consumption_for(tx, owner.as_str(), &ids).await?;
        for source in &ids {
            if consumed.iter().any(|row| &row.source_id == source) {
                continue;
            }
            self.create_row(
                tx,
                "CREATE memory_source_consumption CONTENT $record RETURN NONE;",
                ConsumptionRecord {
                    owner_id: owner.to_string(),
                    project_id: output.project_id.clone(),
                    source_id: source.clone(),
                    job_id: output.job_id.clone(),
                },
            )
            .await?;
        }
        Ok((output.revision_id, true))
    }

    pub async fn review(
        &self,
        owner: &OwnerId,
        actor: &ActorId,
        request: &ReviewRequest,
        now_ms: u64,
    ) -> Result<RuntimeRevision, RuntimeError> {
        if request.action != ReviewAction::Promote && request.expected_active.is_some() {
            return Err(RuntimeError::InvalidInput(
                "active guard is only valid for promotion",
            ));
        }
        if (request.action == ReviewAction::Conflict) != request.conflict_with.is_some() {
            return Err(RuntimeError::InvalidInput(
                "conflict requires exactly one counterpart",
            ));
        }
        self.validate_activity_scope(owner, &request.project)?;
        let _guard = self.capture.writes.lock().await;
        let tx = self.capture.db.clone().begin().await.map_err(storage)?;
        let was_active =
            if self.activity_commit.is_some() && request.action == ReviewAction::Promote {
                self.state_row(&tx, owner.as_str(), request.revision.as_str())
                    .await?
                    .is_some_and(|state| state.state == "active")
            } else {
                false
            };
        match self.review_tx(&tx, owner, actor, request, now_ms).await {
            Ok(revision) => {
                let counts = if request.action == ReviewAction::Promote {
                    Some(crate::domain_models::runtime_diagnostics::ActivityCounts {
                        revisions_published: Some(u64::from(!was_active)),
                        ..Default::default()
                    })
                } else {
                    None
                };
                self.finish_attached_activity(
                    &tx,
                    &[],
                    std::slice::from_ref(&request.revision),
                    counts,
                )
                .await?;
                tx.commit().await.map_err(storage)?;
                self.hydrate(&self.capture.db, owner, &revision).await
            }
            Err(error) => {
                let _ = tx.cancel().await;
                Err(error)
            }
        }
    }

    /// Retires a processing successor after another revision has taken its publication slot.
    pub async fn retire_stale(
        &self,
        owner: &OwnerId,
        actor: &ActorId,
        project: &ProjectId,
        revision: &RevisionId,
        now_ms: u64,
    ) -> Result<bool, RuntimeError> {
        self.validate_activity_scope(owner, project)?;
        let _guard = self.capture.writes.lock().await;
        let tx = self.capture.db.clone().begin().await.map_err(storage)?;
        let result = async {
            let Some(record) = self
                .revision_row(&tx, owner.as_str(), revision.as_str())
                .await?
            else {
                return Ok(false);
            };
            if record.owner_id != owner.as_str() || record.project_id != project.as_str() {
                return Err(RuntimeError::Unauthorized);
            }
            let state =
                |row: Option<StateRecord>| row.map(|row| parse_state(&row.state)).transpose();
            let from = state(
                self.state_row(&tx, owner.as_str(), &record.revision_id)
                    .await?,
            )?
            .unwrap_or(LifecycleState::Processing);
            let Some(parent) = record.predecessor.as_deref() else {
                return Ok(false);
            };
            let parent_state = state(self.state_row(&tx, owner.as_str(), parent).await?)?;
            let lost_slot = match parent_state {
                Some(LifecycleState::Superseded) => true,
                Some(LifecycleState::Rejected) => self
                    .current_active(&tx, owner.as_str(), project.as_str(), &record.memory_id)
                    .await?
                    .is_some(),
                _ => false,
            };
            if from != LifecycleState::Processing || !lost_slot {
                return Ok(false);
            }
            self.reject(&tx, owner, actor, &record, from, now_ms)
                .await?;
            Ok(true)
        }
        .await;
        match result {
            Ok(true) => {
                tx.commit().await.map_err(storage)?;
                Ok(true)
            }
            other => {
                let _ = tx.cancel().await;
                other
            }
        }
    }

    async fn review_tx(
        &self,
        tx: &Transaction<Db>,
        owner: &OwnerId,
        actor: &ActorId,
        request: &ReviewRequest,
        now_ms: u64,
    ) -> Result<String, RuntimeError> {
        let record = self
            .revision_row(tx, owner.as_str(), request.revision.as_str())
            .await?
            .ok_or(RuntimeError::Missing)?;
        if record.owner_id != owner.as_str() || record.project_id != request.project.as_str() {
            return Err(RuntimeError::Unauthorized);
        }
        for source in &record.sources {
            if self
                .source_row(tx, owner.as_str(), source)
                .await?
                .is_empty()
            {
                return Err(RuntimeError::Missing);
            }
        }
        let current = self
            .state_row(tx, owner.as_str(), record.revision_id.as_str())
            .await?;
        let from = current
            .as_ref()
            .map(|row| parse_state(&row.state))
            .transpose()?
            .unwrap_or(LifecycleState::Processing);
        let head = self
            .current_active(
                tx,
                owner.as_str(),
                record.project_id.as_str(),
                record.memory_id.as_str(),
            )
            .await?;
        let expected = request.expected_active.as_ref().map(|id| id.to_string());
        match request.action {
            ReviewAction::Promote => {
                self.promote(
                    tx,
                    owner,
                    actor,
                    &record,
                    from,
                    head,
                    expected.as_deref(),
                    now_ms,
                )
                .await?
            }
            ReviewAction::Reject => self.reject(tx, owner, actor, &record, from, now_ms).await?,
            ReviewAction::Conflict => {
                let other = request
                    .conflict_with
                    .as_ref()
                    .ok_or(RuntimeError::InvalidInput(
                        "conflict requires exactly one counterpart",
                    ))?;
                self.conflict(
                    tx,
                    owner,
                    actor,
                    &record,
                    other.as_str(),
                    from,
                    head.as_deref(),
                    now_ms,
                )
                .await?;
            }
        }
        Ok(record.revision_id.clone())
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "Explicit transaction, review provenance and CAS guard"
    )]
    async fn promote(
        &self,
        tx: &Transaction<Db>,
        owner: &OwnerId,
        actor: &ActorId,
        record: &RevisionRecord,
        from: LifecycleState,
        head: Option<String>,
        expected: Option<&str>,
        now_ms: u64,
    ) -> Result<(), RuntimeError> {
        if from == LifecycleState::Active {
            if head.as_deref() != Some(record.revision_id.as_str()) {
                return Err(RuntimeError::Conflict);
            }
            let mut response = tx.query("SELECT owner_id, project_id, revision_id, actor_id, action, from_state, to_state, expected_active, conflict_with, at_ms FROM memory_lifecycle_audit WHERE owner_id = $owner AND project_id = $project AND revision_id = $revision AND action = 'promote' ORDER BY at_ms DESC LIMIT 1;")
                .bind(("owner", owner.to_string())).bind(("project", record.project_id.clone()))
                .bind(("revision", record.revision_id.clone())).await.map_err(storage)?.check().map_err(storage)?;
            let audits: Vec<AuditRecord> = response.take(0).map_err(storage)?;
            return if audits
                .first()
                .is_some_and(|audit| audit.expected_active.as_deref() == expected)
            {
                Ok(())
            } else {
                Err(RuntimeError::Conflict)
            };
        }
        if let Some(pair) = self
            .open_conflict(
                tx,
                owner.as_str(),
                record.project_id.as_str(),
                record.revision_id.as_str(),
            )
            .await?
        {
            return self
                .resolve(tx, owner, actor, record, pair, from, head, expected, now_ms)
                .await;
        }
        if !matches!(
            from,
            LifecycleState::Processing | LifecycleState::Conflicted
        ) {
            return Err(RuntimeError::Conflict);
        }
        if expected != head.as_deref() {
            return Err(RuntimeError::Conflict);
        }
        if record.predecessor.as_deref() != head.as_deref() {
            let Some(parent) = record.predecessor.as_deref() else {
                return Err(RuntimeError::Conflict);
            };
            let parent_state = self.state_row(tx, owner.as_str(), parent).await?;
            if head.is_some() || parent_state.is_none_or(|state| state.state != "rejected") {
                return Err(RuntimeError::Conflict);
            }
        }
        if let Some(previous) = &head {
            self.write_state(
                tx,
                owner.as_str(),
                record.project_id.as_str(),
                record.memory_id.as_str(),
                previous,
                LifecycleState::Superseded,
                Some(now_ms),
                Some(actor.as_str()),
            )
            .await?;
        }
        self.write_state(
            tx,
            owner.as_str(),
            record.project_id.as_str(),
            record.memory_id.as_str(),
            record.revision_id.as_str(),
            LifecycleState::Active,
            Some(now_ms),
            Some(actor.as_str()),
        )
        .await?;
        self.set_head(
            tx,
            owner.as_str(),
            record.project_id.as_str(),
            record.memory_id.as_str(),
            record.revision_id.as_str(),
        )
        .await?;
        self.audit(
            tx,
            owner,
            actor,
            record,
            from,
            LifecycleState::Active,
            ReviewAction::Promote,
            expected,
            None,
            now_ms,
        )
        .await
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "Explicit transaction, review provenance and CAS guard"
    )]
    async fn resolve(
        &self,
        tx: &Transaction<Db>,
        owner: &OwnerId,
        actor: &ActorId,
        record: &RevisionRecord,
        pair: ConflictRecord,
        from: LifecycleState,
        head: Option<String>,
        expected: Option<&str>,
        now_ms: u64,
    ) -> Result<(), RuntimeError> {
        let counterpart = if pair.left_id == record.revision_id {
            pair.right_id.clone()
        } else {
            pair.left_id.clone()
        };
        if from != LifecycleState::Conflicted {
            return Err(RuntimeError::Conflict);
        }
        if expected != head.as_deref() {
            return Err(RuntimeError::Conflict);
        }
        if let Some(previous) = &head {
            self.write_state(
                tx,
                owner.as_str(),
                record.project_id.as_str(),
                record.memory_id.as_str(),
                previous,
                LifecycleState::Superseded,
                Some(now_ms),
                Some(actor.as_str()),
            )
            .await?;
        }
        self.write_state(
            tx,
            owner.as_str(),
            record.project_id.as_str(),
            record.memory_id.as_str(),
            record.revision_id.as_str(),
            LifecycleState::Active,
            Some(now_ms),
            Some(actor.as_str()),
        )
        .await?;
        let other = self
            .revision_row(tx, owner.as_str(), &counterpart)
            .await?
            .ok_or(RuntimeError::Missing)?;
        self.write_state(
            tx,
            owner.as_str(),
            other.project_id.as_str(),
            other.memory_id.as_str(),
            other.revision_id.as_str(),
            LifecycleState::Rejected,
            Some(now_ms),
            Some(actor.as_str()),
        )
        .await?;
        if tx.query("UPDATE memory_revision_conflict SET resolved = true, selected_id = $selected WHERE owner_id = $owner AND project_id = $project AND left_id = $left AND right_id = $right RETURN NONE;")
            .bind(("selected", record.revision_id.clone())).bind(("owner", owner.to_string()))
            .bind(("project", record.project_id.clone())).bind(("left", pair.left_id.clone())).bind(("right", pair.right_id.clone()))
            .await.and_then(|response| response.check()).is_err() {
            return Err(RuntimeError::StorageUnavailable);
        }
        self.set_head(
            tx,
            owner.as_str(),
            record.project_id.as_str(),
            record.memory_id.as_str(),
            record.revision_id.as_str(),
        )
        .await?;
        self.audit(
            tx,
            owner,
            actor,
            record,
            from,
            LifecycleState::Active,
            ReviewAction::Promote,
            expected,
            Some(&counterpart),
            now_ms,
        )
        .await
    }

    async fn reject(
        &self,
        tx: &Transaction<Db>,
        owner: &OwnerId,
        actor: &ActorId,
        record: &RevisionRecord,
        from: LifecycleState,
        now_ms: u64,
    ) -> Result<(), RuntimeError> {
        if from == LifecycleState::Rejected {
            return Ok(());
        }
        if from == LifecycleState::Active {
            return Err(RuntimeError::InvalidInput("cannot reject active"));
        }
        if from == LifecycleState::Superseded {
            return Err(RuntimeError::Conflict);
        }
        self.write_state(
            tx,
            owner.as_str(),
            record.project_id.as_str(),
            record.memory_id.as_str(),
            record.revision_id.as_str(),
            LifecycleState::Rejected,
            Some(now_ms),
            Some(actor.as_str()),
        )
        .await?;
        self.audit(
            tx,
            owner,
            actor,
            record,
            from,
            LifecycleState::Rejected,
            ReviewAction::Reject,
            None,
            None,
            now_ms,
        )
        .await
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "Explicit transaction and both source-backed conflict identities"
    )]
    async fn conflict(
        &self,
        tx: &Transaction<Db>,
        owner: &OwnerId,
        actor: &ActorId,
        record: &RevisionRecord,
        other_id: &str,
        from: LifecycleState,
        head: Option<&str>,
        now_ms: u64,
    ) -> Result<(), RuntimeError> {
        if other_id == record.revision_id {
            return Err(RuntimeError::InvalidInput(
                "conflict requires a distinct counterpart",
            ));
        }
        let other = self
            .revision_row(tx, owner.as_str(), other_id)
            .await?
            .ok_or(RuntimeError::Missing)?;
        if other.project_id != record.project_id
            || other.memory_id != record.memory_id
            || other.kind != record.kind
        {
            return Err(RuntimeError::InvalidInput(
                "conflict requires the same memory",
            ));
        }
        for source in &other.sources {
            if self
                .source_row(tx, owner.as_str(), source)
                .await?
                .is_empty()
            {
                return Err(RuntimeError::Missing);
            }
        }
        let (left, right) = pair_ids(&record.revision_id, other_id);
        let existing = self
            .conflict_row(
                tx,
                owner.as_str(),
                record.project_id.as_str(),
                &left,
                &right,
            )
            .await?;
        if let Some(pair) = &existing {
            if !pair.resolved && from == LifecycleState::Conflicted {
                return Ok(());
            }
            if pair.resolved {
                return Err(RuntimeError::Conflict);
            }
        }
        let other_state = self
            .state_row(tx, owner.as_str(), other_id)
            .await?
            .map(|row| parse_state(&row.state))
            .transpose()?
            .unwrap_or(LifecycleState::Processing);
        if !matches!(
            from,
            LifecycleState::Active | LifecycleState::Processing | LifecycleState::Conflicted
        ) || !matches!(
            other_state,
            LifecycleState::Active | LifecycleState::Processing | LifecycleState::Conflicted
        ) || self
            .open_conflict(tx, owner.as_str(), &record.project_id, &record.revision_id)
            .await?
            .is_some()
            || self
                .open_conflict(tx, owner.as_str(), &record.project_id, other_id)
                .await?
                .is_some()
        {
            return Err(RuntimeError::Conflict);
        }
        self.write_state(
            tx,
            owner.as_str(),
            record.project_id.as_str(),
            record.memory_id.as_str(),
            record.revision_id.as_str(),
            LifecycleState::Conflicted,
            Some(now_ms),
            Some(actor.as_str()),
        )
        .await?;
        self.write_state(
            tx,
            owner.as_str(),
            other.project_id.as_str(),
            other.memory_id.as_str(),
            other.revision_id.as_str(),
            LifecycleState::Conflicted,
            Some(now_ms),
            Some(actor.as_str()),
        )
        .await?;
        if existing.is_none() {
            self.create_row(
                tx,
                "CREATE memory_revision_conflict CONTENT $record RETURN NONE;",
                ConflictRecord {
                    owner_id: owner.to_string(),
                    project_id: record.project_id.clone(),
                    memory_id: record.memory_id.clone(),
                    left_id: left,
                    right_id: right,
                    resolved: false,
                    selected_id: None,
                },
            )
            .await?;
        }
        if head == Some(record.revision_id.as_str()) || head == Some(other_id) {
            self.clear_head(
                tx,
                owner.as_str(),
                record.project_id.as_str(),
                record.memory_id.as_str(),
            )
            .await?;
        }
        self.audit(
            tx,
            owner,
            actor,
            record,
            from,
            LifecycleState::Conflicted,
            ReviewAction::Conflict,
            None,
            Some(other_id),
            now_ms,
        )
        .await
    }

    pub async fn list(
        &self,
        owner: &OwnerId,
        project: &ProjectId,
        states: &[LifecycleState],
        limit: usize,
    ) -> Result<Vec<RuntimeRevision>, RuntimeError> {
        if limit == 0 || limit > 32 || states.is_empty() || states.len() > 5 {
            return Err(RuntimeError::InvalidInput("review list bounds"));
        }
        let _guard = self.capture.writes.lock().await;
        // SurrealQL row predicates re-evaluate subqueries; materialize scoped sets once.
        let mut response = self.capture.db.query(format!(
            "LET $selected = (SELECT VALUE revision_id FROM memory_revision_state WHERE owner_id = $owner AND project_id = $project AND state IN $states); LET $known = IF 'processing' IN $states THEN (SELECT VALUE revision_id FROM memory_revision_state WHERE owner_id = $owner AND project_id = $project) ELSE [] END; SELECT {REV_FIELDS} FROM memory_revision WHERE owner_id = $owner AND project_id = $project AND (revision_id IN $selected OR ('processing' IN $states AND revision_id NOT IN $known)) ORDER BY watermark_ms, revision_id LIMIT $limit;"
        )).bind(("owner", owner.to_string())).bind(("project", project.to_string()))
            .bind(("states", states.iter().map(|state| state_name(*state).to_owned()).collect::<Vec<_>>()))
            .bind(("limit", limit)).await.map_err(storage)?.check().map_err(storage)?;
        let revisions: Vec<RevisionRecord> = response.take(2).map_err(storage)?;
        let (state_rows, jobs) = self.selected_metadata(owner, revisions.iter()).await?;
        revisions
            .into_iter()
            .map(|record| self.assemble(record, &state_rows, &jobs))
            .collect()
    }

    /// Active revisions, excluding the playbook, whose every source is a legacy tool-metadata one.
    pub async fn tool_metadata_revisions(
        &self,
        owner: &OwnerId,
        project: &ProjectId,
        limit: usize,
    ) -> Result<Vec<RevisionId>, RuntimeError> {
        if limit == 0 || limit > 32 {
            return Err(RuntimeError::InvalidInput("review list bounds"));
        }
        self.tool_metadata_ids(owner, project, Some(limit)).await
    }

    /// Operator-only: moves active legacy tool-metadata revisions to `Rejected` and clears their
    /// head in one transaction. Each revision is re-checked; ineligible ones are skipped, not
    /// counted. Distinct from `review`, whose "cannot reject active" rule is unchanged.
    pub async fn retire_tool_metadata(
        &self,
        owner: &OwnerId,
        actor: &ActorId,
        project: &ProjectId,
        revisions: &[RevisionId],
        now_ms: u64,
    ) -> Result<usize, RuntimeError> {
        if revisions.is_empty() || revisions.len() > 32 {
            return Err(RuntimeError::InvalidInput("retire batch bounds"));
        }
        self.validate_activity_scope(owner, project)?;
        let _guard = self.capture.writes.lock().await;
        let tx = self.capture.db.clone().begin().await.map_err(storage)?;
        match self
            .retire_tool_metadata_tx(&tx, owner, actor, project, revisions, now_ms)
            .await
        {
            Ok(retired) => {
                tx.commit().await.map_err(storage)?;
                Ok(retired)
            }
            Err(error) => {
                let _ = tx.cancel().await;
                Err(error)
            }
        }
    }
    async fn retire_tool_metadata_tx(
        &self,
        tx: &Transaction<Db>,
        owner: &OwnerId,
        actor: &ActorId,
        project: &ProjectId,
        revisions: &[RevisionId],
        now_ms: u64,
    ) -> Result<usize, RuntimeError> {
        let mut retired = 0;
        for id in revisions {
            let Some(record) = self.revision_row(tx, owner.as_str(), id.as_str()).await? else {
                continue;
            };
            if record.project_id != project.as_str()
                || record.memory_id == crate::domain_models::procedural::PLAYBOOK_MEMORY
                || record.sources.is_empty()
            {
                continue;
            }
            let active = self
                .state_row(tx, owner.as_str(), &record.revision_id)
                .await?
                .is_some_and(|state| state.state == "active");
            if !active {
                continue;
            }
            let mut legacy = true;
            for source in &record.sources {
                let rows = self.source_row(tx, owner.as_str(), source).await?;
                legacy &= rows.first().is_some_and(|row| {
                    row.origin
                        .starts_with(crate::domain_models::procedural::LEGACY_TOOL_ORIGIN)
                });
            }
            if !legacy {
                continue;
            }
            self.write_state(
                tx,
                owner.as_str(),
                &record.project_id,
                &record.memory_id,
                &record.revision_id,
                LifecycleState::Rejected,
                Some(now_ms),
                Some(actor.as_str()),
            )
            .await?;
            if self
                .current_active(tx, owner.as_str(), &record.project_id, &record.memory_id)
                .await?
                .as_deref()
                == Some(record.revision_id.as_str())
            {
                self.clear_head(tx, owner.as_str(), &record.project_id, &record.memory_id)
                    .await?;
            }
            self.create_row(
                tx,
                "CREATE memory_lifecycle_audit CONTENT $record RETURN NONE;",
                AuditRecord {
                    owner_id: owner.to_string(),
                    project_id: record.project_id.clone(),
                    revision_id: record.revision_id.clone(),
                    actor_id: actor.to_string(),
                    action: RETIRE_ACTION.into(),
                    from_state: state_name(LifecycleState::Active).into(),
                    to_state: state_name(LifecycleState::Rejected).into(),
                    expected_active: None,
                    conflict_with: None,
                    at_ms: now_ms,
                },
            )
            .await?;
            retired += 1;
        }
        Ok(retired)
    }

    /// Total number of revisions `tool_metadata_revisions` would eventually return (dry-run report).
    pub async fn tool_metadata_count(
        &self,
        owner: &OwnerId,
        project: &ProjectId,
    ) -> Result<usize, RuntimeError> {
        Ok(self.tool_metadata_ids(owner, project, None).await?.len())
    }

    async fn tool_metadata_ids(
        &self,
        owner: &OwnerId,
        project: &ProjectId,
        limit: Option<usize>,
    ) -> Result<Vec<RevisionId>, RuntimeError> {
        let limit_clause = if limit.is_some() { " LIMIT $limit" } else { "" };
        let _guard = self.capture.writes.lock().await;
        // SurrealQL row predicates re-evaluate subqueries; materialize scoped sets once.
        let mut response = self.capture.db.query(format!(
            "LET $active = (SELECT VALUE revision_id FROM memory_revision_state WHERE owner_id = $owner AND project_id = $project AND state = 'active'); LET $legacy = (SELECT VALUE source_id FROM memory_source WHERE owner_id = $owner AND project_id = $project AND string::starts_with(origin, $origin)); SELECT VALUE revision_id FROM memory_revision WHERE owner_id = $owner AND project_id = $project AND memory_id != $playbook AND revision_id IN $active AND array::len(sources) > 0 AND sources ALLINSIDE $legacy ORDER BY watermark_ms, revision_id{limit_clause};"
        )).bind(("owner", owner.to_string())).bind(("project", project.to_string()))
            .bind(("origin", crate::domain_models::procedural::LEGACY_TOOL_ORIGIN))
            .bind(("playbook", crate::domain_models::procedural::PLAYBOOK_MEMORY))
            .bind(("limit", limit.unwrap_or(0))).await.map_err(storage)?.check().map_err(storage)?;
        let ids: Vec<String> = response.take(2).map_err(storage)?;
        ids.into_iter()
            .map(|id| RevisionId::new(id).map_err(|_| RuntimeError::StorageUnavailable))
            .collect()
    }

    /// Keyset page on `(watermark_ms, revision_id)`, optionally one memory's lineage, with its
    /// lifecycle audit when `with_audit` is set. `list` stays the unpaged internal listing.
    pub async fn list_page(
        &self,
        owner: &OwnerId,
        query: &ListQuery,
        with_audit: bool,
    ) -> Result<ListPage, RuntimeError> {
        if query.limit == 0 || query.limit > 32 || query.states.is_empty() || query.states.len() > 5
        {
            return Err(RuntimeError::InvalidInput("review list bounds"));
        }
        let _guard = self.capture.writes.lock().await;
        let memory = if query.memory.is_some() {
            " AND memory_id = $memory"
        } else {
            ""
        };
        let after = if query.after.is_some() {
            " AND (watermark_ms > $after_ms OR (watermark_ms = $after_ms AND revision_id > $after_id))"
        } else {
            ""
        };
        let (after_ms, after_id) = query
            .after
            .as_ref()
            .map_or((0, String::new()), |(ms, id)| (*ms, id.to_string()));
        // SurrealQL row predicates re-evaluate subqueries; materialize scoped sets once.
        let mut response = self.capture.db.query(format!(
            "LET $selected = (SELECT VALUE revision_id FROM memory_revision_state WHERE owner_id = $owner AND project_id = $project AND state IN $states); LET $known = IF 'processing' IN $states THEN (SELECT VALUE revision_id FROM memory_revision_state WHERE owner_id = $owner AND project_id = $project) ELSE [] END; SELECT {REV_FIELDS} FROM memory_revision WHERE owner_id = $owner AND project_id = $project{memory}{after} AND (revision_id IN $selected OR ('processing' IN $states AND revision_id NOT IN $known)) ORDER BY watermark_ms, revision_id LIMIT $limit;"
        )).bind(("owner", owner.to_string())).bind(("project", query.project.to_string()))
            .bind(("states", query.states.iter().map(|state| state_name(*state).to_owned()).collect::<Vec<_>>()))
            .bind(("memory", query.memory.as_ref().map(|m| m.to_string()).unwrap_or_default()))
            .bind(("after_ms", after_ms)).bind(("after_id", after_id))
            .bind(("limit", query.limit + 1)).await.map_err(storage)?.check().map_err(storage)?;
        let mut revisions: Vec<RevisionRecord> = response.take(2).map_err(storage)?;
        let more = revisions.len() > query.limit;
        revisions.truncate(query.limit);
        let audit = if with_audit && !revisions.is_empty() {
            let ids: Vec<String> = revisions.iter().map(|r| r.revision_id.clone()).collect();
            let mut response = self.capture.db
                .query("SELECT revision_id, action, from_state, to_state, expected_active, conflict_with, at_ms FROM memory_lifecycle_audit WHERE owner_id = $owner AND project_id = $project AND revision_id IN $ids ORDER BY at_ms, revision_id;")
                .bind(("owner", owner.to_string())).bind(("project", query.project.to_string()))
                .bind(("ids", ids)).await.map_err(storage)?.check().map_err(storage)?;
            let rows: Vec<AuditRow> = response.take(0).map_err(storage)?;
            rows.into_iter()
                .map(audit_entry)
                .collect::<Result<Vec<_>, _>>()?
        } else {
            Vec::new()
        };
        let (state_rows, jobs) = self.selected_metadata(owner, revisions.iter()).await?;
        let items = revisions
            .into_iter()
            .map(|record| self.assemble(record, &state_rows, &jobs))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(ListPage { items, audit, more })
    }

    pub async fn recall(
        &self,
        owner: &OwnerId,
        projects: &[ProjectId],
        terms: &[String],
        limit: usize,
    ) -> Result<Vec<RuntimeHit>, RuntimeError> {
        if limit == 0
            || limit > 32
            || projects.is_empty()
            || projects.len() > 16
            || terms.is_empty()
            || terms.len() > 12
            || terms.iter().any(|term| term.is_empty() || term.len() > 256)
        {
            return Err(RuntimeError::InvalidInput("recall bounds"));
        }
        let _guard = self.capture.writes.lock().await;
        let mut normalized = terms
            .iter()
            .map(|term| term.trim().to_lowercase())
            .collect::<Vec<_>>();
        normalized.sort();
        normalized.dedup();
        let predicates = (0..normalized.len())
            .map(|i| format!("string::lowercase(content) CONTAINS $term{i}"))
            .collect::<Vec<_>>();
        let scores = predicates
            .iter()
            .map(|term| format!("(IF {term} THEN 1 ELSE 0 END)"))
            .collect::<Vec<_>>();
        let object = REV_FIELDS
            .split(", ")
            .map(|field| format!("{field}: {field}"))
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "LET $active = (SELECT VALUE revision_id FROM memory_revision_state WHERE owner_id = $owner AND project_id IN $projects AND state = 'active' AND memory_id != $playbook); SELECT {{{object}}} AS revision, {} AS lexical_score, memory_id AS memory_key, revision_id AS revision_key FROM memory_revision WHERE owner_id = $owner AND project_id IN $projects AND revision_id IN $active AND ({}) ORDER BY lexical_score DESC, memory_key, revision_key LIMIT $limit;",
            scores.join(" + "),
            predicates.join(" OR ")
        );
        let mut query = self
            .capture
            .db
            .query(sql)
            .bind(("owner", owner.to_string()))
            .bind((
                "projects",
                projects
                    .iter()
                    .map(|project| project.to_string())
                    .collect::<Vec<_>>(),
            ))
            .bind(("limit", limit))
            .bind((
                "playbook",
                crate::domain_models::procedural::PLAYBOOK_MEMORY,
            ));
        for (i, term) in normalized.into_iter().enumerate() {
            query = query.bind((format!("term{i}"), term));
        }
        let mut response = query.await.map_err(storage)?.check().map_err(storage)?;
        let ranked: Vec<RankedRecord> = response.take(1).map_err(storage)?;
        let (states, jobs) = self
            .selected_metadata(owner, ranked.iter().map(|hit| &hit.revision))
            .await?;
        ranked
            .into_iter()
            .map(|hit| {
                let _ = (hit.memory_key, hit.revision_key);
                self.assemble(hit.revision, &states, &jobs)
                    .map(|item| RuntimeHit {
                        item,
                        lexical_score: hit.lexical_score,
                    })
            })
            .collect()
    }

    pub async fn purge(
        &self,
        owner: &OwnerId,
        project: &ProjectId,
        sources: &[SourceId],
    ) -> Result<PurgeReceipt, RuntimeError> {
        self.validate_activity_scope(owner, project)?;
        let _guard = self.capture.writes.lock().await;
        let tx = self.capture.db.clone().begin().await.map_err(storage)?;
        match self.purge_tx(&tx, owner, project, sources).await {
            Ok(receipt) => {
                self.finish_attached_activity(
                    &tx,
                    &[],
                    &[],
                    Some(crate::domain_models::runtime_diagnostics::ActivityCounts {
                        sources_removed: Some(receipt.sources as u64),
                        revisions_removed: Some(receipt.revisions as u64),
                        jobs_removed: Some(receipt.jobs as u64),
                        ..Default::default()
                    }),
                )
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

    pub async fn retain(
        &self,
        owner: &OwnerId,
        project: &ProjectId,
        before_ms: u64,
        limit: usize,
    ) -> Result<PurgeReceipt, RuntimeError> {
        if limit == 0 {
            return Ok(PurgeReceipt::default());
        }
        let _guard = self.capture.writes.lock().await;
        let tx = self.capture.db.clone().begin().await.map_err(storage)?;
        let mut response = tx.query(format!(
            "SELECT {SOURCE_PROJECTION} FROM memory_source WHERE owner_id = $owner AND project_id = $project AND recorded_at_ms < $before ORDER BY recorded_at_ms, source_id LIMIT $limit;"
        )).bind(("owner", owner.to_string())).bind(("project", project.to_string()))
            .bind(("before", before_ms)).bind(("limit", limit))
            .await.map_err(storage)?.check().map_err(storage)?;
        let rows: Vec<SourceRecord> = response.take(0).map_err(storage)?;
        let sources = rows
            .into_iter()
            .map(|row| SourceId::new(row.source_id).map_err(|_| RuntimeError::StorageUnavailable))
            .collect::<Result<Vec<_>, _>>()?;
        match self.purge_tx(&tx, owner, project, &sources).await {
            Ok(receipt) => {
                tx.commit().await.map_err(storage)?;
                Ok(receipt)
            }
            Err(error) => {
                let _ = tx.cancel().await;
                Err(error)
            }
        }
    }

    async fn purge_tx(
        &self,
        tx: &Transaction<Db>,
        owner: &OwnerId,
        project: &ProjectId,
        sources: &[SourceId],
    ) -> Result<PurgeReceipt, RuntimeError> {
        if sources.is_empty() {
            return Ok(PurgeReceipt::default());
        }
        let wanted = sources.iter().map(|id| id.to_string()).collect::<Vec<_>>();
        let mut response = tx.query("SELECT VALUE source_id FROM memory_source WHERE owner_id = $owner AND project_id = $project AND source_id IN $ids;")
            .bind(("owner", owner.to_string())).bind(("project", project.to_string())).bind(("ids", wanted))
            .await.map_err(storage)?.check().map_err(storage)?;
        let delete_sources: Vec<String> = response.take(0).map_err(storage)?;
        if delete_sources.is_empty() {
            return Ok(PurgeReceipt::default());
        }
        let jobs = self
            .purge_jobs(tx, owner.as_str(), project.as_str())
            .await?;
        let revisions = self
            .purge_revisions(tx, owner.as_str(), project.as_str())
            .await?;
        let mut delete_jobs = BTreeSet::new();
        let mut delete_revs = BTreeSet::new();
        for job in &jobs {
            if job
                .source_ids
                .iter()
                .any(|source| delete_sources.contains(source))
            {
                delete_jobs.insert(job.job_id.clone());
            }
        }
        for revision in &revisions {
            if delete_jobs.contains(&revision.job_id)
                || revision
                    .sources
                    .iter()
                    .any(|id| delete_sources.iter().any(|source| source == id))
            {
                delete_revs.insert(revision.revision_id.clone());
            }
        }
        let mut growing = true;
        while growing {
            growing = false;
            for revision in &revisions {
                if delete_revs.contains(&revision.revision_id) {
                    continue;
                }
                if delete_jobs.contains(&revision.job_id)
                    || revision
                        .predecessor
                        .as_ref()
                        .is_some_and(|parent| delete_revs.contains(parent))
                {
                    delete_revs.insert(revision.revision_id.clone());
                    delete_jobs.insert(revision.job_id.clone());
                    growing = true;
                }
            }
        }
        let source_ids = delete_sources.clone();
        let job_ids = delete_jobs.into_iter().collect::<Vec<_>>();
        let rev_ids = delete_revs.into_iter().collect::<Vec<_>>();
        let operation_sources = source_ids
            .iter()
            .cloned()
            .chain(
                jobs.iter()
                    .filter(|job| job_ids.contains(&job.job_id))
                    .flat_map(|job| job.source_ids.iter().cloned()),
            )
            .chain(
                revisions
                    .iter()
                    .filter(|revision| rev_ids.contains(&revision.revision_id))
                    .flat_map(|revision| revision.sources.iter().cloned()),
            )
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let timeline_entries = self
            .purge_timeline_tx(tx, owner, project, &source_ids, &rev_ids)
            .await?;
        let receipt = PurgeReceipt {
            sources: source_ids.len(),
            revisions: rev_ids.len(),
            jobs: job_ids.len(),
            timeline_entries,
        };
        self.purge_activity_tx(tx, owner, project, &source_ids, &rev_ids)
            .await?;
        self.delete_in(tx, "DELETE memory_lifecycle_audit WHERE owner_id = $owner AND project_id = $project AND revision_id IN $ids RETURN NONE;", owner, project, &rev_ids).await?;
        self.delete_in(tx, "DELETE memory_revision_state WHERE owner_id = $owner AND project_id = $project AND revision_id IN $ids RETURN NONE;", owner, project, &rev_ids).await?;
        self.delete_in(tx, "DELETE memory_runtime_embedding WHERE owner_id = $owner AND project_id = $project AND revision_id IN $ids RETURN NONE;", owner, project, &rev_ids).await?;
        tx.query("DELETE memory_runtime_operation WHERE owner_id = $owner AND project_id = $project AND array::intersect(sources, $sources) != [] RETURN NONE;")
            .bind(("owner", owner.to_string())).bind(("project", project.to_string())).bind(("sources", operation_sources))
            .await.map_err(storage)?.check().map_err(storage)?;
        if !rev_ids.is_empty() {
            if tx.query("DELETE memory_revision_conflict WHERE owner_id = $owner AND project_id = $project AND (left_id IN $ids OR right_id IN $ids) RETURN NONE;")
                .bind(("owner", owner.to_string())).bind(("project", project.to_string())).bind(("ids", rev_ids.clone()))
                .await.and_then(|response| response.check()).is_err() {
                return Err(RuntimeError::StorageUnavailable);
            }
            if tx.query("DELETE memory_active_head WHERE owner_id = $owner AND project_id = $project AND revision_id IN $ids RETURN NONE;")
                .bind(("owner", owner.to_string())).bind(("project", project.to_string())).bind(("ids", rev_ids.clone()))
                .await.and_then(|response| response.check()).is_err() {
                return Err(RuntimeError::StorageUnavailable);
            }
            if tx.query("DELETE experiment_revision_embedding WHERE owner_id = $owner AND project_id = $project AND revision_id IN $ids RETURN NONE;")
                .bind(("owner", owner.to_string())).bind(("project", project.to_string())).bind(("ids", rev_ids.clone()))
                .await.and_then(|response| response.check()).is_err() {
                return Err(RuntimeError::StorageUnavailable);
            }
        }
        self.delete_in(tx, "DELETE memory_revision WHERE owner_id = $owner AND project_id = $project AND revision_id IN $ids RETURN NONE;", owner, project, &rev_ids).await?;
        self.delete_in(tx, "DELETE memory_job WHERE owner_id = $owner AND project_id = $project AND job_id IN $ids RETURN NONE;", owner, project, &job_ids).await?;
        if job_ids.is_empty() {
            self.delete_in(tx, "DELETE memory_source_consumption WHERE owner_id = $owner AND project_id = $project AND source_id IN $ids RETURN NONE;", owner, project, &source_ids).await?;
        } else if tx.query("DELETE memory_source_consumption WHERE owner_id = $owner AND (source_id IN $sources OR job_id IN $jobs) RETURN NONE;")
            .bind(("owner", owner.to_string())).bind(("sources", source_ids.clone())).bind(("jobs", job_ids.clone()))
            .await.and_then(|response| response.check()).is_err() {
            return Err(RuntimeError::StorageUnavailable);
        }
        self.delete_in(tx, "DELETE experiment_source_policy WHERE owner_id = $owner AND project_id = $project AND source_id IN $ids RETURN NONE;", owner, project, &source_ids).await?;
        self.delete_in(tx, "DELETE memory_source WHERE owner_id = $owner AND project_id = $project AND source_id IN $ids RETURN NONE;", owner, project, &source_ids).await?;
        Ok(receipt)
    }

    async fn delete_in(
        &self,
        tx: &Transaction<Db>,
        sql: &str,
        owner: &OwnerId,
        project: &ProjectId,
        ids: &[String],
    ) -> Result<(), RuntimeError> {
        if ids.is_empty() {
            return Ok(());
        }
        if tx
            .query(sql)
            .bind(("owner", owner.to_string()))
            .bind(("project", project.to_string()))
            .bind(("ids", ids.to_vec()))
            .await
            .and_then(|response| response.check())
            .is_err()
        {
            return Err(RuntimeError::StorageUnavailable);
        }
        Ok(())
    }
    async fn create_row<T: SurrealValue>(
        &self,
        tx: &Transaction<Db>,
        sql: &str,
        record: T,
    ) -> Result<(), RuntimeError> {
        if tx
            .query(sql)
            .bind(("record", record))
            .await
            .and_then(|response| response.check())
            .is_err()
        {
            return Err(RuntimeError::StorageUnavailable);
        }
        Ok(())
    }
    async fn source_row(
        &self,
        db: &impl QueryConnection,
        owner: &str,
        source: &str,
    ) -> Result<Vec<SourceRecord>, RuntimeError> {
        let mut response = db.query(format!("SELECT {SOURCE_PROJECTION} FROM memory_source WHERE owner_id = $owner AND source_id = $source;"))
            .bind(("owner", owner.to_string())).bind(("source", source.to_string()))
            .await.map_err(storage)?.check().map_err(storage)?;
        response.take(0).map_err(storage)
    }
    async fn require_live_originals(
        &self,
        db: &impl QueryConnection,
        owner: &str,
        sources: &[SourceRecord],
    ) -> Result<(), RuntimeError> {
        for source in sources {
            let stored = self.source_row(db, owner, &source.source_id).await?;
            match stored.first() {
                None => return Err(RuntimeError::Missing),
                Some(row) if row != source => {
                    return Err(RuntimeError::InvalidInput("immutable original mismatch"));
                }
                Some(_) => {}
            }
        }
        Ok(())
    }
    async fn consumption_for(
        &self,
        db: &impl QueryConnection,
        owner: &str,
        sources: &[String],
    ) -> Result<Vec<ConsumptionRecord>, RuntimeError> {
        if sources.is_empty() {
            return Ok(Vec::new());
        }
        let mut response = db.query(format!("SELECT {CONSUMPTION_FIELDS} FROM memory_source_consumption WHERE owner_id = $owner AND source_id IN $sources;"))
            .bind(("owner", owner.to_string())).bind(("sources", sources.to_vec()))
            .await.map_err(storage)?.check().map_err(storage)?;
        response.take(0).map_err(storage)
    }
    async fn lineage_rows(
        &self,
        db: &impl QueryConnection,
        owner: &str,
        project: &str,
        ids: &[String],
        roots: &[String],
    ) -> Result<Vec<RevisionRecord>, RuntimeError> {
        let mut response = db.query(format!("SELECT {REV_FIELDS} FROM memory_revision WHERE owner_id = $owner AND revision_id IN $ids;"))
            .bind(("owner", owner.to_string())).bind(("ids", ids.to_vec()))
            .await.map_err(storage)?.check().map_err(storage)?;
        let mut rows: Vec<RevisionRecord> = response.take(0).map_err(storage)?;
        for memory in roots {
            // Memory identities are project-scoped, like heads and lifecycle state.
            let mut response = db.query(format!("SELECT {REV_FIELDS} FROM memory_revision WHERE owner_id = $owner AND project_id = $project AND memory_id = $memory LIMIT 1;"))
                .bind(("owner", owner.to_string())).bind(("project", project.to_string())).bind(("memory", memory.clone()))
                .await.map_err(storage)?.check().map_err(storage)?;
            let existing: Vec<RevisionRecord> = response.take(0).map_err(storage)?;
            rows.extend(existing);
        }
        Ok(rows)
    }
    async fn revision_row(
        &self,
        db: &impl QueryConnection,
        owner: &str,
        revision: &str,
    ) -> Result<Option<RevisionRecord>, RuntimeError> {
        let mut response = db.query(format!("SELECT {REV_FIELDS} FROM memory_revision WHERE owner_id = $owner AND revision_id = $revision;"))
            .bind(("owner", owner.to_string())).bind(("revision", revision.to_string()))
            .await.map_err(storage)?.check().map_err(storage)?;
        let rows: Vec<RevisionRecord> = response.take(0).map_err(storage)?;
        Ok(rows.into_iter().next())
    }
    async fn state_row(
        &self,
        db: &impl QueryConnection,
        owner: &str,
        revision: &str,
    ) -> Result<Option<StateRecord>, RuntimeError> {
        let mut response = db.query(format!("SELECT {STATE_FIELDS} FROM memory_revision_state WHERE owner_id = $owner AND revision_id = $revision;"))
            .bind(("owner", owner.to_string())).bind(("revision", revision.to_string()))
            .await.map_err(storage)?.check().map_err(storage)?;
        let rows: Vec<StateRecord> = response.take(0).map_err(storage)?;
        Ok(rows.into_iter().next())
    }
    async fn purge_revisions(
        &self,
        db: &impl QueryConnection,
        owner: &str,
        project: &str,
    ) -> Result<Vec<PurgeRevision>, RuntimeError> {
        let mut response = db.query("SELECT revision_id, job_id, sources, predecessor FROM memory_revision WHERE owner_id = $owner AND project_id = $project;")
            .bind(("owner", owner.to_string())).bind(("project", project.to_string()))
            .await.map_err(storage)?.check().map_err(storage)?;
        response.take(0).map_err(storage)
    }
    async fn purge_jobs(
        &self,
        db: &impl QueryConnection,
        owner: &str,
        project: &str,
    ) -> Result<Vec<PurgeJob>, RuntimeError> {
        let mut response = db.query("SELECT job_id, sources.source_id AS source_ids FROM memory_job WHERE owner_id = $owner AND project_id = $project;")
            .bind(("owner", owner.to_string())).bind(("project", project.to_string()))
            .await.map_err(storage)?.check().map_err(storage)?;
        response.take(0).map_err(storage)
    }
    async fn selected_metadata<'a>(
        &self,
        owner: &OwnerId,
        revisions: impl Iterator<Item = &'a RevisionRecord>,
    ) -> Result<(Vec<StateRecord>, Vec<JobRecord>), RuntimeError> {
        self.selected_metadata_in(&self.capture.db, owner, revisions)
            .await
    }
    async fn selected_metadata_in<'a>(
        &self,
        db: &impl QueryConnection,
        owner: &OwnerId,
        revisions: impl Iterator<Item = &'a RevisionRecord>,
    ) -> Result<(Vec<StateRecord>, Vec<JobRecord>), RuntimeError> {
        let mut ids = Vec::new();
        let mut jobs = BTreeSet::new();
        for revision in revisions {
            ids.push(revision.revision_id.clone());
            jobs.insert(revision.job_id.clone());
        }
        if ids.is_empty() {
            return Ok((Vec::new(), Vec::new()));
        }
        let jobs = jobs.into_iter().collect::<Vec<_>>();
        let mut response = db.query(format!("SELECT {STATE_FIELDS} FROM memory_revision_state WHERE owner_id = $owner AND revision_id IN $ids; SELECT {JOB_FIELDS} FROM memory_job WHERE owner_id = $owner AND job_id IN $jobs;"))
            .bind(("owner", owner.to_string())).bind(("ids", ids)).bind(("jobs", jobs))
            .await.map_err(storage)?.check().map_err(storage)?;
        Ok((
            response.take(0).map_err(storage)?,
            response.take(1).map_err(storage)?,
        ))
    }
    async fn current_active(
        &self,
        db: &impl QueryConnection,
        owner: &str,
        project: &str,
        memory: &str,
    ) -> Result<Option<String>, RuntimeError> {
        let mut response = db.query(format!("SELECT {HEAD_FIELDS} FROM memory_active_head WHERE owner_id = $owner AND project_id = $project AND memory_id = $memory;"))
            .bind(("owner", owner.to_string())).bind(("project", project.to_string())).bind(("memory", memory.to_string()))
            .await.map_err(storage)?.check().map_err(storage)?;
        let heads: Vec<HeadRecord> = response.take(0).map_err(storage)?;
        if let Some(head) = heads.into_iter().next() {
            return Ok(Some(head.revision_id));
        }
        let mut response = db.query(format!("SELECT {STATE_FIELDS} FROM memory_revision_state WHERE owner_id = $owner AND project_id = $project AND memory_id = $memory AND state = 'active';"))
            .bind(("owner", owner.to_string())).bind(("project", project.to_string())).bind(("memory", memory.to_string()))
            .await.map_err(storage)?.check().map_err(storage)?;
        let rows: Vec<StateRecord> = response.take(0).map_err(storage)?;
        Ok(rows.into_iter().next().map(|row| row.revision_id))
    }
    async fn conflict_row(
        &self,
        db: &impl QueryConnection,
        owner: &str,
        project: &str,
        left: &str,
        right: &str,
    ) -> Result<Option<ConflictRecord>, RuntimeError> {
        let mut response = db.query(format!("SELECT {CONFLICT_FIELDS} FROM memory_revision_conflict WHERE owner_id = $owner AND project_id = $project AND left_id = $left AND right_id = $right;"))
            .bind(("owner", owner.to_string())).bind(("project", project.to_string())).bind(("left", left.to_string())).bind(("right", right.to_string()))
            .await.map_err(storage)?.check().map_err(storage)?;
        let rows: Vec<ConflictRecord> = response.take(0).map_err(storage)?;
        Ok(rows.into_iter().next())
    }
    async fn open_conflict(
        &self,
        db: &impl QueryConnection,
        owner: &str,
        project: &str,
        revision: &str,
    ) -> Result<Option<ConflictRecord>, RuntimeError> {
        let mut response = db.query(format!("SELECT {CONFLICT_FIELDS} FROM memory_revision_conflict WHERE owner_id = $owner AND project_id = $project AND resolved = false AND (left_id = $revision OR right_id = $revision) LIMIT 1;"))
            .bind(("owner", owner.to_string())).bind(("project", project.to_string())).bind(("revision", revision.to_string()))
            .await.map_err(storage)?.check().map_err(storage)?;
        let rows: Vec<ConflictRecord> = response.take(0).map_err(storage)?;
        Ok(rows.into_iter().next())
    }
    async fn ensure_state(
        &self,
        tx: &Transaction<Db>,
        record: &StateRecord,
        overwrite: bool,
    ) -> Result<(), RuntimeError> {
        if self
            .state_row(tx, &record.owner_id, &record.revision_id)
            .await?
            .is_some()
        {
            if overwrite {
                return self.update_state(tx, record).await;
            }
            return Ok(());
        }
        self.create_row(
            tx,
            "CREATE memory_revision_state CONTENT $record RETURN NONE;",
            record.clone(),
        )
        .await
    }
    async fn update_state(
        &self,
        tx: &Transaction<Db>,
        record: &StateRecord,
    ) -> Result<(), RuntimeError> {
        if tx.query("UPDATE memory_revision_state SET state = $state, reviewed_at_ms = $at, reviewed_by = $actor WHERE owner_id = $owner AND revision_id = $revision RETURN NONE;")
            .bind(("state", record.state.clone())).bind(("at", record.reviewed_at_ms)).bind(("actor", record.reviewed_by.clone()))
            .bind(("owner", record.owner_id.clone())).bind(("revision", record.revision_id.clone()))
            .await.and_then(|response| response.check()).is_err() {
            return Err(RuntimeError::StorageUnavailable);
        }
        Ok(())
    }
    #[expect(
        clippy::too_many_arguments,
        reason = "Explicit persisted state fields at the database boundary"
    )]
    async fn write_state(
        &self,
        tx: &Transaction<Db>,
        owner: &str,
        project: &str,
        memory: &str,
        revision: &str,
        state: LifecycleState,
        reviewed_at_ms: Option<u64>,
        reviewed_by: Option<&str>,
    ) -> Result<(), RuntimeError> {
        self.ensure_state(
            tx,
            &StateRecord {
                owner_id: owner.into(),
                project_id: project.into(),
                revision_id: revision.into(),
                memory_id: memory.into(),
                state: state_name(state).into(),
                reviewed_at_ms,
                reviewed_by: reviewed_by.map(|value| value.into()),
            },
            true,
        )
        .await
    }
    async fn set_head(
        &self,
        tx: &Transaction<Db>,
        owner: &str,
        project: &str,
        memory: &str,
        revision: &str,
    ) -> Result<(), RuntimeError> {
        let mut response = tx.query(format!("SELECT {HEAD_FIELDS} FROM memory_active_head WHERE owner_id = $owner AND project_id = $project AND memory_id = $memory;"))
            .bind(("owner", owner.to_string())).bind(("project", project.to_string())).bind(("memory", memory.to_string()))
            .await.map_err(storage)?.check().map_err(storage)?;
        let rows: Vec<HeadRecord> = response.take(0).map_err(storage)?;
        if rows.is_empty() {
            return self
                .create_row(
                    tx,
                    "CREATE memory_active_head CONTENT $record RETURN NONE;",
                    HeadRecord {
                        owner_id: owner.into(),
                        project_id: project.into(),
                        memory_id: memory.into(),
                        revision_id: revision.into(),
                    },
                )
                .await;
        }
        if tx.query("UPDATE memory_active_head SET revision_id = $revision WHERE owner_id = $owner AND project_id = $project AND memory_id = $memory RETURN NONE;")
            .bind(("revision", revision.to_string())).bind(("owner", owner.to_string())).bind(("project", project.to_string())).bind(("memory", memory.to_string()))
            .await.and_then(|response| response.check()).is_err() {
            return Err(RuntimeError::StorageUnavailable);
        }
        Ok(())
    }
    async fn clear_head(
        &self,
        tx: &Transaction<Db>,
        owner: &str,
        project: &str,
        memory: &str,
    ) -> Result<(), RuntimeError> {
        if tx.query("DELETE memory_active_head WHERE owner_id = $owner AND project_id = $project AND memory_id = $memory RETURN NONE;")
            .bind(("owner", owner.to_string())).bind(("project", project.to_string())).bind(("memory", memory.to_string()))
            .await.and_then(|response| response.check()).is_err() {
            return Err(RuntimeError::StorageUnavailable);
        }
        Ok(())
    }
    #[expect(
        clippy::too_many_arguments,
        reason = "Explicit immutable audit fields at the database boundary"
    )]
    async fn audit(
        &self,
        tx: &Transaction<Db>,
        owner: &OwnerId,
        actor: &ActorId,
        record: &RevisionRecord,
        from: LifecycleState,
        to: LifecycleState,
        action: ReviewAction,
        expected: Option<&str>,
        conflict_with: Option<&str>,
        at_ms: u64,
    ) -> Result<(), RuntimeError> {
        self.create_row(
            tx,
            "CREATE memory_lifecycle_audit CONTENT $record RETURN NONE;",
            AuditRecord {
                owner_id: owner.to_string(),
                project_id: record.project_id.clone(),
                revision_id: record.revision_id.clone(),
                actor_id: actor.to_string(),
                action: action_name(action).into(),
                from_state: state_name(from).into(),
                to_state: state_name(to).into(),
                expected_active: expected.map(|value| value.into()),
                conflict_with: conflict_with.map(|value| value.into()),
                at_ms,
            },
        )
        .await
    }
    async fn hydrate(
        &self,
        db: &Surreal<Db>,
        owner: &OwnerId,
        revision: &str,
    ) -> Result<RuntimeRevision, RuntimeError> {
        let record = self
            .revision_row(db, owner.as_str(), revision)
            .await?
            .ok_or(RuntimeError::Missing)?;
        let state_rows = match self.state_row(db, owner.as_str(), revision).await? {
            Some(row) => vec![row],
            None => Vec::new(),
        };
        let mut response = db
            .query(format!(
                "SELECT {JOB_FIELDS} FROM memory_job WHERE owner_id = $owner AND job_id = $job;"
            ))
            .bind(("owner", owner.to_string()))
            .bind(("job", record.job_id.clone()))
            .await
            .map_err(storage)?
            .check()
            .map_err(storage)?;
        let jobs: Vec<JobRecord> = response.take(0).map_err(storage)?;
        self.assemble(record, &state_rows, &jobs)
    }
    fn assemble(
        &self,
        record: RevisionRecord,
        states: &[StateRecord],
        jobs: &[JobRecord],
    ) -> Result<RuntimeRevision, RuntimeError> {
        let state_row = states
            .iter()
            .find(|row| row.revision_id == record.revision_id);
        let state = match state_row {
            Some(row) => parse_state(&row.state)?,
            None => LifecycleState::Processing,
        };
        let reviewed_at_ms = state_row.and_then(|row| row.reviewed_at_ms);
        let job = jobs
            .iter()
            .find(|job| job.job_id == record.job_id)
            .ok_or(RuntimeError::StorageUnavailable)?;
        let watermark_ms = record.watermark_ms;
        let revision = decode_revision(record)?;
        let mut sources = Vec::new();
        for id in &revision.sources {
            let event = job
                .sources
                .iter()
                .find(|source| source.source_id == id.as_str())
                .cloned()
                .ok_or(RuntimeError::StorageUnavailable)?;
            sources.push(source_event(event)?);
        }
        Ok(RuntimeRevision {
            revision,
            state,
            sources,
            reviewed_at_ms,
            watermark_ms,
        })
    }
}
