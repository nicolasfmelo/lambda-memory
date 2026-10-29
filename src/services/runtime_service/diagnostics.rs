//! Explicit operation lifetimes, separate known results, and observational read APIs.
use super::*;
use crate::domain_models::runtime_diagnostics::*;
use crate::domain_models::timeline::*;
use crate::domain_models::{ActivityId, WorkerInstanceId};
use std::collections::BTreeMap;
use std::sync::Mutex as StateMutex;
use tokio::sync::OnceCell;

/// Oldest timestamp a history import may claim: 400 days.
const MAX_IMPORT_AGE_MS: u64 = 400 * 24 * 60 * 60 * 1000;
static INSTANCE_SEQUENCE: AtomicU64 = AtomicU64::new(0);
struct LiveEntry {
    id: ActivityId,
    root: ActivityId,
    project: ProjectId,
    phase: ActivityPhase,
    background: bool,
    paid: Option<String>,
}
pub(super) struct Diagnostics {
    instance: WorkerInstanceId,
    started_at_ms: u64,
    sequence: AtomicU64,
    entries: StateMutex<BTreeMap<ActivityId, LiveEntry>>,
    stopped: StateMutex<Option<ActivityReason>>,
    recording_problem: AtomicUsize,
    initialized: OnceCell<()>,
    last_maintenance: AtomicU64,
}
impl Diagnostics {
    pub(super) fn new() -> Result<Self, RuntimeError> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| RuntimeError::InvalidInput("clock precedes epoch"))?;
        let mut hash = Sha256::new();
        hash.update(std::process::id().to_be_bytes());
        hash.update(now.as_nanos().to_be_bytes());
        hash.update(
            INSTANCE_SEQUENCE
                .fetch_add(1, Ordering::SeqCst)
                .to_be_bytes(),
        );
        let instance = WorkerInstanceId::new(format!("{:x}", hash.finalize()))
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        Ok(Self {
            instance,
            started_at_ms: now_ms()?,
            sequence: AtomicU64::new(0),
            entries: StateMutex::new(BTreeMap::new()),
            stopped: StateMutex::new(None),
            recording_problem: AtomicUsize::new(0),
            initialized: OnceCell::new(),
            last_maintenance: AtomicU64::new(0),
        })
    }
    fn next_id(&self) -> Result<ActivityId, RuntimeError> {
        let mut hash = Sha256::new();
        hash.update(self.instance.as_str().as_bytes());
        hash.update(self.sequence.fetch_add(1, Ordering::SeqCst).to_be_bytes());
        ActivityId::new(format!("{:x}", hash.finalize()))
            .map_err(|_| RuntimeError::StorageUnavailable)
    }
}
struct RootState {
    project: ProjectId,
    phase: ActivityPhase,
    counts: ActivityCounts,
}
pub(super) struct OperationTrace<'a> {
    runtime: &'a MemoryRuntime,
    id: ActivityId,
    operation: ActivityOperation,
    roots: Vec<RootState>,
    started: Instant,
    recording_failed: bool,
    observed_failure: Option<ActivityReason>,
}
pub(super) struct OperationStage<'a> {
    runtime: &'a MemoryRuntime,
    id: ActivityId,
    project: ProjectId,
    phase: ActivityPhase,
    started: Instant,
    recorded: bool,
}
impl Drop for OperationTrace<'_> {
    fn drop(&mut self) {
        match self.runtime.diagnostics.entries.lock() {
            Ok(mut entries) => entries.retain(|_, entry| entry.root != self.id),
            Err(_) => self
                .runtime
                .diagnostics
                .recording_problem
                .store(1, Ordering::SeqCst),
        }
    }
}
impl Drop for OperationStage<'_> {
    fn drop(&mut self) {
        match self.runtime.diagnostics.entries.lock() {
            Ok(mut entries) => {
                entries.remove(&self.id);
            }
            Err(_) => self
                .runtime
                .diagnostics
                .recording_problem
                .store(1, Ordering::SeqCst),
        }
    }
}
impl<'a> OperationTrace<'a> {
    pub(super) fn counts(&mut self, project: &ProjectId, counts: &ActivityCounts) {
        if let Some(root) = self.roots.iter_mut().find(|r| &r.project == project) {
            root.counts.accumulate(counts);
        }
    }
    pub(super) fn observed_failure(&mut self, reason: ActivityReason) {
        self.observed_failure = Some(reason);
    }
    pub(super) fn success_commit(
        &self,
        project: &ProjectId,
        phase: ActivityPhase,
        counts: ActivityCounts,
    ) -> Result<ActivityCommit, RuntimeError> {
        Ok(ActivityCommit {
            owner: self.runtime.scope.owner.clone(),
            project: project.clone(),
            finish: ActivityFinish {
                id: self.id.clone(),
                phase,
                outcome: ActivityOutcome::Succeeded,
                finished_at_ms: Some(now_ms()?),
                classified_at_ms: None,
                duration_ms: Some(self.started.elapsed().as_millis().min(u64::MAX as u128) as u64),
                reason: None,
                counts,
            },
            root_finish: None,
        })
    }
    pub(super) async fn stage(
        &mut self,
        project: &ProjectId,
        phase: ActivityPhase,
        sources: &[SourceId],
        revisions: &[RevisionId],
        paid: Option<&str>,
    ) -> Result<OperationStage<'a>, RuntimeError> {
        let id = self.runtime.diagnostics.next_id()?;
        let now = now_ms()?;
        if let Some(root) = self.roots.iter_mut().find(|r| &r.project == project) {
            root.phase = phase;
        }
        {
            let mut entries = self
                .runtime
                .diagnostics
                .entries
                .lock()
                .map_err(|_| RuntimeError::StorageUnavailable)?;
            let background = entries.values().any(|e| e.root == self.id && e.background);
            for root in entries
                .values_mut()
                .filter(|e| e.root == self.id && e.project == *project)
            {
                root.phase = phase;
            }
            entries.insert(
                id.clone(),
                LiveEntry {
                    id: id.clone(),
                    root: self.id.clone(),
                    project: project.clone(),
                    phase,
                    background,
                    paid: None,
                },
            );
        }
        let mut stage = OperationStage {
            runtime: self.runtime,
            id: id.clone(),
            project: project.clone(),
            phase,
            started: Instant::now(),
            recorded: false,
        };
        let entry = ActivityEntry {
            id,
            parent: Some(self.id.clone()),
            project: project.clone(),
            entry_kind: ActivityEntryKind::Stage,
            operation: self.operation,
            phase,
            instance: self.runtime.diagnostics.instance.clone(),
            started_at_ms: now,
            finished_at_ms: None,
            classified_at_ms: None,
            duration_ms: None,
            outcome: ActivityOutcome::Running,
            reason: None,
            counts: ActivityCounts::default(),
        };
        stage.recorded = match self
            .runtime
            .store
            .start_activity(
                &self.runtime.scope.owner,
                ActivityRecord {
                    entry,
                    sources: sources.to_vec(),
                    revisions: revisions.to_vec(),
                    paid_operation: paid.map(ToOwned::to_owned),
                },
            )
            .await
        {
            Ok(ActivityWrite::Recorded) => true,
            Ok(ActivityWrite::Removed) => false,
            Ok(ActivityWrite::AlreadyClosed) => return Err(RuntimeError::Conflict),
            Err(error) => {
                self.runtime
                    .diagnostics
                    .recording_problem
                    .store(1, Ordering::SeqCst);
                return Err(error);
            }
        };
        Ok(stage)
    }
    pub(super) async fn finish_stage<T>(
        &mut self,
        stage: OperationStage<'_>,
        result: &Result<T, RuntimeError>,
        counts: ActivityCounts,
    ) {
        let recorded_counts = if result.is_ok() {
            counts
        } else {
            ActivityCounts::default()
        };
        let finish = ActivityFinish {
            id: stage.id.clone(),
            phase: stage.phase,
            outcome: if result.is_ok() {
                ActivityOutcome::Succeeded
            } else {
                ActivityOutcome::Failed
            },
            finished_at_ms: now_ms().ok(),
            classified_at_ms: None,
            duration_ms: Some(stage.started.elapsed().as_millis().min(u64::MAX as u128) as u64),
            reason: result.as_ref().err().map(ActivityReason::from_error),
            counts: recorded_counts.clone(),
        };
        let completed = if stage.recorded {
            match self
                .runtime
                .store
                .finish_activity(&self.runtime.scope.owner, &stage.project, &finish)
                .await
            {
                Ok(ActivityWrite::AlreadyClosed) => {
                    self.runtime
                        .store
                        .recorded_activity_effect(
                            &self.runtime.scope.owner,
                            &stage.project,
                            &stage.id,
                        )
                        .await
                }
                Ok(ActivityWrite::Recorded) => Ok(Some((finish.outcome, recorded_counts))),
                Ok(ActivityWrite::Removed) => Ok(None),
                Err(error) => Err(error),
            }
        } else {
            Ok(None)
        };
        match completed {
            Ok(Some((ActivityOutcome::Succeeded, counts))) => self.counts(&stage.project, &counts),
            Ok(_) => {}
            Err(_) => {
                self.recording_failed = true;
                self.runtime
                    .diagnostics
                    .recording_problem
                    .store(2, Ordering::SeqCst);
            }
        }
    }
}
impl OperationStage<'_> {
    #[cfg(feature = "router-http")]
    pub(super) fn admitted(&self, paid: &str) -> Result<(), RuntimeError> {
        if let Some(entry) = self
            .runtime
            .diagnostics
            .entries
            .lock()
            .map_err(|_| RuntimeError::StorageUnavailable)?
            .get_mut(&self.id)
        {
            entry.paid = Some(paid.to_owned());
        }
        Ok(())
    }
    pub(super) fn store(
        &self,
        counts: ActivityCounts,
    ) -> Result<EmbeddedRuntimeStore, RuntimeError> {
        if !self.recorded {
            return Ok(self.runtime.store.clone());
        }
        Ok(self.runtime.store.with_activity_commit(ActivityCommit {
            owner: self.runtime.scope.owner.clone(),
            project: self.project.clone(),
            finish: ActivityFinish {
                id: self.id.clone(),
                phase: self.phase,
                outcome: ActivityOutcome::Succeeded,
                finished_at_ms: Some(now_ms()?),
                classified_at_ms: None,
                duration_ms: Some(self.started.elapsed().as_millis().min(u64::MAX as u128) as u64),
                reason: None,
                counts,
            },
            root_finish: None,
        }))
    }
}

