//! Pure procedural-memory policy: deterministic fact aggregation and the cited-playbook validator.
//! No clock (the caller passes `now_ms`), no I/O, no serialization.
use std::collections::{BTreeMap, BTreeSet};

use crate::domain_models::capture::WorkOutcome;
pub use crate::domain_models::procedural::{
    EpisodeRecord, FactKind, PlaybookRejection, ProceduralFact,
};

const MAX_EPISODES: usize = 5_000;
const MAX_PROCEDURES: usize = 15;
const MAX_PITFALLS: usize = 10;
const MAX_HOT_FILES: usize = 5;
const MIN_SESSIONS: usize = 2;
const PITFALL_WINDOW: u64 = 5;
const PITFALL_PREFIX_TOKENS: usize = 2;
const PLAYBOOK_MAX_BYTES: usize = 1536;
const DAY_MS: f64 = 86_400_000.0;
const FULL_WEIGHT_DAYS: f64 = 7.0;
const FLOOR_WEIGHT_DAYS: f64 = 90.0;
const FLOOR_WEIGHT: f64 = 0.25;

fn canonical_tool(tool: &str) -> String {
    match tool {
        "Bash" => "bash".into(),
        "Edit" | "MultiEdit" | "NotebookEdit" => "edit".into(),
        "Write" => "write".into(),
        "Task" | "Agent" => "task".into(),
        other => other.to_lowercase(),
    }
}
fn is_file_tool(tool: &str) -> bool {
    matches!(tool, "edit" | "write")
}
fn is_command_tool(tool: &str) -> bool {
    matches!(tool, "bash" | "eval")
}
fn looks_like_path(token: &str) -> bool {
    !token.starts_with('-')
        && !token.contains("://")
        && (token.contains('/') || token.starts_with('~'))
}
fn is_absolute_path(token: &str) -> bool {
    (token.starts_with('/') || token.starts_with('~')) && !token.contains("://")
}

/// `<prefix>=<path>` for a non-empty prefix and an absolute path value.
fn mask_assignment(token: &str) -> Option<String> {
    let (prefix, value) = token.split_once('=')?;
    (!prefix.is_empty() && is_absolute_path(value)).then(|| format!("{prefix}=<path>"))
}

/// `tool` (canonical, lowercase) plus the summary with whitespace collapsed. In command tools,
/// every absolute path token (`/…` or `~…`) becomes `<path>` wherever it appears, a
/// `<prefix>=<absolute path>` token becomes `<prefix>=<path>`, and a trailing run of path
/// arguments becomes one `<path>`; consecutive `<path>` tokens collapse into one (assignments
/// do not). File tools keep the path as is.
pub fn normalize_key(tool: &str, summary: &str) -> String {
    let tool = canonical_tool(tool);
    let mut tokens: Vec<String> = summary.split_whitespace().map(str::to_owned).collect();
    if is_command_tool(&tool) {
        for token in &mut tokens {
            if is_absolute_path(token) {
                *token = "<path>".into();
            } else if let Some(masked) = mask_assignment(token) {
                *token = masked;
            }
        }
        tokens.dedup_by(|next, previous| next == "<path>" && previous == "<path>");
        let mut end = tokens.len();
        while end > 1 && (tokens[end - 1] == "<path>" || looks_like_path(&tokens[end - 1])) {
            end -= 1;
        }
        if end < tokens.len() {
            tokens.truncate(end);
            tokens.push("<path>".into());
        }
    }
    if tokens.is_empty() {
        tool
    } else {
        format!("{tool} {}", tokens.join(" "))
    }
}

/// 1.0 up to 7 days, linear down to 0.25 at 90 days, 0.25 after.
fn recency_weight(age_ms: u64) -> f64 {
    let days = age_ms as f64 / DAY_MS;
    if days <= FULL_WEIGHT_DAYS {
        1.0
    } else if days >= FLOOR_WEIGHT_DAYS {
        FLOOR_WEIGHT
    } else {
        1.0 - (1.0 - FLOOR_WEIGHT) * (days - FULL_WEIGHT_DAYS)
            / (FLOOR_WEIGHT_DAYS - FULL_WEIGHT_DAYS)
    }
}

struct Entry<'a> {
    record: &'a EpisodeRecord,
    tool: String,
    key: String,
    /// Summary tokens of the normalized key, without the tool.
    tokens: Vec<String>,
}
struct Scored {
    fact: ProceduralFact,
    score: f64,
}

fn shared_prefix(a: &[String], b: &[String]) -> usize {
    a.iter().zip(b).take_while(|(x, y)| x == y).count()
}
fn words(text: &str) -> Vec<String> {
    text.split_whitespace().map(str::to_lowercase).collect()
}

