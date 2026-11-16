//! Skill candidate persistence, scoped to the worker's owner/project.
use super::*;
use crate::domain_models::skills::*;
const FIELDS: &str = "owner_id, project_id, candidate_id, scope, memory_id, revision_id, slug, status, draft, sessions_at_draft, sessions_at_decision, created_at_ms, decided_at_ms, install_paths, install_hashes, pitfall_memories, pitfall_revisions, pitfall_source_counts, installed_pitfall_memories";
#[derive(Clone, Debug, SurrealValue)]
struct CandidateRecord {
    owner_id: String,
    project_id: String,
    candidate_id: String,
    scope: String,
    memory_id: String,
    revision_id: String,
    slug: String,
    status: String,
    draft: Option<String>,
    sessions_at_draft: usize,
    sessions_at_decision: Option<usize>,
    created_at_ms: u64,
    decided_at_ms: Option<u64>,
    install_paths: Vec<String>,
    install_hashes: Vec<String>,
    pitfall_memories: Vec<String>,
    pitfall_revisions: Vec<String>,
    pitfall_source_counts: Vec<u32>,
    installed_pitfall_memories: Vec<String>,
}
impl CandidateRecord {
    fn domain(self) -> Result<SkillCandidate, RuntimeError> {
        Ok(SkillCandidate {
            candidate_id: MemoryId::new(self.candidate_id).map_err(storage)?,
            scope: match self.scope.as_str() {
                "user" => "user",
                "project" => "project",
                _ => return Err(RuntimeError::StorageUnavailable),
            },
            memory_id: MemoryId::new(self.memory_id).map_err(storage)?,
            revision_id: RevisionId::new(self.revision_id).map_err(storage)?,
            slug: self.slug,
            status: SkillStatus::parse(&self.status).ok_or(RuntimeError::StorageUnavailable)?,
            draft: self.draft,
            sessions_at_draft: self.sessions_at_draft,
            sessions_at_decision: self.sessions_at_decision,
            created_at_ms: self.created_at_ms,
            decided_at_ms: self.decided_at_ms,
            install_paths: self.install_paths,
            install_hashes: self.install_hashes,
            pitfall_memories: self
                .pitfall_memories
                .into_iter()
                .map(|id| MemoryId::new(id).map_err(storage))
                .collect::<Result<_, _>>()?,
            pitfall_revisions: self
                .pitfall_revisions
                .into_iter()
                .map(|id| RevisionId::new(id).map_err(storage))
                .collect::<Result<_, _>>()?,
            pitfall_source_counts: self.pitfall_source_counts,
            installed_pitfall_memories: self
                .installed_pitfall_memories
                .into_iter()
                .map(|id| MemoryId::new(id).map_err(storage))
                .collect::<Result<_, _>>()?,
        })
    }
    fn from_domain(owner: &OwnerId, project: &ProjectId, c: &SkillCandidate) -> Self {
        Self {
            owner_id: owner.to_string(),
            project_id: project.to_string(),
            candidate_id: c.candidate_id.to_string(),
            scope: c.scope.into(),
            memory_id: c.memory_id.to_string(),
            revision_id: c.revision_id.to_string(),
            slug: c.slug.clone(),
            status: c.status.as_str().into(),
            draft: c.draft.clone(),
            sessions_at_draft: c.sessions_at_draft,
            sessions_at_decision: c.sessions_at_decision,
            created_at_ms: c.created_at_ms,
            decided_at_ms: c.decided_at_ms,
            install_paths: c.install_paths.clone(),
            install_hashes: c.install_hashes.clone(),
            pitfall_memories: c.pitfall_memories.iter().map(ToString::to_string).collect(),
            pitfall_revisions: c
                .pitfall_revisions
                .iter()
                .map(ToString::to_string)
                .collect(),
            pitfall_source_counts: c.pitfall_source_counts.clone(),
            installed_pitfall_memories: c
                .installed_pitfall_memories
                .iter()
                .map(ToString::to_string)
                .collect(),
        }
    }
}
impl EmbeddedRuntimeStore {
    /// Bound admission to lesson evidence, not to the most recent arbitrary sources.
    pub async fn skill_recent_lesson_sources(
        &self,
        owner: &OwnerId,
        project: &ProjectId,
        memory: &MemoryId,
    ) -> Result<Vec<CaptureEvent>, RuntimeError> {
        let mut r = self.capture.db.query(format!("LET $linked = (SELECT VALUE source_id FROM memory_node_source WHERE owner_id=$owner AND project_id=$project AND memory_id=$memory); SELECT {SOURCE_PROJECTION} FROM memory_source WHERE owner_id=$owner AND project_id=$project AND source_id IN $linked AND kind='lesson' ORDER BY recorded_at_ms DESC, source_id LIMIT 32;"))
            .bind(("owner",owner.to_string())).bind(("project",project.to_string())).bind(("memory",memory.to_string())).await.map_err(storage)?.check().map_err(storage)?;
        let rows: Vec<SourceRecord> = r.take(1).map_err(storage)?;
        rows.into_iter().map(source_event).collect()
    }
    pub async fn skill_candidates(
        &self,
        owner: &OwnerId,
        project: &ProjectId,
        status: Option<SkillStatus>,
    ) -> Result<Vec<SkillCandidate>, RuntimeError> {
        let mut r = self.capture.db.query(format!("SELECT {FIELDS} FROM skill_candidate WHERE owner_id=$owner AND project_id=$project AND ($status=NONE OR status=$status) ORDER BY created_at_ms, candidate_id;"))
            .bind(("owner",owner.to_string())).bind(("project",project.to_string())).bind(("status",status.map(SkillStatus::as_str))).await.map_err(storage)?.check().map_err(storage)?;
        let rows: Vec<CandidateRecord> = r.take(0).map_err(storage)?;
        rows.into_iter().map(CandidateRecord::domain).collect()
    }
    pub async fn skill_candidate(
        &self,
        owner: &OwnerId,
        project: &ProjectId,
        id: &MemoryId,
    ) -> Result<SkillCandidate, RuntimeError> {
        let mut r = self.capture.db.query(format!("SELECT {FIELDS} FROM skill_candidate WHERE owner_id=$owner AND project_id=$project AND candidate_id=$id;"))
            .bind(("owner",owner.to_string())).bind(("project",project.to_string())).bind(("id",id.to_string())).await.map_err(storage)?.check().map_err(storage)?;
        let rows: Vec<CandidateRecord> = r.take(0).map_err(storage)?;
        rows.into_iter()
            .next()
            .ok_or(RuntimeError::Missing)?
            .domain()
    }
    pub async fn skill_pending(
        &self,
        owner: &OwnerId,
        project: &ProjectId,
    ) -> Result<usize, RuntimeError> {
        let mut r = self.capture.db.query("RETURN array::len(SELECT id FROM skill_candidate WHERE owner_id=$owner AND project_id=$project AND status='drafted');")
            .bind(("owner",owner.to_string())).bind(("project",project.to_string())).await.map_err(storage)?.check().map_err(storage)?;
        r.take::<Option<usize>>(0)
            .map_err(storage)?
            .ok_or(RuntimeError::StorageUnavailable)
    }
    pub async fn skill_mark_stale(
        &self,
        owner: &OwnerId,
        project: &ProjectId,
        memory: &MemoryId,
        revision: &RevisionId,
    ) -> Result<(), RuntimeError> {
        let _guard = self.capture.writes.lock().await;
        self.capture.db.query("UPDATE skill_candidate SET status='stale' WHERE owner_id=$owner AND project_id=$project AND memory_id=$memory AND revision_id!=$revision AND status IN ['drafted','approved','installed'] RETURN NONE;")
            .bind(("owner",owner.to_string())).bind(("project",project.to_string())).bind(("memory",memory.to_string())).bind(("revision",revision.to_string())).await.map_err(storage)?.check().map_err(storage)?;
        Ok(())
    }
    pub async fn skill_failed(
        &self,
        owner: &OwnerId,
        project: &ProjectId,
        memory: &MemoryId,
        revision: &RevisionId,
    ) -> Result<bool, RuntimeError> {
        let mut r = self.capture.db.query("SELECT VALUE revision_id FROM skill_draft_failure WHERE owner_id=$owner AND project_id=$project AND memory_id=$memory AND revision_id=$revision;")
            .bind(("owner",owner.to_string())).bind(("project",project.to_string())).bind(("memory",memory.to_string())).bind(("revision",revision.to_string())).await.map_err(storage)?.check().map_err(storage)?;
        Ok(!r.take::<Vec<String>>(0).map_err(storage)?.is_empty())
    }
    pub async fn skill_record_failure(
        &self,
        owner: &OwnerId,
        project: &ProjectId,
        memory: &MemoryId,
        revision: &RevisionId,
        at: u64,
    ) -> Result<(), RuntimeError> {
        let _guard = self.capture.writes.lock().await;
        self.capture.db.query("UPSERT skill_draft_failure SET owner_id=$owner, project_id=$project, memory_id=$memory, revision_id=$revision, failed_at_ms=$at WHERE owner_id=$owner AND project_id=$project AND memory_id=$memory AND revision_id=$revision RETURN NONE;")
            .bind(("owner",owner.to_string())).bind(("project",project.to_string())).bind(("memory",memory.to_string())).bind(("revision",revision.to_string())).bind(("at",at)).await.map_err(storage)?.check().map_err(storage)?;
        Ok(())
    }
    /// Only settled calls contribute to the next request number. A crash reuses the
    /// unsettled key and admission rejects it as uncertain instead of billing again.
    pub async fn skill_admission_count(
        &self,
        owner: &OwnerId,
        project: &ProjectId,
        revision: &RevisionId,
    ) -> Result<usize, RuntimeError> {
        let mut r = self.capture.db.query("RETURN array::len(SELECT id FROM memory_runtime_operation WHERE owner_id=$owner AND project_id=$project AND skill_revision_id=$revision AND skill_settled=true AND string::starts_with(operation_id,'skill-draft-job:'));")
            .bind(("owner",owner.to_string())).bind(("project",project.to_string())).bind(("revision",revision.to_string())).await.map_err(storage)?.check().map_err(storage)?;
        r.take::<Option<usize>>(0)
            .map_err(storage)?
            .ok_or(RuntimeError::StorageUnavailable)
    }
    pub async fn save_skill_draft(
        &self,
        owner: &OwnerId,
        project: &ProjectId,
        candidate: &SkillCandidate,
        operation: &str,
        expected_sources: u32,
    ) -> Result<(), RuntimeError> {
        let _guard = self.capture.writes.lock().await;
        let tx = self.capture.db.clone().begin().await.map_err(storage)?;
        if self
            .current_active(
                &tx,
                owner.as_str(),
                project.as_str(),
                candidate.memory_id.as_str(),
            )
            .await?
            .as_deref()
            != Some(candidate.revision_id.as_str())
        {
            return cancel_err(tx, RuntimeError::Conflict).await;
        }
        let mut r = tx.query("RETURN array::len(array::distinct(SELECT VALUE source_id FROM memory_node_source WHERE owner_id=$owner AND project_id=$project AND memory_id=$memory));")
            .bind(("owner",owner.to_string())).bind(("project",project.to_string())).bind(("memory",candidate.memory_id.to_string())).await.map_err(storage)?.check().map_err(storage)?;
        if r.take::<Option<u32>>(0).map_err(storage)? != Some(expected_sources) {
            return cancel_err(tx, RuntimeError::Conflict).await;
        }
        if candidate.pitfall_memories.len() != candidate.pitfall_revisions.len()
            || candidate.pitfall_memories.len() != candidate.pitfall_source_counts.len()
        {
            return cancel_err(tx, RuntimeError::ProviderInvalidOutput).await;
        }
        for ((memory, revision), expected_count) in candidate
            .pitfall_memories
            .iter()
            .zip(&candidate.pitfall_revisions)
            .zip(&candidate.pitfall_source_counts)
        {
            if self
                .current_active(&tx, owner.as_str(), project.as_str(), memory.as_str())
                .await?
                .as_deref()
                != Some(revision.as_str())
            {
                return cancel_err(tx, RuntimeError::ProviderInvalidOutput).await;
            }
            let mut r = tx.query("RETURN array::len(array::distinct(SELECT VALUE source_id FROM memory_node_source WHERE owner_id=$owner AND project_id=$project AND memory_id=$memory));")
                .bind(("owner",owner.to_string())).bind(("project",project.to_string())).bind(("memory",memory.to_string())).await.map_err(storage)?.check().map_err(storage)?;
            if r.take::<Option<u32>>(0).map_err(storage)? != Some(*expected_count) {
                return cancel_err(tx, RuntimeError::ProviderInvalidOutput).await;
            }
        }
        let row = CandidateRecord::from_domain(owner, project, candidate);
        tx.query("UPSERT skill_candidate CONTENT $row WHERE owner_id=$owner AND project_id=$project AND candidate_id=$id RETURN NONE; UPDATE memory_runtime_operation SET skill_settled=true WHERE owner_id=$owner AND project_id=$project AND operation_id=$operation RETURN NONE;")
            .bind(("row",row)).bind(("owner",owner.to_string())).bind(("project",project.to_string())).bind(("id",candidate.candidate_id.to_string())).bind(("operation",operation.to_owned())).await.map_err(storage)?.check().map_err(storage)?;
        tx.commit().await.map(|_| ()).map_err(storage)
    }
    pub async fn save_skill_decision(
        &self,
        owner: &OwnerId,
        project: &ProjectId,
        candidate: &SkillCandidate,
        expected: SkillStatus,
        revision: &RevisionId,
    ) -> Result<(), RuntimeError> {
        let _guard = self.capture.writes.lock().await;
        let tx = self.capture.db.clone().begin().await.map_err(storage)?;
        let mut r = tx.query("SELECT VALUE revision_id FROM skill_candidate WHERE owner_id=$owner AND project_id=$project AND candidate_id=$id;")
            .bind(("owner",owner.to_string())).bind(("project",project.to_string())).bind(("id",candidate.candidate_id.to_string())).await.map_err(storage)?.check().map_err(storage)?;
        let revisions: Vec<String> = r.take(0).map_err(storage)?;
        if revisions.is_empty() {
            return cancel_err(tx, RuntimeError::Conflict).await;
        }
        if revisions.len() != 1
            || revisions[0] != revision.as_str()
            || &candidate.revision_id != revision
        {
            return cancel_err(tx, RuntimeError::InvalidInput("skill revision mismatch")).await;
        }
        let mut r = tx.query("UPDATE skill_candidate CONTENT $row WHERE owner_id=$owner AND project_id=$project AND candidate_id=$id AND revision_id=$revision AND status=$expected AND draft=$draft RETURN AFTER;")
            .bind(("row",CandidateRecord::from_domain(owner,project,candidate))).bind(("owner",owner.to_string())).bind(("project",project.to_string())).bind(("id",candidate.candidate_id.to_string())).bind(("revision",revision.to_string())).bind(("expected",expected.as_str())).bind(("draft",candidate.draft.clone())).await.map_err(storage)?.check().map_err(storage)?;
        let rows: Vec<CandidateRecord> = r.take(0).map_err(storage)?;
        if rows.len() != 1 {
            return cancel_err(tx, RuntimeError::Conflict).await;
        }
        tx.commit().await.map(|_| ()).map_err(storage)
    }
    /// Admission source arrays are immutable. Replace affected skill admissions
    /// atomically with the remaining sources so request counts survive evidence purge.
    pub(super) async fn purge_skill_admissions_tx(
        &self,
        tx: &Transaction<Db>,
        owner: &OwnerId,
        project: &ProjectId,
        sources: &[String],
        purged_sources: &[String],
    ) -> Result<(), RuntimeError> {
        #[derive(SurrealValue)]
        struct Admission {
            owner_id: String,
            project_id: String,
            operation_id: String,
            sources: Vec<String>,
            skill_revision_id: Option<String>,
            skill_settled: bool,
        }
        let mut r = tx.query("SELECT owner_id, project_id, operation_id, sources, skill_revision_id, skill_settled FROM memory_runtime_operation WHERE owner_id=$owner AND project_id=$project AND skill_revision_id!=NONE AND array::intersect(sources,$sources)!=[];")
            .bind(("owner",owner.to_string())).bind(("project",project.to_string())).bind(("sources",sources.to_vec())).await.map_err(storage)?.check().map_err(storage)?;
        let rows: Vec<Admission> = r.take(0).map_err(storage)?;
        for mut row in rows {
            row.sources
                .retain(|source| !purged_sources.contains(source));
            tx.query("DELETE memory_runtime_operation WHERE owner_id=$owner AND project_id=$project AND operation_id=$operation RETURN NONE; CREATE memory_runtime_operation CONTENT $row RETURN NONE;")
                .bind(("owner",owner.to_string())).bind(("project",project.to_string())).bind(("operation",row.operation_id.clone())).bind(("row",row)).await.map_err(storage)?.check().map_err(storage)?;
        }
        Ok(())
    }
    pub(super) async fn purge_skills_tx(
        &self,
        tx: &Transaction<Db>,
        owner: &OwnerId,
        project: &ProjectId,
        sources: &[String],
    ) -> Result<Vec<String>, RuntimeError> {
        let mut r = tx.query("SELECT VALUE memory_id FROM memory_node_source WHERE owner_id=$owner AND project_id=$project AND source_id IN $sources;")
            .bind(("owner",owner.to_string())).bind(("project",project.to_string())).bind(("sources",sources.to_vec())).await.map_err(storage)?.check().map_err(storage)?;
        let mut nodes: Vec<String> = r.take(0).map_err(storage)?;
        // Admissions retain the drafting evidence even when a split moves its links.
        let mut r = tx.query("SELECT VALUE skill_revision_id FROM memory_runtime_operation WHERE owner_id=$owner AND project_id=$project AND skill_revision_id!=NONE AND array::intersect(sources,$sources)!=[];")
            .bind(("owner",owner.to_string())).bind(("project",project.to_string())).bind(("sources",sources.to_vec())).await.map_err(storage)?.check().map_err(storage)?;
        let revisions: Vec<String> = r.take(0).map_err(storage)?;
        let mut r = tx.query("SELECT VALUE memory_id FROM memory_revision WHERE owner_id=$owner AND project_id=$project AND revision_id IN $revisions; SELECT VALUE memory_id FROM skill_candidate WHERE owner_id=$owner AND project_id=$project AND revision_id IN $revisions;")
            .bind(("owner",owner.to_string())).bind(("project",project.to_string())).bind(("revisions",revisions)).await.map_err(storage)?.check().map_err(storage)?;
        nodes.extend(r.take::<Vec<String>>(0).map_err(storage)?);
        nodes.extend(r.take::<Vec<String>>(1).map_err(storage)?);
        let mut r = tx.query("LET $affected = array::union($nodes, (SELECT VALUE memory_id FROM skill_candidate WHERE owner_id=$owner AND project_id=$project AND (array::intersect(pitfall_memories,$nodes)!=[] OR array::intersect(installed_pitfall_memories,$nodes)!=[]))); SELECT VALUE install_paths FROM skill_candidate WHERE owner_id=$owner AND project_id=$project AND memory_id IN $affected AND (status='installed' OR install_paths!=[]); DELETE skill_candidate WHERE owner_id=$owner AND project_id=$project AND memory_id IN $affected AND status!='installed' AND install_paths=[] RETURN NONE; UPDATE skill_candidate SET draft=NONE, status='stale' WHERE owner_id=$owner AND project_id=$project AND memory_id IN $affected AND (status='installed' OR install_paths!=[]) RETURN NONE; DELETE skill_draft_failure WHERE owner_id=$owner AND project_id=$project AND memory_id IN $affected RETURN NONE;")
            .bind(("owner",owner.to_string())).bind(("project",project.to_string())).bind(("nodes",nodes)).await.map_err(storage)?.check().map_err(storage)?;
        let paths: Vec<Vec<String>> = r.take(1).map_err(storage)?;
        Ok(paths
            .into_iter()
            .flatten()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect())
    }
}
