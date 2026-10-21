//! Exact operator consent for one project and embedding space; no provider credentials.
use super::embeddings::EmbeddingSpace;
use super::{JobId, RevisionId};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EmbeddingRecoveryItem {
    pub revision: RevisionId,
    /// Digest of the immutable revision and its complete original source snapshots.
    pub snapshot: String,
    pub previous_operations: Vec<JobId>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EmbeddingRecoveryPlan {
    pub space: EmbeddingSpace,
    pub items: Vec<EmbeddingRecoveryItem>,
}
