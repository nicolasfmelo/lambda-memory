//! Operator-only embedding recovery; previous paid admissions are immutable evidence.
use super::*;
use crate::domain_models::embeddings::EmbeddingSpace;
use crate::domain_models::runtime_embedding::{EmbeddingRecoveryItem, EmbeddingRecoveryPlan};
use crate::integrations::runtime_embeddings::{digest, legacy_operation, operation, snapshot};

impl EmbeddedRuntimeStore {
    pub(super) async fn embedding_admissions(
        &self,
        connection: &impl QueryConnection,
        owner: &OwnerId,
        project: &ProjectId,
        revision: &RevisionId,
        space: &EmbeddingSpace,
    ) -> Result<Vec<String>, RuntimeError> {
        let base = operation(owner, project, revision, space);
        let mut response = connection.query("SELECT VALUE operation_id FROM memory_runtime_operation WHERE owner_id = $owner AND project_id = $project AND (operation_id = $base OR operation_id = $legacy OR string::starts_with(operation_id, $recovery)) ORDER BY operation_id;")
            .bind(("owner", owner.to_string())).bind(("project", project.to_string()))
            .bind(("legacy", legacy_operation(revision, space))).bind(("recovery", format!("{base}:recovery:")))
            .bind(("base", base)).await.map_err(storage)?.check().map_err(storage)?;
        response.take(0).map_err(storage)
    }
    async fn embedding_original(
        &self,
        connection: &impl QueryConnection,
        owner: &OwnerId,
        project: &ProjectId,
        revision: &RevisionId,
    ) -> Result<RuntimeRevision, RuntimeError> {
        let record = self
            .revision_row(connection, owner.as_str(), revision.as_str())
            .await?
            .ok_or(RuntimeError::Missing)?;
        if record.project_id != project.as_str() {
            return Err(RuntimeError::Unauthorized);
        }
        let (states, jobs) = self
            .selected_metadata(owner, std::iter::once(&record))
            .await?;
        let item = self.assemble(record, &states, &jobs)?;
        if !matches!(
            item.state,
            LifecycleState::Processing | LifecycleState::Active
        ) {
            return Err(RuntimeError::Conflict);
        }
        self.require_live_originals(
            connection,
            owner.as_str(),
            &item
                .sources
                .iter()
                .cloned()
                .map(SourceRecord::from)
                .collect::<Vec<_>>(),
        )
        .await?;
        Ok(item)
    }
    pub async fn admit_runtime_embedding(
        &self,
        wanted: &RuntimeRevision,
        space: &EmbeddingSpace,
    ) -> Result<(), RuntimeError> {
        let _guard = self.capture.writes.lock().await;
        let tx = self.capture.db.clone().begin().await.map_err(storage)?;
        let result = async {
            let r = &wanted.revision;
            let current = self.embedding_original(&tx, &r.owner, &r.project, &r.id).await?;
            if current.revision != wanted.revision || current.sources != wanted.sources { return Err(RuntimeError::Conflict); }
            if !self.embedding_admissions(&tx, &r.owner, &r.project, &r.id, space).await?.is_empty() {
                return Err(RuntimeError::UncertainAdmission);
            }
            tx.query("CREATE memory_runtime_operation SET owner_id = $owner, project_id = $project, operation_id = $operation, sources = $sources RETURN NONE;")
                .bind(("owner", r.owner.to_string())).bind(("project", r.project.to_string()))
                .bind(("operation", operation(&r.owner, &r.project, &r.id, space)))
                .bind(("sources", r.sources.iter().map(ToString::to_string).collect::<Vec<_>>()))
                .await.map_err(storage)?.check().map_err(storage)?;
            Ok(())
        }.await;
        match result {
            Ok(()) => tx.commit().await.map(|_| ()).map_err(storage),
            Err(e) => cancel_err(tx, e).await,
        }
    }
    /// Content-free plan, bound to immutable originals and exact previous admissions.
    pub async fn embedding_recovery_plan(
        &self,
        owner: &OwnerId,
        project: &ProjectId,
        space: &EmbeddingSpace,
        limit: usize,
    ) -> Result<EmbeddingRecoveryPlan, RuntimeError> {
        if limit == 0 || limit > 16 {
            return Err(RuntimeError::InvalidInput("embedding recovery bound"));
        }
        let _guard = self.capture.writes.lock().await;
        let mut response = self.capture.db.query("SELECT VALUE revision_id FROM memory_revision WHERE owner_id = $owner AND project_id = $project ORDER BY revision_id;")
            .bind(("owner", owner.to_string())).bind(("project", project.to_string())).await.map_err(storage)?.check().map_err(storage)?;
        let ids: Vec<String> = response.take(0).map_err(storage)?;
        let mut items = Vec::new();
        let mut admission_count = 0;
        for id in ids {
            let revision = RevisionId::new(id).map_err(|_| RuntimeError::StorageUnavailable)?;
            let previous = self
                .embedding_admissions(&self.capture.db, owner, project, &revision, space)
                .await?;
            if previous.is_empty() {
                continue;
            }
            if previous.len() > 32 {
                return Err(RuntimeError::InvalidInput(
                    "embedding recovery admission bound",
                ));
            }
            let item = match self
                .embedding_original(&self.capture.db, owner, project, &revision)
                .await
            {
                Ok(item) => item,
                Err(RuntimeError::Conflict | RuntimeError::Missing) => continue,
                Err(e) => return Err(e),
            };
            if self.has_runtime_embedding(&item, space).await? {
                continue;
            }
            if admission_count + previous.len() > 32 {
                continue;
            }
            admission_count += previous.len();
            items.push(EmbeddingRecoveryItem {
                revision,
                snapshot: snapshot(&item),
                previous_operations: previous
                    .into_iter()
                    .map(|id| JobId::new(id).map_err(|_| RuntimeError::StorageUnavailable))
                    .collect::<Result<_, _>>()?,
            });
            if items.len() == limit {
                break;
            }
        }
        Ok(EmbeddingRecoveryPlan {
            space: space.clone(),
            items,
        })
    }
    /// Revalidate the whole consent set transactionally before any new provider call.
    pub async fn admit_embedding_recovery(
        &self,
        owner: &OwnerId,
        actor: &ActorId,
        project: &ProjectId,
        plan: &EmbeddingRecoveryPlan,
        authorized_at_ms: u64,
    ) -> Result<Vec<(RuntimeRevision, String)>, RuntimeError> {
        if plan.items.is_empty()
            || plan.items.len() > 16
            || authorized_at_ms == 0
            || plan
                .items
                .iter()
                .map(|item| item.previous_operations.len())
                .sum::<usize>()
                > 32
            || plan.items.iter().enumerate().any(|(i, item)| {
                plan.items[..i]
                    .iter()
                    .any(|old| old.revision == item.revision)
            })
        {
            return Err(RuntimeError::InvalidInput(
                "embedding recovery authorization",
            ));
        }
        let _guard = self.capture.writes.lock().await;
        let tx = self.capture.db.clone().begin().await.map_err(storage)?;
        let result = async {
            let mut admitted = Vec::with_capacity(plan.items.len());
            for expected in &plan.items {
                if expected.previous_operations.is_empty() || expected.previous_operations.len() > 32 {
                    return Err(RuntimeError::InvalidInput("embedding recovery admissions"));
                }
                let current = self.embedding_original(&tx, owner, project, &expected.revision).await?;
                if snapshot(&current) != expected.snapshot || self.has_runtime_embedding(&current, &plan.space).await? {
                    return Err(RuntimeError::Conflict);
                }
                let actual = self.embedding_admissions(&tx, owner, project, &expected.revision, &plan.space).await?;
                let mut previous = expected.previous_operations.iter().map(ToString::to_string).collect::<Vec<_>>();
                previous.sort();
                if actual != previous { return Err(RuntimeError::UncertainAdmission); }
                let mut parts = vec![expected.snapshot.as_str()];
                parts.extend(previous.iter().map(String::as_str));
                let operation = format!("{}:recovery:{}", operation(owner, project, &expected.revision, &plan.space), digest(&parts));
                tx.query("CREATE memory_runtime_operation SET owner_id = $owner, project_id = $project, operation_id = $operation, sources = $sources, recovery_of = $previous, authorized_by_actor = $actor, authorized_at_ms = $at RETURN NONE;")
                    .bind(("owner", owner.to_string())).bind(("project", project.to_string())).bind(("operation", operation.clone()))
                    .bind(("sources", current.revision.sources.iter().map(ToString::to_string).collect::<Vec<_>>()))
                    .bind(("previous", previous)).bind(("actor", actor.to_string())).bind(("at", authorized_at_ms))
                    .await.map_err(storage)?.check().map_err(storage)?;
                admitted.push((current, operation));
            }
            Ok(admitted)
        }.await;
        match result {
            Ok(items) => {
                tx.commit().await.map_err(storage)?;
                Ok(items)
            }
            Err(e) => cancel_err(tx, e).await,
        }
    }
}
