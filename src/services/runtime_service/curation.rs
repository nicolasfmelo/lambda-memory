use super::*;
use crate::domain_models::memory_tree::*;
use crate::domain_models::runtime_diagnostics::ActivityReason;
use crate::engines::tree_policy;

/// A maintenance step that the bounds make impossible (depth, provider slots).
const MAINTENANCE_BOUNDS: &str = "tree maintenance bounds";
/// Changes whenever the curation request shape or instructions change.
const CURATION_PROMPT_VERSION: &str = "tree-curation-v2";

#[cfg(test)]
mod tests;

#[cfg(feature = "router-http")]
impl MemoryRuntime {
    pub(super) async fn has_curation_work(
        &self,
        project: &ProjectId,
    ) -> Result<bool, RuntimeError> {
        let owner = &self.scope.owner;
        let now = now_ms()?;
        let parked = self.store.tree_parked_cycles(owner, project).await?;
        let split_pending = self
            .store
            .tree_queue_peek(owner, project, TreeQueue::Split)
            .await?
            .is_some();
        let admitted = self.store.tree_admitted_sources(owner, project).await?;
        if parked.iter().any(|(source, reason, cycles)| {
            source.recorded_at_ms <= now
                && self
                    .limits
                    .retention_ms
                    .is_none_or(|retention| source.recorded_at_ms >= now.saturating_sub(retention))
                && admitted.iter().filter(|id| **id == source.source).count() as u64 <= *cycles
                && ((*cycles == 1 && matches!(reason, ParkReason::Expand(_)))
                    || (!split_pending && matches!(reason, ParkReason::Split)))
        }) {
            return Ok(true);
        }
        let sources = self
            .store
            .eligible_sources(
                owner,
                project,
                32,
                true,
                self.limits.retention_ms.map(|v| now.saturating_sub(v)),
                Some(now),
                true,
            )
            .await?;
        if sources.iter().any(|s| {
            !admitted.contains(&s.source) && !parked.iter().any(|(p, _, _)| p.source == s.source)
        }) {
            return Ok(true);
        }
        if sources.len() == 32 {
            let excluded = admitted
                .into_iter()
                .chain(parked.into_iter().map(|(source, _, _)| source.source))
                .collect::<Vec<_>>();
            return Ok(!self
                .store
                .tree_eligible_after_exclusions(
                    owner,
                    project,
                    1,
                    self.limits.retention_ms.map(|v| now.saturating_sub(v)),
                    Some(now),
                    &excluded,
                )
                .await?
                .is_empty());
        }
        Ok(false)
    }
    pub(super) async fn curation_tick(
        &self,
        project: &ProjectId,
        trace: &mut OperationTrace<'_>,
    ) -> Result<Option<DreamReceipt>, RuntimeError> {
        if self.providers.is_none() {
            return Ok(None);
        }
        let owner = &self.scope.owner;
        let now = now_ms()?;
        let parked = self.store.tree_parked_cycles(owner, project).await?;
        let split_pending = self
            .store
            .tree_queue_peek(owner, project, TreeQueue::Split)
            .await?
            .is_some();
        let mut sources = self
            .store
            .eligible_sources(
                owner,
                project,
                32,
                true,
                self.limits.retention_ms.map(|v| now.saturating_sub(v)),
                Some(now),
                true,
            )
            .await?;
        let admitted = self.store.tree_admitted_sources(owner, project).await?;
        let page_full = sources.len() == 32;
        sources.retain(|s| {
            !admitted.contains(&s.source) && !parked.iter().any(|(p, _, _)| p.source == s.source)
        });
        if page_full && sources.len() < self.limits.curation_batch {
            let excluded = admitted
                .iter()
                .cloned()
                .chain(parked.iter().map(|(source, _, _)| source.source.clone()))
                .collect::<Vec<_>>();
            sources = self
                .store
                .tree_eligible_after_exclusions(
                    owner,
                    project,
                    self.limits.curation_batch,
                    self.limits.retention_ms.map(|v| now.saturating_sub(v)),
                    Some(now),
                    &excluded,
                )
                .await?;
        }
        sources.extend(
            parked
                .iter()
                .filter(|(source, reason, cycles)| {
                    source.recorded_at_ms <= now
                        && self.limits.retention_ms.is_none_or(|retention| {
                            source.recorded_at_ms >= now.saturating_sub(retention)
                        })
                        && admitted.iter().filter(|id| **id == source.source).count() as u64
                            <= *cycles
                        && ((*cycles == 1 && matches!(reason, ParkReason::Expand(_)))
                            || (!split_pending && matches!(reason, ParkReason::Split)))
                })
                .map(|(s, _, _)| s.clone()),
        );
        sources.sort_by(|a, b| {
            a.recorded_at_ms
                .cmp(&b.recorded_at_ms)
                .then_with(|| a.source.as_str().cmp(b.source.as_str()))
        });
        sources.truncate(self.limits.curation_batch);
        if sources.is_empty() {
            return Ok(None);
        }
        self.curate_batch(project, sources, LinkOperation::Create, trace)
            .await
    }

