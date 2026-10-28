use lambda_memory::domain_models::capture::WorkOutcome;
use lambda_memory::domain_models::procedural::*;
use lambda_memory::engines::procedural_policy::*;

const DAY: u64 = 86_400_000;
const NOW: u64 = 200 * DAY;

fn record(
    tool: &str,
    summary: &str,
    outcome: WorkOutcome,
    session: &str,
    seq: u64,
    age_days: u64,
) -> EpisodeRecord {
    EpisodeRecord {
        episode: ActionEpisode {
            tool: tool.into(),
            intent: String::new(),
            summary: summary.into(),
            error_head: None,
            session: session.into(),
            seq,
        },
        outcome,
        recorded_at_ms: NOW - age_days * DAY,
    }
}
fn ok(tool: &str, summary: &str, session: &str, seq: u64, age_days: u64) -> EpisodeRecord {
    record(
        tool,
        summary,
        WorkOutcome::Succeeded,
        session,
        seq,
        age_days,
    )
}
fn failed(tool: &str, summary: &str, head: &str, session: &str, seq: u64) -> EpisodeRecord {
    let mut item = record(tool, summary, WorkOutcome::Failed, session, seq, 1);
    item.episode.error_head = Some(head.into());
    item
}
fn kind(facts: &[ProceduralFact], wanted: FactKind) -> Vec<&ProceduralFact> {
    facts.iter().filter(|fact| fact.kind == wanted).collect()
}
fn fact(id: &str) -> ProceduralFact {
    ProceduralFact {
        id: id.into(),
        kind: FactKind::Procedure,
        key: "bash x".into(),
        detail: None,
        successes: 1,
        failures: 0,
        unverified: 0,
        sessions: 2,
        last_seen_ms: 0,
    }
}

#[test]
fn normalize_key_collapses_paths_and_spaces() {
    assert_eq!(
        normalize_key("bash", "cargo  test   ./src/a.rs"),
        "bash cargo test <path>"
    );
    assert_eq!(normalize_key("edit", "./src/a.rs"), "edit ./src/a.rs");
    assert_eq!(
        normalize_key("Bash", "git status"),
        normalize_key("bash", "git status")
    );
    assert_eq!(normalize_key("Edit", "./a"), "edit ./a");
    assert_eq!(normalize_key("MultiEdit", "./a"), "edit ./a");
    assert_eq!(normalize_key("Write", "./a"), "write ./a");
    assert_eq!(normalize_key("Task", ""), "task");
    assert_eq!(normalize_key("Agent", ""), "task");
    assert_eq!(normalize_key("NotebookEdit", "./n.ipynb"), "edit ./n.ipynb");
}

#[test]
fn normalize_key_masks_absolute_paths_anywhere() {
    assert_eq!(
        normalize_key("bash", "cd /tmp/agent-1000/a/scratchpad && python3 x.py"),
        "bash cd <path> && python3 x.py"
    );
    assert_eq!(
        normalize_key("bash", "python -m mytool build src/mytool_cli.mt -o out/x"),
        "bash python -m mytool build src/mytool_cli.mt -o <path>"
    );
    assert_eq!(normalize_key("bash", "cat /a/b /c/d"), "bash cat <path>");
    assert_eq!(
        normalize_key("bash", "ls ~/work && cat ~/a"),
        "bash ls <path> && cat <path>"
    );
    assert_eq!(
        normalize_key("bash", "git clone https://x/y"),
        "bash git clone https://x/y"
    );
    assert_eq!(
        normalize_key("bash", "git clone https://x/y /tmp/z"),
        "bash git clone https://x/y <path>"
    );
    assert_eq!(normalize_key("edit", "/abs/file.rs"), "edit /abs/file.rs");
}

#[test]
fn normalize_key_masks_absolute_paths_in_assignments() {
    assert_eq!(
        normalize_key("bash", "N=/tmp/a/b python3 x.py"),
        "bash N=<path> python3 x.py"
    );
    assert_eq!(
        normalize_key("bash", "tool --out=/abs/x --fast"),
        "bash tool --out=<path> --fast"
    );
    assert_eq!(
        normalize_key("bash", "URL=https://x/y curl"),
        "bash URL=https://x/y curl"
    );
    assert_eq!(
        normalize_key("bash", "run A=/a /b"),
        "bash run A=<path> <path>"
    );
}

