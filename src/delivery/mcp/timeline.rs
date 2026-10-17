//! Decision timeline wire framing. The writing actor and owner are never projected.
use super::*;
use crate::domain_models::capture::WorkOutcome;
use crate::domain_models::timeline::*;

const CURSOR_KIND: &str = "timeline";
const WRITE_REQUIRED: &[&str] = &[
    "project",
    "entry",
    "title",
    "summary",
    "occurred_at_ms",
    "status",
    "sources",
];
const WRITE_OPTIONAL: &[&str] = &[
    "revisions",
    "memories",
    "supersedes",
    "tags",
    "expected_version",
];

fn ids<T>(
    args: &Map<String, Value>,
    name: &str,
    max: usize,
    parse: impl Fn(&str) -> Result<T, crate::domain_models::MemoryError>,
) -> Result<Vec<T>, CallError> {
    let Some(value) = args.get(name) else {
        return Ok(Vec::new());
    };
    let array = value
        .as_array()
        .filter(|a| a.len() <= max)
        .ok_or(CallError::Params)?;
    array
        .iter()
        .map(|v| parse(v.as_str().ok_or(CallError::Params)?).map_err(|_| CallError::Params))
        .collect()
}
fn entry_id(value: &str) -> Result<TimelineEntryId, CallError> {
    TimelineEntryId::new(value).map_err(|_| CallError::Params)
}
fn status(value: Option<&str>) -> Result<TimelineStatus, CallError> {
    value
        .and_then(TimelineStatus::parse)
        .ok_or(CallError::Params)
}

/// Schema-level shape is checked here; content bounds stay in `timeline_policy`.
pub(super) fn draft(args: &Map<String, Value>) -> Result<TimelineDraft, CallError> {
    keys(args, WRITE_REQUIRED, WRITE_OPTIONAL)?;
    let expected_version = match args.get("expected_version") {
        None => None,
        Some(value) => Some(
            value
                .as_u64()
                .filter(|v| *v >= 1)
                .and_then(|v| u32::try_from(v).ok())
                .ok_or(CallError::Params)?,
        ),
    };
    let supersedes = match args.get("supersedes") {
        None | Some(Value::Null) => None,
        Some(Value::String(value)) => Some(entry_id(value)?),
        _ => return Err(CallError::Params),
    };
    Ok(TimelineDraft {
        project: project(args)?,
        entry: entry_id(text(args, "entry")?)?,
        expected_version,
        title: text(args, "title")?.into(),
        summary: text(args, "summary")?.into(),
        occurred_at_ms: args["occurred_at_ms"].as_u64().ok_or(CallError::Params)?,
        status: status(args["status"].as_str())?,
        sources: sources(args)?,
        revisions: ids(args, "revisions", TIMELINE_MAX_LINKS, |v| {
            RevisionId::new(v)
        })?,
        memories: ids(args, "memories", TIMELINE_MAX_LINKS, |v| MemoryId::new(v))?,
        supersedes,
        tags: ids(args, "tags", TIMELINE_MAX_TAGS, |v| Ok(v.to_owned()))?,
    })
}

fn entry_json(entry: &TimelineEntry) -> Value {
    let strings = |values: Vec<&str>| json!(values);
    json!({"entry":entry.entry.as_str(),"version":entry.version,"project":entry.project.as_str(),
        "title":entry.title,"summary":entry.summary,"occurred_at_ms":entry.occurred_at_ms,
        "recorded_at_ms":entry.recorded_at_ms,"status":entry.status.as_str(),
        "sources":strings(entry.sources.iter().map(SourceId::as_str).collect()),
        "revisions":strings(entry.revisions.iter().map(RevisionId::as_str).collect()),
        "memories":strings(entry.memories.iter().map(MemoryId::as_str).collect()),
        "supersedes":entry.supersedes.as_ref().map(TimelineEntryId::as_str),"tags":entry.tags})
}
pub(super) fn receipt_json(receipt: TimelineReceipt) -> Value {
    json!({"write":match receipt.write {
        TimelineWrite::Recorded => "recorded",
        TimelineWrite::AlreadyRecorded => "already_recorded",
    },"entry":entry_json(&receipt.entry)})
}