    pub(super) async fn curate_batch(
        &self,
        project: &ProjectId,
        sources: Vec<CaptureEvent>,
        link_operation: LinkOperation,
        trace: &mut OperationTrace<'_>,
    ) -> Result<Option<DreamReceipt>, RuntimeError> {
        let providers = self
            .providers
            .as_ref()
            .ok_or(RuntimeError::ProviderUnavailable)?;
        let owner = &self.scope.owner;
        let now = now_ms()?;
        let parked = self.store.tree_parked_cycles(owner, project).await?;
        let nodes = self.store.tree_nodes(owner, project).await?;
        let map = tree_policy::tree_map(&nodes, self.limits.tree_map_bytes);
        let terms = tree_policy::source_terms(
            &sources
                .iter()
                .map(|s| s.content.as_str())
                .collect::<Vec<_>>()
                .join(" "),
        );
        let mut detailed = self
            .store
            .tree_lexical_candidates(owner, project, &terms, 8)
            .await?;
        for (source, reason, cycles) in &parked {
            if *cycles == 1
                && sources.iter().any(|s| s.source == source.source)
                && let ParkReason::Expand(id) = reason
                && let Some(node) = nodes.iter().find(|n| &n.memory == id)
                && !detailed.iter().any(|n| n.memory == node.memory)
            {
                detailed.push(node.clone());
            }
        }
        let candidates = detailed.clone();
        for candidate in candidates {
            for ancestor in self
                .store
                .tree_ancestors(owner, project, &candidate.memory)
                .await?
            {
                if map
                    .iter()
                    .chain(&detailed)
                    .all(|n| n.memory != ancestor.memory)
                    && map.len() + detailed.len() < 128
                {
                    detailed.push(ancestor);
                }
            }
        }
        let second_cycle = sources
            .iter()
            .any(|s| parked.iter().any(|(p, _, _)| p.source == s.source));
        let expandable = if second_cycle {
            vec![]
        } else {
            map.iter()
                .enumerate()
                .filter(|(_, n)| !detailed.iter().any(|d| d.memory == n.memory))
                .map(|(i, _)| i)
                .collect()
        };
        let view = CurationView {
            map,
            detailed,
            expandable,
            root_children: self.store.tree_root_children(owner, project).await?,
        };
        // The prompt version is part of the request identity: a corrected request is new work,
        // while an earlier admission of the old request stays recorded and is never replayed.
        let mut parts = vec![CURATION_PROMPT_VERSION, owner.as_str(), project.as_str()];
        parts.extend(sources.iter().map(|s| s.source.as_str()));
        parts.extend(
            view.map
                .iter()
                .chain(&view.detailed)
                .map(|n| n.revision.as_str()),
        );
        // A migration rerun after a completed but unusable answer must be a new request, so the
        // identity counts earlier admissions of these sources; nothing uncertain is replayed.
        let earlier = if link_operation == LinkOperation::Migrate {
            self.store.tree_admitted_sources(owner, project).await?
        } else {
            Vec::new()
        };
        let migration_cycles: Vec<String> = if link_operation == LinkOperation::Migrate {
            sources
                .iter()
                .map(|source| {
                    format!(
                        "{}:{}",
                        parked
                            .iter()
                            .find(|(p, _, _)| p.source == source.source)
                            .map_or(0, |(_, _, cycles)| *cycles),
                        earlier.iter().filter(|id| **id == source.source).count()
                    )
                })
                .collect()
        } else {
            Vec::new()
        };
        parts.extend(migration_cycles.iter().map(String::as_str));
        let prefix = if link_operation == LinkOperation::Migrate {
            "curation-job:migrate:"
        } else {
            "curation-job:"
        };
        let operation = format!("{prefix}{}", stable_id(&parts));
        let source_ids: Vec<_> = sources.iter().map(|s| s.source.clone()).collect();
        let stage = trace
            .stage(
                project,
                ActivityPhase::Curation,
                &source_ids,
                &[],
                Some(&operation),
            )
            .await?;
        let answer = async {
            self.store
                .admit_runtime_operation(owner, project, &operation, &sources)
                .await?;
            stage.admitted(&operation)?;
            match providers.curate(&sources, &view).await {
                Err(RuntimeError::ProviderRejected) => {
                    self.store
                        .release_runtime_operation(owner, project, &operation)
                        .await?;
                    Err(RuntimeError::ProviderRejected)
                }
                other => other,
            }
        }
        .await;
        trace
            .finish_stage(stage, &answer, ActivityCounts::default())
            .await;
        let answer = answer?;
        // A completed but unusable answer is recorded with the rule it broke. It is not an
        // uncertain admission, so it does not stop background work; these sources stay out of
        // automatic selection until an operator retries them.
        let validated =
            match tree_policy::validate_curation_rules(&view, &answer, sources.len(), &self.limits)
            {
                Ok(validated) => validated,
                Err(rule) => {
                    trace.observed_failure(match rule {
                        tree_policy::TreeRule::UnknownSlot => ActivityReason::TreeUnknownSlot,
                        tree_policy::TreeRule::MissingOperation => {
                            ActivityReason::TreeMissingOperation
                        }
                        tree_policy::TreeRule::TooManyOperations => {
                            ActivityReason::TreeTooManyOperations
                        }
                        tree_policy::TreeRule::InvalidText => ActivityReason::TreeInvalidText,
                        tree_policy::TreeRule::SecretInText => ActivityReason::TreeSecretInText,
                        tree_policy::TreeRule::Bounds => ActivityReason::TreeBounds,
                    });
                    return Err(RuntimeError::ProviderInvalidOutput);
                }
            };
        let all: Vec<_> = view.map.iter().chain(&view.detailed).collect();
        let mut plan = CurationCommit {
            operation_id: operation,
            sources: source_ids.clone(),
            merges: vec![],
            creates: vec![],
            parked: vec![],
            stale: vec![],
            split: vec![],
            operation: link_operation,
        };
        for (i, ops) in validated.per_source.iter().enumerate() {
            for op in ops {
                match op {
                    CurationOp::Discard => {}
                    CurationOp::Merge {
                        node,
                        title,
                        content,
                    } => {
                        let n = all[*node];
                        // Several sources may update one node; the last merge carries the full text.
                        if let Some(existing) = plan.merges.iter_mut().find(|m| m.0 == n.memory) {
                            existing.2 = title.clone();
                            existing.3 = content.clone();
                            if !existing.4.contains(&source_ids[i]) {
                                existing.4.push(source_ids[i].clone());
                            }
                            continue;
                        }
                        plan.merges.push((
                            n.memory.clone(),
                            n.revision.clone(),
                            title.clone(),
                            content.clone(),
                            vec![source_ids[i].clone()],
                        ));
                        if let Some(parent) = &n.parent
                            && nodes.iter().any(|x| &x.memory == parent)
                            && !plan.stale.contains(parent)
                        {
                            plan.stale.push(parent.clone());
                        }
                        if n.direct_sources as usize + 1 > self.limits.tree_split_sources {
                            plan.split.push(n.memory.clone());
                        }
                    }
                    CurationOp::Create {
                        parent,
                        title,
                        content,
                    } => {
                        let parent = match parent {
                            Slot::Root(r) => ParentRef::Root(*r),
                            Slot::Node(i) => {
                                let n = all[*i];
                                if !plan.stale.contains(&n.memory) {
                                    plan.stale.push(n.memory.clone());
                                }
                                ParentRef::Node(n.memory.clone())
                            }
                        };
                        plan.creates.push((
                            parent,
                            title.clone(),
                            content.clone(),
                            vec![source_ids[i].clone()],
                        ));
                    }
                    CurationOp::Expand { .. } => unreachable!(),
                }
            }
        }
        for i in &validated.parked {
            let reason = answer.per_source[*i]
                .iter()
                .find_map(|op| match op {
                    CurationOp::Expand { node } => {
                        Some(ParkReason::Expand(all[*node].memory.clone()))
                    }
                    // A merge into a map-only node is parked as an implicit expand of that node.
                    CurationOp::Merge { node, .. }
                        if *node < view.map.len() && view.expandable.contains(node) =>
                    {
                        Some(ParkReason::Expand(all[*node].memory.clone()))
                    }
                    _ => None,
                })
                .unwrap_or(ParkReason::Split);
            plan.parked.push((source_ids[*i].clone(), reason));
        }
        plan.split.extend(
            validated
                .split_requests
                .iter()
                .map(|i| all[*i].memory.clone()),
        );
        plan.split.extend(
            validated
                .root_split_requests
                .iter()
                .map(TreeRoot::memory_id),
        );
        let counts = ActivityCounts {
            sources_curated: Some((sources.len() - plan.parked.len()) as u64),
            sources_discarded: Some(
                validated
                    .per_source
                    .iter()
                    .filter(|ops| matches!(ops.as_slice(), [CurationOp::Discard]))
                    .count() as u64,
            ),
            nodes_created: Some(plan.creates.len() as u64),
            nodes_merged: Some(plan.merges.len() as u64),
            sources_parked: Some(plan.parked.len() as u64),
            operations_dropped: Some(validated.dropped as u64),
            ..Default::default()
        };
        let result = self
            .store
            .commit_curation(owner, &self.scope.actor, project, plan, now)
            .await;
        if matches!(result, Err(RuntimeError::Conflict)) {
            trace.observed_failure(ActivityReason::LifecycleConflict);
        }
        for item in result? {
            if link_operation == LinkOperation::Migrate {
                self.prepare_revision(item, trace).await?;
            } else {
                prepared(self.prepare_revision(item, trace).await)?;
            }
        }
        trace.counts(project, &counts);
        Ok(Some(DreamReceipt {
            batches: 1,
            sources: sources.len(),
        }))
    }
}