#[test]
fn pitfall_needs_two_sessions() {
    let one = vec![
        failed("bash", "cargo build", "error[E0599]", "s1", 1),
        ok("bash", "cargo build --features x", "s1", 2, 1),
    ];
    assert!(kind(&aggregate(&one, NOW), FactKind::Pitfall).is_empty());
    let mut two = one.clone();
    two.push(failed("bash", "cargo build", "error[E0599]", "s2", 1));
    two.push(ok("bash", "cargo build --features x", "s2", 2, 1));
    let facts = aggregate(&two, NOW);
    let pitfalls = kind(&facts, FactKind::Pitfall);
    assert_eq!(pitfalls.len(), 1);
    assert_eq!(pitfalls[0].sessions, 2);
    let repeated = vec![
        failed("bash", "cargo build", "error[E0599]", "s1", 1),
        ok("bash", "cargo build --features x", "s1", 2, 1),
        failed("bash", "cargo build", "error[E0599]", "s1", 10),
        ok("bash", "cargo build --features x", "s1", 11, 1),
    ];
    assert!(
        kind(&aggregate(&repeated, NOW), FactKind::Pitfall).is_empty(),
        "same session twice is one session"
    );
}

#[test]
fn procedure_needs_two_sessions() {
    let one_session = vec![
        ok("bash", "cargo test", "s1", 1, 1),
        ok("bash", "cargo test", "s1", 2, 1),
        ok("bash", "cargo test", "s1", 3, 1),
    ];
    assert!(kind(&aggregate(&one_session, NOW), FactKind::Procedure).is_empty());
    let two_sessions = vec![
        ok("bash", "cargo test", "s1", 1, 1),
        ok("bash", "cargo test", "s2", 1, 2),
    ];
    let facts = aggregate(&two_sessions, NOW);
    let procedures = kind(&facts, FactKind::Procedure);
    assert_eq!(procedures.len(), 1);
    assert_eq!(procedures[0].sessions, 2);
    assert_eq!(procedures[0].successes, 2);
    assert_eq!(procedures[0].key, "bash cargo test");
    assert_eq!(procedures[0].last_seen_ms, NOW - DAY);
}

#[test]
fn pitfall_window_and_prefix() {
    let fixed = vec![
        failed("bash", "cargo build", "error[E0599]", "s", 1),
        ok("bash", "cargo build --features x", "s", 4, 1),
        failed("bash", "cargo build", "error[E0599]", "t", 1),
        ok("bash", "cargo build --features x", "t", 4, 1),
    ];
    let facts = aggregate(&fixed, NOW);
    let pitfalls = kind(&facts, FactKind::Pitfall);
    assert_eq!(pitfalls.len(), 1);
    assert_eq!(pitfalls[0].detail.as_deref(), Some("error[E0599]"));
    assert!(pitfalls[0].key.contains("cargo build --features x"));

    let too_late = vec![
        failed("bash", "cargo build", "error[E0599]", "s", 1),
        ok("bash", "cargo build --features x", "s", 7, 1),
    ];
    assert!(kind(&aggregate(&too_late, NOW), FactKind::Pitfall).is_empty());
    let edge = vec![
        failed("bash", "cargo build", "error[E0599]", "s", 1),
        ok("bash", "cargo build --features x", "s", 6, 1),
        failed("bash", "cargo build", "error[E0599]", "t", 1),
        ok("bash", "cargo build --features x", "t", 6, 1),
    ];
    assert_eq!(kind(&aggregate(&edge, NOW), FactKind::Pitfall).len(), 1);

    let unrelated = vec![
        failed("bash", "cargo build", "error[E0599]", "s", 1),
        ok("bash", "git status", "s", 2, 1),
    ];
    assert!(kind(&aggregate(&unrelated, NOW), FactKind::Pitfall).is_empty());
    let one_token = vec![
        failed("bash", "cargo build", "error[E0599]", "s", 1),
        ok("bash", "cargo test", "s", 2, 1),
    ];
    assert!(kind(&aggregate(&one_token, NOW), FactKind::Pitfall).is_empty());
    let other_session = vec![
        failed("bash", "cargo build", "error[E0599]", "s", 1),
        ok("bash", "cargo build --features x", "t", 2, 1),
    ];
    assert!(kind(&aggregate(&other_session, NOW), FactKind::Pitfall).is_empty());
    let other_tool = vec![
        failed("bash", "cargo build", "error[E0599]", "s", 1),
        ok("eval", "cargo build --features x", "s", 2, 1),
    ];
    assert!(kind(&aggregate(&other_tool, NOW), FactKind::Pitfall).is_empty());
}

