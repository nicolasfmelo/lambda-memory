//! Wire decoding of stored `action` sources into aggregation records; JSON stays in integrations.
use crate::domain_models::SourceId;
use crate::domain_models::capture::CaptureEvent;
use crate::domain_models::procedural::{ActionEpisode, EpisodeRecord};
use serde_json::Value;

/// Episode content is versioned JSON of at most 1024 bytes with exactly the known fields.
pub fn parse_episode(content: &str) -> Option<ActionEpisode> {
    if content.len() > 1024 {
        return None;
    }
    let Value::Object(map) = serde_json::from_str(content).ok()? else {
        return None;
    };
    const FIELDS: [&str; 7] = [
        "v",
        "tool",
        "intent",
        "summary",
        "error_head",
        "session",
        "seq",
    ];
    if map.keys().any(|key| !FIELDS.contains(&key.as_str())) || map.get("v")?.as_u64()? != 1 {
        return None;
    }
    let string = |name: &str| map.get(name)?.as_str().map(str::to_owned);
    let error_head = match map.get("error_head") {
        None | Some(Value::Null) => None,
        Some(Value::String(head)) => Some(head.clone()),
        _ => return None,
    };
    Some(ActionEpisode {
        tool: string("tool")?,
        intent: string("intent")?,
        summary: string("summary")?,
        error_head,
        session: string("session")?,
        seq: map.get("seq")?.as_u64()?,
    })
}

/// Records of the events whose content parses, keeping each source ID; the rest are skipped.
pub fn records(events: &[CaptureEvent]) -> Vec<(SourceId, EpisodeRecord)> {
    events
        .iter()
        .filter_map(|event| {
            let episode = parse_episode(&event.content)?;
            Some((
                event.source.clone(),
                EpisodeRecord {
                    episode,
                    outcome: event.outcome.clone(),
                    recorded_at_ms: event.recorded_at_ms,
                },
            ))
        })
        .collect()
}
