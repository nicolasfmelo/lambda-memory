use super::{Memory, MemoryError, RecallOutput};
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EmbeddingSpace {
    pub provider: String,
    pub model: String,
    /// Local index/algorithm revision; never implies an immutable provider release.
    pub revision: String,
    pub dimensions: usize,
}
#[derive(Clone, Debug)]
pub struct Embedding {
    pub space: EmbeddingSpace,
    pub values: Vec<f64>,
}
#[derive(Clone, Debug)]
pub struct IndexedEmbedding {
    pub memory: Memory,
    pub embedding: Embedding,
}
#[derive(Clone, Debug)]
pub struct HybridPolicy {
    pub stream_limit: usize,
    pub fusion_limit: usize,
    pub rrf_k: usize,
}
#[derive(Debug)]
pub struct HybridOutput {
    pub recall: RecallOutput,
    pub vector_failure: Option<MemoryError>,
    pub vector_candidates: usize,
}

pub fn cosine(a: &[f64], b: &[f64]) -> Option<f64> {
    if a.is_empty() || a.len() != b.len() || a.iter().chain(b).any(|v| !v.is_finite()) {
        return None;
    }
    let aa = a.iter().map(|v| v * v).sum::<f64>();
    let bb = b.iter().map(|v| v * v).sum::<f64>();
    if aa <= 0.0 || bb <= 0.0 {
        return None;
    }
    Some(a.iter().zip(b).map(|(a, b)| a * b).sum::<f64>() / (aa.sqrt() * bb.sqrt()))
}
