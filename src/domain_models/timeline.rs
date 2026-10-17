//! Curated, versioned decision timeline. No transport, clock or datastore dependencies.
use super::capture::WorkOutcome;
use super::{MemoryError, MemoryId, ProjectId, RevisionId, SourceId};
use std::fmt;

identity!(TimelineEntryId, "timeline entry ID");

pub const TIMELINE_MAX_TITLE_BYTES: usize = 200;
pub const TIMELINE_MAX_LINKS: usize = 32;
pub const TIMELINE_MAX_TAGS: usize = 16;
pub const TIMELINE_MAX_TAG_BYTES: usize = 64;
pub const TIMELINE_UNCOVERED_LIMIT: usize = 16;
/// Largest page of entries or versions a single query may request.
pub const TIMELINE_MAX_PAGE: usize = 32;
/// Number of distinct statuses, the most a status filter can name.
pub const TIMELINE_MAX_STATUSES: usize = 5;
pub const TIMELINE_PREVIEW_BYTES: usize = 512;
/// 2000-01-01T00:00:00Z.
pub const TIMELINE_MIN_OCCURRED_MS: u64 = 946_684_800_000;
pub const TIMELINE_FUTURE_SKEW_MS: u64 = 86_400_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum TimelineStatus {
    Proposed,
    Adopted,
    Superseded,
    Reverted,
    Retracted,
}
impl TimelineStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Proposed => "proposed",
            Self::Adopted => "adopted",
            Self::Superseded => "superseded",
            Self::Reverted => "reverted",
            Self::Retracted => "retracted",
        }
    }
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "proposed" => Some(Self::Proposed),
            "adopted" => Some(Self::Adopted),
            "superseded" => Some(Self::Superseded),
            "reverted" => Some(Self::Reverted),
            "retracted" => Some(Self::Retracted),
            _ => None,
        }
    }
}

/// A requested entry version. `expected_version` is the compare-and-swap guard.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TimelineDraft {
    pub project: ProjectId,
    pub entry: TimelineEntryId,
    pub expected_version: Option<u32>,
    pub title: String,
    pub summary: String,
    pub occurred_at_ms: u64,
    pub status: TimelineStatus,
    pub sources: Vec<SourceId>,
    pub revisions: Vec<RevisionId>,
    pub memories: Vec<MemoryId>,
    /// Bare link: it may dangle after the superseded entry is purged.
    pub supersedes: Option<TimelineEntryId>,
    pub tags: Vec<String>,
}

/// One immutable stored version. The writing actor is stored but never exposed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TimelineEntry {
    pub project: ProjectId,
    pub entry: TimelineEntryId,
    pub version: u32,
    pub title: String,
    pub summary: String,
    pub occurred_at_ms: u64,
    pub recorded_at_ms: u64,
    pub status: TimelineStatus,
    pub sources: Vec<SourceId>,
    pub revisions: Vec<RevisionId>,
    pub memories: Vec<MemoryId>,
    pub supersedes: Option<TimelineEntryId>,
    pub tags: Vec<String>,
}
impl TimelineEntry {
    /// Content equality used for safe replay. Arrays compare in order.
    pub fn same_content(&self, draft: &TimelineDraft) -> bool {
        self.project == draft.project
            && self.entry == draft.entry
            && self.title == draft.title
            && self.summary == draft.summary
            && self.occurred_at_ms == draft.occurred_at_ms
            && self.status == draft.status
            && self.sources == draft.sources
            && self.revisions == draft.revisions
            && self.memories == draft.memories
            && self.supersedes == draft.supersedes
            && self.tags == draft.tags
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TimelineWrite {
    Recorded,
    AlreadyRecorded,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TimelineReceipt {
    pub entry: TimelineEntry,
    pub write: TimelineWrite,
}

/// Exclusive keyset position: list mode orders by `(occurred_at_ms, entry)`,
/// history mode by version.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TimelineKey {
    Position {
        occurred_at_ms: u64,
        entry: TimelineEntryId,
    },
    Version(u32),
}
/// `entry` selects history mode, which excludes `statuses` and `descending`.
/// Empty `statuses` means every status.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TimelineQuery {
    pub project: ProjectId,
    pub limit: usize,
    pub after: Option<TimelineKey>,
    pub statuses: Vec<TimelineStatus>,
    pub descending: bool,
    pub entry: Option<TimelineEntryId>,
}

/// A captured decision source not yet cited by the latest version of any entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UncoveredDecision {
    pub source: SourceId,
    pub origin: String,
    pub recorded_at_ms: u64,
    pub outcome: WorkOutcome,
    pub preview: String,
    pub preview_truncated: bool,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TimelinePage {
    pub entries: Vec<TimelineEntry>,
    pub more: bool,
    pub uncovered: Vec<UncoveredDecision>,
    pub uncovered_more: bool,
}

/// At most `TIMELINE_PREVIEW_BYTES`, cut on a UTF-8 character boundary.
pub fn timeline_preview(content: &str) -> (String, bool) {
    if content.len() <= TIMELINE_PREVIEW_BYTES {
        return (content.to_owned(), false);
    }
    let mut end = TIMELINE_PREVIEW_BYTES;
    while !content.is_char_boundary(end) {
        end -= 1;
    }
    (content[..end].to_owned(), true)
}
