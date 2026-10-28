//! Real SurrealKV capture adapter. SDK DTOs and transactions stay private.
use crate::domain_models::capture::*;
use crate::domain_models::{ActorId, MemoryError, OwnerId, ProjectId, SourceId};
use crate::integrations::capture_store::AsyncCaptureStore;
use std::sync::Arc;
use surrealdb::{
    Surreal,
    engine::local::{Db, SurrealKv},
    types::SurrealValue,
};
use tokio::sync::Mutex;
const SOURCE_PROJECTION: &str =
    "owner_id, source_id, actor_id, project_id, origin, recorded_at_ms, kind, outcome, content";
mod knowledge;
pub use knowledge::EmbeddedKnowledgeStore;
#[cfg(feature = "memory-runtime")]
mod runtime;
#[cfg(feature = "memory-runtime")]
pub use runtime::EmbeddedRuntimeStore;
const MIGRATION: &str = include_str!("../../../migrations/0001_capture.surql");
#[derive(Clone)]
pub struct EmbeddedCaptureStore {
    db: Surreal<Db>,
    writes: Arc<Mutex<()>>,
}
#[derive(Clone, Debug, PartialEq, Eq, SurrealValue)]
struct SourceRecord {
    owner_id: String,
    source_id: String,
    actor_id: String,
    project_id: String,
    origin: String,
    recorded_at_ms: u64,
    kind: String,
    outcome: String,
    content: String,
}
#[derive(Debug, SurrealValue)]
struct MigrationRecord {
    version: u64,
    body: String,
}
impl From<CaptureEvent> for SourceRecord {
    fn from(event: CaptureEvent) -> Self {
        Self {
            owner_id: event.owner.to_string(),
            source_id: event.source.to_string(),
            actor_id: event.actor.to_string(),
            project_id: event.project.to_string(),
            origin: event.origin,
            recorded_at_ms: event.recorded_at_ms,
            kind: match event.kind {
                WorkEventKind::Attempt => "attempt",
                WorkEventKind::Result => "result",
                WorkEventKind::Error => "error",
                WorkEventKind::Decision => "decision",
                WorkEventKind::Action => "action",
                WorkEventKind::Exchange => "exchange",
            }
            .into(),
            outcome: match event.outcome {
                WorkOutcome::Unknown => "unknown",
                WorkOutcome::Succeeded => "succeeded",
                WorkOutcome::Failed => "failed",
            }
            .into(),
            content: event.content,
        }
    }
}
impl SourceRecord {
    fn into_domain(self) -> Result<CaptureEvent, CaptureError> {
        let kind = match self.kind.as_str() {
            "attempt" => WorkEventKind::Attempt,
            "result" => WorkEventKind::Result,
            "error" => WorkEventKind::Error,
            "decision" => WorkEventKind::Decision,
            "action" => WorkEventKind::Action,
            "exchange" => WorkEventKind::Exchange,
            _ => return Err(CaptureError::StorageUnavailable),
        };
        let outcome = match self.outcome.as_str() {
            "unknown" => WorkOutcome::Unknown,
            "succeeded" => WorkOutcome::Succeeded,
            "failed" => WorkOutcome::Failed,
            _ => return Err(CaptureError::StorageUnavailable),
        };
        Ok(CaptureEvent {
            source: SourceId::new(self.source_id).map_err(|_| CaptureError::StorageUnavailable)?,
            owner: OwnerId::new(self.owner_id).map_err(|_| CaptureError::StorageUnavailable)?,
            actor: ActorId::new(self.actor_id).map_err(|_| CaptureError::StorageUnavailable)?,
            project: ProjectId::new(self.project_id)
                .map_err(|_| CaptureError::StorageUnavailable)?,
            kind,
            outcome,
            origin: self.origin,
            recorded_at_ms: self.recorded_at_ms,
            content: self.content,
        })
    }
}
impl EmbeddedCaptureStore {
    async fn open_raw(path: &str) -> surrealdb::Result<Self> {
        let db = Surreal::new::<SurrealKv>(path).await?;
        db.use_ns("lambda_memory").use_db("experiment").await?;
        db.query("DEFINE TABLE IF NOT EXISTS memory_schema_migration SCHEMAFULL; DEFINE FIELD IF NOT EXISTS version ON memory_schema_migration TYPE int; DEFINE FIELD IF NOT EXISTS body ON memory_schema_migration TYPE string; DEFINE INDEX IF NOT EXISTS migration_version ON memory_schema_migration FIELDS version UNIQUE;").await?.check()?;
        Ok(Self {
            db,
            writes: Arc::new(Mutex::new(())),
        })
    }
    pub async fn open(path: &str) -> Result<Self, MemoryError> {
        if path.trim().is_empty() {
            return Err(MemoryError::InvalidInput("empty datastore path"));
        }
        let store = Self::open_raw(path)
            .await
            .map_err(|_| MemoryError::StorageUnavailable)?;
        store.migrate(1, MIGRATION).await?;
        store
            .migrate(2, include_str!("../../../migrations/0002_knowledge.surql"))
            .await?;
        store
            .migrate(
                3,
                include_str!("../../../migrations/0003_experiment_index.surql"),
            )
            .await?;
        // Runtime tables only. Capture/knowledge/experiment migrations and pending counts stay unchanged.
        #[cfg(feature = "memory-runtime")]
        store
            .migrate(4, include_str!("../../../migrations/0004_runtime.surql"))
            .await?;
        #[cfg(feature = "memory-runtime")]
        store
            .migrate(
                5,
                include_str!("../../../migrations/0005_runtime_semantic.surql"),
            )
            .await?;
        #[cfg(feature = "memory-runtime")]
        store
            .migrate(
                6,
                include_str!("../../../migrations/0006_runtime_automatic.surql"),
            )
            .await?;
        #[cfg(feature = "memory-runtime")]
        store
            .migrate(
                7,
                include_str!("../../../migrations/0007_runtime_diagnostics.surql"),
            )
            .await?;
        #[cfg(feature = "memory-runtime")]
        store
            .migrate(
                8,
                include_str!("../../../migrations/0008_runtime_recovery.surql"),
            )
            .await?;
        #[cfg(feature = "memory-runtime")]
        store
            .migrate(
                9,
                include_str!("../../../migrations/0009_runtime_scope_indexes.surql"),
            )
            .await?;
        #[cfg(feature = "memory-runtime")]
        store
            .migrate(10, include_str!("../../../migrations/0010_timeline.surql"))
            .await?;
        #[cfg(feature = "memory-runtime")]
        store
            .migrate(
                11,
                include_str!("../../../migrations/0011_memory_tree.surql"),
            )
            .await?;
        store
            .migrate(
                12,
                include_str!("../../../migrations/0012_tree_migration_progress.surql"),
            )
            .await?;
        Ok(store)
    }
    /// Exact body fingerprint avoids silently applying an edited migration version.
    async fn migrate(&self, version: u64, body: &str) -> Result<(), MemoryError> {
        let _guard = self.writes.lock().await;
        let tx = self
            .db
            .clone()
            .begin()
            .await
            .map_err(|_| MemoryError::StorageUnavailable)?;
        let mut response = tx
            .query("SELECT version, body FROM memory_schema_migration WHERE version = $version;")
            .bind(("version", version))
            .await
            .map_err(|_| MemoryError::StorageUnavailable)?
            .check()
            .map_err(|_| MemoryError::StorageUnavailable)?;
        let existing: Vec<MigrationRecord> = response
            .take(0)
            .map_err(|_| MemoryError::StorageUnavailable)?;
        if let Some(record) = existing.first() {
            let matches = record.body == body;
            tx.cancel()
                .await
                .map_err(|_| MemoryError::StorageUnavailable)?;
            return if matches {
                Ok(())
            } else {
                Err(MemoryError::InvalidInput("migration body mismatch"))
            };
        }
        if tx
            .query(body)
            .await
            .and_then(|response| response.check())
            .is_err()
        {
            let _ = tx.cancel().await;
            return Err(MemoryError::StorageUnavailable);
        }
        if tx
            .query("CREATE memory_schema_migration CONTENT $record RETURN NONE;")
            .bind((
                "record",
                MigrationRecord {
                    version,
                    body: body.into(),
                },
            ))
            .await
            .and_then(|response| response.check())
            .is_err()
        {
            let _ = tx.cancel().await;
            return Err(MemoryError::StorageUnavailable);
        }
        tx.commit()
            .await
            .map_err(|_| MemoryError::StorageUnavailable)?;
        Ok(())
    }
}
impl AsyncCaptureStore for EmbeddedCaptureStore {
    async fn save_pending_async(
        &self,
        event: CaptureEvent,
        capacity: usize,
    ) -> Result<CaptureWrite, CaptureError> {
        let _guard = self.writes.lock().await;
        let source = SourceRecord::from(event);
        let tx = self
            .db
            .clone()
            .begin()
            .await
            .map_err(|_| CaptureError::StorageUnavailable)?;
        let sql = format!(
            "SELECT {SOURCE_PROJECTION} FROM memory_source WHERE owner_id = $owner AND source_id = $source;"
        );
        let mut response = tx
            .query(sql)
            .bind(("owner", source.owner_id.clone()))
            .bind(("source", source.source_id.clone()))
            .await
            .map_err(|_| CaptureError::StorageUnavailable)?
            .check()
            .map_err(|_| CaptureError::StorageUnavailable)?;
        let existing: Vec<SourceRecord> = response
            .take(0)
            .map_err(|_| CaptureError::StorageUnavailable)?;
        if let Some(old) = existing.first() {
            let result = if old == &source {
                CaptureWrite::ExistingIdentical
            } else {
                CaptureWrite::Conflict
            };
            tx.cancel()
                .await
                .map_err(|_| CaptureError::StorageUnavailable)?;
            return Ok(result);
        }
        let mut count = tx
            .query("RETURN array::len(SELECT id FROM memory_source WHERE owner_id = $owner);")
            .bind(("owner", source.owner_id.clone()))
            .await
            .map_err(|_| CaptureError::StorageUnavailable)?
            .check()
            .map_err(|_| CaptureError::StorageUnavailable)?;
        let pending: Option<u64> = count
            .take(0)
            .map_err(|_| CaptureError::StorageUnavailable)?;
        let pending = pending.ok_or(CaptureError::StorageUnavailable)?;
        if pending >= capacity as u64 {
            tx.cancel()
                .await
                .map_err(|_| CaptureError::StorageUnavailable)?;
            return Ok(CaptureWrite::Full);
        }
        if tx
            .query("CREATE memory_source CONTENT $source RETURN NONE;")
            .bind(("source", source))
            .await
            .and_then(|response| response.check())
            .is_err()
        {
            let _ = tx.cancel().await;
            return Err(CaptureError::StorageUnavailable);
        }
        tx.commit()
            .await
            .map_err(|_| CaptureError::StorageUnavailable)?;
        Ok(CaptureWrite::Inserted)
    }
    async fn pending_async(&self, owner: &OwnerId) -> Result<Vec<PendingEvidence>, CaptureError> {
        let sql = format!(
            "SELECT {SOURCE_PROJECTION} FROM memory_source WHERE owner_id = $owner ORDER BY recorded_at_ms, source_id;"
        );
        let mut response = self
            .db
            .query(sql)
            .bind(("owner", owner.to_string()))
            .await
            .map_err(|_| CaptureError::StorageUnavailable)?
            .check()
            .map_err(|_| CaptureError::StorageUnavailable)?;
        let records: Vec<SourceRecord> = response
            .take(0)
            .map_err(|_| CaptureError::StorageUnavailable)?;
        records
            .into_iter()
            .map(|record| {
                record.into_domain().map(|event| PendingEvidence {
                    event,
                    provisional: true,
                })
            })
            .collect()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    fn path() -> String {
        format!(
            "{}/lambda-memory-schema-{}-{}",
            std::env::temp_dir().display(),
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        )
    }
    #[tokio::test]
    async fn knowledge_migration_statement_validation() {
        let store = EmbeddedCaptureStore::open_raw(&path()).await.unwrap();
        store
            .db
            .query(include_str!("../../../migrations/0002_knowledge.surql"))
            .await
            .unwrap()
            .check()
            .unwrap();
    }
    #[tokio::test]
    async fn migration_replay_mismatch_and_failure_rollback() {
        let store = EmbeddedCaptureStore::open_raw(&path()).await.unwrap();
        store.migrate(1, MIGRATION).await.unwrap();
        store.migrate(1, MIGRATION).await.unwrap();
        assert_eq!(
            store.migrate(1, "DEFINE TABLE changed;").await,
            Err(MemoryError::InvalidInput("migration body mismatch"))
        );
        store
            .db
            .query("DEFINE TABLE rollback_probe SCHEMALESS;")
            .await
            .unwrap()
            .check()
            .unwrap();
        let failed = "CREATE rollback_probe:one SET marker = 'temporary'; THROW 'synthetic migration failure';";
        assert_eq!(
            store.migrate(2, failed).await,
            Err(MemoryError::StorageUnavailable)
        );
        let mut response=store.db.query("RETURN array::len(SELECT id FROM rollback_probe); RETURN array::len(SELECT id FROM memory_schema_migration WHERE version = 2);").await.unwrap().check().unwrap();
        assert_eq!(response.take::<Option<u64>>(0).unwrap(), Some(0));
        assert_eq!(response.take::<Option<u64>>(1).unwrap(), Some(0));
    }
    #[tokio::test]
    async fn datastore_lock_is_exclusive_and_close_reopens() {
        let path = path();
        let first = EmbeddedCaptureStore::open(&path).await.unwrap();
        assert!(EmbeddedCaptureStore::open(&path).await.is_err());
        drop(first);
        // The SDK worker may release its lock asynchronously after final handle drop.
        let mut reopened = None;
        for _ in 0..20 {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            if let Ok(store) = EmbeddedCaptureStore::open(&path).await {
                reopened = Some(store);
                break;
            }
        }
        assert!(
            reopened.is_some(),
            "datastore lock did not release after close"
        );
    }
    #[tokio::test]
    async fn readonly_source_content_cannot_be_overwritten() {
        let store = EmbeddedCaptureStore::open(&path()).await.unwrap();
        store.db.query("CREATE memory_source CONTENT {owner_id:'owner',source_id:'source',actor_id:'actor',project_id:'project',origin:'fixture://schema',recorded_at_ms:1,kind:'attempt',outcome:'failed',content:'original'};").await.unwrap().check().unwrap();
        let _ = store
            .db
            .query("UPDATE memory_source SET content = 'overwritten' WHERE owner_id = 'owner';")
            .await
            .unwrap()
            .check();
        let mut response = store
            .db
            .query("SELECT VALUE content FROM memory_source WHERE owner_id = 'owner';")
            .await
            .unwrap()
            .check()
            .unwrap();
        assert_eq!(response.take::<Vec<String>>(0).unwrap(), vec!["original"]);
    }
    #[cfg(feature = "memory-runtime")]
    #[tokio::test]
    async fn automatic_migration_preserves_legacy_claims_and_original_outcomes() {
        use crate::domain_models::runtime::{LifecycleState, RevisionProposal, RuntimeLimits};
        use crate::domain_models::{MemoryId, RevisionId};
        let store = EmbeddedCaptureStore::open_raw(&path()).await.unwrap();
        for (version, body) in [
            (1, MIGRATION),
            (2, include_str!("../../../migrations/0002_knowledge.surql")),
            (
                3,
                include_str!("../../../migrations/0003_experiment_index.surql"),
            ),
            (4, include_str!("../../../migrations/0004_runtime.surql")),
            (
                5,
                include_str!("../../../migrations/0005_runtime_semantic.surql"),
            ),
        ] {
            store.migrate(version, body).await.unwrap();
        }
        let runtime = EmbeddedRuntimeStore::new(store.clone());
        let owner = OwnerId::new("owner").unwrap();
        let actor = ActorId::new("actor").unwrap();
        let project = ProjectId::new("project").unwrap();
        let event = CaptureEvent {
            owner: owner.clone(),
            actor: actor.clone(),
            project: project.clone(),
            source: SourceId::new("legacy-source").unwrap(),
            kind: WorkEventKind::Error,
            outcome: WorkOutcome::Failed,
            origin: "fixture://legacy".into(),
            recorded_at_ms: 1,
            content: "The original migration failed.".into(),
        };
        runtime
            .capture(event.clone(), &RuntimeLimits::default())
            .await
            .unwrap();
        runtime
            .propose(
                &owner,
                &actor,
                RevisionProposal {
                    project: project.clone(),
                    memory: MemoryId::new("legacy-memory").unwrap(),
                    revision: RevisionId::new("legacy-revision").unwrap(),
                    sources: vec![event.source.clone()],
                    content: event.content.clone(),
                    predecessor: None,
                },
                1,
            )
            .await
            .unwrap();
        store
            .db
            .query("UPDATE memory_revision_state SET state = 'pending_review' RETURN NONE;")
            .await
            .unwrap()
            .check()
            .unwrap();
        store
            .migrate(
                6,
                include_str!("../../../migrations/0006_runtime_automatic.surql"),
            )
            .await
            .unwrap();
        let migrated = runtime
            .list(&owner, &project, &[LifecycleState::Processing], 8)
            .await
            .unwrap();
        assert_eq!(migrated[0].revision.id.as_str(), "legacy-revision");
        assert_eq!(migrated[0].revision.content, event.content);
        assert_eq!(migrated[0].sources, vec![event]);
        store
            .migrate(
                6,
                include_str!("../../../migrations/0006_runtime_automatic.surql"),
            )
            .await
            .unwrap();
        assert_eq!(
            runtime
                .list(&owner, &project, &[LifecycleState::Processing], 8)
                .await
                .unwrap()[0]
                .revision
                .id,
            migrated[0].revision.id
        );
    }
    #[cfg(feature = "memory-runtime")]
    #[tokio::test]
    async fn recovery_migration_keeps_legacy_admissions_and_readonly_provenance() {
        let store = EmbeddedCaptureStore::open_raw(&path()).await.unwrap();
        for (version, body) in [
            (1, MIGRATION),
            (2, include_str!("../../../migrations/0002_knowledge.surql")),
            (
                3,
                include_str!("../../../migrations/0003_experiment_index.surql"),
            ),
            (4, include_str!("../../../migrations/0004_runtime.surql")),
            (
                5,
                include_str!("../../../migrations/0005_runtime_semantic.surql"),
            ),
            (
                6,
                include_str!("../../../migrations/0006_runtime_automatic.surql"),
            ),
            (
                7,
                include_str!("../../../migrations/0007_runtime_diagnostics.surql"),
            ),
        ] {
            store.migrate(version, body).await.unwrap();
        }
        store.db.query("CREATE memory_runtime_operation SET owner_id = 'o', project_id = 'p', operation_id = 'dream-job:legacy', sources = ['s1'];").await.unwrap().check().unwrap();
        store
            .migrate(
                8,
                include_str!("../../../migrations/0008_runtime_recovery.surql"),
            )
            .await
            .unwrap();
        store
            .migrate(
                9,
                include_str!("../../../migrations/0009_runtime_scope_indexes.surql"),
            )
            .await
            .unwrap();
        let mut response = store.db.query("SELECT VALUE recovery_of FROM memory_runtime_operation WHERE operation_id = 'dream-job:legacy';").await.unwrap().check().unwrap();
        assert_eq!(
            response.take::<Vec<Option<Vec<String>>>>(0).unwrap(),
            vec![None]
        );
        store.db.query("CREATE memory_runtime_operation SET owner_id = 'o', project_id = 'p', operation_id = 'dream-job:recovery', sources = ['s1'], recovery_of = ['dream-job:legacy'], authorized_by_actor = 'operator', authorized_at_ms = 5;").await.unwrap().check().unwrap();
        let _ = store.db.query("UPDATE memory_runtime_operation SET recovery_of = [], authorized_by_actor = 'forged' WHERE operation_id = 'dream-job:recovery';").await.unwrap().check();
        let mut response = store.db.query("SELECT VALUE [recovery_of, authorized_by_actor] FROM memory_runtime_operation WHERE operation_id = 'dream-job:recovery';").await.unwrap().check().unwrap();
        let provenance: Vec<serde_json::Value> = response.take(0).unwrap();
        assert_eq!(
            provenance,
            vec![serde_json::json!([["dream-job:legacy"], "operator"])]
        );
    }
    #[cfg(feature = "memory-runtime")]
    #[tokio::test]
    async fn timeline_migration_covers_sources_and_revisions_written_under_schema_9() {
        use crate::domain_models::runtime::{RevisionProposal, RuntimeLimits};
        use crate::domain_models::timeline::*;
        use crate::domain_models::{MemoryId, RevisionId};
        let store = EmbeddedCaptureStore::open_raw(&path()).await.unwrap();
        for (version, body) in [
            (1, MIGRATION),
            (2, include_str!("../../../migrations/0002_knowledge.surql")),
            (
                3,
                include_str!("../../../migrations/0003_experiment_index.surql"),
            ),
            (4, include_str!("../../../migrations/0004_runtime.surql")),
            (
                5,
                include_str!("../../../migrations/0005_runtime_semantic.surql"),
            ),
            (
                6,
                include_str!("../../../migrations/0006_runtime_automatic.surql"),
            ),
            (
                7,
                include_str!("../../../migrations/0007_runtime_diagnostics.surql"),
            ),
            (
                8,
                include_str!("../../../migrations/0008_runtime_recovery.surql"),
            ),
            (
                9,
                include_str!("../../../migrations/0009_runtime_scope_indexes.surql"),
            ),
        ] {
            store.migrate(version, body).await.unwrap();
        }
        let runtime = EmbeddedRuntimeStore::new(store.clone());
        let owner = OwnerId::new("owner").unwrap();
        let actor = ActorId::new("actor").unwrap();
        let project = ProjectId::new("project").unwrap();
        let at = 1_760_000_000_000;
        let event = |id: &str, kind, offset| CaptureEvent {
            owner: owner.clone(),
            actor: actor.clone(),
            project: project.clone(),
            source: SourceId::new(id).unwrap(),
            kind,
            outcome: WorkOutcome::Unknown,
            origin: "fixture://legacy".into(),
            recorded_at_ms: at + offset,
            content: format!("Legacy {id}."),
        };
        // Written before migration 10 exists: two decisions and one non-decision, plus a revision.
        for source in [
            event("d1", WorkEventKind::Decision, 1),
            event("d2", WorkEventKind::Decision, 2),
            event("n1", WorkEventKind::Error, 3),
        ] {
            runtime
                .capture(source, &RuntimeLimits::default())
                .await
                .unwrap();
        }
        runtime
            .propose(
                &owner,
                &actor,
                RevisionProposal {
                    project: project.clone(),
                    memory: MemoryId::new("m1").unwrap(),
                    revision: RevisionId::new("r1").unwrap(),
                    sources: vec![SourceId::new("d1").unwrap()],
                    content: "Legacy d1.".into(),
                    predecessor: None,
                },
                at + 5,
            )
            .await
            .unwrap();
        store
            .migrate(10, include_str!("../../../migrations/0010_timeline.surql"))
            .await
            .unwrap();
        let query = TimelineQuery {
            project: project.clone(),
            limit: 32,
            after: None,
            statuses: vec![],
            descending: false,
            entry: None,
        };
        let page = runtime.timeline_page(&owner, &query).await.unwrap();
        assert!(page.entries.is_empty());
        let uncovered: Vec<_> = page.uncovered.iter().map(|u| u.source.as_str()).collect();
        assert_eq!(
            uncovered,
            vec!["d2", "d1"],
            "pre-existing decision sources are uncovered after the upgrade"
        );
        // Pre-existing sources, revisions and memories are linkable.
        let draft = TimelineDraft {
            project: project.clone(),
            entry: TimelineEntryId::new("legacy").unwrap(),
            expected_version: None,
            title: "Legacy decision".into(),
            summary: "Recorded before the timeline existed.".into(),
            occurred_at_ms: at,
            status: TimelineStatus::Adopted,
            sources: vec![SourceId::new("d1").unwrap()],
            revisions: vec![RevisionId::new("r1").unwrap()],
            memories: vec![MemoryId::new("m1").unwrap()],
            supersedes: None,
            tags: vec![],
        };
        assert_eq!(
            runtime
                .timeline_write(&owner, &actor, draft, at + 10)
                .await
                .unwrap()
                .write,
            TimelineWrite::Recorded
        );
        let page = runtime.timeline_page(&owner, &query).await.unwrap();
        assert_eq!(
            page.uncovered
                .iter()
                .map(|u| u.source.as_str())
                .collect::<Vec<_>>(),
            vec!["d2"]
        );
        store
            .migrate(10, include_str!("../../../migrations/0010_timeline.surql"))
            .await
            .unwrap();
    }
}
