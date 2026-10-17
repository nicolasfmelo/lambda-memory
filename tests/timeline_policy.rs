use lambda_memory::domain_models::runtime::RuntimeError;
use lambda_memory::domain_models::timeline::*;
use lambda_memory::domain_models::*;
use lambda_memory::engines::timeline_policy::validate_draft;

const NOW: u64 = 1_800_000_000_000;
const CAP: usize = 64;

fn draft() -> TimelineDraft {
    TimelineDraft {
        project: ProjectId::new("project").unwrap(),
        entry: TimelineEntryId::new("adopt-surrealkv").unwrap(),
        expected_version: None,
        title: "Adopt SurrealKV".into(),
        summary: "Embedded store chosen for single-process ownership.".into(),
        occurred_at_ms: NOW - 1000,
        status: TimelineStatus::Adopted,
        sources: vec![SourceId::new("s1").unwrap()],
        revisions: vec![RevisionId::new("r1").unwrap()],
        memories: vec![MemoryId::new("m1").unwrap()],
        supersedes: Some(TimelineEntryId::new("older").unwrap()),
        tags: vec!["storage".into(), "db.v2_x-y".into()],
    }
}
fn rejected(change: impl FnOnce(&mut TimelineDraft)) -> bool {
    let mut value = draft();
    change(&mut value);
    matches!(
        validate_draft(&value, NOW, CAP),
        Err(RuntimeError::InvalidInput(_))
    )
}
fn accepted(change: impl FnOnce(&mut TimelineDraft)) -> bool {
    let mut value = draft();
    change(&mut value);
    validate_draft(&value, NOW, CAP).is_ok()
}

#[test]
fn a_complete_draft_is_valid() {
    assert_eq!(validate_draft(&draft(), NOW, CAP), Ok(()));
    assert!(accepted(|d| {
        d.revisions.clear();
        d.memories.clear();
        d.tags.clear();
        d.supersedes = None;
    }));
}

#[test]
fn title_bounds() {
    assert!(rejected(|d| d.title = String::new()));
    assert!(rejected(|d| d.title = "   ".into()));
    assert!(rejected(|d| d.title = "line\nbreak".into()));
    assert!(rejected(
        |d| d.title = "x".repeat(TIMELINE_MAX_TITLE_BYTES + 1)
    ));
    assert!(accepted(|d| d.title = "x".repeat(TIMELINE_MAX_TITLE_BYTES)));
    // 200 bytes of two-byte characters is exactly at the bound.
    assert!(accepted(|d| d.title = "é".repeat(100)));
    assert!(rejected(|d| d.title = "é".repeat(101)));
}

#[test]
fn summary_bounds() {
    assert!(rejected(|d| d.summary = " \n\t".into()));
    assert!(rejected(|d| d.summary = "x".repeat(CAP + 1)));
    assert!(accepted(|d| d.summary = "x".repeat(CAP)));
    assert!(accepted(|d| d.summary = "multi\nline".into()));
}

#[test]
fn occurred_at_range() {
    assert!(rejected(|d| d.occurred_at_ms = TIMELINE_MIN_OCCURRED_MS - 1));
    assert!(accepted(|d| d.occurred_at_ms = TIMELINE_MIN_OCCURRED_MS));
    assert!(accepted(
        |d| d.occurred_at_ms = NOW + TIMELINE_FUTURE_SKEW_MS
    ));
    assert!(rejected(
        |d| d.occurred_at_ms = NOW + TIMELINE_FUTURE_SKEW_MS + 1
    ));
    let mut late = draft();
    late.occurred_at_ms = u64::MAX;
    assert!(validate_draft(&late, u64::MAX, CAP).is_ok());
}

