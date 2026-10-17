use crate::domain_models::embeddings::{Embedding, IndexedEmbedding};
use crate::domain_models::{Memory, MemoryError, MemoryQueryPlan, MemoryRelationFilter};
use crate::integrations::embedding_ports::VectorRepository;
pub struct MockVectorRepository {
    pub corpus: Vec<IndexedEmbedding>,
    pub minimum_cosine: f64,
}
fn valid(embedding: &Embedding) -> bool {
    embedding.values.len() == embedding.space.dimensions
        && (128..=3072).contains(&embedding.space.dimensions)
        && embedding
            .values
            .iter()
            .all(|v| v.is_finite() && v.abs() <= 1e6)
        && embedding.values.iter().any(|v| *v != 0.0)
}
impl VectorRepository for MockVectorRepository {
    async fn search_vector(
        &self,
        plan: &MemoryQueryPlan,
        query: &Embedding,
        limit: usize,
    ) -> Result<Vec<Memory>, MemoryError> {
        if !valid(query)
            || !self.minimum_cosine.is_finite()
            || !(0.0..=1.0).contains(&self.minimum_cosine)
            || limit > 32
        {
            return Err(MemoryError::InvalidInput("invalid vector search"));
        }
        let mut hits = Vec::new();
        for item in &self.corpus {
            let m = &item.memory;
            if m.owner != plan.owner
                || !(m.project == plan.project || m.project.as_str() == "global")
                || !m.active
                || m.restricted
                || item.embedding.space != query.space
                || !valid(&item.embedding)
                || !plan.relations.iter().all(|f| match f {
                    MemoryRelationFilter::Sources(ids) => ids.contains(&m.source),
                })
            {
                continue;
            }
            let dot: f64 = query
                .values
                .iter()
                .zip(&item.embedding.values)
                .map(|(a, b)| a * b)
                .sum();
            let q_norm = query.values.iter().map(|v| v * v).sum::<f64>().sqrt();
            let d_norm = item
                .embedding
                .values
                .iter()
                .map(|v| v * v)
                .sum::<f64>()
                .sqrt();
            let similarity = dot / (q_norm * d_norm);
            if similarity >= self.minimum_cosine {
                hits.push((similarity, m.clone()));
            }
        }
        hits.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.id.cmp(&b.1.id)));
        Ok(hits.into_iter().take(limit).map(|(_, m)| m).collect())
    }
}
