//! Pure bounds, trigger, validation and observed-wins conflict policy.
use super::tree_policy::{validate_content, validate_title};
use crate::domain_models::{
    MemoryId,
    memory_tree::{Basis, Confidence},
    reflection::*,
};
pub const PROMPT_VERSION: &str = "runtime-reflection-v1";
pub fn label(basis: Basis, confidence: Option<Confidence>) -> String {
    if basis == Basis::Concluded {
        format!("Concluded ({}): ", confidence.map_or("low", |c| c.as_str()))
    } else {
        String::new()
    }
}
pub use crate::domain_models::reflection::{INPUT_BYTES, in_branch};
pub const POLICY: ReflectionPolicy = ReflectionPolicy {
    due,
    bound,
    validate,
    normalize,
};
pub fn due(count: u64, baseline: i64, mature: &[MemoryId], seen: &[MemoryId]) -> bool {
    i128::from(count) - i128::from(baseline) >= 5 || mature.iter().any(|id| !seen.contains(id))
}
/// Reserve structural overhead and measure the escaped strings without infrastructure types.
fn escaped_bytes(text: &str) -> usize {
    2 + text
        .chars()
        .map(|c| match c {
            '"' | '\\' | '\n' | '\r' | '\t' | '\u{8}' | '\u{c}' => 2,
            c if c <= '\u{1f}' => 6,
            c => c.len_utf8(),
        })
        .sum::<usize>()
}
fn entry_bytes(n: &ReflectionNode) -> usize {
    256 + escaped_bytes(&n.node.title)
        + escaped_bytes(&n.node.content)
        + n.path.iter().map(|p| escaped_bytes(p) + 1).sum::<usize>()
}
pub fn input_bytes(input: &ReflectionInput) -> usize {
    128 + input.nodes.iter().map(entry_bytes).sum::<usize>()
}
pub fn bound(mut input: ReflectionInput) -> ReflectionInput {
    // Inferences cannot crowd their observed evidence out of the request.
    while input
        .nodes
        .iter()
        .filter(|n| n.node.basis == Basis::Concluded)
        .map(entry_bytes)
        .sum::<usize>()
        > (INPUT_BYTES - 128) / 2
    {
        let Some(i) = input
            .nodes
            .iter()
            .enumerate()
            .filter(|(_, n)| n.node.basis == Basis::Concluded)
            .min_by_key(|(_, n)| n.node.last_linked_ms)
            .map(|(i, _)| i)
        else {
            break;
        };
        input.nodes.remove(i);
    }
    while input_bytes(&input) > INPUT_BYTES {
        let newest = input
            .nodes
            .iter()
            .enumerate()
            .filter(|(_, n)| n.node.basis == Basis::Observed)
            .max_by_key(|(_, n)| n.node.last_linked_ms)
            .map(|(i, _)| i);
        let candidate = input
            .nodes
            .iter()
            .enumerate()
            .filter(|(i, n)| n.node.basis == Basis::Observed && Some(*i) != newest)
            .max_by_key(|(_, n)| (n.node.depth, std::cmp::Reverse(n.node.last_linked_ms)))
            .map(|(i, _)| i)
            .or_else(|| {
                input
                    .nodes
                    .iter()
                    .enumerate()
                    .filter(|(_, n)| n.node.basis == Basis::Concluded)
                    .min_by_key(|(_, n)| n.node.last_linked_ms)
                    .map(|(i, _)| i)
            });
        let Some(i) = candidate else { break };
        input.nodes.remove(i);
    }
    input
}
pub fn validate(input: &ReflectionInput, answer: &ReflectionAnswer) -> bool {
    let node = |i: usize| input.nodes.get(i);
    answer.conclusions.len() <= 3
        && answer.confidence_updates.len() <= 10
        && answer.contradictions.len() <= 10
        && answer.conclusions.iter().all(|c| {
            validate_title(&c.title).is_ok()
                && validate_content(&c.content).is_ok()
                && node(c.parent).is_some_and(|n| n.inside)
                && c.derived_from.iter().all(|i| node(*i).is_some())
                && c.derived_from
                    .iter()
                    .collect::<std::collections::HashSet<_>>()
                    .len()
                    >= 2
        })
        && answer
            .confidence_updates
            .iter()
            .all(|(i, _)| node(*i).is_some_and(|n| n.inside && n.node.basis == Basis::Concluded))
        && answer.contradictions.iter().all(|(c, o)| {
            node(*c).is_some_and(|n| n.inside && n.node.basis == Basis::Concluded)
                && node(*o).is_some_and(|n| n.node.basis == Basis::Observed)
        })
}
/// Normalize precedence and drop only conclusions invalidated by same-answer retirement.
pub fn normalize(input: &ReflectionInput, mut answer: ReflectionAnswer) -> ReflectionAnswer {
    let retired: Vec<_> = answer
        .contradictions
        .iter()
        .filter(|(c, _)| input.nodes[*c].node.confidence == Some(Confidence::Low))
        .map(|(c, _)| *c)
        .collect();
    answer.conclusions.retain(|c| {
        !retired.contains(&c.parent) && !c.derived_from.iter().any(|i| retired.contains(i))
    });
    let mut seen = std::collections::HashSet::new();
    answer
        .confidence_updates
        .retain(|(i, _)| !answer.contradictions.iter().any(|(c, _)| c == i) && seen.insert(*i));
    answer
}
pub fn conflicts(
    concluded: &MemoryId,
    selected_observed: &[MemoryId],
    contradictions: &[(MemoryId, MemoryId)],
) -> bool {
    contradictions
        .iter()
        .any(|(c, o)| c == concluded && selected_observed.contains(o))
}
