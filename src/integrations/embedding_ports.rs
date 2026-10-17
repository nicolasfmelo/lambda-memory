use crate::domain_models::embeddings::Embedding;
use crate::domain_models::{Memory, MemoryError, MemoryQueryPlan};
use std::future::Future;
pub trait EmbeddingClient {
    fn embed(&self, text: &str) -> impl Future<Output = Result<Embedding, MemoryError>>;
}
pub trait VectorRepository {
    fn search_vector(
        &self,
        plan: &MemoryQueryPlan,
        query: &Embedding,
        limit: usize,
    ) -> impl Future<Output = Result<Vec<Memory>, MemoryError>>;
}
