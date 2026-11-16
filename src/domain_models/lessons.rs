//! Source-bound session extraction vocabulary. No wire or storage types.
use super::capture::CaptureEvent;

#[derive(Clone, Debug)]
pub struct LessonExchange {
    pub slot: usize,
    pub prompt: String,
    pub reply: String,
}
#[derive(Clone, Debug)]
pub struct DigestLine {
    pub failed: String,
    pub error: String,
    pub fixed_by: Option<String>,
    pub evidence: Vec<usize>,
}
#[derive(Clone, Debug)]
pub struct LessonInput {
    /// Slot i always denotes sources[i], exchanges first, then digest action sources.
    pub sources: Vec<CaptureEvent>,
    pub exchanges: Vec<LessonExchange>,
    pub digest: Vec<DigestLine>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TaskOutcome {
    Succeeded,
    Failed,
    CorrectedByUser,
    Unknown,
}
impl TaskOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::CorrectedByUser => "corrected_by_user",
            Self::Unknown => "unknown",
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LessonKind {
    Strategy,
    Pitfall,
}
impl LessonKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Strategy => "strategy",
            Self::Pitfall => "pitfall",
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LessonScope {
    Project,
    User,
}
impl LessonScope {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Project => "project",
            Self::User => "user",
        }
    }
}
#[derive(Clone, Debug)]
pub struct LessonTask {
    pub title: String,
    pub evidence: Vec<usize>,
    pub outcome: TaskOutcome,
}
#[derive(Clone, Debug)]
pub struct Lesson {
    pub kind: LessonKind,
    pub scope: LessonScope,
    pub title: String,
    pub applies_when: String,
    pub guidance: String,
    pub task: usize,
    pub evidence: Vec<usize>,
}
#[derive(Clone, Debug)]
pub struct LessonAnswer {
    pub tasks: Vec<LessonTask>,
    pub lessons: Vec<Lesson>,
}
