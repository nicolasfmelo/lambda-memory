//! Atomic derived lesson sources, immutable citations and session extraction watermark.
use super::*;
use crate::domain_models::{lessons::*, session::MemorySession};
use serde_json::json;

impl EmbeddedRuntimeStore {
    /// Decode only immutable lesson text; JSON stays out of the pure placement policy.
    pub fn lesson_placement(
        event: &CaptureEvent,
    ) -> Result<Option<crate::domain_models::memory_tree::LessonPlacement>, RuntimeError> {
        if event.kind != WorkEventKind::Lesson {
            return Ok(None);
        }
        let value: serde_json::Value = serde_json::from_str(&event.content)
            .map_err(|_| RuntimeError::ProviderInvalidOutput)?;
        let field = |key| {
            value
                .get(key)
                .and_then(serde_json::Value::as_str)
                .ok_or(RuntimeError::ProviderInvalidOutput)
        };
        Ok(Some(crate::domain_models::memory_tree::LessonPlacement {
            title: field("title")?.to_owned(),
            content: format!("{}\n{}", field("applies_when")?, field("guidance")?),
        }))
    }

    /// Derived from all live direct links (not the paginated node-source view). Purge and
    /// split therefore change these statistics without any counter maintenance.
    pub async fn lesson_stats(
        &self,
        owner: &OwnerId,
        project: &ProjectId,
        memory: &MemoryId,
    ) -> Result<crate::domain_models::memory_tree::LessonStats, RuntimeError> {
        self.lesson_stats_in(&self.capture.db, owner, project, memory)
            .await
    }
    pub(super) async fn lesson_stats_in(
        &self,
        db: &impl QueryConnection,
        owner: &OwnerId,
        project: &ProjectId,
        memory: &MemoryId,
    ) -> Result<crate::domain_models::memory_tree::LessonStats, RuntimeError> {
        let mut r = db.query("SELECT VALUE basis FROM memory_tree_revision WHERE owner_id=$owner AND project_id=$project AND memory_id=$memory AND basis='concluded' LIMIT 1;").bind(("owner",owner.to_string())).bind(("project",project.to_string())).bind(("memory",memory.to_string())).await.map_err(storage)?.check().map_err(storage)?;
        if !r.take::<Vec<String>>(0).map_err(storage)?.is_empty() {
            return Ok(Default::default());
        }
        let mut r = db.query(format!("LET $linked = (SELECT VALUE source_id FROM memory_node_source WHERE owner_id=$owner AND project_id=$project AND memory_id=$memory); SELECT {SOURCE_PROJECTION} FROM memory_source WHERE owner_id=$owner AND project_id=$project AND source_id IN $linked AND kind='lesson';"))
            .bind(("owner",owner.to_string())).bind(("project",project.to_string())).bind(("memory",memory.to_string()))
            .await.map_err(storage)?.check().map_err(storage)?;
        let rows: Vec<SourceRecord> = r.take(1).map_err(storage)?;
        let mut stats = crate::domain_models::memory_tree::LessonStats::default();
        let mut sessions = BTreeSet::new();
        for row in rows {
            let event = source_event(row)?;
            let id = event.source.as_str();
            // Forwarded IDs retain the original source ID unless it exceeded the ID bound.
            let original = id
                .strip_prefix("promote/")
                .and_then(|tail| tail.split_once('/'))
                .filter(|(hash, _)| hash.len() == 16 && hash.bytes().all(|b| b.is_ascii_hexdigit()))
                .map_or(id, |(_, original)| original);
            let session = original
                .strip_prefix("lesson/")
                .and_then(|tail| tail.split_once('/'))
                .map_or(id, |(session, _)| session);
            sessions.insert(session.to_owned());
            let value: serde_json::Value = serde_json::from_str(&event.content).map_err(storage)?;
            match value.get("kind").and_then(serde_json::Value::as_str) {
                Some("strategy") => stats.strategies += 1,
                Some("pitfall") => stats.pitfalls += 1,
                _ => {}
            }
        }
        stats.sessions = sessions.into_iter().collect();
        Ok(stats)
    }