#[test]
fn link_counts_and_uniqueness() {
    let sources = |n: usize| {
        (0..n)
            .map(|i| SourceId::new(format!("s{i}")).unwrap())
            .collect::<Vec<_>>()
    };
    assert!(rejected(|d| d.sources.clear()));
    assert!(accepted(|d| d.sources = sources(TIMELINE_MAX_LINKS)));
    assert!(rejected(|d| d.sources = sources(TIMELINE_MAX_LINKS + 1)));
    assert!(rejected(|d| d.sources.push(d.sources[0].clone())));
    let revisions = (0..=TIMELINE_MAX_LINKS)
        .map(|i| RevisionId::new(format!("r{i}")).unwrap())
        .collect::<Vec<_>>();
    assert!(accepted(
        |d| d.revisions = revisions[..TIMELINE_MAX_LINKS].to_vec()
    ));
    assert!(rejected(|d| d.revisions = revisions.clone()));
    assert!(rejected(|d| d.revisions.push(d.revisions[0].clone())));
    let memories = (0..=TIMELINE_MAX_LINKS)
        .map(|i| MemoryId::new(format!("m{i}")).unwrap())
        .collect::<Vec<_>>();
    assert!(accepted(
        |d| d.memories = memories[..TIMELINE_MAX_LINKS].to_vec()
    ));
    assert!(rejected(|d| d.memories = memories.clone()));
    assert!(rejected(|d| d.memories.push(d.memories[0].clone())));
}

#[test]
fn tag_pattern_count_and_uniqueness() {
    for bad in [
        "", "Upper", "-lead", ".lead", "_lead", "sp ace", "ümlaut", "slash/x",
    ] {
        assert!(
            rejected(|d| d.tags = vec![bad.into()]),
            "{bad:?} should be rejected"
        );
    }
    assert!(rejected(
        |d| d.tags = vec!["a".repeat(TIMELINE_MAX_TAG_BYTES + 1)]
    ));
    assert!(accepted(
        |d| d.tags = vec!["a".repeat(TIMELINE_MAX_TAG_BYTES)]
    ));
    for good in ["a", "0", "a.b-c_9", "v1.2"] {
        assert!(
            accepted(|d| d.tags = vec![good.into()]),
            "{good:?} should be accepted"
        );
    }
    let tags = (0..=TIMELINE_MAX_TAGS)
        .map(|i| format!("t{i}"))
        .collect::<Vec<_>>();
    assert!(accepted(|d| d.tags = tags[..TIMELINE_MAX_TAGS].to_vec()));
    assert!(rejected(|d| d.tags = tags.clone()));
    assert!(rejected(|d| d.tags = vec!["x".into(), "x".into()]));
}

#[test]
fn supersedes_and_expected_version() {
    assert!(rejected(|d| d.supersedes = Some(d.entry.clone())));
    assert!(rejected(|d| d.expected_version = Some(0)));
    assert!(accepted(|d| d.expected_version = Some(1)));
}

#[test]
fn status_codes_round_trip() {
    for status in [
        TimelineStatus::Proposed,
        TimelineStatus::Adopted,
        TimelineStatus::Superseded,
        TimelineStatus::Reverted,
        TimelineStatus::Retracted,
    ] {
        assert_eq!(TimelineStatus::parse(status.as_str()), Some(status));
    }
    assert_eq!(TimelineStatus::Retracted.as_str(), "retracted");
    assert_eq!(TimelineStatus::parse("Adopted"), None);
}

#[test]
fn preview_truncates_on_a_utf8_boundary() {
    assert_eq!(timeline_preview("short"), ("short".to_string(), false));
    let exact = "x".repeat(TIMELINE_PREVIEW_BYTES);
    assert_eq!(timeline_preview(&exact), (exact.clone(), false));
    // 511 ASCII bytes + a 2-byte char straddles the bound: the char is dropped.
    let straddle = format!("{}é tail", "x".repeat(TIMELINE_PREVIEW_BYTES - 1));
    let (preview, truncated) = timeline_preview(&straddle);
    assert!(truncated);
    assert_eq!(preview, "x".repeat(TIMELINE_PREVIEW_BYTES - 1));
    let (preview, truncated) = timeline_preview(&"€".repeat(400));
    assert!(truncated);
    assert!(preview.len() <= TIMELINE_PREVIEW_BYTES && preview.len() == 510);
}
