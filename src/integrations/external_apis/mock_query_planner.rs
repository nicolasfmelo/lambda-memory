//! Structured planning mock, not Luna Decisions or a paid text generator.
use crate::domain_models::{MemoryError, MemoryQueryPlan, RecallRequest};
use crate::integrations::memory_ports::MemoryQueryPlanner;
pub struct MockQueryPlanner {
    reformulate: bool,
}
impl MockQueryPlanner {
    pub fn new(reformulate: bool) -> Self {
        Self { reformulate }
    }
}
impl MemoryQueryPlanner for MockQueryPlanner {
    async fn plan(&self, request: &RecallRequest) -> Result<MemoryQueryPlan, MemoryError> {
        let mut terms: Vec<String> = request
            .task
            .to_lowercase()
            .split(|c: char| !c.is_alphanumeric())
            .filter(|term| !term.is_empty())
            .map(str::to_owned)
            .collect();
        if self.reformulate {
            // Tiny disclosed lexical expansion, no gold labels or fixture lookup.
            for term in terms.clone() {
                match term.as_str() {
                    "database" => terms.push("storage".into()),
                    "remembered" | "knowledge" => terms.push("memory".into()),
                    _ => {}
                }
            }
        }
        terms.sort();
        terms.dedup();
        Ok(MemoryQueryPlan {
            terms,
            owner: request.owner.clone(),
            actor: request.actor.clone(),
            project: request.project.clone(),
            relations: vec![],
        })
    }
}