#[test]
fn pitfall_matches_on_shared_intent_prefix() {
    let mut failure = failed("bash", "make a", "boom", "s", 1);
    failure.episode.intent = "Running the unit tests".into();
    let mut fix = ok("bash", "make b", "s", 2, 1);
    fix.episode.intent = "running the unit suite".into();
    let (mut failure_two, mut fix_two) = (failure.clone(), fix.clone());
    failure_two.episode.session = "t".into();
    fix_two.episode.session = "t".into();
    assert_eq!(
        kind(
            &aggregate(&[failure, fix, failure_two, fix_two], NOW),
            FactKind::Pitfall
        )
        .len(),
        1
    );
}

#[test]
fn pitfall_counts_occurrences_across_sessions() {
    let records = vec![
        failed("bash", "cargo build", "error[E0599]", "s1", 1),
        ok("bash", "cargo build -p x", "s1", 2, 3),
        failed("bash", "cargo build", "error[E0599]", "s2", 1),
        ok("bash", "cargo build -p x", "s2", 2, 2),
    ];
    let facts = aggregate(&records, NOW);
    let pitfalls = kind(&facts, FactKind::Pitfall);
    assert_eq!(pitfalls.len(), 1);
    assert_eq!(pitfalls[0].successes, 2);
    assert_eq!(pitfalls[0].sessions, 2);
}

#[test]
fn hot_files_need_two_sessions_and_are_capped() {
    let mut records = Vec::new();
    for file in 0..8 {
        for session in ["s1", "s2"] {
            records.push(ok(
                "edit",
                &format!("./src/f{file}.rs"),
                session,
                file as u64,
                1,
            ));
        }
    }
    records.push(ok("edit", "./src/single.rs", "s1", 50, 1));
    records.push(ok("edit", "./src/single.rs", "s1", 51, 1));
    let facts = aggregate(&records, NOW);
    let hot = kind(&facts, FactKind::HotFile);
    assert_eq!(hot.len(), 5);
    assert!(
        hot.iter()
            .all(|fact| fact.key != "./src/single.rs" && fact.sessions == 2)
    );
    assert!(
        kind(&facts, FactKind::Procedure).is_empty(),
        "edits are never procedures"
    );
}

#[test]
fn ranking_caps_and_ids() {
    let mut records = Vec::new();
    for index in 0..40 {
        for session in ["s1", "s2"] {
            records.push(ok(
                "bash",
                &format!("tool{index:02} run"),
                session,
                index as u64,
                1,
            ));
        }
    }
    let facts = aggregate(&records, NOW);
    assert_eq!(kind(&facts, FactKind::Procedure).len(), 15);
    assert_eq!(facts.len(), 15);
    for (index, item) in facts.iter().enumerate() {
        assert_eq!(item.id, format!("F{}", index + 1));
    }
    let keys: Vec<_> = facts.iter().map(|item| item.key.clone()).collect();
    let mut sorted = keys.clone();
    sorted.sort();
    assert_eq!(keys, sorted, "equal scores break ties by key ascending");
    assert_eq!(keys[0], "bash tool00 run");
}

#[test]
fn caps_total_thirty_facts() {
    let mut records = Vec::new();
    for index in 0..20u64 {
        for session in ["s1", "s2"] {
            records.push(ok(
                "bash",
                &format!("proc{index:02} run"),
                session,
                index,
                1,
            ));
            records.push(ok(
                "edit",
                &format!("./src/h{index:02}.rs"),
                session,
                100 + index,
                1,
            ));
        }
    }
    for index in 0..15u64 {
        for session in ["p1", "p2"] {
            records.push(failed(
                "bash",
                &format!("fail{index:02} go"),
                "err",
                session,
                1,
            ));
            records.push(ok("bash", &format!("fail{index:02} go now"), session, 2, 1));
        }
    }
    let facts = aggregate(&records, NOW);
    assert_eq!(kind(&facts, FactKind::Pitfall).len(), 10);
    assert_eq!(kind(&facts, FactKind::Procedure).len(), 15);
    assert_eq!(kind(&facts, FactKind::HotFile).len(), 5);
    assert_eq!(facts.len(), 30);
}

