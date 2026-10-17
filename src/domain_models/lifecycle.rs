use super::{OwnerId, ProjectId, SourceId};
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceLifecycle {
    pub owner: OwnerId,
    pub project: ProjectId,
    pub source: SourceId,
    pub authoritative: bool,
    pub expires_at_ms: Option<u64>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LifecycleRejection {
    Unverified,
    Expired,
    OlderOrEqual,
}
