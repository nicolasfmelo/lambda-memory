//! Single-project, provider-free operator source transfer.
use lambda_memory::domain_models::SourceId;
use lambda_memory::domain_models::capture::*;
use lambda_memory::domain_models::runtime::{RuntimeError, RuntimeLimits, RuntimeScope};
use lambda_memory::engines::{capture_policy, runtime_policy};
use lambda_memory::integrations::action_episodes::parse_episode;
use lambda_memory::integrations::database::embedded_capture::{
    EmbeddedCaptureStore, EmbeddedRuntimeStore,
};
use lambda_memory::integrations::exchange_content::parse_exchange;
use lambda_memory::integrations::runtime_config::RuntimeConfig;
use serde_json::{Value, json};
use std::io::{Read, Write};

const MAX_BYTES: usize = 64 * 1024 * 1024;
const MAX_LINES: usize = 100_000;
const FIELDS: [&str; 6] = [
    "source",
    "origin",
    "recorded_at_ms",
    "kind",
    "outcome",
    "content",
];

fn invalid() -> RuntimeError {
    RuntimeError::InvalidInput(
        "source transfer requires one project and valid JSON Lines (64 MiB, 100000 lines maximum)",
    )
}

struct Line {
    source: String,
    origin: String,
    recorded_at_ms: u64,
    kind: String,
    outcome: String,
    content: String,
}

/// Frame and type-check the entire stream before opening the store or writing anything.
fn read_input() -> Result<Vec<Line>, RuntimeError> {
    let mut bytes = Vec::new();
    std::io::stdin()
        .take(MAX_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| invalid())?;
    if bytes.len() > MAX_BYTES {
        return Err(invalid());
    }
    let text = std::str::from_utf8(&bytes).map_err(|_| invalid())?;
    let mut lines = Vec::new();
    for line in text.lines() {
        if lines.len() == MAX_LINES {
            return Err(invalid());
        }
        let value: Value = serde_json::from_str(line).map_err(|_| invalid())?;
        let map = value.as_object().ok_or_else(invalid)?;
        if map.len() != FIELDS.len() || map.keys().any(|key| !FIELDS.contains(&key.as_str())) {
            return Err(invalid());
        }
        let string = |key: &str| {
            map.get(key)
                .and_then(Value::as_str)
                .map(str::to_owned)
                .ok_or_else(invalid)
        };
        lines.push(Line {
            source: string("source")?,
            origin: string("origin")?,
            recorded_at_ms: map
                .get("recorded_at_ms")
                .and_then(Value::as_u64)
                .filter(|timestamp| i64::try_from(*timestamp).is_ok())
                .ok_or_else(invalid)?,
            kind: string("kind")?,
            outcome: string("outcome")?,
            content: string("content")?,
        });
    }
    Ok(lines)
}

fn validated(line: Line, scope: &RuntimeScope, limits: &RuntimeLimits) -> Option<CaptureEvent> {
    let event = CaptureEvent {
        source: SourceId::new(line.source).ok()?,
        owner: scope.owner.clone(),
        actor: scope.actor.clone(),
        project: scope.projects[0].clone(),
        origin: line.origin.clone(),
        recorded_at_ms: line.recorded_at_ms,
        kind: match line.kind.as_str() {
            "attempt" => WorkEventKind::Attempt,
            "result" => WorkEventKind::Result,
            "error" => WorkEventKind::Error,
            "decision" => WorkEventKind::Decision,
            "action" => WorkEventKind::Action,
            "exchange" => WorkEventKind::Exchange,
            "lesson"
                if line.origin.starts_with("lesson://")
                    || line.origin.starts_with("promote://") =>
            {
                WorkEventKind::Lesson
            }
            _ => return None,
        },
        outcome: match line.outcome.as_str() {
            "unknown" => WorkOutcome::Unknown,
            "succeeded" => WorkOutcome::Succeeded,
            "failed" => WorkOutcome::Failed,
            _ => return None,
        },
        content: line.content,
    };
    capture_policy::validate(
        &event,
        &CaptureLimits {
            max_content_bytes: limits.max_content_bytes,
            max_pending_per_owner: limits.max_pending,
        },
    )
    .ok()?;
    match event.kind {
        WorkEventKind::Action => {
            let episode = parse_episode(&event.content)?;
            capture_policy::validate_action(&episode).ok()?;
            if [
                Some(&episode.intent),
                Some(&episode.summary),
                episode.error_head.as_ref(),
            ]
            .into_iter()
            .flatten()
            .any(|text| capture_policy::contains_secret(text))
            {
                return None;
            }
        }
        WorkEventKind::Exchange => {
            let (prompt, reply) = parse_exchange(&event.content)?;
            capture_policy::validate_exchange(&prompt, &reply).ok()?;
        }
        WorkEventKind::Lesson => {
            let lesson = lambda_memory::integrations::lesson_content::parse_lesson(&event.content)?;
            if lesson.strings().any(capture_policy::contains_secret) {
                return None;
            }
        }
        _ => {
            if capture_policy::contains_secret(&event.content) {
                return None;
            }
        }
    }
    Some(event)
}

