use super::capture::CaptureEvent;
use super::consolidation::Revision;
use super::{OwnerId, ProjectId};
#[derive(Clone, Debug)]
pub struct RevisionRetrievalRequest {
    pub owner: OwnerId,
    pub project: ProjectId,
    pub terms: Vec<String>,
    pub as_of_ms: u64,
    pub limit: usize,
}
#[derive(Clone, Debug)]
pub struct ProvisionalRevisionHit {
    pub revision: Revision,
    pub sources: Vec<CaptureEvent>,
    pub watermark_ms: u64,
    pub lexical_score: u64,
    pub provisional: bool,
}
