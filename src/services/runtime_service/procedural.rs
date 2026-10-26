//! Idle playbook generation: deterministic facts, one paid phrasing call, a pure citation gate.
use super::*;
use crate::domain_models::procedural::PlaybookView;
#[cfg(feature = "router-http")]
use crate::domain_models::procedural::{EpisodeRecord, FactKind, PLAYBOOK_MEMORY, ProceduralFact};
#[cfg(feature = "router-http")]
use crate::domain_models::runtime_diagnostics::ActivityReason;
#[cfg(feature = "router-http")]
use crate::engines::procedural_policy;
#[cfg(feature = "router-http")]
use crate::integrations::action_episodes;

/// The aggregation never reads more than this many episodes.
#[cfg(feature = "router-http")]
const EPISODE_LIMIT: usize = 5_000;
#[cfg(feature = "router-http")]
const NEW_EPISODES: usize = 20;
#[cfg(feature = "router-http")]
const REFRESH_AFTER_MS: u64 = 24 * 60 * 60 * 1000;
#[cfg(feature = "router-http")]
const MAX_SOURCES: usize = 32;

/// Everything a generation attempt needs; built once the trigger fired and the paid operation
/// was admitted, so one invalid or unknown attempt is never repeated for the same snapshot.
#[cfg(feature = "router-http")]
pub(super) struct PlaybookPlan {
    facts: Vec<ProceduralFact>,
    records: Vec<(SourceId, EpisodeRecord)>,
    active: Option<RuntimeRevision>,
    newest: SourceId,
    job: String,
}
#[cfg(feature = "router-http")]
impl PlaybookPlan {
    fn operation(&self) -> String {
        format!("playbook-job:{}", self.job)
    }
}

impl MemoryRuntime {
    /// Read-only: no foreground entry, no provider call, so it never postpones idle dreaming.
    pub async fn playbook(&self, project: &ProjectId) -> Result<PlaybookView, RuntimeError> {
        self.authorize(project)?;
        let active = self
            .store
            .active_playbook(&self.scope.owner, project)
            .await?;
        let episodes = self
            .store
            .action_episode_count(&self.scope.owner, project)
            .await?;
        Ok(match active {
            Some(item) => PlaybookView {
                generated_at_ms: Some(item.reviewed_at_ms.unwrap_or(item.watermark_ms)),
                revision: Some(item.revision.id),
                content: Some(item.revision.content),
                episodes,
            },
            None => PlaybookView {
                episodes,
                ..PlaybookView::default()
            },
        })
    }
}

#[cfg(feature = "router-http")]
impl MemoryRuntime {
    /// Decides whether a generation is due and, if so, admits its paid operation. `None` when
    /// running locally, not due, nothing to phrase, or this exact snapshot was already attempted
    /// (the admission persists, so a rejected or lost attempt is not repeated automatically).
    pub(super) async fn playbook_plan(
        &self,
        project: &ProjectId,
        now: u64,
    ) -> Result<Option<PlaybookPlan>, RuntimeError> {
        if self.providers.is_none() {
            return Ok(None);
        }
        let active = self
            .store
            .active_playbook(&self.scope.owner, project)
            .await?;
        let since = active
            .as_ref()
            .map(|item| item.reviewed_at_ms.unwrap_or(item.watermark_ms));
        let recent = self
            .store
            .action_episodes(&self.scope.owner, project, NEW_EPISODES)
            .await?;
        let Some(first) = recent.first() else {
            return Ok(None);
        };
        let predecessor = active
            .as_ref()
            .map_or("none", |item| item.revision.id.as_str());
        let job = stable_id(&[
            self.scope.owner.as_str(),
            project.as_str(),
            predecessor,
            first.source.as_str(),
        ]);
        // The same snapshot was already evaluated or attempted: nothing changed, nothing to read.
        if self.playbook_evaluated()?.get(project) == Some(&job) {
            return Ok(None);
        }
        let new = recent
            .iter()
            .filter(|event| since.is_none_or(|since| event.recorded_at_ms > since))
            .count();
        let due = new >= NEW_EPISODES
            || (new >= 1
                && since.is_some_and(|since| now.saturating_sub(since) >= REFRESH_AFTER_MS));
        if !due {
            return Ok(None);
        }
        #[cfg(test)]
        self.playbook_full_reads.fetch_add(1, Ordering::SeqCst);
        let events = self
            .store
            .action_episodes(&self.scope.owner, project, EPISODE_LIMIT)
            .await?;
        let records = action_episodes::records(&events);
        let plain = records
            .iter()
            .map(|(_, record)| record.clone())
            .collect::<Vec<_>>();
        let facts = procedural_policy::aggregate(&plain, now);
        let Some(first) = events.first() else {
            return Ok(None);
        };
        // From here the snapshot counts as evaluated, whether it ends in nothing or an attempt.
        self.playbook_evaluated()?
            .insert(project.clone(), job.clone());
        if facts.is_empty() {
            return Ok(None);
        }
        let (newest, admitted) = (first.source.clone(), first.clone());
        let plan = PlaybookPlan {
            facts,
            records,
            active,
            newest,
            job,
        };
        match self
            .store
            .admit_runtime_operation(&self.scope.owner, project, &plan.operation(), &[admitted])
            .await
        {
            Ok(()) => Ok(Some(plan)),
            Err(RuntimeError::UncertainAdmission) => Ok(None),
            Err(error) => {
                // Nothing was attempted, so a later tick may evaluate this snapshot again.
                self.playbook_evaluated()?.remove(project);
                Err(error)
            }
        }
    }