/// A parsed read: the domain query plus the fingerprint its cursors must carry.
pub(super) struct TimelineRead {
    pub query: TimelineQuery,
    fingerprint: String,
    first_page: bool,
}
pub(super) fn read(args: &Map<String, Value>) -> Result<TimelineRead, CallError> {
    keys(
        args,
        &["project"],
        &["limit", "statuses", "order", "entry", "cursor"],
    )?;
    let project = project(args)?;
    let limit = number(args, "limit", TIMELINE_MAX_PAGE)?;
    if !(1..=TIMELINE_MAX_PAGE).contains(&limit) {
        return Err(CallError::Params);
    }
    let entry = args
        .get("entry")
        .map(|v| entry_id(v.as_str().ok_or(CallError::Params)?))
        .transpose()?;
    if entry.is_some() && (args.contains_key("statuses") || args.contains_key("order")) {
        return Err(CallError::Params);
    }
    let mut statuses = Vec::new();
    if let Some(value) = args.get("statuses") {
        let values = value
            .as_array()
            .filter(|v| !v.is_empty() && v.len() <= TIMELINE_MAX_STATUSES)
            .ok_or(CallError::Params)?;
        for value in values {
            let parsed = status(value.as_str())?;
            if statuses.contains(&parsed) {
                return Err(CallError::Params);
            }
            statuses.push(parsed);
        }
    }
    let descending = match args.get("order").map(Value::as_str) {
        None | Some(Some("asc")) => false,
        Some(Some("desc")) => true,
        _ => return Err(CallError::Params),
    };
    // Status order does not change results, so it does not change the fingerprint.
    let mut names: Vec<&str> = statuses.iter().map(|s| s.as_str()).collect();
    names.sort_unstable();
    let fingerprint = match &entry {
        Some(entry) => cursor::fingerprint(&[project.as_str(), "history", entry.as_str()]),
        None => {
            let mut parts = vec![
                project.as_str(),
                "list",
                if descending { "desc" } else { "asc" },
            ];
            parts.extend(names);
            cursor::fingerprint(&parts)
        }
    };
    let after = match args.get("cursor") {
        None => None,
        Some(value) => {
            let key = cursor::decode(value, CURSOR_KIND, &fingerprint)?;
            Some(match &entry {
                Some(_) => TimelineKey::Version(
                    key.get("version")
                        .and_then(Value::as_u64)
                        .and_then(|v| u32::try_from(v).ok())
                        .ok_or(CallError::Params)?,
                ),
                None => TimelineKey::Position {
                    occurred_at_ms: key
                        .get("at")
                        .and_then(Value::as_u64)
                        .ok_or(CallError::Params)?,
                    entry: entry_id(
                        key.get("entry")
                            .and_then(Value::as_str)
                            .ok_or(CallError::Params)?,
                    )?,
                },
            })
        }
    };
    Ok(TimelineRead {
        first_page: after.is_none() && entry.is_none(),
        query: TimelineQuery {
            project,
            limit,
            after,
            statuses,
            descending,
            entry,
        },
        fingerprint,
    })
}

fn uncovered_json(item: &UncoveredDecision) -> Value {
    json!({"source":item.source.as_str(),"origin":item.origin,"recorded_at_ms":item.recorded_at_ms,
        "outcome":match item.outcome {
            WorkOutcome::Unknown => "unknown",
            WorkOutcome::Succeeded => "succeeded",
            WorkOutcome::Failed => "failed",
        },"preview":item.preview,"preview_truncated":item.preview_truncated})
}
/// Packs within the `memory_list` budget; the cursor resumes after the last packed entry.
pub(super) fn page_json(read: &TimelineRead, page: TimelinePage) -> Result<Value, CallError> {
    let (uncovered, uncovered_truncated) = if read.first_page {
        (
            page.uncovered.iter().map(uncovered_json).collect(),
            page.uncovered_more,
        )
    } else {
        (Vec::new(), false)
    };
    let mut bytes = Value::Array(uncovered.clone()).to_string().len();
    let count = page.entries.len();
    let mut packed = Vec::new();
    let mut last = None;
    for entry in &page.entries {
        let value = entry_json(entry);
        bytes += value.to_string().len() + 1;
        // The JSON text content envelope may double quotes/escape bytes.
        if bytes > (MAX_RESPONSE_BYTES - 4096) / 2 {
            break;
        }
        packed.push(value);
        last = Some(entry);
    }
    let truncated = packed.len() < count;
    let next_cursor = match last {
        Some(entry) if truncated || page.more => Some(cursor::encode(
            CURSOR_KIND,
            &read.fingerprint,
            match read.query.entry {
                Some(_) => json!({"version":entry.version}),
                None => json!({"at":entry.occurred_at_ms,"entry":entry.entry.as_str()}),
            },
        )?),
        _ => None,
    };
    Ok(
        json!({"project":read.query.project.as_str(),"entries":packed,"truncated":truncated,
        "next_cursor":next_cursor,"uncovered_decisions":uncovered,"uncovered_truncated":uncovered_truncated}),
    )
}

pub(super) fn descriptors() -> [Value; 2] {
    let string = json!({"type":"string"});
    let links = json!({"type":"array","items":{"type":"string"},"maxItems":TIMELINE_MAX_LINKS});
    let statuses = ["proposed", "adopted", "superseded", "reverted", "retracted"];
    [
        tool(
            "memory_timeline_write",
            WRITE_REQUIRED,
            json!({"project":string,"entry":{"type":"string","maxLength":512},
                "title":{"type":"string","maxLength":TIMELINE_MAX_TITLE_BYTES},"summary":string,
                "occurred_at_ms":{"type":"integer","minimum":TIMELINE_MIN_OCCURRED_MS},
                "status":{"enum":statuses},
                "sources":{"type":"array","items":{"type":"string"},"minItems":1,"maxItems":TIMELINE_MAX_LINKS},
                "revisions":links,"memories":links,"supersedes":{"type":"string","maxLength":512},
                "tags":{"type":"array","items":{"type":"string","pattern":"^[a-z0-9][a-z0-9._-]{0,63}$"},"maxItems":TIMELINE_MAX_TAGS},
                "expected_version":{"type":"integer","minimum":1}}),
        ),
        tool(
            "memory_timeline",
            &["project"],
            json!({"project":string,
                "limit":{"type":"integer","minimum":1,"maximum":TIMELINE_MAX_PAGE},
                "statuses":{"type":"array","items":{"enum":statuses},"minItems":1,"maxItems":TIMELINE_MAX_STATUSES},
                "order":{"enum":["asc","desc"]},"entry":{"type":"string","maxLength":512},
                "cursor":{"type":"string","maxLength":cursor::MAX_CURSOR_BYTES}}),
        ),
    ]
}