impl MemoryRuntime {
    pub async fn initialize_diagnostics(&self) -> Result<(), RuntimeError> {
        self.diagnostics
            .initialized
            .get_or_try_init(|| async {
                self.store
                    .initialize_activity(
                        &self.scope.owner,
                        &self.scope.projects,
                        &self.diagnostics.instance,
                        now_ms()?,
                    )
                    .await
            })
            .await
            .map(|_| ())
    }
    fn read_context(&self, project: &ProjectId) -> Result<ActivityReadContext, RuntimeError> {
        let entries = self
            .diagnostics
            .entries
            .lock()
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        Ok(ActivityReadContext {
            instance: self.diagnostics.instance.clone(),
            active_entries: entries
                .values()
                .filter(|e| &e.project == project)
                .map(|e| e.id.clone())
                .collect(),
            active_paid_operations: entries
                .values()
                .filter(|e| &e.project == project)
                .filter_map(|e| e.paid.clone())
                .collect(),
        })
    }
    pub async fn maintain_diagnostics(&self) -> Result<(), RuntimeError> {
        self.initialize_diagnostics().await?;
        let elapsed = self.elapsed_ms();
        let last = self.diagnostics.last_maintenance.load(Ordering::SeqCst);
        if elapsed.saturating_sub(last) < 60_000 {
            return Ok(());
        }
        if self
            .diagnostics
            .last_maintenance
            .compare_exchange(last, elapsed, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return Ok(());
        }
        for project in &self.scope.projects {
            self.store
                .maintain_activity_with(
                    &self.scope.owner,
                    project,
                    || self.read_context(project),
                    now_ms()?,
                    100,
                )
                .await?;
        }
        Ok(())
    }
    pub(super) fn background_stopped(&self) -> Result<bool, RuntimeError> {
        Ok(self
            .diagnostics
            .stopped
            .lock()
            .map_err(|_| RuntimeError::StorageUnavailable)?
            .is_some())
    }
    pub(super) fn stop_background(&self, reason: ActivityReason) -> Result<(), RuntimeError> {
        *self
            .diagnostics
            .stopped
            .lock()
            .map_err(|_| RuntimeError::StorageUnavailable)? = Some(reason);
        Ok(())
    }
    pub(super) async fn begin_trace(
        &self,
        projects: &[ProjectId],
        operation: ActivityOperation,
        phase: ActivityPhase,
        background: bool,
    ) -> Result<OperationTrace<'_>, RuntimeError> {
        runtime_policy::authorize(&self.scope, projects)?;
        self.initialize_diagnostics().await?;
        let id = self.diagnostics.next_id()?;
        let now = now_ms()?;
        let mut trace = OperationTrace {
            runtime: self,
            id: id.clone(),
            operation,
            roots: Vec::with_capacity(projects.len()),
            started: Instant::now(),
            recording_failed: false,
            observed_failure: None,
        };
        for project in projects {
            // One runtime root is tracked per project; the shared ID is only a correlation handle.
            let live_id = if projects.len() == 1 {
                id.clone()
            } else {
                ActivityId::new(stable_id(&[id.as_str(), project.as_str()]))
                    .map_err(|_| RuntimeError::StorageUnavailable)?
            };
            self.diagnostics
                .entries
                .lock()
                .map_err(|_| RuntimeError::StorageUnavailable)?
                .insert(
                    live_id,
                    LiveEntry {
                        id: id.clone(),
                        root: id.clone(),
                        project: project.clone(),
                        phase,
                        background,
                        paid: None,
                    },
                );
            let entry = ActivityEntry {
                id: id.clone(),
                parent: None,
                project: project.clone(),
                entry_kind: ActivityEntryKind::Operation,
                operation,
                phase,
                instance: self.diagnostics.instance.clone(),
                started_at_ms: now,
                finished_at_ms: None,
                classified_at_ms: None,
                duration_ms: None,
                outcome: ActivityOutcome::Running,
                reason: None,
                counts: ActivityCounts::default(),
            };
            match self
                .store
                .start_activity(
                    &self.scope.owner,
                    ActivityRecord {
                        entry,
                        sources: Vec::new(),
                        revisions: Vec::new(),
                        paid_operation: None,
                    },
                )
                .await
            {
                Ok(ActivityWrite::Recorded) => trace.roots.push(RootState {
                    project: project.clone(),
                    phase,
                    counts: ActivityCounts::default(),
                }),
                Ok(_) => return Err(RuntimeError::Conflict),
                Err(error) => {
                    self.diagnostics
                        .recording_problem
                        .store(1, Ordering::SeqCst);
                    for root in &trace.roots {
                        let finish = ActivityFinish {
                            id: id.clone(),
                            phase,
                            outcome: ActivityOutcome::Failed,
                            finished_at_ms: Some(now),
                            classified_at_ms: None,
                            duration_ms: Some(0),
                            reason: Some(ActivityReason::StorageUnavailable),
                            counts: ActivityCounts::default(),
                        };
                        if self
                            .store
                            .finish_activity(&self.scope.owner, &root.project, &finish)
                            .await
                            .is_err()
                        {
                            self.diagnostics
                                .recording_problem
                                .store(2, Ordering::SeqCst);
                        }
                    }
                    return Err(error);
                }
            }
        }
        Ok(trace)
    }
    pub(super) async fn finish_trace<T>(
        &self,
        trace: OperationTrace<'_>,
        result: Result<T, RuntimeError>,
    ) -> RecordedOperation<T> {
        let mut failed = trace.recording_failed;
        let now = now_ms();
        for root in &trace.roots {
            let reason = trace
                .observed_failure
                .or_else(|| result.as_ref().err().map(ActivityReason::from_error));
            let outcome = if failed {
                ActivityOutcome::Indeterminate
            } else if reason.is_some() {
                ActivityOutcome::Failed
            } else {
                ActivityOutcome::Succeeded
            };
            let finish = ActivityFinish {
                id: trace.id.clone(),
                phase: root.phase,
                outcome,
                finished_at_ms: if outcome == ActivityOutcome::Indeterminate {
                    None
                } else {
                    now.as_ref().ok().copied()
                },
                classified_at_ms: if outcome == ActivityOutcome::Indeterminate {
                    now.as_ref().ok().copied()
                } else {
                    None
                },
                duration_ms: if outcome == ActivityOutcome::Indeterminate {
                    None
                } else {
                    Some(trace.started.elapsed().as_millis().min(u64::MAX as u128) as u64)
                },
                reason: if outcome == ActivityOutcome::Indeterminate {
                    Some(ActivityReason::FinalRecordUnavailable)
                } else {
                    reason
                },
                counts: root.counts.clone(),
            };
            if now.is_err()
                || self
                    .store
                    .finish_activity(&self.scope.owner, &root.project, &finish)
                    .await
                    .is_err()
            {
                failed = true;
                self.diagnostics
                    .recording_problem
                    .store(2, Ordering::SeqCst);
            }
        }
        let diagnostics_warning = failed.then(|| DiagnosticsWarning {
            operation_id: trace.id.clone(),
            reason: ActivityReason::FinalRecordUnavailable,
        });
        RecordedOperation {
            result,
            diagnostics_warning,
        }
    }
    pub async fn status(&self, project: &ProjectId) -> Result<RuntimeStatus, RuntimeError> {
        self.authorize(project)?;
        let now = now_ms()?;
        let context = self.read_context(project)?;
        #[cfg(feature = "router-http")]
        let space = self.providers.as_ref().map(|p| p.space()).transpose()?;
        #[cfg(not(feature = "router-http"))]
        let space: Option<crate::domain_models::embeddings::EmbeddingSpace> = None;
        let stored = self
            .store
            .activity_snapshot(&self.scope.owner, project, space.as_ref(), &context, now)
            .await?;
        let stopped = *self
            .diagnostics
            .stopped
            .lock()
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        let phase = self
            .diagnostics
            .entries
            .lock()
            .map_err(|_| RuntimeError::StorageUnavailable)?
            .values()
            .find(|e| &e.project == project && e.background)
            .map(|e| e.phase);
        let foreground = self.foreground.load(Ordering::SeqCst);
        let remaining = self.limits.idle_ms.saturating_sub(
            self.elapsed_ms()
                .saturating_sub(self.last_activity.load(Ordering::SeqCst)),
        );
        let state = if phase.is_some() {
            DreamState::Running
        } else if stopped.is_some() {
            DreamState::Stopped
        } else if foreground != 0 {
            DreamState::WaitingForeground
        } else if remaining != 0 {
            DreamState::WaitingIdle
        } else if stored.pending.unconsumed_sources > stored.pending.blocked_sources
            || stored.pending.processing_revisions > stored.pending.blocked_revisions
        {
            DreamState::Ready
        } else {
            DreamState::Idle
        };
        #[cfg(feature = "router-http")]
        let mode = if self.providers.is_some() {
            "openrouter"
        } else {
            "local"
        };
        #[cfg(not(feature = "router-http"))]
        let mode = "local";
        let recording_reason = match self.diagnostics.recording_problem.load(Ordering::SeqCst) {
            0 => None,
            1 => Some(ActivityReason::StorageUnavailable),
            _ => Some(ActivityReason::FinalRecordUnavailable),
        };
        Ok(RuntimeStatus {
            project: project.clone(),
            mode,
            observed_at_ms: now,
            worker: WorkerStatus {
                instance: self.diagnostics.instance.clone(),
                started_at_ms: self.diagnostics.started_at_ms,
                foreground_active: foreground,
                dreaming: DreamStatus {
                    state,
                    phase,
                    stop_reason: stopped,
                    idle_ms: self.limits.idle_ms,
                    tick_ms: self.limits.tick_ms,
                    dream_batch: self.limits.dream_batch,
                    remaining_idle_ms: if matches!(
                        state,
                        DreamState::WaitingForeground | DreamState::Running | DreamState::Stopped
                    ) {
                        None
                    } else {
                        Some(remaining)
                    },
                },
            },
            stored,
            recording_state: if recording_reason.is_some() {
                DiagnosticRecordingState::Degraded
            } else {
                DiagnosticRecordingState::Ok
            },
            recording_reason,
        })
    }
    pub async fn activity(&self, request: ActivityRequest) -> Result<ActivityPage, RuntimeError> {
        self.authorize(&request.project)?;
        let context = self.read_context(&request.project)?;
        self.store
            .activity_page(&self.scope.owner, &request, &context, now_ms()?)
            .await
    }
    pub async fn capture(
        &self,
        project: &ProjectId,
        source: SourceId,
        kind: WorkEventKind,
        outcome: WorkOutcome,
        origin: String,
        content: String,
    ) -> RecordedOperation<CaptureReceipt> {
        self.capture_at(project, source, kind, outcome, origin, content, None)
            .await
    }
    /// Like `capture`, with an optional caller-supplied timestamp (history imports).
    /// The value must lie within `[now - 400 days, now]`; `None` uses the server clock.
    #[allow(clippy::too_many_arguments)]
    pub async fn capture_at(
        &self,
        project: &ProjectId,
        source: SourceId,
        kind: WorkEventKind,
        outcome: WorkOutcome,
        origin: String,
        content: String,
        recorded_at_ms: Option<u64>,
    ) -> RecordedOperation<CaptureReceipt> {
        let _foreground = self.enter();
        let mut trace = match self
            .begin_trace(
                std::slice::from_ref(project),
                ActivityOperation::Capture,
                ActivityPhase::Capture,
                false,
            )
            .await
        {
            Ok(v) => v,
            Err(e) => {
                return RecordedOperation {
                    result: Err(e),
                    diagnostics_warning: None,
                };
            }
        };
        let result = async {
            let event = CaptureEvent {
                source,
                owner: self.scope.owner.clone(),
                actor: self.scope.actor.clone(),
                project: project.clone(),
                kind,
                outcome,
                origin,
                content,
                recorded_at_ms: match recorded_at_ms {
                    None => now_ms()?,
                    Some(at) => {
                        let now = now_ms()?;
                        if at > now || at < now.saturating_sub(MAX_IMPORT_AGE_MS) {
                            return Err(RuntimeError::InvalidInput("recorded_at_ms out of range"));
                        }
                        at
                    }
                },
            };
            self.capture_effect(event, &mut trace).await
        }
        .await;
        if let Ok(receipt) = &result {
            trace.counts(
                project,
                &ActivityCounts {
                    sources_created: Some(u64::from(receipt.status == CaptureStatus::SavedPending)),
                    ..Default::default()
                },
            );
        }
        self.finish_trace(trace, result).await
    }
    pub async fn propose(&self, proposal: RevisionProposal) -> RecordedOperation<RuntimeRevision> {
        let _foreground = self.enter();
        let project = proposal.project.clone();
        let mut trace = match self
            .begin_trace(
                std::slice::from_ref(&project),
                ActivityOperation::Propose,
                ActivityPhase::Propose,
                false,
            )
            .await
        {
            Ok(v) => v,
            Err(e) => {
                return RecordedOperation {
                    result: Err(e),
                    diagnostics_warning: None,
                };
            }
        };
        let result = self.propose_effect(proposal, &mut trace).await;
        self.finish_trace(trace, result).await
    }
    pub async fn timeline_write(&self, draft: TimelineDraft) -> RecordedOperation<TimelineReceipt> {
        let _foreground = self.enter();
        let project = draft.project.clone();
        let mut trace = match self
            .begin_trace(
                std::slice::from_ref(&project),
                ActivityOperation::Timeline,
                ActivityPhase::Timeline,
                false,
            )
            .await
        {
            Ok(v) => v,
            Err(e) => {
                return RecordedOperation {
                    result: Err(e),
                    diagnostics_warning: None,
                };
            }
        };
        let result = self.timeline_write_effect(draft, &mut trace).await;
        self.finish_trace(trace, result).await
    }
    /// Observational read: no foreground accounting, so it never postpones idle dreaming.
    pub async fn timeline(&self, query: TimelineQuery) -> Result<TimelinePage, RuntimeError> {
        self.authorize(&query.project)?;
        if !(1..=TIMELINE_MAX_PAGE).contains(&query.limit) {
            return Err(RuntimeError::InvalidInput("timeline limit"));
        }
        self.store.timeline_page(&self.scope.owner, &query).await
    }
    pub async fn review(&self, request: ReviewRequest) -> RecordedOperation<RuntimeRevision> {
        let _foreground = self.enter();
        let project = request.project.clone();
        let mut trace = match self
            .begin_trace(
                std::slice::from_ref(&project),
                ActivityOperation::Review,
                ActivityPhase::Review,
                false,
            )
            .await
        {
            Ok(v) => v,
            Err(e) => {
                return RecordedOperation {
                    result: Err(e),
                    diagnostics_warning: None,
                };
            }
        };
        let result = self.review_effect(request, &mut trace).await;
        self.finish_trace(trace, result).await
    }
    pub async fn list(&self, query: ListQuery) -> RecordedOperation<ListPage> {
        let _foreground = self.enter();
        let trace = match self
            .begin_trace(
                std::slice::from_ref(&query.project),
                ActivityOperation::List,
                ActivityPhase::List,
                false,
            )
            .await
        {
            Ok(v) => v,
            Err(e) => {
                return RecordedOperation {
                    result: Err(e),
                    diagnostics_warning: None,
                };
            }
        };
        let result = self.list_effect(&query).await;
        self.finish_trace(trace, result).await
    }
    pub async fn recall(
        &self,
        task: &str,
        projects: &[ProjectId],
        budget: usize,
    ) -> RecordedOperation<RuntimeRecall> {
        let _foreground = self.enter();
        let mut trace = match self
            .begin_trace(
                projects,
                ActivityOperation::Recall,
                ActivityPhase::Preparation,
                false,
            )
            .await
        {
            Ok(v) => v,
            Err(e) => {
                return RecordedOperation {
                    result: Err(e),
                    diagnostics_warning: None,
                };
            }
        };
        let result = self.recall_effect(task, projects, budget, &mut trace).await;
        if let Ok(recall) = &result {
            for project in projects {
                let local: Vec<_> = recall
                    .hits
                    .iter()
                    .filter(|h| &h.item.revision.project == project)
                    .collect();
                let sources = local
                    .iter()
                    .flat_map(|h| h.item.revision.sources.iter().cloned())
                    .collect::<std::collections::BTreeSet<_>>()
                    .into_iter()
                    .collect::<Vec<_>>();
                let revisions = local
                    .iter()
                    .map(|h| h.item.revision.id.clone())
                    .collect::<Vec<_>>();
                if self
                    .store
                    .link_activity(
                        &self.scope.owner,
                        project,
                        &trace.id,
                        &sources,
                        &revisions,
                        None,
                    )
                    .await
                    .is_err()
                {
                    trace.recording_failed = true;
                }
            }
        }
        self.finish_trace(trace, result).await
    }
    pub async fn purge(
        &self,
        project: &ProjectId,
        sources: &[SourceId],
    ) -> RecordedOperation<PurgeReceipt> {
        let _foreground = self.enter();
        let mut trace = match self
            .begin_trace(
                std::slice::from_ref(project),
                ActivityOperation::Purge,
                ActivityPhase::Purge,
                false,
            )
            .await
        {
            Ok(v) => v,
            Err(e) => {
                return RecordedOperation {
                    result: Err(e),
                    diagnostics_warning: None,
                };
            }
        };
        let result = self.purge_effect(project, sources, &mut trace).await;
        if let Ok(receipt) = &result {
            trace.counts(
                project,
                &ActivityCounts {
                    sources_removed: Some(receipt.sources as u64),
                    revisions_removed: Some(receipt.revisions as u64),
                    jobs_removed: Some(receipt.jobs as u64),
                    ..Default::default()
                },
            );
        }
        self.finish_trace(trace, result).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain_models::{ActorId, OwnerId};
    use crate::integrations::database::embedded_capture::EmbeddedCaptureStore;
    static NEXT: AtomicU64 = AtomicU64::new(0);
    async fn runtime() -> MemoryRuntime {
        let path = format!(
            "{}/lambda-diagnostic-fault-{}-{}",
            std::env::temp_dir().display(),
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        );
        MemoryRuntime::new(
            EmbeddedRuntimeStore::new(EmbeddedCaptureStore::open(&path).await.unwrap()),
            RuntimeScope {
                owner: OwnerId::new("owner").unwrap(),
                actor: ActorId::new("actor").unwrap(),
                projects: vec![ProjectId::new("p").unwrap(), ProjectId::new("q").unwrap()],
            },
            RuntimeLimits::default(),
        )
        .unwrap()
    }
    #[tokio::test]
    async fn status_pending_counts_only_curatable_sources() {
        let runtime = runtime().await;
        let project = ProjectId::new("p").unwrap();
        for (id, kind, origin) in [
            ("action", WorkEventKind::Action, "claude-code://action"),
            ("legacy", WorkEventKind::Result, "omp://tool_result"),
            ("decision", WorkEventKind::Decision, "fixture"),
            ("exchange", WorkEventKind::Exchange, "codex://exchange"),
        ] {
            runtime
                .capture(
                    &project,
                    SourceId::new(id).unwrap(),
                    kind,
                    WorkOutcome::Unknown,
                    origin.into(),
                    format!("{id} source"),
                )
                .await
                .result
                .unwrap();
        }
        let status = runtime.status(&project).await.unwrap();
        assert_eq!(status.stored.pending.unconsumed_sources, 2);
    }
    #[tokio::test]
    async fn journal_start_failure_prevents_the_new_capture_effect() {
        let runtime = runtime().await;
        let project = ProjectId::new("p").unwrap();
        runtime
            .capture(
                &project,
                SourceId::new("first").unwrap(),
                WorkEventKind::Decision,
                WorkOutcome::Unknown,
                "fixture".into(),
                "first source".into(),
            )
            .await
            .result
            .unwrap();
        // A genuine unique-index failure in journal admission, without a production fault hook.
        runtime.diagnostics.sequence.store(0, Ordering::SeqCst);
        let second = runtime
            .capture(
                &project,
                SourceId::new("second").unwrap(),
                WorkEventKind::Decision,
                WorkOutcome::Unknown,
                "fixture".into(),
                "second source".into(),
            )
            .await;
        assert_eq!(second.result.unwrap_err(), RuntimeError::StorageUnavailable);
        let status = runtime.status(&project).await.unwrap();
        assert_eq!(status.stored.pending.unconsumed_sources, 1);
        assert_eq!(status.recording_state, DiagnosticRecordingState::Degraded);
        assert_eq!(
            runtime
                .store
                .pending_sources(&runtime.scope.owner, &project, 8, false)
                .await
                .unwrap()[0]
                .source
                .as_str(),
            "first"
        );
    }
    #[tokio::test]
    async fn refused_final_record_preserves_a_confirmed_effect_and_exposes_uncertainty() {
        let runtime = runtime().await;
        let project = ProjectId::new("p").unwrap();
        let mut trace = runtime
            .begin_trace(
                std::slice::from_ref(&project),
                ActivityOperation::Capture,
                ActivityPhase::Capture,
                false,
            )
            .await
            .unwrap();
        let stage = trace
            .stage(&project, ActivityPhase::Preparation, &[], &[], None)
            .await
            .unwrap();
        let effect = runtime
            .store
            .capture(
                CaptureEvent {
                    source: SourceId::new("confirmed").unwrap(),
                    owner: runtime.scope.owner.clone(),
                    actor: runtime.scope.actor.clone(),
                    project: project.clone(),
                    origin: "fixture".into(),
                    content: "confirmed source".into(),
                    recorded_at_ms: now_ms().unwrap(),
                    kind: WorkEventKind::Decision,
                    outcome: WorkOutcome::Unknown,
                },
                &runtime.limits,
            )
            .await;
        // An unfinished child makes the real datastore refuse a false successful root.
        let recorded = runtime.finish_trace(trace, effect).await;
        assert_eq!(recorded.result.unwrap().status, CaptureStatus::SavedPending);
        assert_eq!(
            recorded.diagnostics_warning.unwrap().reason,
            ActivityReason::FinalRecordUnavailable
        );
        drop(stage);
        let status = runtime.status(&project).await.unwrap();
        assert_eq!(status.stored.pending.unconsumed_sources, 1);
        assert_eq!(status.recording_state, DiagnosticRecordingState::Degraded);
        let page = runtime
            .activity(ActivityRequest {
                project,
                limit: 32,
                operations: Vec::new(),
                outcomes: Vec::new(),
                cursor: None,
            })
            .await
            .unwrap();
        assert!(
            page.entries
                .iter()
                .all(|entry| entry.outcome == ActivityOutcome::Indeterminate)
        );
    }
    #[tokio::test]
    async fn correlated_project_roots_remain_running_only_while_owned() {
        let runtime = runtime().await;
        let projects = runtime.scope.projects.clone();
        let trace = runtime
            .begin_trace(
                &projects,
                ActivityOperation::Recall,
                ActivityPhase::Preparation,
                false,
            )
            .await
            .unwrap();
        for project in &projects {
            let page = runtime
                .activity(ActivityRequest {
                    project: project.clone(),
                    limit: 32,
                    operations: Vec::new(),
                    outcomes: vec![ActivityOutcome::Running],
                    cursor: None,
                })
                .await
                .unwrap();
            assert_eq!(page.entries[0].id, trace.id);
            assert_eq!(page.entries[0].outcome, ActivityOutcome::Running);
        }
        drop(trace);
        for project in projects {
            let page = runtime
                .activity(ActivityRequest {
                    project,
                    limit: 32,
                    operations: Vec::new(),
                    outcomes: vec![ActivityOutcome::Indeterminate],
                    cursor: None,
                })
                .await
                .unwrap();
            assert_eq!(page.entries[0].outcome, ActivityOutcome::Indeterminate);
        }
    }
}
