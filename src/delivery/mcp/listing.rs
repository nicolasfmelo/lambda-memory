//! `memory_list` wire framing: keyset cursor, lineage filter and per-item lifecycle audit.
//! The acting principal recorded in the audit is never projected.
use super::*;

const CURSOR_KIND: &str = "list";

/// A parsed listing: the domain query plus the fingerprint its cursors must carry.
pub(super) struct ListRead {
    pub query: ListQuery,
    fingerprint: String,
}
pub(super) fn read(args: &Map<String, Value>) -> Result<ListRead, CallError> {
    keys(args, &["project"], &["states", "limit", "memory", "cursor"])?;
    let project = project(args)?;
    let states = match args.get("states") {
        None => vec![LifecycleState::Active, LifecycleState::Processing],
        Some(Value::Array(values)) => values
            .iter()
            .map(|value| match value.as_str() {
                Some("processing") => Ok(LifecycleState::Processing),
                Some("active") => Ok(LifecycleState::Active),
                Some("rejected") => Ok(LifecycleState::Rejected),
                Some("conflicted") => Ok(LifecycleState::Conflicted),
                Some("superseded") => Ok(LifecycleState::Superseded),
                _ => Err(CallError::Params),
            })
            .collect::<Result<Vec<_>, _>>()?,
        _ => return Err(CallError::Params),
    };
    let limit = number(args, "limit", 32)?;
    let memory = args
        .get("memory")
        .map(|value| {
            MemoryId::new(value.as_str().ok_or(CallError::Params)?).map_err(|_| CallError::Params)
        })
        .transpose()?;
    // State order and repetition do not change results, so they do not change the fingerprint.
    let mut names: Vec<&str> = states.iter().map(|state| state_name(*state)).collect();
    names.sort_unstable();
    names.dedup();
    let mut parts = vec![project.as_str()];
    match &memory {
        Some(memory) => parts.extend(["memory", memory.as_str()]),
        None => parts.push("all"),
    }
    parts.extend(names);
    let fingerprint = cursor::fingerprint(&parts);
    let after = match args.get("cursor") {
        None => None,
        Some(value) => {
            let key = cursor::decode(value, CURSOR_KIND, &fingerprint)?;
            let at = key
                .get("at")
                .and_then(Value::as_u64)
                .ok_or(CallError::Params)?;
            let revision = key
                .get("revision")
                .and_then(Value::as_str)
                .and_then(|id| RevisionId::new(id).ok())
                .ok_or(CallError::Params)?;
            Some((at, revision))
        }
    };
    Ok(ListRead {
        query: ListQuery {
            project,
            states,
            limit,
            memory,
            after,
        },
        fingerprint,
    })
}

fn audit_json(entry: &LifecycleAuditEntry) -> Value {
    json!({"action":match entry.action {
            ReviewAction::Promote => "promote",
            ReviewAction::Reject => "reject",
            ReviewAction::Conflict => "conflict",
        },"from_state":state_name(entry.from),"to_state":state_name(entry.to),
        "expected_active":entry.expected_active.as_ref().map(RevisionId::as_str),
        "conflict_with":entry.conflict_with.as_ref().map(RevisionId::as_str),"at_ms":entry.at_ms})
}
/// Packs within the response budget; the cursor resumes after the last packed item.
pub(super) fn page_json(read: &ListRead, page: ListPage) -> Result<Value, CallError> {
    let lineage = read.query.memory.is_some();
    let count = page.items.len();
    let mut packed = Vec::new();
    let mut bytes = 0;
    let mut last = None;
    for item in &page.items {
        let mut value = revision_json(item, true);
        value["watermark_ms"] = json!(item.watermark_ms);
        if lineage {
            value["audit"] = page
                .audit
                .iter()
                .filter(|entry| entry.revision == item.revision.id)
                .map(audit_json)
                .collect();
        }
        bytes += value.to_string().len() + 1;
        // The JSON text content envelope may double quotes/escape bytes.
        if bytes > (MAX_RESPONSE_BYTES - 4096) / 2 {
            break;
        }
        packed.push(value);
        last = Some(item);
    }
    let truncated = packed.len() < count;
    let next_cursor = match last {
        Some(item) if truncated || page.more => Some(cursor::encode(
            CURSOR_KIND,
            &read.fingerprint,
            json!({"at":item.watermark_ms,"revision":item.revision.id.as_str()}),
        )?),
        _ => None,
    };
    Ok(json!({"truncated":truncated,"revisions":packed,"next_cursor":next_cursor}))
}
