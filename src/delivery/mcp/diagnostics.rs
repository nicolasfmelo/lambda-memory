//! Diagnostic transport projections contain no internal evidence associations.
use super::*;
use crate::domain_models::ActivityId;
use crate::domain_models::runtime_diagnostics::*;

fn counts_json(c: &ActivityCounts) -> Value {
    let mut result = Map::new();
    for (name, value) in [
        ("sources_created", c.sources_created),
        ("sources_consumed", c.sources_consumed),
        ("revisions_created", c.revisions_created),
        ("revisions_indexed", c.revisions_indexed),
        ("revisions_published", c.revisions_published),
        ("hits_returned", c.hits_returned),
        ("sources_removed", c.sources_removed),
        ("revisions_removed", c.revisions_removed),
        ("jobs_removed", c.jobs_removed),
    ] {
        if let Some(value) = value {
            result.insert(name.into(), json!(value));
        }
    }
    Value::Object(result)
}
fn entry_json(entry: &ActivityEntry) -> Value {
    json!({"id":entry.id.as_str(),"parent_id":entry.parent.as_ref().map(ActivityId::as_str),"entry_kind":entry.entry_kind.as_str(),
        "operation":entry.operation.as_str(),"phase":entry.phase.as_str(),"worker_instance":entry.instance.as_str(),
        "started_at_ms":entry.started_at_ms,"finished_at_ms":entry.finished_at_ms,"classified_at_ms":entry.classified_at_ms,
        "duration_ms":entry.duration_ms,"outcome":entry.outcome.as_str(),"reason":entry.reason.map(ActivityReason::as_str),"counts":counts_json(&entry.counts)})
}
fn history_json(history: &HistoryCoverage) -> Value {
    json!({"retention_days":history.retention_days,"instrumentation_started_at_ms":history.instrumentation_started_at_ms,"retained_since_ms":history.retained_since_ms})
}
pub(super) fn status_json(status: RuntimeStatus) -> Value {
    let worker = &status.worker;
    let dream = &worker.dreaming;
    let stored = &status.stored;
    let pending = &stored.pending;
    let lifecycle = &stored.lifecycle;
    json!({"project":status.project.as_str(),"mode":status.mode,"observed_at_ms":status.observed_at_ms,
        "worker":{"instance":worker.instance.as_str(),"started_at_ms":worker.started_at_ms,"foreground_active":worker.foreground_active,
            "dreaming":{"state":dream.state.as_str(),"phase":dream.phase.map(ActivityPhase::as_str),"stop_reason":dream.stop_reason.map(ActivityReason::as_str),
                "idle_ms":dream.idle_ms,"tick_ms":dream.tick_ms,"dream_batch":dream.dream_batch,"remaining_idle_ms":dream.remaining_idle_ms}},
        "pending":{"unconsumed_sources":pending.unconsumed_sources,"processing_revisions":pending.processing_revisions,"blocked_sources":pending.blocked_sources,
            "blocked_revisions":pending.blocked_revisions,"active_without_current_embedding":pending.active_without_current_embedding},
        "lifecycle":{"active":lifecycle.active,"processing":lifecycle.processing,"conflicted":lifecycle.conflicted,"rejected":lifecycle.rejected,"superseded":lifecycle.superseded},
        "review":{"activation_requires_review":false,"unresolved_conflicts":stored.unresolved_conflicts},
        "last_success":stored.last_success.as_ref().map(entry_json),"last_failure":stored.last_failure.as_ref().map(entry_json),
        "last_dream_success":stored.last_dream_success.as_ref().map(entry_json),"last_dream_failure":stored.last_dream_failure.as_ref().map(entry_json),
        "diagnostics":{"state":status.recording_state.as_str(),"reason":status.recording_reason.map(ActivityReason::as_str)},"history":history_json(&stored.history)})
}
pub(super) fn activity_json(page: ActivityPage) -> Value {
    let entries = page.entries.iter().map(entry_json).collect::<Vec<_>>();
    json!({"project":page.project.as_str(),"observed_at_ms":page.observed_at_ms,"retention_days":page.history.retention_days,
        "history":history_json(&page.history),"entries":entries,"next_cursor":page.next_cursor})
}
pub(super) fn activity_request(args: &Map<String, Value>) -> Result<ActivityRequest, CallError> {
    keys(
        args,
        &["project"],
        &["limit", "operations", "outcomes", "cursor"],
    )?;
    let limit = number(args, "limit", ACTIVITY_DEFAULT_LIMIT)?;
    if limit == 0 || limit > ACTIVITY_MAX_LIMIT {
        return Err(CallError::Params);
    }
    fn filter<T: PartialEq>(
        args: &Map<String, Value>,
        name: &str,
        parse: impl Fn(&str) -> Option<T>,
    ) -> Result<Vec<T>, CallError> {
        let Some(value) = args.get(name) else {
            return Ok(Vec::new());
        };
        let values = value
            .as_array()
            .filter(|v| !v.is_empty() && v.len() <= 32)
            .ok_or(CallError::Params)?;
        let mut result = Vec::with_capacity(values.len());
        for value in values {
            let value = value.as_str().and_then(&parse).ok_or(CallError::Params)?;
            if result.contains(&value) {
                return Err(CallError::Params);
            }
            result.push(value);
        }
        Ok(result)
    }
    let cursor = match args.get("cursor") {
        None => None,
        Some(Value::String(value))
            if !value.is_empty() && value.len() <= ACTIVITY_MAX_CURSOR_BYTES =>
        {
            Some(value.clone())
        }
        _ => return Err(CallError::Params),
    };
    Ok(ActivityRequest {
        project: project(args)?,
        limit,
        operations: filter(args, "operations", ActivityOperation::parse)?,
        outcomes: filter(args, "outcomes", ActivityOutcome::parse)?,
        cursor,
    })
}
pub(super) fn warning_json(warning: &DiagnosticsWarning) -> Value {
    json!({"code":warning.reason.as_str(),"operation_id":warning.operation_id.as_str()})
}
pub(super) fn recorded<T>(
    recorded: RecordedOperation<T>,
    serialize: impl FnOnce(T) -> Value,
) -> Result<Value, CallError> {
    match recorded.result {
        Ok(value) => {
            let mut value = serialize(value);
            if let Some(warning) = recorded.diagnostics_warning {
                value["diagnostics_warning"] = warning_json(&warning);
            }
            Ok(value)
        }
        Err(error) => Err(CallError::Domain {
            error,
            diagnostics_warning: recorded.diagnostics_warning,
        }),
    }
}