    fn playbook_evaluated(
        &self,
    ) -> Result<
        std::sync::MutexGuard<'_, std::collections::BTreeMap<ProjectId, String>>,
        RuntimeError,
    > {
        self.playbook_evaluated
            .lock()
            .map_err(|_| RuntimeError::StorageUnavailable)
    }

    pub(super) async fn release_playbook(
        &self,
        project: &ProjectId,
        plan: &PlaybookPlan,
    ) -> Result<(), RuntimeError> {
        // The operation did no billable work, so the snapshot must be evaluable again.
        self.playbook_evaluated()?.remove(project);
        self.store
            .release_runtime_operation(&self.scope.owner, project, &plan.operation())
            .await
    }

    /// One paid phrasing call, the pure gate, then publication through the existing path.
    /// `Ok(false)` leaves the previous version active (invalid output, no retry).
    pub(super) async fn playbook_tick(
        &self,
        project: &ProjectId,
        plan: PlaybookPlan,
        trace: &mut OperationTrace<'_>,
    ) -> Result<bool, RuntimeError> {
        let Some(providers) = &self.providers else {
            return Err(RuntimeError::ProviderUnavailable);
        };
        let operation = plan.operation();
        let stage = match trace
            .stage(
                project,
                ActivityPhase::Playbook,
                std::slice::from_ref(&plan.newest),
                &[],
                Some(&operation),
            )
            .await
        {
            Ok(stage) => stage,
            Err(error) => {
                self.release_playbook(project, &plan).await?;
                return Err(error);
            }
        };
        let current = plan
            .active
            .as_ref()
            .map(|item| item.revision.content.as_str());
        let phrased = async {
            stage.admitted(&operation)?;
            match providers.playbook(&plan.facts, current).await {
                Err(RuntimeError::ProviderRejected) => {
                    // Refused before billable work: the operation may be attempted again later.
                    self.release_playbook(project, &plan).await?;
                    Err(RuntimeError::ProviderRejected)
                }
                other => other,
            }
        }
        .await;
        trace
            .finish_stage(stage, &phrased, ActivityCounts::default())
            .await;
        let text = phrased?.trim().to_owned();
        let sources = representative_sources(&text, &plan);
        if procedural_policy::validate_playbook(&text, &plan.facts).is_err() || sources.is_empty() {
            trace.observed_failure(ActivityReason::PlaybookInvalidOutput);
            return Ok(false);
        }
        let proposal = RevisionProposal {
            project: project.clone(),
            memory: MemoryId::new(PLAYBOOK_MEMORY).map_err(|_| RuntimeError::StorageUnavailable)?,
            revision: RevisionId::new(format!("playbook-revision:{}", plan.job))
                .map_err(|_| RuntimeError::StorageUnavailable)?,
            sources,
            content: text,
            predecessor: plan.active.as_ref().map(|item| item.revision.id.clone()),
        };
        self.propose_effect(proposal, trace).await?;
        Ok(true)
    }
}

/// The most recent episode backing each cited fact, in fact-rank order, at most 32.
#[cfg(feature = "router-http")]
fn representative_sources(text: &str, plan: &PlaybookPlan) -> Vec<SourceId> {
    let cited = procedural_policy::cited_facts(text);
    let mut sources: Vec<SourceId> = Vec::new();
    for fact in plan.facts.iter().filter(|fact| cited.contains(&fact.id)) {
        let newest = plan
            .records
            .iter()
            .filter(|(_, record)| record.outcome == WorkOutcome::Succeeded)
            .filter(|(_, record)| {
                let key =
                    procedural_policy::normalize_key(&record.episode.tool, &record.episode.summary);
                match fact.kind {
                    FactKind::Procedure | FactKind::Pitfall => key == fact.key,
                    FactKind::HotFile => key.strip_prefix("edit ") == Some(fact.key.as_str()),
                }
            })
            .max_by_key(|(source, record)| (record.recorded_at_ms, source.as_str().to_owned()));
        if let Some((source, _)) = newest
            && !sources.contains(source)
            && sources.len() < MAX_SOURCES
        {
            sources.push(source.clone());
        }
    }
    sources
}

#[cfg(all(test, feature = "router-http"))]
mod tests;
