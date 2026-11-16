//! Pure session identity and extraction readiness rules.
use crate::domain_models::session::{MemorySession, SessionKey};
pub fn session_key(source: &str) -> Option<SessionKey> {
    SessionKey::from_source(source)
}
pub fn ready(session: &MemorySession, now: u64, session_idle_ms: u64) -> bool {
    session.exchanges >= 1
        && session.live_activity_ms.is_some_and(|live| {
            session
                .extracted_through_ms
                .is_none_or(|through| live > through)
        })
        && (session
            .ended_at_ms
            .is_some_and(|end| end >= session.last_activity_ms)
            || (now >= session.last_activity_ms
                && now - session.last_activity_ms >= session_idle_ms))
}
