//! Authorized foreground operations and cooperative source-bound idle consolidation.
use crate::domain_models::capture::*;
use crate::domain_models::consolidation::*;
use crate::domain_models::procedural::ActionEpisode;
use crate::domain_models::runtime::*;
use crate::domain_models::timeline::{TimelineDraft, TimelineReceipt};
use crate::domain_models::{JobId, MemoryId, ProjectId, RevisionId, SourceId};
use crate::engines::{capture_policy, consolidation_policy, runtime_policy, timeline_policy};
use crate::integrations::database::embedded_capture::EmbeddedRuntimeStore;
use sha2::{Digest, Sha256};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::Mutex;

mod curation;
mod diagnostics;
mod dreaming;
#[cfg(feature = "router-http")]
mod embedding_maintenance;
#[cfg(feature = "router-http")]
mod openrouter;
mod procedural;
pub mod tree_migration;
#[cfg(feature = "router-http")]
mod tree_recall;
use crate::domain_models::runtime_diagnostics::{ActivityCounts, ActivityPhase};
use diagnostics::{Diagnostics, OperationTrace};

pub struct MemoryRuntime {
    store: EmbeddedRuntimeStore,
    scope: RuntimeScope,
    limits: RuntimeLimits,
    started: Instant,
    last_activity: AtomicU64,
    foreground: AtomicUsize,
    background: Mutex<()>,
    diagnostics: Diagnostics,
    #[cfg(feature = "router-http")]
    providers: Option<crate::integrations::external_apis::router_runtime::RouterRuntimeProviders>,
    #[cfg(feature = "router-http")]
    indexing: Mutex<()>,
    /// Last playbook snapshot key evaluated per project; an unchanged snapshot is not re-read.
    #[cfg(feature = "router-http")]
    playbook_evaluated: std::sync::Mutex<std::collections::BTreeMap<ProjectId, String>>,
    /// Full 5,000-episode reads done by `playbook_plan`, to pin the evaluation cache.
    #[cfg(all(test, feature = "router-http"))]
    playbook_full_reads: AtomicUsize,
}
struct Foreground<'a>(&'a MemoryRuntime);
impl Drop for Foreground<'_> {
    fn drop(&mut self) {
        self.0
            .last_activity
            .store(self.0.elapsed_ms(), Ordering::SeqCst);
        self.0.foreground.fetch_sub(1, Ordering::SeqCst);
    }
}
fn now_ms() -> Result<u64, RuntimeError> {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| RuntimeError::InvalidInput("clock precedes epoch"))?
        .as_millis();
    u64::try_from(millis).map_err(|_| RuntimeError::InvalidInput("clock exceeds supported range"))
}
/// Preparation passes skip revisions that cannot be promoted; their conflict was already reported.
fn prepared(result: Result<RuntimeRevision, RuntimeError>) -> Result<(), RuntimeError> {
    match result {
        Ok(_) | Err(RuntimeError::Conflict) => Ok(()),
        Err(error) => Err(error),
    }
}
fn stable_id(parts: &[&str]) -> String {
    let mut hash = Sha256::new();
    for part in parts {
        hash.update((part.len() as u64).to_be_bytes());
        hash.update(part.as_bytes());
    }
    format!("{:x}", hash.finalize())
}
impl MemoryRuntime {
    pub fn new(
        store: EmbeddedRuntimeStore,
        scope: RuntimeScope,
        limits: RuntimeLimits,
    ) -> Result<Self, RuntimeError> {
        runtime_policy::validate_config(&scope, &limits)?;
        Ok(Self {
            store,
            scope,
            limits,
            started: Instant::now(),
            last_activity: AtomicU64::new(0),
            foreground: AtomicUsize::new(0),
            background: Mutex::new(()),
            diagnostics: Diagnostics::new()?,
            #[cfg(feature = "router-http")]
            providers: None,
            #[cfg(feature = "router-http")]
            indexing: Mutex::new(()),
            #[cfg(feature = "router-http")]
            playbook_evaluated: std::sync::Mutex::default(),
            #[cfg(all(test, feature = "router-http"))]
            playbook_full_reads: AtomicUsize::new(0),
        })
    }
    pub fn scope(&self) -> &RuntimeScope {
        &self.scope
    }
    pub fn limits(&self) -> &RuntimeLimits {
        &self.limits
    }
    fn elapsed_ms(&self) -> u64 {
        self.started.elapsed().as_millis().min(u64::MAX as u128) as u64
    }
    fn enter(&self) -> Foreground<'_> {
        self.foreground.fetch_add(1, Ordering::SeqCst);
        self.last_activity
            .store(self.elapsed_ms(), Ordering::SeqCst);
        Foreground(self)
    }
    fn idle(&self) -> bool {
        self.foreground.load(Ordering::SeqCst) == 0
            && self
                .elapsed_ms()
                .saturating_sub(self.last_activity.load(Ordering::SeqCst))
                >= self.limits.idle_ms
    }
    /// Structural and secret check of a parsed action episode; pure, no state touched.
    pub fn validate_action(&self, episode: &ActionEpisode) -> Result<(), RuntimeError> {
        let leaked = [
            Some(&episode.intent),
            Some(&episode.summary),
            episode.error_head.as_ref(),
        ]
        .into_iter()
        .flatten()
        .any(|text| capture_policy::contains_secret(text));
        if leaked || capture_policy::validate_action(episode).is_err() {
            return Err(RuntimeError::InvalidInput("invalid action episode"));
        }
        Ok(())
    }
    pub fn validate_exchange(&self, prompt: &str, reply: &str) -> Result<(), RuntimeError> {
        capture_policy::validate_exchange(prompt, reply)
            .map_err(|_| RuntimeError::InvalidInput("invalid exchange"))
    }
    fn authorize(&self, project: &ProjectId) -> Result<(), RuntimeError> {
        runtime_policy::authorize(&self.scope, std::slice::from_ref(project))
    }
    async fn capture_effect(
        &self,
        event: CaptureEvent,
        trace: &mut OperationTrace<'_>,
    ) -> Result<CaptureReceipt, RuntimeError> {
        self.authorize(&event.project)?;
        capture_policy::validate(
            &event,
            &CaptureLimits {
                max_content_bytes: self.limits.max_content_bytes,
                max_pending_per_owner: self.limits.max_pending,
            },
        )
        .map_err(|_| RuntimeError::InvalidInput("invalid capture evidence"))?;
        let commit = trace.success_commit(
            &event.project,
            ActivityPhase::Capture,
            ActivityCounts {
                sources_created: Some(1),
                ..Default::default()
            },
        )?;
        self.store
            .with_activity_commit(commit)
            .capture(event, &self.limits)
            .await
    }
    async fn propose_effect(
        &self,
        proposal: RevisionProposal,
        trace: &mut OperationTrace<'_>,
    ) -> Result<RuntimeRevision, RuntimeError> {
        self.authorize(&proposal.project)?;
        if proposal.content.trim().is_empty()
            || proposal.content.len() > self.limits.max_content_bytes
            || proposal.sources.is_empty()
            || proposal.sources.len() > 32
            || proposal
                .sources
                .iter()
                .enumerate()
                .any(|(i, s)| proposal.sources[..i].contains(s))
        {
            return Err(RuntimeError::InvalidInput("invalid source-backed proposal"));
        }
        let project = proposal.project.clone();
        let stage = trace
            .stage(
                &project,
                ActivityPhase::Propose,
                &proposal.sources,
                &[],
                None,
            )
            .await?;
        let counts = ActivityCounts {
            revisions_created: Some(1),
            ..Default::default()
        };
        let result = stage
            .store(counts.clone())?
            .propose(&self.scope.owner, &self.scope.actor, proposal, now_ms()?)
            .await;
        trace.finish_stage(stage, &result, counts).await;
        self.prepare_revision(result?, trace).await
    }
    /// Pure bounds first: the store does not reject duplicate links or oversized text.
    async fn timeline_write_effect(
        &self,
        draft: TimelineDraft,
        trace: &mut OperationTrace<'_>,
    ) -> Result<TimelineReceipt, RuntimeError> {
        self.authorize(&draft.project)?;
        timeline_policy::validate_draft(&draft, now_ms()?, self.limits.max_content_bytes)?;
        let project = draft.project.clone();
        let stage = trace
            .stage(
                &project,
                ActivityPhase::Timeline,
                &draft.sources,
                &draft.revisions,
                None,
            )
            .await?;
        let counts = ActivityCounts::default();
        let result = stage
            .store(counts.clone())?
            .timeline_write(&self.scope.owner, &self.scope.actor, draft, now_ms()?)
            .await;
        trace.finish_stage(stage, &result, counts).await;
        result
    }
    async fn prepare_revision(
        &self,
        item: RuntimeRevision,
        trace: &mut OperationTrace<'_>,
    ) -> Result<RuntimeRevision, RuntimeError> {
        if item.state != LifecycleState::Processing {
            return Ok(item);
        }
        // The playbook is injected by fixed lookup, never recalled, so it gets no vector.
        #[cfg(feature = "router-http")]
        if let Some(providers) = &self.providers
            && item.revision.memory.as_str() != crate::domain_models::procedural::PLAYBOOK_MEMORY
        {
            self.index_revision(providers, &item, trace).await?;
        }
        // Lineage survives rejection, but a rejected predecessor is not an active head.
        let expected_active = match &item.revision.predecessor {
            Some(predecessor) => {
                let parent = self
                    .store
                    .runtime_revision(&self.scope.owner, &item.revision.project, predecessor)
                    .await?;
                (parent.state != LifecycleState::Rejected).then(|| predecessor.clone())
            }
            None => None,
        };
        let project = item.revision.project.clone();
        let stage = trace
            .stage(
                &project,
                ActivityPhase::Publication,
                &item.revision.sources,
                std::slice::from_ref(&item.revision.id),
                None,
            )
            .await?;
        let counts = ActivityCounts {
            revisions_published: Some(1),
            ..Default::default()
        };
        let result = stage
            .store(counts.clone())?
            .review(
                &self.scope.owner,
                &self.scope.actor,
                &ReviewRequest {
                    project,
                    revision: item.revision.id.clone(),
                    action: ReviewAction::Promote,
                    expected_active,
                    conflict_with: None,
                },
                now_ms()?,
            )
            .await;
        trace.finish_stage(stage, &result, counts).await;
        if matches!(result, Err(RuntimeError::Conflict)) {
            // A successor that lost its promotion race is retired, so it cannot block later passes.
            self.store
                .retire_stale(
                    &self.scope.owner,
                    &self.scope.actor,
                    &item.revision.project,
                    &item.revision.id,
                    now_ms()?,
                )
                .await?;
        }
        result
    }
    async fn prepare_project(
        &self,
        project: &ProjectId,
        limit: usize,
        trace: &mut OperationTrace<'_>,
    ) -> Result<usize, RuntimeError> {
        let items = self
            .store
            .list(
                &self.scope.owner,
                project,
                &[LifecycleState::Processing],
                limit,
            )
            .await?;
        let count = items.len();
        for item in items {
            prepared(self.prepare_revision(item, trace).await)?;
        }
        Ok(count)
    }
    async fn review_effect(
        &self,
        request: ReviewRequest,
        trace: &mut OperationTrace<'_>,
    ) -> Result<RuntimeRevision, RuntimeError> {
        self.authorize(&request.project)?;
        if (request.action == ReviewAction::Conflict) != request.conflict_with.is_some() {
            return Err(RuntimeError::InvalidInput(
                "conflict requires exactly one counterpart",
            ));
        }
        #[cfg(feature = "router-http")]
        if let Some(providers) = &self.providers
            && request.action == ReviewAction::Promote
        {
            let item = self
                .store
                .runtime_revision(&self.scope.owner, &request.project, &request.revision)
                .await?;
            // The playbook is injected by fixed lookup, never recalled, so it gets no vector.
            if item.revision.memory.as_str() != crate::domain_models::procedural::PLAYBOOK_MEMORY {
                self.index_revision(providers, &item, trace).await?;
            }
        }
        let phase = if request.action == ReviewAction::Promote {
            ActivityPhase::Publication
        } else {
            ActivityPhase::Review
        };
        let counts = if request.action == ReviewAction::Promote {
            ActivityCounts {
                revisions_published: Some(1),
                ..Default::default()
            }
        } else {
            ActivityCounts::default()
        };
        let stage = trace
            .stage(
                &request.project,
                phase,
                &[],
                std::slice::from_ref(&request.revision),
                None,
            )
            .await?;
        let result = stage
            .store(counts.clone())?
            .review(&self.scope.owner, &self.scope.actor, &request, now_ms()?)
            .await;
        trace.finish_stage(stage, &result, counts).await;
        result
    }
    async fn list_effect(&self, query: &ListQuery) -> Result<ListPage, RuntimeError> {
        self.authorize(&query.project)?;
        if query.states.is_empty() || query.states.len() > 5 || query.limit == 0 || query.limit > 32
        {
            return Err(RuntimeError::InvalidInput("invalid review listing bounds"));
        }
        // Lineage reads carry their lifecycle audit; plain listings stay lean.
        self.store
            .list_page(&self.scope.owner, query, query.memory.is_some())
            .await
    }
    async fn recall_effect(
        &self,
        task: &str,
        projects: &[ProjectId],
        budget_bytes: usize,
        trace: &mut OperationTrace<'_>,
    ) -> Result<RuntimeRecall, RuntimeError> {
        runtime_policy::authorize(&self.scope, projects)?;
        if budget_bytes > self.limits.context_bytes {
            return Err(RuntimeError::InvalidInput(
                "context budget exceeds configured cap",
            ));
        }
        let terms = runtime_policy::recall_terms(task)?;
        if terms.is_empty() || budget_bytes == 0 {
            #[cfg(feature = "router-http")]
            if let Some(providers) = &self.providers {
                return self
                    .recall_openrouter(providers, task, projects, 0, trace)
                    .await;
            }
            return Ok(runtime_policy::pack(Vec::new(), 0));
        }
        for project in projects {
            self.prepare_project(project, 4, trace).await?;
            if !self
                .store
                .list(&self.scope.owner, project, &[LifecycleState::Processing], 1)
                .await?
                .is_empty()
            {
                return Err(RuntimeError::InvalidInput(
                    "automatic preparation catch-up incomplete",
                ));
            }
        }
        #[cfg(feature = "router-http")]
        if let Some(providers) = &self.providers {
            return self
                .recall_openrouter(providers, task, projects, budget_bytes, trace)
                .await;
        }
        let mut hits = Vec::new();
        for project in projects {
            let stage = trace
                .stage(project, ActivityPhase::Retrieval, &[], &[], None)
                .await?;
            let result = self
                .store
                .recall(
                    &self.scope.owner,
                    std::slice::from_ref(project),
                    &terms,
                    self.limits.max_candidates,
                )
                .await;
            trace
                .finish_stage(stage, &result, ActivityCounts::default())
                .await;
            hits.extend(result?);
        }
        let result = runtime_policy::pack(hits, budget_bytes);
        for project in projects {
            let stage = trace
                .stage(project, ActivityPhase::Packing, &[], &[], None)
                .await?;
            let count = result
                .hits
                .iter()
                .filter(|hit| &hit.item.revision.project == project)
                .count() as u64;
            trace
                .finish_stage(
                    stage,
                    &Ok::<(), RuntimeError>(()),
                    ActivityCounts {
                        hits_returned: Some(count),
                        ..Default::default()
                    },
                )
                .await;
        }
        Ok(result)
    }
    async fn purge_effect(
        &self,
        project: &ProjectId,
        sources: &[SourceId],
        trace: &mut OperationTrace<'_>,
    ) -> Result<PurgeReceipt, RuntimeError> {
        self.authorize(project)?;
        if sources.is_empty()
            || sources.len() > 32
            || sources
                .iter()
                .enumerate()
                .any(|(i, s)| sources[..i].contains(s))
        {
            return Err(RuntimeError::InvalidInput(
                "purge requires 1..32 unique source IDs",
            ));
        }
        let commit =
            trace.success_commit(project, ActivityPhase::Purge, ActivityCounts::default())?;
        self.store
            .with_activity_commit(commit)
            .purge(&self.scope.owner, project, sources)
            .await
    }
}

