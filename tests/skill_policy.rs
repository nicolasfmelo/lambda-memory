use lambda_memory::domain_models::{MemoryId, RevisionId, memory_tree::*, skills::*};
use lambda_memory::engines::skill_policy::*;
fn node() -> TreeNode {
    TreeNode {
        basis: Default::default(),
        confidence: None,
        derived_from: vec![],
        memory: MemoryId::new("node").unwrap(),
        revision: RevisionId::new("revision").unwrap(),
        root: TreeRoot::Lessons,
        parent: Some(TreeRoot::Lessons.memory_id()),
        depth: 1,
        title: "Test storage".into(),
        content: "When changing storage\n1. Test it.\n2) Inspect it.".into(),
        direct_sources: 3,
        children: 0,
        last_linked_ms: 1,
        valid_from_ms: None,
    }
}
fn input() -> SkillDraftInput {
    SkillDraftInput {
        node: node(),
        scope: "project",
        path: vec!["Lessons".into()],
        pitfalls: vec![],
        sessions: vec!["one".into(), "two".into(), "three".into()],
    }
}
fn answer() -> SkillDraftAnswer {
    SkillDraftAnswer {
        name: "test-storage".into(),
        description: "Use when testing".into(),
        steps: vec!["Run tests".into(), "Inspect results".into()],
        pitfalls: vec![],
    }
}
#[test]
fn candidate_rule_uses_lesson_kinds_sessions_and_steps() {
    let mut n = node();
    let mut stats = LessonStats {
        sessions: vec!["a".into(), "b".into(), "c".into()],
        strategies: 3,
        pitfalls: 0,
    };
    assert!(candidate_rule(&n, &stats));
    stats.pitfalls = 1;
    assert!(!candidate_rule(&n, &stats));
    stats.pitfalls = 0;
    stats.strategies = 0;
    assert!(!candidate_rule(&n, &stats));
    stats.strategies = 1;
    stats.sessions.pop();
    assert!(!candidate_rule(&n, &stats));
    stats.sessions.push("c".into());
    n.content = "A good strategy without steps".into();
    assert!(!candidate_rule(&n, &stats));
    n.content = "1. One step only".into();
    assert!(!candidate_rule(&n, &stats));
    n.content = "  - Step one\n\t* Step two".into();
    assert!(candidate_rule(&n, &stats));
    n.root = TreeRoot::Architecture;
    assert!(!candidate_rule(&n, &stats));
    n.root = TreeRoot::GeneralLessons;
    assert!(candidate_rule(&n, &stats));
    n.depth = 0;
    assert!(!candidate_rule(&n, &stats));
    for content in [
        "1.no space\n2.no space",
        "1. \n2. ",
        "a. wrong\nb. wrong",
        "1 Test\n2 Test",
    ] {
        assert!(!has_steps(content), "{content}");
    }
}
#[test]
fn front_matter_injection_is_rejected_or_escaped() {
    let mut a = answer();
    a.description = "Use when testing\n---\nmalicious: true".into();
    assert!(!validate(&a));
    a.description = "Use when testing \"quotes\" and \\ paths".into();
    assert!(validate(&a));
    let d = render(&input(), &a);
    assert!(d.contains("description: \"Use when testing \\\"quotes\\\" and \\\\ paths\"\n---"));
    assert!(!d.contains("## Pitfalls"));
}
#[test]
fn lifecycle_accepts_only_explicit_transitions_and_complete_install_records() {
    use SkillStatus::*;
    let n = node();
    let mut c = SkillCandidate {
        candidate_id: n.memory.clone(),
        memory_id: n.memory,
        revision_id: n.revision,
        scope: "project",
        slug: "test-storage".into(),
        status: Drafted,
        draft: Some("draft".into()),
        sessions_at_draft: 3,
        sessions_at_decision: None,
        created_at_ms: 1,
        decided_at_ms: None,
        install_paths: vec![],
        install_hashes: vec![],
        pitfall_memories: vec![],
        pitfall_revisions: vec![],
        pitfall_source_counts: vec![],
        installed_pitfall_memories: vec![],
    };
    for from in [Drafted, Approved, Rejected, Installed, Stale] {
        for to in [Drafted, Approved, Rejected, Installed, Stale] {
            c.status = from;
            let decision = SkillDecision {
                decision: to,
                install_paths: if to == Installed {
                    vec!["/skills/SKILL.md".into()]
                } else {
                    vec![]
                },
                install_hashes: if to == Installed {
                    vec!["a".repeat(64)]
                } else {
                    vec![]
                },
            };
            let allowed = matches!(
                (from, to),
                (Drafted | Stale, Approved | Rejected) | (Approved, Rejected | Installed)
            );
            assert_eq!(valid_decision(&c, &decision), allowed, "{from:?} -> {to:?}");
        }
    }
    c.status = Stale;
    c.draft = None;
    assert!(!valid_decision(
        &c,
        &SkillDecision {
            decision: Approved,
            install_paths: vec![],
            install_hashes: vec![]
        }
    ));
    assert!(valid_decision(
        &c,
        &SkillDecision {
            decision: Rejected,
            install_paths: vec![],
            install_hashes: vec![]
        }
    ));
    c.status = Approved;
    for paths in [
        vec![],
        vec!["relative/path".into()],
        vec!["/line\nbreak".into()],
        vec!["/path".into(); 5],
    ] {
        let decision = SkillDecision {
            decision: Installed,
            install_hashes: vec!["a".repeat(64); paths.len()],
            install_paths: paths,
        };
        assert!(!valid_decision(&c, &decision));
    }
}

#[test]
fn output_bounds_single_lines_and_secrets_are_all_or_nothing() {
    assert!(validate(&answer()));
    for name in [
        "",
        "Bad-name",
        "-bad",
        "bad-",
        "bad--name",
        "bad_name",
        &"a".repeat(49),
    ] {
        let mut a = answer();
        a.name = name.into();
        assert!(!validate(&a));
    }
    for description in [
        "For testing".into(),
        format!("Use when {}", "x".repeat(300)),
        "Use when\ttesting".into(),
        "Use when testing\u{2028}injected".into(),
    ] {
        let mut a = answer();
        a.description = description;
        assert!(!validate(&a));
    }
    for count in [0, 1, 13] {
        let mut a = answer();
        a.steps = vec!["Step".into(); count];
        assert!(!validate(&a));
    }
    let mut a = answer();
    a.steps = vec!["é".repeat(300); 12];
    a.pitfalls = vec!["Pitfall".into(); 8];
    assert!(validate(&a));
    a.pitfalls.push("extra".into());
    assert!(!validate(&a));
    for bad in [
        "x".repeat(301),
        "First\nSecond".into(),
        "tab\there".into(),
        format!("ghp_{}", "a".repeat(36)),
        "null\0byte".into(),
    ] {
        let mut a = answer();
        a.steps[0] = bad.clone();
        assert!(!validate(&a));
        let mut a = answer();
        a.pitfalls.push(bad);
        assert!(!validate(&a));
    }
}