#[test]
fn recency_weight() {
    let records = vec![
        ok("bash", "old thing", "s1", 1, 60),
        ok("bash", "old thing", "s2", 1, 60),
        ok("bash", "new thing", "s1", 2, 1),
        ok("bash", "new thing", "s2", 2, 1),
        ok("bash", "ancient thing", "s1", 3, 120),
        ok("bash", "ancient thing", "s2", 3, 120),
    ];
    let facts = aggregate(&records, NOW);
    let keys: Vec<_> = facts.iter().map(|item| item.key.as_str()).collect();
    assert_eq!(
        keys,
        ["bash new thing", "bash old thing", "bash ancient thing"]
    );
}

#[test]
fn recency_weight_decays_to_a_floor() {
    // The floor weight is 0.25, not 0: 10 occurrences (score 2.5) beat 2 fresh ones (score 2.0).
    let mut records = Vec::new();
    for seq in 0..5u64 {
        for session in ["a1", "a2"] {
            records.push(ok("bash", "floor thing", session, seq, 190));
        }
    }
    for session in ["b1", "b2"] {
        records.push(ok("bash", "fresh thing", session, 0, 1));
    }
    let facts = aggregate(&records, NOW);
    assert_eq!(facts[0].key, "bash floor thing");
    assert_eq!(facts[1].key, "bash fresh thing");
}

#[test]
fn only_the_most_recent_five_thousand_episodes_count() {
    let mut records = vec![
        ok("bash", "ancient cmd", "s1", 0, 150),
        ok("bash", "ancient cmd", "s2", 0, 150),
    ];
    for index in 0..5000u64 {
        records.push(ok("bash", "same cmd", &format!("s{}", index % 3), index, 1));
    }
    let facts = aggregate(&records, NOW);
    assert!(facts.iter().all(|item| item.key != "bash ancient cmd"));
    assert!(facts.iter().any(|item| item.key == "bash same cmd"));
}

#[test]
fn unknown_only_procedure_in_two_sessions_is_a_fact() {
    let records = vec![
        record("bash", "cargo test", WorkOutcome::Unknown, "s1", 1, 1),
        record("bash", "cargo test", WorkOutcome::Unknown, "s2", 1, 1),
    ];
    let facts = aggregate(&records, NOW);
    assert_eq!(facts.len(), 1);
    assert_eq!(facts[0].kind, FactKind::Procedure);
    assert_eq!(
        (facts[0].successes, facts[0].unverified, facts[0].sessions),
        (0, 2, 2)
    );
}

#[test]
fn unknown_never_forms_a_pitfall() {
    let mut records = Vec::new();
    for session in ["s1", "s2"] {
        records.push(failed("bash", "cargo build", "error", session, 1));
        records.push(record(
            "bash",
            "cargo build",
            WorkOutcome::Unknown,
            session,
            2,
            1,
        ));
    }
    assert!(kind(&aggregate(&records, NOW), FactKind::Pitfall).is_empty());
    for item in &mut records {
        item.outcome = if item.outcome == WorkOutcome::Failed {
            WorkOutcome::Unknown
        } else {
            WorkOutcome::Succeeded
        };
    }
    assert!(kind(&aggregate(&records, NOW), FactKind::Pitfall).is_empty());
}

#[test]
fn apply_patch_merges_with_edit_hot_file() {
    let records = vec![
        ok("Edit", "src/a.rs", "s1", 1, 1),
        record("apply_patch", "src/a.rs", WorkOutcome::Unknown, "s2", 1, 1),
    ];
    let facts = aggregate(&records, NOW);
    assert_eq!(facts.len(), 1);
    assert_eq!(
        (
            facts[0].kind,
            facts[0].key.as_str(),
            facts[0].sessions,
            facts[0].unverified
        ),
        (FactKind::HotFile, "src/a.rs", 2, 1)
    );
}

