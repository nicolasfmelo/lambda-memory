use super::{MemoryId, SourceId, capture::CaptureEvent, lifecycle::SourceLifecycle};
#[derive(Clone, Debug)]
pub struct AgentSource {
    pub event: CaptureEvent,
    pub policy: SourceLifecycle,
    pub arrival_ms: u64,
    pub fact_key: MemoryId,
}
#[derive(Clone, Debug)]
pub struct AgentDerivationJob {
    pub id: String,
    pub sources: Vec<AgentSource>,
}
#[derive(Clone, Debug)]
pub struct AgentProposal {
    pub job: String,
    pub source: SourceId,
    pub eligible: bool,
    pub content: String,
    pub supersedes: Option<SourceId>,
}
