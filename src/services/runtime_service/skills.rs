//! Skill lifecycle reads/decisions and the source-bound idle drafting job.
use super::*;
use crate::domain_models::skills::*;
use crate::engines::skill_policy;

impl MemoryRuntime {
    pub async fn skill_candidates(
        &self,
        project: &ProjectId,
        status: Option<SkillStatus>,
    ) -> Result<Vec<SkillCandidate>, RuntimeError> {
        self.authorize(project)?;
        self.store
            .skill_candidates(&self.scope.owner, project, status)
            .await
    }
    pub async fn skill_candidate(
        &self,
        project: &ProjectId,
        id: &MemoryId,
    ) -> Result<SkillCandidate, RuntimeError> {
        self.authorize(project)?;
        self.store
            .skill_candidate(&self.scope.owner, project, id)
            .await
    }
    pub async fn skill_decide(
        &self,
        project: &ProjectId,
        id: &MemoryId,
        revision: &RevisionId,
        decision: SkillDecision,
    ) -> Result<SkillCandidate, RuntimeError> {
        self.authorize(project)?;
        let _background = self.background.lock().await;
        let owner = &self.scope.owner;
        let mut candidate = self.store.skill_candidate(owner, project, id).await?;
        if &candidate.revision_id != revision {
            return Err(RuntimeError::InvalidInput("skill revision mismatch"));
        }
        if !skill_policy::valid_decision(&candidate, &decision) {
            return Err(RuntimeError::InvalidInput(
                "invalid skill transition or install paths",
            ));
        }
        let sessions = self
            .store
            .lesson_stats(owner, project, &candidate.memory_id)
            .await?
            .sessions
            .len();
        let expected = candidate.status;
        candidate.status = decision.decision;
        candidate.sessions_at_decision = Some(sessions);
        candidate.decided_at_ms = Some(now_ms()?);
        if decision.decision == SkillStatus::Installed {
            // Earlier files the new decision does not overwrite stay on disk with their provenance.
            let keeps_earlier = candidate
                .install_paths
                .iter()
                .any(|path| !decision.install_paths.contains(path));
            let mut paths = Vec::new();
            let mut hashes = Vec::new();
            for (path, hash) in decision
                .install_paths
                .into_iter()
                .zip(decision.install_hashes)
            {
                if let Some(index) = paths.iter().position(|old| old == &path) {
                    hashes[index] = hash;
                } else {
                    paths.push(path);
                    hashes.push(hash);
                }
            }
            for (path, hash) in candidate
                .install_paths
                .into_iter()
                .zip(candidate.install_hashes)
            {
                if paths.len() == 16 {
                    break;
                }
                if !paths.contains(&path) {
                    paths.push(path);
                    hashes.push(hash);
                }
            }
            candidate.install_paths = paths;
            candidate.install_hashes = hashes;
            let mut pitfalls = candidate.pitfall_memories.clone();
            if keeps_earlier {
                for memory in &candidate.installed_pitfall_memories {
                    if !pitfalls.contains(memory) {
                        pitfalls.push(memory.clone());
                    }
                }
            }
            candidate.installed_pitfall_memories = pitfalls;
        }
        self.store
            .save_skill_decision(owner, project, &candidate, expected, revision)
            .await?;
        Ok(candidate)
    }
    #[cfg(feature = "router-http")]
    pub(super) async fn skill_work(
        &self,
        project: &ProjectId,
    ) -> Result<Vec<(SkillDraftInput, Option<SkillCandidate>)>, RuntimeError> {
        if self.providers.is_none() {
            return Ok(Vec::new());
        }
        let owner = &self.scope.owner;
        let nodes = self.store.tree_nodes(owner, project).await?;
        let mut candidates = self.store.skill_candidates(owner, project, None).await?;
        // Staleness is checked for all live candidates, even those no longer eligible.
        for c in &mut candidates {
            if nodes
                .iter()
                .any(|n| n.memory == c.memory_id && n.revision != c.revision_id)
                && matches!(
                    c.status,
                    SkillStatus::Drafted | SkillStatus::Approved | SkillStatus::Installed
                )
            {
                let node = nodes.iter().find(|n| n.memory == c.memory_id).unwrap();
                self.store
                    .skill_mark_stale(owner, project, &node.memory, &node.revision)
                    .await?;
                c.status = SkillStatus::Stale;
            }
        }
        let mut work = Vec::new();
        for node in &nodes {
            if node.basis != crate::domain_models::memory_tree::Basis::Observed
                || !node.root.is_lesson()
                || node.depth == 0
            {
                continue;
            }
            let stats = self
                .store
                .lesson_stats(owner, project, &node.memory)
                .await?;
            let previous = candidates.iter().find(|c| c.memory_id == node.memory);
            if !skill_policy::candidate_rule(node, &stats)
                || !skill_policy::eligible(previous, stats.sessions.len())
                || self
                    .store
                    .skill_failed(owner, project, &node.memory, &node.revision)
                    .await?
            {
                continue;
            }
            let mut pitfalls = Vec::new();
            let mut related: Vec<_> = nodes
                .iter()
                .filter(|n| n.root == node.root && n.memory != node.memory && n.depth > 0)
                .collect();
            related.sort_by(|a, b| {
                b.last_linked_ms
                    .cmp(&a.last_linked_ms)
                    .then(a.memory.cmp(&b.memory))
            });
            for n in related {
                if self
                    .store
                    .lesson_stats(owner, project, &n.memory)
                    .await?
                    .pitfalls
                    > 0
                {
                    pitfalls.push(n.clone());
                }
                if pitfalls.len() == 5 {
                    break;
                }
            }
            let mut path = vec![node.root.label().to_owned()];
            path.extend(
                self.store
                    .tree_ancestors(owner, project, &node.memory)
                    .await?
                    .into_iter()
                    .map(|n| n.title),
            );
            path.push(node.title.clone());
            work.push((
                SkillDraftInput {
                    node: node.clone(),
                    path,
                    pitfalls,
                    sessions: stats.sessions,
                    scope: if project.as_str().starts_with("user:") {
                        "user"
                    } else {
                        "project"
                    },
                },
                previous.cloned(),
            ));
        }
        Ok(work)
    }
    #[cfg(feature = "router-http")]
    pub(super) async fn skill_tick(
        &self,
        project: &ProjectId,
        work: Vec<(SkillDraftInput, Option<SkillCandidate>)>,
        trace: &mut OperationTrace<'_>,
    ) -> Result<Option<DreamReceipt>, RuntimeError> {
        use crate::domain_models::runtime_diagnostics::ActivityReason;
        let Some(providers) = &self.providers else {
            return Ok(None);
        };
        let owner = &self.scope.owner;
        let mut handled = false;
        for (input, previous) in work {
            if !self.idle() {
                break;
            }
            let node = &input.node;
            let count = self
                .store
                .skill_admission_count(owner, project, &node.revision)
                .await?
                .to_string();
            let operation = format!(
                "skill-draft-job:{}",
                stable_id(&[
                    skill_policy::PROMPT_VERSION,
                    owner.as_str(),
                    project.as_str(),
                    node.revision.as_str(),
                    &count
                ])
            );
            let events = self
                .store
                .skill_recent_lesson_sources(owner, project, &node.memory)
                .await?;
            if events.is_empty() {
                continue;
            }
            let ids: Vec<_> = events.iter().map(|e| e.source.clone()).collect();
            let stage = trace
                .stage(
                    project,
                    ActivityPhase::Derivation,
                    &ids,
                    std::slice::from_ref(&node.revision),
                    Some(&operation),
                )
                .await?;
            let result = async {
                self.store
                    .admit_skill_operation(owner, project, &operation, &events, &node.revision)
                    .await?;
                stage.admitted(&operation)?;
                match providers.draft_skill(&input).await {
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
                .finish_stage(stage, &result, ActivityCounts::default())
                .await;
            let answer = match result {
                Ok(answer) if skill_policy::validate(&answer) => answer,
                Ok(_) | Err(RuntimeError::ProviderInvalidOutput) => {
                    trace.observed_failure(ActivityReason::ProviderInvalidOutput);
                    self.store
                        .skill_record_failure(
                            owner,
                            project,
                            &node.memory,
                            &node.revision,
                            now_ms()?,
                        )
                        .await?;
                    return Ok(Some(DreamReceipt::default()));
                }
                Err(RuntimeError::UncertainAdmission) => {
                    trace.observed_failure(ActivityReason::UncertainAdmission);
                    self.store
                        .skill_record_failure(
                            owner,
                            project,
                            &node.memory,
                            &node.revision,
                            now_ms()?,
                        )
                        .await?;
                    handled = true;
                    continue;
                }
                Err(error) => return Err(error),
            };
            let candidate = SkillCandidate {
                candidate_id: node.memory.clone(),
                memory_id: node.memory.clone(),
                revision_id: node.revision.clone(),
                scope: input.scope,
                slug: answer.name.clone(),
                status: SkillStatus::Drafted,
                draft: Some(skill_policy::render(&input, &answer)),
                sessions_at_draft: input.sessions.len(),
                sessions_at_decision: previous.as_ref().and_then(|c| c.sessions_at_decision),
                created_at_ms: now_ms()?,
                decided_at_ms: previous.as_ref().and_then(|c| c.decided_at_ms),
                install_paths: previous
                    .as_ref()
                    .map_or_else(Vec::new, |c| c.install_paths.clone()),
                install_hashes: previous
                    .as_ref()
                    .map_or_else(Vec::new, |c| c.install_hashes.clone()),
                pitfall_memories: input.pitfalls.iter().map(|n| n.memory.clone()).collect(),
                pitfall_revisions: input.pitfalls.iter().map(|n| n.revision.clone()).collect(),
                pitfall_source_counts: input.pitfalls.iter().map(|n| n.direct_sources).collect(),
                installed_pitfall_memories: previous
                    .as_ref()
                    .map_or_else(Vec::new, |c| c.installed_pitfall_memories.clone()),
            };
            match self
                .store
                .save_skill_draft(owner, project, &candidate, &operation, node.direct_sources)
                .await
            {
                Ok(()) => {}
                Err(RuntimeError::ProviderInvalidOutput) => {
                    // The pitfall snapshot changed during provider work. Discard the
                    // entire answer and wait for a new strategy revision, as for bad output.
                    trace.observed_failure(ActivityReason::ProviderInvalidOutput);
                    self.store
                        .skill_record_failure(
                            owner,
                            project,
                            &node.memory,
                            &node.revision,
                            now_ms()?,
                        )
                        .await?;
                    return Ok(Some(DreamReceipt::default()));
                }
                Err(error) => return Err(error),
            }
            return Ok(Some(DreamReceipt {
                batches: 1,
                sources: 0,
            }));
        }
        Ok(handled.then_some(DreamReceipt::default()))
    }
}

#[cfg(all(test, feature = "router-http"))]
#[path = "../../../tests/support/skill_runtime.rs"]
mod tests;