    pub async fn session_sources(
        &self,
        owner: &OwnerId,
        project: &ProjectId,
        session: &MemorySession,
    ) -> Result<Vec<CaptureEvent>, RuntimeError> {
        let mut r = self.capture.db.query(format!("SELECT {SOURCE_PROJECTION} FROM memory_source WHERE owner_id=$owner AND project_id=$project AND kind IN ['exchange','action'] AND string::starts_with(source_id,$prefix) AND ($through=NONE OR recorded_at_ms>$through) AND recorded_at_ms<=$last ORDER BY recorded_at_ms, source_id;"))
            .bind(("owner",owner.to_string())).bind(("project",project.to_string())).bind(("prefix",format!("{}/",session.session_key.as_str())))
            .bind(("through",session.extracted_through_ms)).bind(("last",session.last_activity_ms))
            .await.map_err(storage)?.check().map_err(storage)?;
        let rows: Vec<SourceRecord> = r.take(0).map_err(storage)?;
        rows.into_iter().map(source_event).collect()
    }
    pub async fn lesson_capacity(
        &self,
        owner: &OwnerId,
        project: &ProjectId,
        max_pending: usize,
    ) -> Result<bool, RuntimeError> {
        let tx = self.capture.db.clone().begin().await.map_err(storage)?;
        let result = self
            .pending_backlog(&tx, owner.as_str(), project.as_str())
            .await
            .map(|n| n < max_pending.saturating_sub(6));
        tx.cancel().await.map_err(storage)?;
        result
    }
    /// Native policy is applied by the service before this transaction. All originals are
    /// rechecked under the writer lock so a concurrent purge cannot leave dangling citations.
    #[allow(clippy::too_many_arguments)]
    pub async fn commit_lessons(
        &self,
        owner: &OwnerId,
        actor: &ActorId,
        project: &ProjectId,
        session: &MemorySession,
        input: &LessonInput,
        answer: &LessonAnswer,
        operation: &str,
        now: u64,
        max_pending: usize,
    ) -> Result<usize, RuntimeError> {
        let _guard = self.capture.writes.lock().await;
        let tx = self.capture.db.clone().begin().await.map_err(storage)?;
        let result = async {
            let originals: Vec<_> = input.sources.iter().cloned().map(SourceRecord::from).collect();
            if originals.iter().any(|s| s.owner_id != owner.as_str() || s.project_id != project.as_str()) { return Err(RuntimeError::Unauthorized); }
            self.require_live_originals(&tx, owner.as_str(), &originals).await?;
            let mut r = tx.query("SELECT VALUE lesson_count FROM memory_session WHERE owner_id=$owner AND project_id=$project AND session_key=$key AND extracted_through_ms=$through AND last_activity_ms>=$last;")
                .bind(("owner",owner.to_string())).bind(("project",project.to_string())).bind(("key",session.session_key.as_str().to_owned())).bind(("through",session.extracted_through_ms)).bind(("last",session.last_activity_ms))
                .await.map_err(storage)?.check().map_err(storage)?;
            let counts: Vec<u64> = r.take(0).map_err(storage)?;
            let counter = *counts.first().ok_or(RuntimeError::Conflict)?;
            // Imports (and other source kinds) may already occupy lesson IDs without updating
            // session bookkeeping. Source identity is unique per owner, including other projects.
            let prefix = format!("lesson/{}/", session.session_key.as_str());
            let mut existing = tx.query("SELECT VALUE source_id FROM memory_source WHERE owner_id=$owner AND string::starts_with(source_id,$prefix);")
                .bind(("owner",owner.to_string())).bind(("prefix",prefix.clone()))
                .await.map_err(storage)?.check().map_err(storage)?;
            let ids: Vec<String> = existing.take(0).map_err(storage)?;
            let count = ids.iter().filter_map(|id| id.strip_prefix(&prefix)?.parse::<u64>().ok()).max().unwrap_or(0).max(counter);
            let through = count.checked_add(answer.lessons.len() as u64).ok_or(RuntimeError::InvalidInput("lesson numbering exhausted"))?;
            let pending = self.pending_backlog(&tx, owner.as_str(), project.as_str()).await?;
            if pending.saturating_add(answer.lessons.len()) > max_pending { return Err(RuntimeError::BacklogFull); }
            for (i, lesson) in answer.lessons.iter().enumerate() {
                let n = count + i as u64 + 1;
                let source = SourceId::new(format!("lesson/{}/{n}",session.session_key.as_str())).map_err(storage)?;
                let evidence: Vec<_> = lesson.evidence.iter().map(|i| &input.sources[*i]).collect();
                let task = &answer.tasks[lesson.task];
                let content = json!({"v":1,"kind":lesson.kind.as_str(),"scope":lesson.scope.as_str(),"title":lesson.title,"applies_when":lesson.applies_when,"guidance":lesson.guidance,"task":task.title,"task_outcome":task.outcome.as_str(),"evidence":evidence.iter().map(|e| e.source.as_str()).collect::<Vec<_>>()}).to_string();
                let event = CaptureEvent { source:source.clone(), owner:owner.clone(), actor:actor.clone(), project:project.clone(), origin:format!("lesson://{}/{n}",session.session_key.as_str()), recorded_at_ms:evidence.iter().map(|e| e.recorded_at_ms).max().ok_or(RuntimeError::ProviderInvalidOutput)?, kind:WorkEventKind::Lesson, outcome:WorkOutcome::Unknown, content };
                self.create_row(&tx,"CREATE memory_source CONTENT $record RETURN NONE;",SourceRecord::from(event)).await?;
                let distinct: BTreeSet<_> = evidence.iter().map(|e|e.source.to_string()).collect();
                for cites in distinct {
                    tx.query("CREATE memory_source_citation CONTENT {owner_id:$owner,project_id:$project,source_id:$source,cites:$cites} RETURN NONE;")
                        .bind(("owner",owner.to_string())).bind(("project",project.to_string())).bind(("source",source.to_string())).bind(("cites",cites))
                        .await.map_err(storage)?.check().map_err(storage)?;
                }
                if lesson.scope == LessonScope::User {
                    self.capture_forward_tx(&tx,owner,project,&source,now).await?;
                    self.create_row(&tx,"CREATE memory_source_consumption CONTENT $record RETURN NONE;",ConsumptionRecord { owner_id:owner.to_string(), project_id:project.to_string(), source_id:source.to_string(), job_id:operation.to_owned() }).await?;
                }
            }
            tx.query("UPDATE memory_session SET extracted_through_ms=$last, lesson_count=$count WHERE owner_id=$owner AND project_id=$project AND session_key=$key RETURN NONE;")
                .bind(("owner",owner.to_string())).bind(("project",project.to_string())).bind(("key",session.session_key.as_str().to_owned())).bind(("last",session.last_activity_ms)).bind(("count",through))
                .await.map_err(storage)?.check().map_err(storage)?;
            Ok(answer.lessons.len())
        }.await;
        match result {
            Ok(n) => {
                tx.commit().await.map_err(storage)?;
                Ok(n)
            }
            Err(e) => {
                let _ = tx.cancel().await;
                Err(e)
            }
        }
    }
    pub async fn lesson_citations(
        &self,
        owner: &OwnerId,
        project: &ProjectId,
        source: &SourceId,
    ) -> Result<Vec<SourceId>, RuntimeError> {
        let mut r = self.capture.db.query("SELECT VALUE cites FROM memory_source_citation WHERE owner_id=$owner AND project_id=$project AND source_id=$source ORDER BY cites;")
            .bind(("owner",owner.to_string())).bind(("project",project.to_string())).bind(("source",source.to_string())).await.map_err(storage)?.check().map_err(storage)?;
        r.take::<Vec<String>>(0)
            .map_err(storage)?
            .into_iter()
            .map(|s| SourceId::new(s).map_err(storage))
            .collect()
    }
    pub(super) async fn expand_lesson_purge(
        &self,
        tx: &Transaction<Db>,
        owner: &OwnerId,
        project: &ProjectId,
        sources: &mut Vec<String>,
    ) -> Result<(), RuntimeError> {
        loop {
            let mut r = tx.query("SELECT VALUE source_id FROM memory_source_citation WHERE owner_id=$owner AND project_id=$project AND cites IN $sources;")
                .bind(("owner",owner.to_string())).bind(("project",project.to_string())).bind(("sources",sources.clone())).await.map_err(storage)?.check().map_err(storage)?;
            let derived: Vec<String> = r.take(0).map_err(storage)?;
            let old = sources.len();
            for s in derived {
                if !sources.contains(&s) {
                    sources.push(s);
                }
            }
            if old == sources.len() {
                return Ok(());
            }
        }
    }
}