#[cfg(feature = "router-http")]
impl MemoryRuntime {
    /// One rebuild, split or refresh step. A step that cannot be done (bounds) or whose paid
    /// answer is unusable is dropped from its queue with the reason recorded, so one bad node
    /// never blocks the rest of the tree or stops the worker.
    pub(super) async fn tree_maintenance_tick(
        &self,
        project: &ProjectId,
        trace: &mut OperationTrace<'_>,
    ) -> Result<Option<DreamReceipt>, RuntimeError> {
        let mut picked = None;
        match self
            .tree_maintenance_step(project, trace, &mut picked)
            .await
        {
            // An identical step already admitted earlier is never sent again; the item is dropped.
            Err(
                error @ (RuntimeError::ProviderInvalidOutput
                | RuntimeError::UncertainAdmission
                | RuntimeError::InvalidInput(MAINTENANCE_BOUNDS)),
            ) => {
                let Some((queue, _)) = picked else {
                    return Err(error);
                };
                self.store
                    .tree_queue_pop(&self.scope.owner, project, queue)
                    .await?;
                trace.observed_failure(match error {
                    RuntimeError::ProviderInvalidOutput => ActivityReason::ProviderInvalidOutput,
                    RuntimeError::UncertainAdmission => ActivityReason::UncertainAdmission,
                    _ => ActivityReason::TreeBounds,
                });
                Ok(Some(DreamReceipt::default()))
            }
            other => other,
        }
    }
    async fn tree_maintenance_step(
        &self,
        project: &ProjectId,
        trace: &mut OperationTrace<'_>,
        picked: &mut Option<(TreeQueue, MemoryId)>,
    ) -> Result<Option<DreamReceipt>, RuntimeError> {
        let owner = &self.scope.owner;
        let Some(providers) = self.providers.as_ref() else {
            return Ok(None);
        };
        let mut selected = None;
        for queue in [TreeQueue::Rebuild, TreeQueue::Split, TreeQueue::Stale] {
            if let Some(memory) = self.store.tree_queue_peek(owner, project, queue).await? {
                selected = Some((queue, memory));
                break;
            }
        }
        let Some((queue, memory)) = selected else {
            return Ok(None);
        };
        *picked = Some((queue, memory.clone()));
        let roots = [
            TreeRoot::Decisions,
            TreeRoot::Preferences,
            TreeRoot::Architecture,
            TreeRoot::OpenItems,
        ];
        let root = roots.into_iter().find(|r| r.memory_id() == memory);
        if root.is_some() && !matches!(queue, TreeQueue::Split) {
            self.store.tree_queue_pop(owner, project, queue).await?;
            return Ok(Some(DreamReceipt::default()));
        }
        let nodes = self.store.tree_nodes(owner, project).await?;
        let node = if let Some(root) = root {
            TreeNode {
                memory: memory.clone(),
                revision: RevisionId::new("tree-root-index")
                    .map_err(|_| RuntimeError::StorageUnavailable)?,
                root,
                parent: None,
                depth: 0,
                title: root.as_str().into(),
                content: String::new(),
                direct_sources: 0,
                children: 0,
                last_linked_ms: 0,
            }
        } else if matches!(queue, TreeQueue::Rebuild) {
            self.store
                .tree_rebuild_node(owner, project, &memory)
                .await?
        } else {
            nodes
                .iter()
                .find(|n| n.memory == memory)
                .cloned()
                .ok_or(RuntimeError::Conflict)?
        };
        let mut children = self.store.tree_children(owner, project, &memory).await?;
        children.sort_by(|a, b| a.memory.as_str().cmp(b.memory.as_str()));
        if root.is_some()
            && matches!(queue, TreeQueue::Split)
            && nodes
                .iter()
                .any(|n| n.root == node.root && n.depth >= self.limits.tree_max_depth)
        {
            return Err(RuntimeError::InvalidInput(MAINTENANCE_BOUNDS));
        }
        let direct = self
            .store
            .tree_recent_sources(owner, project, &memory)
            .await?;
        if matches!(queue, TreeQueue::Rebuild) && direct.is_empty() {
            self.store
                .retire_node(owner, &self.scope.actor, project, &memory, now_ms()?)
                .await?;
            return Ok(Some(DreamReceipt::default()));
        }
        if matches!(queue, TreeQueue::Split)
            && root.is_none()
            && node.direct_sources as usize > direct.len()
        {
            return Err(RuntimeError::InvalidInput(MAINTENANCE_BOUNDS));
        }
        let mut admission_sources = direct.clone();
        if admission_sources.is_empty() && root.is_some() {
            let mut stack: Vec<_> = children.iter().map(|child| child.memory.clone()).collect();
            while let Some(current) = stack.pop() {
                if let Some(event) = self
                    .store
                    .tree_recent_sources(owner, project, &current)
                    .await?
                    .into_iter()
                    .next()
                {
                    admission_sources.push(event);
                    break;
                }
                stack.extend(
                    nodes
                        .iter()
                        .filter(|n| n.parent.as_ref() == Some(&current))
                        .map(|n| n.memory.clone()),
                );
            }
        }
        if admission_sources.is_empty() && matches!(queue, TreeQueue::Stale) {
            admission_sources = self
                .store
                .runtime_revision(owner, project, &node.revision)
                .await?
                .sources;
        }
        if admission_sources.is_empty() {
            return Err(RuntimeError::InvalidInput(MAINTENANCE_BOUNDS));
        }
        let phase = match queue {
            TreeQueue::Rebuild => ActivityPhase::TreeRebuild,
            TreeQueue::Split => ActivityPhase::TreeSplit,
            TreeQueue::Stale => ActivityPhase::TreeRefresh,
        };
        let prefix = match queue {
            TreeQueue::Rebuild => "tree-rebuild-job:",
            TreeQueue::Split => "tree-split-job:",
            TreeQueue::Stale => "tree-refresh-job:",
        };
        let mut parts = vec![
            owner.as_str(),
            project.as_str(),
            memory.as_str(),
            node.revision.as_str(),
        ];
        parts.extend(children.iter().map(|n| n.revision.as_str()));
        parts.extend(direct.iter().map(|s| s.source.as_str()));
        let operation = format!("{prefix}{}", stable_id(&parts));
        let source_ids: Vec<_> = admission_sources.iter().map(|s| s.source.clone()).collect();
        let stage = trace
            .stage(project, phase, &source_ids, &[], Some(&operation))
            .await?;
        enum Output {
            Text(String, String),
            Split(SplitAnswer),
        }
        let output = async {
            self.store
                .admit_runtime_operation(owner, project, &operation, &admission_sources)
                .await?;
            stage.admitted(&operation)?;
            let response = match queue {
                TreeQueue::Rebuild => providers
                    .rebuild_node(&node, &direct)
                    .await
                    .map(|(a, b)| Output::Text(a, b)),
                TreeQueue::Stale => providers
                    .refresh_node(&node, &children)
                    .await
                    .map(|(a, b)| Output::Text(a, b)),
                TreeQueue::Split => providers
                    .split_node(&node, &children, &direct)
                    .await
                    .map(Output::Split),
            };
            match response {
                Err(RuntimeError::ProviderRejected) => {
                    self.store
                        .release_runtime_operation(owner, project, &operation)
                        .await?;
                    Err(RuntimeError::ProviderRejected)
                }
                other => other,
            }
        }
        .await;
        trace
            .finish_stage(stage, &output, ActivityCounts::default())
            .await;
        let mut plan = MaintenanceCommit {
            operation_id: operation,
            operation: match queue {
                TreeQueue::Rebuild => LinkOperation::Rebuild,
                TreeQueue::Split => LinkOperation::Split,
                TreeQueue::Stale => LinkOperation::Merge,
            },
            rewrites: vec![],
            creates: vec![],
            reparent: vec![],
            expected_reparent: vec![],
            move_sources: vec![],
            stale: vec![],
            queue_item: Some((queue, memory.clone())),
        };
        match output? {
            Output::Text(title, content) => {
                tree_policy::validate_title(&title)
                    .and_then(|_| tree_policy::validate_content(&content))
                    .map_err(|_| RuntimeError::ProviderInvalidOutput)?;
                let sources = if matches!(queue, TreeQueue::Rebuild) {
                    direct.iter().map(|s| s.source.clone()).collect()
                } else {
                    self.store
                        .runtime_revision(owner, project, &node.revision)
                        .await?
                        .revision
                        .sources
                };
                plan.rewrites.push((
                    memory.clone(),
                    node.revision.clone(),
                    title,
                    content,
                    sources,
                ));
                if let Some(parent) = &node.parent
                    && nodes.iter().any(|n| &n.memory == parent)
                {
                    plan.stale.push(parent.clone());
                }
            }
            Output::Split(answer) => {
                if root.is_some() {
                    tree_policy::validate_root_split(children.len(), &answer, &self.limits)
                } else {
                    tree_policy::validate_split(&node, direct.len(), &answer, &self.limits)
                }
                .map_err(|_| RuntimeError::ProviderInvalidOutput)?;
                if root.is_some() {
                    for (index, (title, content, partition)) in answer.children.iter().enumerate() {
                        let mut sources = Vec::new();
                        for child_index in partition {
                            let child = &children[*child_index];
                            let mut stack = vec![child.memory.clone()];
                            let mut found = None;
                            while let Some(current) = stack.pop() {
                                if let Some(event) = self
                                    .store
                                    .tree_recent_sources(owner, project, &current)
                                    .await?
                                    .into_iter()
                                    .next()
                                {
                                    found = Some(event.source);
                                    break;
                                }
                                stack.extend(
                                    nodes
                                        .iter()
                                        .filter(|n| n.parent.as_ref() == Some(&current))
                                        .map(|n| n.memory.clone()),
                                );
                            }
                            if let Some(source) = found
                                && sources.is_empty()
                            {
                                sources.push(source);
                            }
                            plan.reparent.push((child.memory.clone(), index));
                            plan.expected_reparent
                                .push((child.memory.clone(), child.revision.clone()));
                        }
                        if sources.is_empty() {
                            return Err(RuntimeError::ProviderInvalidOutput);
                        }
                        plan.creates.push((
                            ParentRef::Root(node.root),
                            title.clone(),
                            content.clone(),
                            sources,
                        ));
                    }
                } else {
                    let old = self
                        .store
                        .runtime_revision(owner, project, &node.revision)
                        .await?;
                    let moved: Vec<_> = direct.iter().map(|s| s.source.clone()).collect();
                    let mut retained: Vec<_> = old
                        .revision
                        .sources
                        .into_iter()
                        .filter(|s| !moved.contains(s))
                        .collect();
                    if retained.is_empty() {
                        retained = answer
                            .children
                            .first()
                            .map(|c| c.2.iter().map(|i| direct[*i].source.clone()).collect())
                            .unwrap_or_default();
                    }
                    if retained.is_empty() {
                        return Err(RuntimeError::ProviderInvalidOutput);
                    }
                    if let Some(parent) = &node.parent
                        && nodes.iter().any(|n| &n.memory == parent)
                    {
                        plan.stale.push(parent.clone());
                    }
                    plan.rewrites.push((
                        memory.clone(),
                        node.revision.clone(),
                        answer.node_title.clone(),
                        answer.node_content.clone(),
                        retained,
                    ));
                    for (index, (title, content, partition)) in answer.children.iter().enumerate() {
                        let sources: Vec<_> = partition
                            .iter()
                            .map(|i| direct[*i].source.clone())
                            .collect();
                        if sources.is_empty() {
                            return Err(RuntimeError::ProviderInvalidOutput);
                        }
                        for source in &sources {
                            plan.move_sources
                                .push((source.clone(), memory.clone(), index));
                        }
                        plan.creates.push((
                            ParentRef::Node(memory.clone()),
                            title.clone(),
                            content.clone(),
                            sources,
                        ));
                    }
                }
            }
        }
        let items = self
            .store
            .commit_tree_maintenance(owner, &self.scope.actor, project, plan, now_ms()?)
            .await?;
        for item in items {
            prepared(self.prepare_revision(item, trace).await)?;
        }
        Ok(Some(DreamReceipt {
            batches: 1,
            sources: 0,
        }))
    }
}
