//! Pure eligibility, validation, rendering and lifecycle rules for skill drafts.
use super::capture_policy::contains_secret;
use crate::domain_models::{
    memory_tree::{LessonStats, TreeNode},
    skills::*,
};
pub const PROMPT_VERSION: &str = "runtime_skill_draft_v1";

pub fn has_steps(content: &str) -> bool {
    content
        .lines()
        .filter(|line| {
            let line = line.trim_start();
            let rest = if line.starts_with(['-', '*']) {
                &line[1..]
            } else {
                let digits = line.bytes().take_while(u8::is_ascii_digit).count();
                if digits == 0 || !line[digits..].starts_with(['.', ')']) {
                    return false;
                }
                &line[digits + 1..]
            };
            rest.starts_with(char::is_whitespace) && !rest.trim().is_empty()
        })
        .take(2)
        .count()
        == 2
}
pub fn candidate_rule(node: &TreeNode, stats: &LessonStats) -> bool {
    node.basis == crate::domain_models::memory_tree::Basis::Observed
        && node.root.is_lesson()
        && node.depth > 0
        && stats.strategies >= 1
        && stats.pitfalls == 0
        && stats.sessions.len() >= SKILL_MIN_SESSIONS
        && has_steps(&node.content)
}
pub fn eligible(candidate: Option<&SkillCandidate>, sessions: usize) -> bool {
    match candidate {
        None => true,
        Some(c) => {
            c.status == SkillStatus::Stale
                || (c.status == SkillStatus::Rejected
                    && c.sessions_at_decision
                        .is_some_and(|n| sessions >= n.saturating_add(REJECT_COOLDOWN_SESSIONS)))
        }
    }
}
fn single_line(text: &str) -> bool {
    !text
        .chars()
        .any(|c| c.is_control() || matches!(c, '\u{2028}' | '\u{2029}'))
        && !contains_secret(text)
}
pub fn validate(answer: &SkillDraftAnswer) -> bool {
    let name = &answer.name;
    !name.is_empty()
        && name.len() <= 48
        && name.split('-').all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        })
        && answer.description.starts_with("Use when")
        && answer.description.chars().count() <= 300
        && (2..=12).contains(&answer.steps.len())
        && answer.pitfalls.len() <= 8
        && std::iter::once(name)
            .chain(std::iter::once(&answer.description))
            .chain(answer.steps.iter())
            .chain(answer.pitfalls.iter())
            .all(|s| single_line(s))
        && answer
            .steps
            .iter()
            .chain(&answer.pitfalls)
            .all(|s| s.chars().count() <= 300)
}
pub fn render(input: &SkillDraftInput, answer: &SkillDraftAnswer) -> String {
    let description = answer
        .description
        .replace('\\', "\\\\")
        .replace('"', "\\\"");
    let mut text = format!(
        "---\nname: {}\ndescription: \"{}\"\n---\n\n# {}\n\n## Steps\n",
        answer.name, description, input.node.title
    );
    for (i, step) in answer.steps.iter().enumerate() {
        text.push_str(&format!("{}. {}\n", i + 1, step));
    }
    if !answer.pitfalls.is_empty() {
        text.push_str("\n## Pitfalls\n");
        for pitfall in &answer.pitfalls {
            text.push_str(&format!("- {pitfall}\n"));
        }
    }
    text.push_str(&format!("\n## Evidence\nLambda Memory {} lesson node {} (revision {}), reinforced by {} sessions: {}.\n", input.scope, input.node.memory, input.node.revision, input.sessions.len(), input.sessions.join(", ")));
    text
}
pub fn valid_decision(candidate: &SkillCandidate, decision: &SkillDecision) -> bool {
    use SkillStatus::*;
    let transition = matches!(
        (candidate.status, decision.decision),
        (Drafted | Stale, Approved | Rejected) | (Approved, Rejected | Installed)
    );
    let files = &decision.install_paths;
    transition
        && (decision.decision != Approved || candidate.draft.is_some())
        && if decision.decision == Installed {
            !files.is_empty()
                && files.len() <= 4
                && files.len() == decision.install_hashes.len()
                && files
                    .iter()
                    .all(|p| p.starts_with('/') && !p.chars().any(char::is_control))
                && decision
                    .install_hashes
                    .iter()
                    .all(|h| h.len() == 64 && h.bytes().all(|b| b.is_ascii_hexdigit()))
        } else {
            files.is_empty() && decision.install_hashes.is_empty()
        }
}
