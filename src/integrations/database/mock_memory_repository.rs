use crate::domain_models::{
    ActorId, Memory, MemoryError, MemoryId, MemoryQueryPlan, MemoryRelationFilter, OwnerId,
    ProjectId, SourceId,
};
use crate::integrations::memory_ports::MemoryRepository;
use crate::integrations::mock_terms::overlap;

pub struct MockMemoryRepository {
    corpus: Vec<Memory>,
}
impl MockMemoryRepository {
    pub fn new(corpus: Vec<Memory>) -> Self {
        Self { corpus }
    }
}
impl MemoryRepository for MockMemoryRepository {
    async fn search(
        &self,
        request: &MemoryQueryPlan,
        limit: usize,
    ) -> Result<Vec<Memory>, MemoryError> {
        // Backend scope filtering, independently enforced again by the service's policy engine.
        let mut hits: Vec<_> = self
            .corpus
            .iter()
            .filter(|m| {
                m.owner == request.owner
                    && (m.project == request.project || m.project.as_str() == "global")
                    && m.active
                    && !m.restricted
            })
            .filter(|m| {
                request.relations.iter().all(|filter| match filter {
                    MemoryRelationFilter::Sources(ids) => ids.contains(&m.source),
                })
            })
            .map(|m| (overlap(&request.terms.join(" "), &m.text), m.clone()))
            .filter(|(score, _)| *score > 0)
            .collect();
        hits.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.id.cmp(&b.1.id)));
        Ok(hits
            .into_iter()
            .take(limit)
            .map(|(_, memory)| memory)
            .collect())
    }
}
pub fn fixture() -> Vec<Memory> {
    [
        (
            "db",
            "demo-project",
            "SurrealDB memory storage preserves source references.",
            true,
            false,
        ),
        (
            "luna",
            "global",
            "Luna Decisions memory selection replaces JEV.",
            true,
            false,
        ),
        ("old", "demo-project", "JEV memory selection.", false, false),
        ("other", "other", "Redis memory storage.", true, false),
        (
            "restricted",
            "demo-project",
            "Restricted synthetic memory.",
            true,
            true,
        ),
        (
            "deploy",
            "demo-project",
            "Deploy through release process.",
            true,
            false,
        ),
    ]
    .into_iter()
    .map(|(id, project, text, active, restricted)| Memory {
        id: MemoryId::new(id).unwrap(),
        owner: OwnerId::new("fixture-owner").unwrap(),
        actor: ActorId::new("fixture-actor").unwrap(),
        project: ProjectId::new(project).unwrap(),
        text: text.into(),
        source: SourceId::new(format!("fixture://{id}")).unwrap(),
        active,
        restricted,
    })
    .collect()
}