#[test]
fn facts_without_unknown_are_unchanged() {
    let mut records = Vec::new();
    for session in ["s1", "s2"] {
        records.push(failed("bash", "cargo build", "error", session, 1));
        records.push(ok("bash", "cargo build", session, 2, 1));
        records.push(ok("edit", "src/a.rs", session, 3, 1));
    }
    let expected = vec![
        ProceduralFact {
            id: "F1".into(),
            kind: FactKind::Procedure,
            key: "bash cargo build".into(),
            detail: None,
            successes: 2,
            failures: 2,
            unverified: 0,
            sessions: 2,
            last_seen_ms: NOW - DAY,
        },
        ProceduralFact {
            id: "F2".into(),
            kind: FactKind::Pitfall,
            key: "bash cargo build".into(),
            detail: Some("error".into()),
            successes: 2,
            failures: 0,
            unverified: 0,
            sessions: 2,
            last_seen_ms: NOW - DAY,
        },
        ProceduralFact {
            id: "F3".into(),
            kind: FactKind::HotFile,
            key: "src/a.rs".into(),
            detail: None,
            successes: 2,
            failures: 0,
            unverified: 0,
            sessions: 2,
            last_seen_ms: NOW - DAY,
        },
    ];
    assert_eq!(aggregate(&records, NOW), expected);
}

#[test]
fn validator_rules() {
    let facts = [fact("F1"), fact("F2")];
    assert_eq!(
        validate_playbook(
            "- Run tests with cargo test [F1]\n- Fix E0599 by enabling features [F2]",
            &facts
        ),
        Ok(())
    );
    assert_eq!(
        validate_playbook("# Commands\n- Run tests [F1][F2]\n", &facts),
        Ok(())
    );
    assert_eq!(validate_playbook("", &facts), Err(PlaybookRejection::Empty));
    assert_eq!(
        validate_playbook("  \n ", &facts),
        Err(PlaybookRejection::Empty)
    );
    let big = format!("- {} [F1]", "x".repeat(1537 - 7));
    assert_eq!(big.len(), 1537);
    assert_eq!(
        validate_playbook(&big, &facts),
        Err(PlaybookRejection::TooLarge)
    );
    let limit = format!("- {} [F1]", "x".repeat(1536 - 7));
    assert_eq!(limit.len(), 1536);
    assert_eq!(validate_playbook(&limit, &facts), Ok(()));
    assert_eq!(
        validate_playbook("- Do X", &facts),
        Err(PlaybookRejection::UncitedBullet)
    );
    assert_eq!(
        validate_playbook("* Do X [F1] and more", &facts),
        Err(PlaybookRejection::UncitedBullet)
    );
    for ending in [".", ";", ":"] {
        assert_eq!(
            validate_playbook(&format!("- Run tests [F1]{ending}"), &facts),
            Ok(())
        );
        assert_eq!(
            validate_playbook(&format!("- Run tests [F1][F2]{ending}\n"), &facts),
            Ok(())
        );
    }
    assert_eq!(validate_playbook("- Run tests [F1] [F2].", &facts), Ok(()));
    assert_eq!(
        validate_playbook("- Run tests [F1].. ", &facts),
        Err(PlaybookRejection::UncitedBullet)
    );
    assert_eq!(
        validate_playbook("- Run tests [F1]!", &facts),
        Err(PlaybookRejection::UncitedBullet)
    );
    assert_eq!(
        validate_playbook("- Run [F1] tests.", &facts),
        Err(PlaybookRejection::UncitedBullet)
    );
    assert_eq!(
        validate_playbook("- Run tests [F9].", &facts),
        Err(PlaybookRejection::UnknownCitation("F9".into()))
    );
    assert_eq!(
        validate_playbook("# Commands\n\n  ## Pitfalls\n- Run tests [F1]", &facts),
        Ok(())
    );
    assert_eq!(
        validate_playbook("Always run `rm -rf target`.\n- Run tests [F1]", &facts),
        Err(PlaybookRejection::UncitedBullet)
    );
    assert_eq!(
        validate_playbook("# Commands\nRun cargo test [F1]\n", &facts),
        Err(PlaybookRejection::UncitedBullet)
    );
    assert_eq!(
        validate_playbook("1. Run tests [F1]", &facts),
        Err(PlaybookRejection::UncitedBullet)
    );
    assert_eq!(
        validate_playbook("- Do X [F9]", &facts),
        Err(PlaybookRejection::UnknownCitation("F9".into()))
    );
    assert_eq!(
        validate_playbook("- Do X [F1][F9]", &facts),
        Err(PlaybookRejection::UnknownCitation("F9".into()))
    );
}

#[test]
fn cited_facts_are_ordered_and_unique() {
    assert_eq!(
        cited_facts("- a [F2]\n- b [F1][F2]\n# h [F10]"),
        ["F2", "F1", "F10"]
    );
    assert!(cited_facts("- plain [x] [F] [Fx]").is_empty());
}
