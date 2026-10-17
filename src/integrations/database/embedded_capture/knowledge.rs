//! Persistent jobs/revisions and local lexical retrieval; no model transport.
use super::{EmbeddedCaptureStore, SOURCE_PROJECTION, SourceRecord};
use crate::domain_models::{
    JobId, MemoryError, MemoryId, OwnerId, ProjectId, RevisionId, SourceId,
};
use crate::domain_models::{consolidation::*, retrieval::*};
use crate::integrations::consolidation_store::{AsyncConsolidationStore, RevisionRetriever};
use surrealdb::types::SurrealValue;
const JOB_FIELDS: &str = "owner_id, project_id, job_id, watermark_ms, sources, status, revisions";
const REV_FIELDS: &str = "owner_id, project_id, revision_id, memory_id, job_id, watermark_ms, kind, status, content, prerequisites, sources, predecessor, model, prompt_version";
#[derive(Clone)]
pub struct EmbeddedKnowledgeStore {
    capture: EmbeddedCaptureStore,
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
#[derive(Debug, SurrealValue)]
struct RankedRecord {
    revision: RevisionRecord,
    lexical_score: u64,
    memory_key: String,
    revision_key: String,
}
impl RevisionRecord {
    fn encode(revision: Revision, job: &JobRecord) -> Self {
        Self {
            owner_id: revision.owner.to_string(),
            project_id: revision.project.to_string(),
            revision_id: revision.id.to_string(),
            memory_id: revision.memory.to_string(),
            job_id: job.job_id.clone(),
            watermark_ms: job.watermark_ms,
            kind: match revision.kind {
                KnowledgeKind::Knowledge => "knowledge",
                KnowledgeKind::Procedure => "procedure",
            }
            .into(),
            status: "pending_review".into(),
            content: revision.content,
            prerequisites: revision.prerequisites,
            sources: revision
                .sources
                .into_iter()
                .map(|id| id.to_string())
                .collect(),
            predecessor: revision.predecessor.map(|id| id.to_string()),
            model: revision.model,
            prompt_version: revision.prompt_version,
        }
    }
    fn decode(self) -> Result<Revision, ConsolidationError> {
        if self.status != "pending_review" {
            return Err(ConsolidationError::StorageUnavailable);
        }
        Ok(Revision {
            id: RevisionId::new(self.revision_id)
                .map_err(|_| ConsolidationError::StorageUnavailable)?,
            memory: MemoryId::new(self.memory_id)
                .map_err(|_| ConsolidationError::StorageUnavailable)?,
            owner: OwnerId::new(self.owner_id)
                .map_err(|_| ConsolidationError::StorageUnavailable)?,
            project: ProjectId::new(self.project_id)
                .map_err(|_| ConsolidationError::StorageUnavailable)?,
            kind: match self.kind.as_str() {
                "knowledge" => KnowledgeKind::Knowledge,
                "procedure" => KnowledgeKind::Procedure,
                _ => return Err(ConsolidationError::StorageUnavailable),
            },
            status: RevisionStatus::PendingReview,
            content: self.content,
            prerequisites: self.prerequisites,
            sources: self
                .sources
                .into_iter()
                .map(|id| SourceId::new(id).map_err(|_| ConsolidationError::StorageUnavailable))
                .collect::<Result<_, _>>()?,
            predecessor: self
                .predecessor
                .map(|id| RevisionId::new(id).map_err(|_| ConsolidationError::StorageUnavailable))
                .transpose()?,
            model: self.model,
            prompt_version: self.prompt_version,
        })
    }
}
impl JobRecord {
    fn encode(snapshot: JobSnapshot) -> Self {
        Self {
            owner_id: snapshot.owner.to_string(),
            project_id: snapshot.project.to_string(),
            job_id: snapshot.id.to_string(),
            watermark_ms: snapshot.watermark_ms,
            sources: snapshot
                .sources
                .into_iter()
                .map(SourceRecord::from)
                .collect(),
            status: "pending".into(),
            revisions: vec![],
        }
    }
    fn decode(self) -> Result<ConsolidationJob, ConsolidationError> {
        Ok(ConsolidationJob {
            snapshot: JobSnapshot {
                id: JobId::new(self.job_id).map_err(|_| ConsolidationError::StorageUnavailable)?,
                owner: OwnerId::new(self.owner_id)
                    .map_err(|_| ConsolidationError::StorageUnavailable)?,
                project: ProjectId::new(self.project_id)
                    .map_err(|_| ConsolidationError::StorageUnavailable)?,
                watermark_ms: self.watermark_ms,
                sources: self
                    .sources
                    .into_iter()
                    .map(|record| {
                        record
                            .into_domain()
                            .map_err(|_| ConsolidationError::StorageUnavailable)
                    })
                    .collect::<Result<_, _>>()?,
            },
            status: match self.status.as_str() {
                "pending" => JobStatus::Pending,
                "completed" => JobStatus::Completed,
                _ => return Err(ConsolidationError::StorageUnavailable),
            },
            revisions: self
                .revisions
                .into_iter()
                .map(RevisionRecord::decode)
                .collect::<Result<_, _>>()?,
        })
    }
}
impl EmbeddedKnowledgeStore {
    /// Shares the already-open datastore and write admission mutex.
    pub fn new(capture: EmbeddedCaptureStore) -> Self {
        Self { capture }
    }
}
impl AsyncConsolidationStore for EmbeddedKnowledgeStore {
    async fn create_async(&self, snapshot: JobSnapshot) -> Result<JobReceipt, ConsolidationError> {
        let _guard = self.capture.writes.lock().await;
        let wanted = JobRecord::encode(snapshot);
        let tx = self
            .capture
            .db
            .clone()
            .begin()
            .await
            .map_err(|_| ConsolidationError::StorageUnavailable)?;
        let mut response = tx
            .query(format!(
                "SELECT {JOB_FIELDS} FROM memory_job WHERE owner_id = $owner AND job_id = $job;"
            ))
            .bind(("owner", wanted.owner_id.clone()))
            .bind(("job", wanted.job_id.clone()))
            .await
            .map_err(|_| ConsolidationError::StorageUnavailable)?
            .check()
            .map_err(|_| ConsolidationError::StorageUnavailable)?;
        let existing: Vec<JobRecord> = response
            .take(0)
            .map_err(|_| ConsolidationError::StorageUnavailable)?;
        if let Some(old) = existing.first() {
            let matches = old.owner_id == wanted.owner_id
                && old.project_id == wanted.project_id
                && old.job_id == wanted.job_id
                && old.watermark_ms == wanted.watermark_ms
                && old.sources == wanted.sources;
            tx.cancel()
                .await
                .map_err(|_| ConsolidationError::StorageUnavailable)?;
            return if matches {
                Ok(JobReceipt::AlreadyCreated)
            } else {
                Err(ConsolidationError::Conflict)
            };
        }
        // A claimed snapshot must exactly match immutable evidence already persisted.
        for source in &wanted.sources {
            let mut response=tx.query(format!("SELECT {SOURCE_PROJECTION} FROM memory_source WHERE owner_id = $owner AND source_id = $source;")).bind(("owner",wanted.owner_id.clone())).bind(("source",source.source_id.clone())).await.map_err(|_|ConsolidationError::StorageUnavailable)?.check().map_err(|_|ConsolidationError::StorageUnavailable)?;
            let records: Vec<SourceRecord> = response
                .take(0)
                .map_err(|_| ConsolidationError::StorageUnavailable)?;
            if records.as_slice() != std::slice::from_ref(source) {
                tx.cancel()
                    .await
                    .map_err(|_| ConsolidationError::StorageUnavailable)?;
                return Err(ConsolidationError::InvalidSnapshot);
            }
        }
        if tx
            .query("CREATE memory_job CONTENT $job RETURN NONE;")
            .bind(("job", wanted))
            .await
            .and_then(|response| response.check())
            .is_err()
        {
            let _ = tx.cancel().await;
            return Err(ConsolidationError::StorageUnavailable);
        }
        tx.commit()
            .await
            .map_err(|_| ConsolidationError::StorageUnavailable)?;
        Ok(JobReceipt::Created)
    }
    async fn get_async(
        &self,
        owner: &OwnerId,
        id: &JobId,
    ) -> Result<ConsolidationJob, ConsolidationError> {
        let mut response = self
            .capture
            .db
            .query(format!(
                "SELECT {JOB_FIELDS} FROM memory_job WHERE owner_id = $owner AND job_id = $job;"
            ))
            .bind(("owner", owner.to_string()))
            .bind(("job", id.to_string()))
            .await
            .map_err(|_| ConsolidationError::StorageUnavailable)?
            .check()
            .map_err(|_| ConsolidationError::StorageUnavailable)?;
        let records: Vec<JobRecord> = response
            .take(0)
            .map_err(|_| ConsolidationError::StorageUnavailable)?;
        records
            .into_iter()
            .next()
            .ok_or(ConsolidationError::Missing)?
            .decode()
    }
    async fn complete_async(
        &self,
        owner: &OwnerId,
        id: &JobId,
        revisions: Vec<Revision>,
    ) -> Result<JobReceipt, ConsolidationError> {
        let _guard = self.capture.writes.lock().await;
        let tx = self
            .capture
            .db
            .clone()
            .begin()
            .await
            .map_err(|_| ConsolidationError::StorageUnavailable)?;
        let mut response = tx
            .query(format!(
                "SELECT {JOB_FIELDS} FROM memory_job WHERE owner_id = $owner AND job_id = $job;"
            ))
            .bind(("owner", owner.to_string()))
            .bind(("job", id.to_string()))
            .await
            .map_err(|_| ConsolidationError::StorageUnavailable)?
            .check()
            .map_err(|_| ConsolidationError::StorageUnavailable)?;
        let records: Vec<JobRecord> = response
            .take(0)
            .map_err(|_| ConsolidationError::StorageUnavailable)?;
        let job = records
            .into_iter()
            .next()
            .ok_or(ConsolidationError::Missing)?;
        let outputs: Vec<_> = revisions
            .into_iter()
            .map(|revision| RevisionRecord::encode(revision, &job))
            .collect();
        if job.status == "completed" {
            let matches = job.revisions == outputs;
            tx.cancel()
                .await
                .map_err(|_| ConsolidationError::StorageUnavailable)?;
            return if matches {
                Ok(JobReceipt::AlreadyCompleted)
            } else {
                Err(ConsolidationError::Conflict)
            };
        }
        let ids: Vec<_> = outputs.iter().map(|r| r.revision_id.clone()).collect();
        let memories: Vec<_> = outputs.iter().map(|r| r.memory_id.clone()).collect();
        let mut response=tx.query(format!("SELECT {REV_FIELDS} FROM memory_revision WHERE owner_id = $owner AND (revision_id IN $ids OR (project_id = $project AND memory_id IN $memories));")).bind(("owner",owner.to_string())).bind(("project",job.project_id.clone())).bind(("ids",ids)).bind(("memories",memories)).await.map_err(|_|ConsolidationError::StorageUnavailable)?.check().map_err(|_|ConsolidationError::StorageUnavailable)?;
        let prior: Vec<RevisionRecord> = response
            .take(0)
            .map_err(|_| ConsolidationError::StorageUnavailable)?;
        for output in &outputs {
            if prior
                .iter()
                .any(|old| old.revision_id == output.revision_id)
            {
                tx.cancel()
                    .await
                    .map_err(|_| ConsolidationError::StorageUnavailable)?;
                return Err(ConsolidationError::Conflict);
            }
            if let Some(predecessor) = &output.predecessor {
                if !prior.iter().any(|old| {
                    &old.revision_id == predecessor
                        && old.memory_id == output.memory_id
                        && old.project_id == output.project_id
                        && old.kind == output.kind
                        && old.watermark_ms <= output.watermark_ms
                }) {
                    tx.cancel()
                        .await
                        .map_err(|_| ConsolidationError::StorageUnavailable)?;
                    return Err(ConsolidationError::InvalidRevision);
                }
                if prior.iter().any(|old| {
                    old.memory_id == output.memory_id
                        && old.project_id == output.project_id
                        && old.predecessor == output.predecessor
                }) {
                    tx.cancel()
                        .await
                        .map_err(|_| ConsolidationError::StorageUnavailable)?;
                    return Err(ConsolidationError::Conflict);
                }
            } else if prior
                .iter()
                .any(|old| old.memory_id == output.memory_id && old.project_id == output.project_id)
            {
                tx.cancel()
                    .await
                    .map_err(|_| ConsolidationError::StorageUnavailable)?;
                return Err(ConsolidationError::InvalidRevision);
            }
        }
        for output in &outputs {
            if tx
                .query("CREATE memory_revision CONTENT $revision RETURN NONE;")
                .bind(("revision", output.clone()))
                .await
                .and_then(|response| response.check())
                .is_err()
            {
                let _ = tx.cancel().await;
                return Err(ConsolidationError::StorageUnavailable);
            }
        }
        if tx.query("UPDATE memory_job SET status = 'completed', revisions = $revisions WHERE owner_id = $owner AND job_id = $job RETURN NONE;").bind(("revisions",outputs)).bind(("owner",owner.to_string())).bind(("job",id.to_string())).await.and_then(|response|response.check()).is_err() {let _=tx.cancel().await;return Err(ConsolidationError::StorageUnavailable);}
        tx.commit()
            .await
            .map_err(|_| ConsolidationError::StorageUnavailable)?;
        Ok(JobReceipt::Completed)
    }
}
impl RevisionRetriever for EmbeddedKnowledgeStore {
    async fn retrieve_pending(
        &self,
        request: &RevisionRetrievalRequest,
    ) -> Result<Vec<ProvisionalRevisionHit>, MemoryError> {
        // Fixed statement structure; caller terms/identity/time/limit are bound values.
        let predicates: Vec<_> = (0..request.terms.len())
            .map(|i| format!("string::lowercase(content) CONTAINS $term{i}"))
            .collect();
        let scores: Vec<_> = predicates
            .iter()
            .map(|p| format!("(IF {p} THEN 1 ELSE 0 END)"))
            .collect();
        let record_object = REV_FIELDS
            .split(", ")
            .map(|field| format!("{field}: {field}"))
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "SELECT {{{record_object}}} AS revision, {} AS lexical_score, memory_id AS memory_key, revision_id AS revision_key FROM memory_revision WHERE owner_id = $owner AND project_id = $project AND status = 'pending_review' AND watermark_ms <= $as_of AND revision_id NOT IN (SELECT VALUE predecessor FROM memory_revision WHERE owner_id = $owner AND project_id = $project AND watermark_ms <= $as_of AND predecessor != NONE) AND ({}) ORDER BY lexical_score DESC, memory_key, revision_key LIMIT $limit;",
            scores.join(" + "),
            predicates.join(" OR ")
        );
        let mut query = self
            .capture
            .db
            .query(sql)
            .bind(("owner", request.owner.to_string()))
            .bind(("project", request.project.to_string()))
            .bind(("as_of", request.as_of_ms))
            .bind(("limit", request.limit));
        for (i, term) in request.terms.iter().enumerate() {
            query = query.bind((format!("term{i}"), term.trim().to_lowercase()));
        }
        let mut response = query
            .await
            .map_err(|_| MemoryError::StorageUnavailable)?
            .check()
            .map_err(|_| MemoryError::StorageUnavailable)?;
        let records: Vec<RankedRecord> = response
            .take(0)
            .map_err(|_| MemoryError::StorageUnavailable)?;
        let mut hits = Vec::new();
        for ranked in records {
            let _ = (&ranked.memory_key, &ranked.revision_key);
            let job = self
                .get_async(&request.owner, &JobId::new(ranked.revision.job_id.clone())?)
                .await
                .map_err(|_| MemoryError::StorageUnavailable)?;
            let watermark_ms = ranked.revision.watermark_ms;
            let revision = ranked
                .revision
                .decode()
                .map_err(|_| MemoryError::StorageUnavailable)?;
            let sources = job
                .snapshot
                .sources
                .into_iter()
                .filter(|source| revision.sources.contains(&source.source))
                .collect();
            hits.push(ProvisionalRevisionHit {
                revision,
                sources,
                watermark_ms,
                lexical_score: ranked.lexical_score,
                provisional: true,
            });
        }
        Ok(hits)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain_models::{ActorId, capture::*};
    use crate::integrations::capture_store::AsyncCaptureStore;
    #[tokio::test]
    async fn backend_write_failure_rolls_back_all_revisions_and_job_state() {
        let path = format!(
            "{}/lambda-revision-atomic-{}-{}",
            std::env::temp_dir().display(),
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
        );
        let capture = EmbeddedCaptureStore::open(&path).await.unwrap();
        capture.db.query("DEFINE FIELD OVERWRITE model ON memory_revision TYPE string READONLY ASSERT $value != 'reject-test';").await.unwrap().check().unwrap();
        let owner = OwnerId::new("owner").unwrap();
        let project = ProjectId::new("project").unwrap();
        let event = CaptureEvent {
            source: SourceId::new("source").unwrap(),
            owner: owner.clone(),
            actor: ActorId::new("actor").unwrap(),
            project: project.clone(),
            origin: "fixture://atomic".into(),
            recorded_at_ms: 1,
            kind: WorkEventKind::Result,
            outcome: WorkOutcome::Succeeded,
            content: "Synthetic evidence.".into(),
        };
        capture.save_pending_async(event.clone(), 2).await.unwrap();
        let jobs = EmbeddedKnowledgeStore::new(capture.clone());
        let id = JobId::new("job").unwrap();
        jobs.create_async(JobSnapshot {
            id: id.clone(),
            owner: owner.clone(),
            project: project.clone(),
            watermark_ms: 1,
            sources: vec![event],
        })
        .await
        .unwrap();
        let first = Revision {
            id: RevisionId::new("r1").unwrap(),
            memory: MemoryId::new("m1").unwrap(),
            owner: owner.clone(),
            project: project.clone(),
            kind: KnowledgeKind::Knowledge,
            status: RevisionStatus::PendingReview,
            content: "First output.".into(),
            prerequisites: vec![],
            sources: vec![SourceId::new("source").unwrap()],
            predecessor: None,
            model: "offline-fixture".into(),
            prompt_version: "test-v1".into(),
        };
        let mut second = first.clone();
        second.id = RevisionId::new("r2").unwrap();
        second.memory = MemoryId::new("m2").unwrap();
        second.model = "reject-test".into();
        assert_eq!(
            jobs.complete_async(&owner, &id, vec![first, second]).await,
            Err(ConsolidationError::StorageUnavailable)
        );
        let job = jobs.get_async(&owner, &id).await.unwrap();
        assert_eq!(job.status, JobStatus::Pending);
        assert!(job.revisions.is_empty());
        let mut response = capture
            .db
            .query("RETURN array::len(SELECT id FROM memory_revision);")
            .await
            .unwrap()
            .check()
            .unwrap();
        assert_eq!(response.take::<Option<u64>>(0).unwrap(), Some(0));
    }
}
