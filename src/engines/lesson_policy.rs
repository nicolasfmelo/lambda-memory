//! Pure input bounds and all-or-nothing lesson validation.
use super::{capture_policy::contains_secret, procedural_policy};
use crate::domain_models::{capture::CaptureEvent, lessons::*, procedural::EpisodeRecord};
pub const INPUT_BYTES: usize = 48 * 1024;
pub const PROMPT_VERSION: &str = "session-lessons-v1";

/// Conservative serialized size, including JSON escapes and field/slot punctuation.
/// The integration emits only these fields. Overhead deliberately exceeds the exact encoding.
fn text_bytes(text: &str) -> usize {
    2 + text
        .chars()
        .map(|c| match c {
            '"' | '\\' => 2,
            c if c <= '\u{1f}' => 6,
            c => c.len_utf8(),
        })
        .sum::<usize>()
}
fn digest_text(text: &str) -> String {
    let mut result = String::new();
    for c in text.chars().take(200) {
        result.push(c);
        if text_bytes(&result) > 600 {
            result.pop();
            break;
        }
    }
    result
}
pub fn input_bytes(input: &LessonInput) -> usize {
    64 + input
        .exchanges
        .iter()
        .map(|e| 96 + e.slot.to_string().len() + text_bytes(&e.prompt) + text_bytes(&e.reply))
        .sum::<usize>()
        + input
            .digest
            .iter()
            .map(|d| {
                160 + text_bytes(&d.failed)
                    + text_bytes(&d.error)
                    + d.fixed_by.as_deref().map_or(4, text_bytes)
                    + d.evidence
                        .iter()
                        .map(|i| 8 + i.to_string().len())
                        .sum::<usize>()
            })
            .sum::<usize>()
}
pub fn build_input(
    mut exchanges: Vec<(CaptureEvent, String, String)>,
    actions: Vec<(CaptureEvent, EpisodeRecord)>,
) -> Option<LessonInput> {
    exchanges.sort_by(|a, b| {
        (a.0.recorded_at_ms, a.0.source.as_str()).cmp(&(b.0.recorded_at_ms, b.0.source.as_str()))
    });
    if exchanges.is_empty() {
        return None;
    }
    let records: Vec<_> = actions.iter().map(|(_, r)| r.clone()).collect();
    let digest = procedural_policy::session_digest(&records);
    let mut action_indexes: Vec<usize> = digest
        .iter()
        .flat_map(|d| d.evidence.iter().copied())
        .collect();
    action_indexes.sort_unstable();
    action_indexes.dedup();
    let mut input = LessonInput {
        sources: exchanges.iter().map(|(e, _, _)| e.clone()).collect(),
        exchanges: exchanges
            .into_iter()
            .enumerate()
            .map(|(slot, (_, prompt, reply))| LessonExchange {
                slot,
                prompt,
                reply,
            })
            .collect(),
        digest,
    };
    // Allocate all action slots after the exchanges, in the input's recorded-time order.
    input
        .sources
        .extend(action_indexes.iter().map(|i| actions[*i].0.clone()));
    for line in &mut input.digest {
        for i in &mut line.evidence {
            *i = input.exchanges.len() + action_indexes.iter().position(|j| j == i).unwrap();
        }
        line.failed = digest_text(&line.failed);
        line.error = digest_text(&line.error);
        line.fixed_by = line.fixed_by.as_deref().map(digest_text);
    }
    for i in 0..input.exchanges.len() {
        if input_bytes(&input) <= INPUT_BYTES {
            break;
        }
        input.exchanges[i].reply = input.exchanges[i].reply.chars().take(1000).collect();
    }
    for i in 0..input.exchanges.len() {
        if input_bytes(&input) <= INPUT_BYTES {
            break;
        }
        input.exchanges[i].prompt = input.exchanges[i].prompt.chars().take(500).collect();
    }
    while input_bytes(&input) > INPUT_BYTES && input.exchanges.len() > 1 {
        input.exchanges.remove(0);
        input.sources.remove(0);
        for e in &mut input.exchanges {
            e.slot -= 1;
        }
        for d in &mut input.digest {
            for i in &mut d.evidence {
                *i -= 1;
            }
        }
    }
    Some(input)
}
fn title_valid(text: &str) -> bool {
    !text.trim().is_empty()
        && text.chars().count() <= 80
        && !text.chars().any(char::is_control)
        && !contains_secret(text)
}
fn text_valid(text: &str) -> bool {
    !text.trim().is_empty() && text.len() <= 600 && !contains_secret(text)
}
pub fn validate(input: &LessonInput, answer: &LessonAnswer) -> bool {
    let slots = |e: &[usize], max| e.len() <= max && e.iter().all(|i| *i < input.sources.len());
    answer.tasks.len() <= 12
        && answer.lessons.len() <= 6
        && answer
            .tasks
            .iter()
            .all(|t| title_valid(&t.title) && slots(&t.evidence, 16))
        && answer.lessons.iter().all(|l| {
            title_valid(&l.title)
                && text_valid(&l.applies_when)
                && text_valid(&l.guidance)
                && !l.evidence.is_empty()
                && slots(&l.evidence, 8)
                && answer.tasks.get(l.task).is_some_and(|t| {
                    t.outcome != TaskOutcome::Unknown
                        && (l.kind != LessonKind::Strategy || t.outcome == TaskOutcome::Succeeded)
                })
        })
}