pub async fn run(command: &str) -> Result<(), RuntimeError> {
    if std::env::args().count() != 2 {
        return Err(invalid());
    }
    let lines = if command == "--import-sources" {
        read_input()?
    } else {
        Vec::new()
    };
    let config = RuntimeConfig::from_environment()?;
    runtime_policy::validate_config(&config.scope, &config.limits)?;
    if config.scope.projects.len() != 1 {
        return Err(invalid());
    }
    let project = &config.scope.projects[0];
    // No MemoryRuntime capture_at: identity consolidation preserves sources of any age.
    let store = EmbeddedRuntimeStore::new(
        EmbeddedCaptureStore::open(&config.path)
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?,
    );
    match command {
        "--inspect-sources" => {
            let (sources, curatable, pending) =
                store.inspect_sources(&config.scope.owner, project).await?;
            println!(
                "{}",
                json!({"project":project.as_str(), "sources":sources, "curatable":curatable,
                "pending":pending, "max_pending":config.limits.max_pending})
            );
        }
        "--export-sources" => {
            let rows = store.export_sources(&config.scope.owner, project).await?;
            let mut stdout = std::io::BufWriter::new(std::io::stdout().lock());
            for row in rows {
                writeln!(stdout, "{row}").map_err(|_| RuntimeError::StorageUnavailable)?;
            }
            stdout
                .flush()
                .map_err(|_| RuntimeError::StorageUnavailable)?;
        }
        "--import-sources" => {
            let (mut inserted, mut duplicates, mut remaining) = (0usize, 0usize, 0usize);
            let (mut conflicts, mut rejected) = (Vec::new(), Vec::new());
            let (mut inserted_ids, mut duplicate_ids, mut remaining_ids) =
                (Vec::new(), Vec::new(), Vec::new());
            for line in lines {
                let id = line.source.clone();
                let Some(event) = validated(line, &config.scope, &config.limits) else {
                    rejected.push(id);
                    continue;
                };
                match store.import_source(event, &config.limits).await {
                    Ok(receipt) => match receipt.status {
                        CaptureStatus::SavedPending => {
                            inserted += 1;
                            inserted_ids.push(id);
                        }
                        CaptureStatus::AlreadySavedPending => {
                            duplicates += 1;
                            duplicate_ids.push(id);
                        }
                    },
                    Err(RuntimeError::Conflict) => conflicts.push(id),
                    Err(RuntimeError::BacklogFull) => {
                        remaining += 1;
                        remaining_ids.push(id);
                    }
                    Err(error) => return Err(error),
                }
            }
            println!(
                "{}",
                json!({"project":project.as_str(), "inserted":inserted, "duplicates":duplicates,
                "conflicts":conflicts, "rejected":rejected, "remaining":remaining,
                "inserted_ids":inserted_ids, "duplicate_ids":duplicate_ids, "remaining_ids":remaining_ids})
            );
        }
        _ => return Err(invalid()),
    }
    Ok(())
}
