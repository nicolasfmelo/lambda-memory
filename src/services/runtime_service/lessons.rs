//! Idle session extraction. One durable admission and at most one call per session input.
use super::*;
use crate::domain_models::runtime_diagnostics::ActivityReason;
use crate::engines::lesson_policy;
use crate::integrations::{action_episodes::parse_episode, exchange_content::parse_exchange};

impl MemoryRuntime {
    pub(super) async fn lesson_tick(
        &self,
        project: &ProjectId,
        trace: &mut OperationTrace<'_>,
    ) -> Result<Option<DreamReceipt>, RuntimeError> {
        let Some(providers) = &self.providers else {
            return Ok(None);
        };
        let owner = &self.scope.owner;
        let sessions = self.ready_sessions(project, now_ms()?, 2).await?;
        let mut receipt = DreamReceipt::default();
        let mut handled = false;
        for session in sessions {
            if !self.idle() {
                break;
            }
            if !self
                .store
                .lesson_capacity(owner, project, self.limits.max_pending)
                .await?
            {
                continue;
            }
            let sources = self.store.session_sources(owner, project, &session).await?;
            let exchanges = sources
                .iter()
                .filter(|e| e.kind == WorkEventKind::Exchange)
                .filter_map(|e| {
                    let (prompt, reply) = parse_exchange(&e.content)?;
                    Some((e.clone(), prompt, reply))
                })
                .collect();
            let actions = sources
                .iter()
                .filter(|e| e.kind == WorkEventKind::Action)
                .filter_map(|e| {
                    let episode = parse_episode(&e.content)?;
                    Some((
                        e.clone(),
                        crate::domain_models::procedural::EpisodeRecord {
                            episode,
                            outcome: e.outcome.clone(),
                            recorded_at_ms: e.recorded_at_ms,
                        },
                    ))
                })
                .collect();
            let Some(input) = lesson_policy::build_input(exchanges, actions) else {
                self.store
                    .mark_extracted(
                        owner,
                        project,
                        &session.session_key,
                        session.last_activity_ms,
                    )
                    .await?;
                handled = true;
                continue;
            };
            let mut parts = vec![
                lesson_policy::PROMPT_VERSION,
                owner.as_str(),
                project.as_str(),
                session.session_key.as_str(),
            ];
            parts.extend(input.sources.iter().map(|e| e.source.as_str()));
            let operation = format!("lesson-job:{}", stable_id(&parts));
            let mut recent = input.sources.clone();
            recent.sort_by(|a, b| {
                (a.recorded_at_ms, a.source.as_str()).cmp(&(b.recorded_at_ms, b.source.as_str()))
            });
            let events = &recent[recent.len().saturating_sub(32)..];
            let ids: Vec<_> = events.iter().map(|e| e.source.clone()).collect();
            let stage = trace
                .stage(
                    project,
                    ActivityPhase::Derivation,
                    &ids,
                    &[],
                    Some(&operation),
                )
                .await?;
            let answer = async {
                self.store
                    .admit_runtime_operation(owner, project, &operation, events)
                    .await?;
                stage.admitted(&operation)?;
                match providers.extract_lessons(&input).await {
                    Err(RuntimeError::ProviderRejected) => {
                        self.store
                            .release_runtime_operation(owner, project, &operation)
                            .await?;
                        Err(RuntimeError::ProviderRejected)
                    }
                    result => result,
                }
            }
            .await;
            trace
                .finish_stage(stage, &answer, ActivityCounts::default())
                .await;
            let answer = match answer {
                Ok(answer) if lesson_policy::validate(&input, &answer) => answer,
                Ok(_) | Err(RuntimeError::ProviderInvalidOutput) => {
                    trace.observed_failure(ActivityReason::ProviderInvalidOutput);
                    self.store
                        .mark_extracted(
                            owner,
                            project,
                            &session.session_key,
                            session.last_activity_ms,
                        )
                        .await?;
                    handled = true;
                    continue;
                }
                Err(RuntimeError::UncertainAdmission) => {
                    // Never replay an unknown paid outcome, and do not stop unrelated work.
                    trace.observed_failure(ActivityReason::UncertainAdmission);
                    self.store
                        .mark_extracted(
                            owner,
                            project,
                            &session.session_key,
                            session.last_activity_ms,
                        )
                        .await?;
                    handled = true;
                    continue;
                }
                Err(error) => return Err(error),
            };
            let n = self
                .store
                .commit_lessons(
                    owner,
                    &self.scope.actor,
                    project,
                    &session,
                    &input,
                    &answer,
                    &operation,
                    now_ms()?,
                    self.limits.max_pending,
                )
                .await?;
            trace.counts(
                project,
                &ActivityCounts {
                    sources_created: Some(n as u64),
                    ..Default::default()
                },
            );
            receipt.batches += 1;
            receipt.sources += n;
            handled = true;
        }
        Ok(handled.then_some(receipt))
    }
}

#[cfg(test)]
#[path = "../../../tests/support/lesson_runtime.rs"]
mod tests;