impl MemoryRuntime {
    pub async fn node(
        &self,
        project: &ProjectId,
        node: &MemoryId,
        history: bool,
        sources: bool,
        cursor: Option<String>,
    ) -> Result<crate::domain_models::memory_tree::NodeView, RuntimeError> {
        use crate::domain_models::memory_tree::{NodeSource, NodeView, TreeRoot};
        self.authorize(project)?;
        let owner = &self.scope.owner;
        let item = self
            .store
            .tree_nodes(owner, project)
            .await?
            .into_iter()
            .find(|n| &n.memory == node)
            .ok_or(RuntimeError::Missing)?;
        let label = match item.root {
            TreeRoot::Decisions => "Decisions",
            TreeRoot::Preferences => "Preferences",
            TreeRoot::Architecture => "Architecture",
            TreeRoot::OpenItems => "Open items",
        };
        let mut path = vec![label.to_owned()];
        path.extend(
            self.store
                .tree_ancestors(owner, project, node)
                .await?
                .into_iter()
                .map(|n| n.title),
        );
        path.push(item.title.clone());
        let children = self
            .store
            .tree_children(owner, project, node)
            .await?
            .into_iter()
            .map(|n| (n.memory, n.title))
            .collect();
        let history = if history {
            self.store.node_history(owner, project, node, 32).await?
        } else {
            Vec::new()
        };
        let (sources, next_cursor) = if sources {
            self.store
                .node_sources(owner, project, node, cursor, 32)
                .await?
        } else {
            (Vec::new(), None)
        };
        let sources = sources
            .into_iter()
            .map(|(event, operation, revision, linked_at_ms)| NodeSource {
                event,
                operation,
                revision,
                linked_at_ms,
            })
            .collect();
        Ok(NodeView {
            node: item,
            path,
            children,
            history,
            sources,
            next_cursor,
        })
    }
}
