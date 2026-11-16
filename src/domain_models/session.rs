//! Source-bound session bookkeeping, independent of harness and storage.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SessionKey(String);
impl SessionKey {
    pub fn new(value: impl Into<String>) -> Option<Self> {
        let value = value.into();
        (value.len() == 16
            && value
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
        .then_some(Self(value))
    }
    pub fn from_source(source: &str) -> Option<Self> {
        if source.as_bytes().get(16) != Some(&b'/') {
            return None;
        }
        Self::new(source.get(..16)?)
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemorySession {
    pub session_key: SessionKey,
    pub first_activity_ms: u64,
    pub last_activity_ms: u64,
    /// Latest live insert, or the boundary explicitly authorized by lesson backfill.
    pub live_activity_ms: Option<u64>,
    pub exchanges: u64,
    pub ended_at_ms: Option<u64>,
    pub extracted_through_ms: Option<u64>,
    pub historical: bool,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionEnd {
    pub session_key: SessionKey,
    pub ended: bool,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LessonBackfillPlan {
    pub sessions: Vec<SessionKey>,
}
impl LessonBackfillPlan {
    pub fn estimated_calls(&self) -> usize {
        self.sessions.len()
    }
}