fn score(successes: u32, failures: u32, last_seen_ms: u64, now_ms: u64) -> f64 {
    f64::from(successes + failures) * recency_weight(now_ms.saturating_sub(last_seen_ms))
}
fn count(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

fn top(mut items: Vec<Scored>, cap: usize) -> Vec<Scored> {
    items.sort_by(rank);
    items.truncate(cap);
    items
}
fn rank(a: &Scored, b: &Scored) -> std::cmp::Ordering {
    b.score
        .total_cmp(&a.score)
        .then_with(|| a.fact.key.cmp(&b.fact.key))
        .then_with(|| a.fact.detail.cmp(&b.fact.detail))
        .then_with(|| (a.fact.kind as u8).cmp(&(b.fact.kind as u8)))
}

#[derive(Default)]
struct Tally {
    successes: u32,
    failures: u32,
    sessions: BTreeSet<String>,
    last_seen_ms: u64,
}

/// Ranked facts (at most 15 procedures, 10 pitfalls and 5 hot files) with IDs `F1..Fn`.
/// Only the 5,000 most recent records are read; `Unknown` outcomes are ignored.
pub fn aggregate(records: &[EpisodeRecord], now_ms: u64) -> Vec<ProceduralFact> {
    let mut recent: Vec<&EpisodeRecord> = records
        .iter()
        .filter(|record| record.outcome != WorkOutcome::Unknown)
        .collect();
    recent.sort_by_key(|record| std::cmp::Reverse(record.recorded_at_ms));
    recent.truncate(MAX_EPISODES);
    let entries: Vec<Entry> = recent
        .into_iter()
        .map(|record| {
            let tool = canonical_tool(&record.episode.tool);
            let key = normalize_key(&tool, &record.episode.summary);
            let tokens = key.split_whitespace().skip(1).map(str::to_string).collect();
            Entry {
                record,
                tool,
                key,
                tokens,
            }
        })
        .collect();

    let mut procedures: BTreeMap<&str, Tally> = BTreeMap::new();
    let mut files: BTreeMap<&str, Tally> = BTreeMap::new();
    for entry in &entries {
        let (episode, succeeded) = (
            &entry.record.episode,
            entry.record.outcome == WorkOutcome::Succeeded,
        );
        if episode.summary.trim().is_empty() {
            continue;
        }
        let tally = if entry.tool == "edit" {
            if !succeeded {
                continue;
            }
            files
                .entry(entry.key.strip_prefix("edit ").unwrap_or(&entry.key))
                .or_default()
        } else if is_file_tool(&entry.tool) {
            continue;
        } else {
            procedures.entry(&entry.key).or_default()
        };
        if succeeded {
            tally.successes += 1;
            tally.sessions.insert(episode.session.clone());
        } else {
            tally.failures += 1;
        }
        tally.last_seen_ms = tally.last_seen_ms.max(entry.record.recorded_at_ms);
    }
    let scored = |kind: FactKind, key: &str, tally: Tally| Scored {
        score: score(tally.successes, tally.failures, tally.last_seen_ms, now_ms),
        fact: ProceduralFact {
            id: String::new(),
            kind,
            key: key.to_string(),
            detail: None,
            successes: tally.successes,
            failures: tally.failures,
            sessions: count(tally.sessions.len()),
            last_seen_ms: tally.last_seen_ms,
        },
    };
    let procedure_facts = procedures
        .into_iter()
        .filter(|(_, tally)| tally.sessions.len() >= MIN_SESSIONS)
        .map(|(key, tally)| scored(FactKind::Procedure, key, tally))
        .collect();
    let hot_facts = files
        .into_iter()
        .filter(|(_, tally)| tally.sessions.len() >= MIN_SESSIONS)
        .map(|(key, tally)| scored(FactKind::HotFile, key, tally))
        .collect();

    let mut selected = top(procedure_facts, MAX_PROCEDURES);
    selected.extend(top(pitfalls(&entries, now_ms), MAX_PITFALLS));
    selected.extend(top(hot_facts, MAX_HOT_FILES));
    selected.sort_by(rank);
    selected
        .into_iter()
        .enumerate()
        .map(|(index, mut item)| {
            item.fact.id = format!("F{}", index + 1);
            item.fact
        })
        .collect()
}

/// A failed episode with an error head, followed within the next 5 `seq` positions of its
/// session by a successful episode of the same tool whose command key or intent shares a prefix
/// of at least 2 tokens. Grouped by (error head, fixing key); a group needs at least 2 sessions.
fn pitfalls(entries: &[Entry], now_ms: u64) -> Vec<Scored> {
    let mut sessions: BTreeMap<&str, Vec<&Entry>> = BTreeMap::new();
    for entry in entries {
        sessions
            .entry(&entry.record.episode.session)
            .or_default()
            .push(entry);
    }
    let mut groups: BTreeMap<(String, String), Tally> = BTreeMap::new();
    for (session, mut episodes) in sessions {
        episodes.sort_by_key(|entry| (entry.record.episode.seq, entry.record.recorded_at_ms));
        for (at, failure) in episodes.iter().enumerate() {
            let head = failure
                .record
                .episode
                .error_head
                .as_deref()
                .map(|head| head.split_whitespace().collect::<Vec<_>>().join(" "))
                .filter(|head| !head.is_empty());
            let Some(head) = head else { continue };
            if failure.record.outcome != WorkOutcome::Failed {
                continue;
            }
            let (seq, intent) = (
                failure.record.episode.seq,
                words(&failure.record.episode.intent),
            );
            let fix = episodes[at + 1..]
                .iter()
                .take_while(|next| next.record.episode.seq <= seq.saturating_add(PITFALL_WINDOW))
                .find(|next| {
                    next.record.outcome == WorkOutcome::Succeeded
                        && next.record.episode.seq > seq
                        && next.tool == failure.tool
                        && (shared_prefix(&failure.tokens, &next.tokens) >= PITFALL_PREFIX_TOKENS
                            || shared_prefix(&intent, &words(&next.record.episode.intent))
                                >= PITFALL_PREFIX_TOKENS)
                });
            let Some(fix) = fix else { continue };
            let tally = groups.entry((head, fix.key.clone())).or_default();
            tally.successes += 1;
            tally.sessions.insert(session.to_string());
            tally.last_seen_ms = tally.last_seen_ms.max(fix.record.recorded_at_ms);
        }
    }
    groups
        .into_iter()
        .filter(|(_, tally)| tally.sessions.len() >= MIN_SESSIONS)
        .map(|((head, key), tally)| Scored {
            score: score(tally.successes, 0, tally.last_seen_ms, now_ms),
            fact: ProceduralFact {
                id: String::new(),
                kind: FactKind::Pitfall,
                key,
                detail: Some(head),
                successes: tally.successes,
                failures: 0,
                sessions: count(tally.sessions.len()),
                last_seen_ms: tally.last_seen_ms,
            },
        })
        .collect()
}

/// Every `[F<digits>]` in `text`, in order of first appearance, without repeats.
pub fn cited_facts(text: &str) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    for line in text.lines() {
        for id in line_citations(line) {
            if !found.contains(&id) {
                found.push(id);
            }
        }
    }
    found
}
fn line_citations(line: &str) -> Vec<String> {
    let mut ids = Vec::new();
    for (at, _) in line.match_indices("[F") {
        let rest = &line[at + 2..];
        let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
        if digits > 0 && rest[digits..].starts_with(']') {
            ids.push(format!("F{}", &rest[..digits]));
        }
    }
    ids
}
/// Accepts only non-empty text of at most 1,536 bytes where every non-empty line is either a
/// heading (starts with `#`) or a bullet (`- ` or `* `) ending with citations (optionally followed
/// by one `.`, `;` or `:`), and every citation names a known fact. Other prose is `UncitedBullet`.
pub fn validate_playbook(text: &str, facts: &[ProceduralFact]) -> Result<(), PlaybookRejection> {
    if text.trim().is_empty() {
        return Err(PlaybookRejection::Empty);
    }
    if text.len() > PLAYBOOK_MAX_BYTES {
        return Err(PlaybookRejection::TooLarge);
    }
    for line in text.lines() {
        let trimmed = line.trim_start();
        let bullet = trimmed.starts_with("- ") || trimmed.starts_with("* ");
        if !trimmed.is_empty()
            && !trimmed.starts_with('#')
            && (!bullet || !bullet_is_cited(trimmed))
        {
            return Err(PlaybookRejection::UncitedBullet);
        }
        if let Some(unknown) = line_citations(line)
            .into_iter()
            .find(|id| !facts.iter().any(|fact| &fact.id == id))
        {
            return Err(PlaybookRejection::UnknownCitation(unknown));
        }
    }
    Ok(())
}
/// Trailing citations: a run of `[F#]` tokens separated only by whitespace at the end of the line,
/// optionally followed by one `.`, `;` or `:`.
fn bullet_is_cited(line: &str) -> bool {
    let mut rest = line.trim_end();
    if let Some(stripped) = rest.strip_suffix(['.', ';', ':']) {
        rest = stripped;
    }
    let mut cited = false;
    while rest.ends_with(']') {
        let Some(open) = rest.rfind('[') else { break };
        if line_citations(&rest[open..]).len() != 1 {
            break;
        }
        cited = true;
        rest = rest[..open].trim_end();
    }
    cited
}
