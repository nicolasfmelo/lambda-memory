#![cfg(feature = "memory-runtime")]
use lambda_memory as lm;
use lm::delivery::mcp::McpSession;
use lm::domain_models::{
    ActorId, MemoryId, OwnerId, ProjectId, SourceId, memory_tree::*, runtime::*, skills::*,
};
use lm::integrations::database::embedded_capture::{EmbeddedCaptureStore, EmbeddedRuntimeStore};
use lm::services::runtime_service::MemoryRuntime;
use serde_json::{Value, json};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
#[path = "support/skill_seed.rs"]
mod seed;
static NEXT: AtomicUsize = AtomicUsize::new(0);
const TOKEN: &str = "operator-token-32-bytes-minimum!!";
async fn setup() -> (EmbeddedRuntimeStore, Arc<MemoryRuntime>, TreeNode) {
    let path = format!(
        "/tmp/lambda-skill-store-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    );
    let store = EmbeddedRuntimeStore::new(EmbeddedCaptureStore::open(&path).await.unwrap());
    let (o, a, p) = (
        OwnerId::new("owner").unwrap(),
        ActorId::new("actor").unwrap(),
        ProjectId::new("project").unwrap(),
    );
    let n = seed::reinforce(&store, &o, &a, &p, "first", None, 0, 3, "strategy").await;
    let c = seed::candidate(&n);
    store
        .save_skill_draft(&o, &p, &c, "fixture", n.direct_sources)
        .await
        .unwrap();
    let r = Arc::new(
        MemoryRuntime::new(
            store.clone(),
            RuntimeScope {
                owner: o,
                actor: a,
                projects: vec![p],
            },
            RuntimeLimits::default(),
        )
        .unwrap(),
    );
    (store, r, n)
}
async fn send(s: &mut McpSession, v: Value) -> Value {
    serde_json::from_str(&s.handle_frame(&v.to_string()).await.unwrap()).unwrap()
}
async fn call(s: &mut McpSession, name: &str, args: Value) -> Value {
    send(s,json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":name,"arguments":args}})).await
}
fn body(v: &Value) -> Value {
    serde_json::from_str(v["result"]["content"][0]["text"].as_str().unwrap()).unwrap()
}
async fn session(r: Arc<MemoryRuntime>) -> McpSession {
    let mut s = McpSession::new(r, TOKEN.into()).unwrap();
    let init=send(&mut s,json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"fixture","version":"1"},"_meta":{"lambdaMemoryToken":TOKEN}}})).await;
    assert!(init.get("result").is_some());
    s.handle_frame(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
        .await;
    s
}
async fn decide(r: &MemoryRuntime, n: &TreeNode, status: SkillStatus) -> SkillCandidate {
    r.skill_decide(
        &r.scope().projects[0],
        &n.memory,
        &n.revision,
        SkillDecision {
            decision: status,
            install_paths: if status == SkillStatus::Installed {
                vec!["/skills/test-storage/SKILL.md".into()]
            } else {
                vec![]
            },
            install_hashes: if status == SkillStatus::Installed {
                vec!["a".repeat(64)]
            } else {
                vec![]
            },
        },
    )
    .await
    .unwrap()
}
#[tokio::test]
async fn skill_tools_round_trip_and_transitions() {
    let (_, r, n) = setup().await;
    let mut s = session(r.clone()).await;
    let args = json!({"project":"project","candidate_id":n.memory.as_str()});
    let listed = body(
        &call(
            &mut s,
            "memory_skill_candidates",
            json!({"project":"project","status":"drafted"}),
        )
        .await,
    );
    assert_eq!(listed["candidates"].as_array().unwrap().len(), 1);
    assert!(listed["candidates"][0].get("draft").is_none());
    assert!(listed["candidates"][0].get("install_hashes").is_none());
    let shown = body(&call(&mut s, "memory_skill_candidate", args.clone()).await);
    assert_eq!(shown["draft"], "draft");
    assert_eq!(shown["status"], "drafted");
    assert_eq!(
        body(&call(&mut s, "memory_brief", json!({"project":"project"})).await)["skill_drafts_pending"],
        1
    );
    let mut decision = args.clone();
    decision["decision"] = json!("approved");
    assert_eq!(
        call(&mut s, "memory_skill_decide", decision.clone()).await["error"]["code"],
        -32602
    );
    decision["revision_id"] = json!(null);
    assert_eq!(
        call(&mut s, "memory_skill_decide", decision.clone()).await["error"]["code"],
        -32602
    );
    decision["revision_id"] = json!("wrong-revision");
    assert_eq!(
        call(&mut s, "memory_skill_decide", decision.clone()).await["result"]["isError"],
        true
    );
    assert_eq!(
        r.skill_candidate(&r.scope().projects[0], &n.memory)
            .await
            .unwrap()
            .status,
        SkillStatus::Drafted
    );
    decision["revision_id"] = json!(n.revision.as_str());
    decision["decision"] = json!("installed");
    assert_eq!(
        call(&mut s, "memory_skill_decide", decision.clone()).await["result"]["isError"],
        true
    );
    decision["decision"] = json!("approved");
    assert_eq!(
        body(&call(&mut s, "memory_skill_decide", decision.clone()).await)["status"],
        "approved"
    );
    assert_eq!(
        call(&mut s, "memory_skill_decide", decision.clone()).await["result"]["isError"],
        true
    );
    decision["decision"] = json!("installed");
    assert_eq!(
        call(&mut s, "memory_skill_decide", decision.clone()).await["result"]["isError"],
        true
    );
    for (paths, hashes) in [
        (json!(["relative/SKILL.md"]), json!(["a".repeat(64)])),
        (json!(["/skills/SKILL.md"]), json!([])),
        (json!(["/skills/SKILL.md"]), json!(["not-a-hash"])),
    ] {
        let mut bad = decision.clone();
        bad["install_paths"] = paths;
        bad["install_hashes"] = hashes;
        assert_eq!(
            call(&mut s, "memory_skill_decide", bad).await["result"]["isError"],
            true
        );
    }
    decision["install_paths"] = json!(["/skills/test-storage/SKILL.md"]);
    decision["install_hashes"] = json!(["a".repeat(64)]);
    assert_eq!(
        body(&call(&mut s, "memory_skill_decide", decision.clone()).await)["status"],
        "installed"
    );
    decision["decision"] = json!("rejected");
    decision.as_object_mut().unwrap().remove("install_paths");
    decision.as_object_mut().unwrap().remove("install_hashes");
    assert_eq!(
        call(&mut s, "memory_skill_decide", decision).await["result"]["isError"],
        true
    );
    assert_eq!(
        body(&call(&mut s, "memory_brief", json!({"project":"project"})).await)["skill_drafts_pending"],
        0
    );
    for args in [
        json!({"project":"project","status":"wrong"}),
        json!({"project":"project","status":null}),
        json!({"project":"project","extra":1}),
    ] {
        assert_eq!(
            call(&mut s, "memory_skill_candidates", args).await["error"]["code"],
            -32602
        );
    }
    assert_eq!(
        call(
            &mut s,
            "memory_skill_candidate",
            json!({"project":"foreign","candidate_id":n.memory.as_str()})
        )
        .await["result"]["isError"],
        true
    );
    assert!(matches!(
        r.skill_candidate(
            &ProjectId::new("project").unwrap(),
            &MemoryId::new("missing").unwrap()
        )
        .await,
        Err(RuntimeError::Missing)
    ));
}
#[tokio::test]
async fn decision_rejects_redrafted_revision_inside_store_transaction() {
    let (store, r, n) = setup().await;
    let (o, a, p) = (&r.scope().owner, &r.scope().actor, &r.scope().projects[0]);
    let mut old = r.skill_candidate(p, &n.memory).await.unwrap();
    let next = seed::reinforce(&store, o, a, p, "first", Some(&n), 3, 1, "strategy").await;
    let updated = seed::candidate(&next);
    store
        .save_skill_draft(o, p, &updated, "redraft", next.direct_sources)
        .await
        .unwrap();
    old.status = SkillStatus::Approved;
    assert!(matches!(
        store
            .save_skill_decision(o, p, &old, SkillStatus::Drafted, &n.revision)
            .await,
        Err(RuntimeError::InvalidInput(_))
    ));
    let persisted = r.skill_candidate(p, &n.memory).await.unwrap();
    assert_eq!(persisted.revision_id, next.revision);
    assert_eq!(persisted.status, SkillStatus::Drafted);
}
#[tokio::test]
async fn purge_of_any_linked_source_removes_draft_and_clears_installed_draft() {
    let (store, r, n) = setup().await;
    let (o, a, p) = (&r.scope().owner, &r.scope().actor, &r.scope().projects[0]);
    // Advance the active revision with ONLY the latest source. Older direct links
    // still reinforce it and must invalidate the draft, even if its head survives.
    let n = seed::reinforce(&store, o, a, p, "first", Some(&n), 3, 1, "strategy").await;
    let c = seed::candidate(&n);
    store
        .save_skill_draft(o, p, &c, "fixture", n.direct_sources)
        .await
        .unwrap();
    store
        .skill_record_failure(o, p, &n.memory, &n.revision, 500)
        .await
        .unwrap();
    let receipt = store
        .purge(o, p, &[SourceId::new("lesson/first-0/1").unwrap()])
        .await
        .unwrap();
    assert!(receipt.installed_skills_with_purged_evidence.is_empty());
    assert!(r.skill_candidates(p, None).await.unwrap().is_empty());
    assert!(
        !store
            .skill_failed(o, p, &n.memory, &n.revision)
            .await
            .unwrap()
    );
    assert!(
        store
            .tree_nodes(o, p)
            .await
            .unwrap()
            .iter()
            .any(|node| node.memory == n.memory && node.revision == n.revision)
    );
    store
        .save_skill_draft(o, p, &c, "fixture", n.direct_sources - 1)
        .await
        .unwrap();
    decide(&r, &n, SkillStatus::Approved).await;
    decide(&r, &n, SkillStatus::Installed).await;
    let mut s = session(r.clone()).await;
    let receipt = body(
        &call(
            &mut s,
            "memory_purge",
            json!({"project":"project","sources":["lesson/first-1/1"]}),
        )
        .await,
    );
    assert_eq!(
        receipt["installed_skills_with_purged_evidence"],
        json!(["/skills/test-storage/SKILL.md"])
    );
    let c = r.skill_candidate(p, &n.memory).await.unwrap();
    assert_eq!(c.status, SkillStatus::Stale);
    assert!(c.draft.is_none());
    assert_eq!(c.install_hashes, vec!["a".repeat(64)]);
    assert!(
        r.skill_decide(
            p,
            &n.memory,
            &n.revision,
            SkillDecision {
                decision: SkillStatus::Approved,
                install_paths: vec![],
                install_hashes: vec![]
            }
        )
        .await
        .is_err()
    );
}
#[tokio::test]
async fn rebuilt_lesson_draft_counts_distinct_sources_not_link_rows() {
    let (store, r, n) = setup().await;
    let (o, a, p) = (&r.scope().owner, &r.scope().actor, &r.scope().projects[0]);
    let sources = store
        .tree_recent_sources(o, p, &n.memory)
        .await
        .unwrap()
        .into_iter()
        .map(|s| s.source)
        .collect();
    let rebuilt = store
        .commit_tree_maintenance(
            o,
            a,
            p,
            MaintenanceCommit {
                operation_id: "rebuild-skill-evidence".into(),
                operation: LinkOperation::Rebuild,
                rewrites: vec![(
                    n.memory.clone(),
                    n.revision.clone(),
                    n.title.clone(),
                    n.content.clone(),
                    sources,
                )],
                creates: vec![],
                reparent: vec![],
                expected_reparent: vec![],
                move_sources: vec![],
                stale: vec![],
                queue_item: None,
            },
            500,
        )
        .await
        .unwrap()
        .pop()
        .unwrap();
    store
        .review(
            o,
            a,
            &ReviewRequest {
                project: p.clone(),
                revision: rebuilt.revision.id.clone(),
                action: ReviewAction::Promote,
                expected_active: rebuilt.revision.predecessor,
                conflict_with: None,
            },
            501,
        )
        .await
        .unwrap();
    let n = store
        .tree_nodes(o, p)
        .await
        .unwrap()
        .into_iter()
        .find(|node| node.memory == n.memory)
        .unwrap();
    assert_eq!(n.direct_sources, 3);
    assert_eq!(
        store
            .node_sources(o, p, &n.memory, None, 32)
            .await
            .unwrap()
            .0
            .len(),
        6
    );
    let c = seed::candidate(&n);
    store
        .save_skill_draft(o, p, &c, "rebuilt-fixture", n.direct_sources)
        .await
        .unwrap();
    assert_eq!(
        r.skill_candidate(p, &n.memory).await.unwrap().revision_id,
        n.revision
    );
}

#[tokio::test]
async fn purge_preserves_install_records_after_staleness_and_redrafting() {
    for redraft in [false, true] {
        let (store, r, n) = setup().await;
        let (o, a, p) = (&r.scope().owner, &r.scope().actor, &r.scope().projects[0]);
        decide(&r, &n, SkillStatus::Approved).await;
        let installed = decide(&r, &n, SkillStatus::Installed).await;
        let n = seed::reinforce(&store, o, a, p, "first", Some(&n), 3, 1, "strategy").await;
        store
            .skill_mark_stale(o, p, &n.memory, &n.revision)
            .await
            .unwrap();
        if redraft {
            let mut c = r.skill_candidate(p, &n.memory).await.unwrap();
            c.status = SkillStatus::Drafted;
            c.revision_id = n.revision.clone();
            c.draft = Some("Updated draft".into());
            store
                .save_skill_draft(o, p, &c, "redraft-fixture", n.direct_sources)
                .await
                .unwrap();
        }
        let receipt = store
            .purge(o, p, &[SourceId::new("lesson/first-0/1").unwrap()])
            .await
            .unwrap();
        assert_eq!(
            receipt.installed_skills_with_purged_evidence,
            installed.install_paths
        );
        let c = r.skill_candidate(p, &n.memory).await.unwrap();
        assert_eq!(c.status, SkillStatus::Stale);
        assert!(c.draft.is_none());
        assert_eq!(c.install_paths, installed.install_paths);
        assert_eq!(c.install_hashes, installed.install_hashes);
    }
}

#[tokio::test]
async fn installed_paths_accumulate_and_reinstall_updates_hash() {
    let (store, r, n) = setup().await;
    let (o, p) = (&r.scope().owner, &r.scope().projects[0]);
    for (path, hash) in [
        ("/p1/SKILL.md", 'a'),
        ("/p2/SKILL.md", 'b'),
        ("/p1/SKILL.md", 'c'),
    ] {
        let mut c = r.skill_candidate(p, &n.memory).await.unwrap();
        if c.status == SkillStatus::Installed {
            c.status = SkillStatus::Drafted;
            store
                .save_skill_draft(o, p, &c, "redraft", n.direct_sources)
                .await
                .unwrap();
        }
        decide(&r, &n, SkillStatus::Approved).await;
        r.skill_decide(
            p,
            &n.memory,
            &n.revision,
            SkillDecision {
                decision: SkillStatus::Installed,
                install_paths: vec![path.into()],
                install_hashes: vec![hash.to_string().repeat(64)],
            },
        )
        .await
        .unwrap();
        let c = r.skill_candidate(p, &n.memory).await.unwrap();
        match hash {
            'a' => {
                assert_eq!(c.install_paths, ["/p1/SKILL.md"]);
                assert_eq!(c.install_hashes, ["a".repeat(64)]);
            }
            'b' => {
                assert_eq!(c.install_paths, ["/p2/SKILL.md", "/p1/SKILL.md"]);
                assert_eq!(c.install_hashes, ["b".repeat(64), "a".repeat(64)]);
            }
            _ => {
                assert_eq!(c.install_paths, ["/p1/SKILL.md", "/p2/SKILL.md"]);
                assert_eq!(c.install_hashes, ["c".repeat(64), "b".repeat(64)]);
            }
        }
    }
}

#[tokio::test]
async fn split_admission_reports_installed_paths_on_each_evidence_purge() {
    let (store, r, initial) = setup().await;
    let (o, a, p) = (&r.scope().owner, &r.scope().actor, &r.scope().projects[0]);
    let first = seed::reinforce(&store, o, a, p, "first", Some(&initial), 3, 1, "strategy").await;
    let n = seed::reinforce(&store, o, a, p, "first", Some(&first), 4, 1, "strategy").await;
    let ids: Vec<_> = [3, 4]
        .map(|i| SourceId::new(format!("lesson/first-{i}/1")).unwrap())
        .into();
    let events: Vec<_> = store
        .skill_recent_lesson_sources(o, p, &n.memory)
        .await
        .unwrap()
        .into_iter()
        .filter(|e| ids.contains(&e.source))
        .collect();
    store
        .admit_skill_operation(o, p, "skill-draft-job:split", &events, &n.revision)
        .await
        .unwrap();
    let c = seed::candidate(&n);
    store
        .save_skill_draft(o, p, &c, "skill-draft-job:split", n.direct_sources)
        .await
        .unwrap();
    decide(&r, &n, SkillStatus::Approved).await;
    let installed = decide(&r, &n, SkillStatus::Installed).await;
    let plan = MaintenanceCommit {
        operation_id: "split-skill".into(),
        operation: LinkOperation::Split,
        rewrites: vec![(
            n.memory.clone(),
            n.revision.clone(),
            "Parent".into(),
            "Summary".into(),
            vec![SourceId::new("lesson/first-0/1").unwrap()],
        )],
        creates: vec![(
            ParentRef::Node(n.memory.clone()),
            "Child".into(),
            "Summary".into(),
            ids.clone(),
        )],
        move_sources: ids
            .iter()
            .map(|id| (id.clone(), n.memory.clone(), 0))
            .collect(),
        reparent: vec![],
        expected_reparent: vec![],
        stale: vec![],
        queue_item: None,
    };
    store
        .commit_tree_maintenance(o, a, p, plan, 600)
        .await
        .unwrap();
    for id in ids {
        let receipt = store.purge(o, p, std::slice::from_ref(&id)).await.unwrap();
        assert_eq!(
            receipt.installed_skills_with_purged_evidence, installed.install_paths,
            "purging {id}"
        );
        let candidate = r.skill_candidate(p, &n.memory).await.unwrap();
        assert_eq!(candidate.status, SkillStatus::Stale);
        assert!(candidate.draft.is_none());
    }
}

#[tokio::test]
async fn purge_during_drafting_cannot_resurrect_a_draft() {
    let (store, r, n) = setup().await;
    let (o, a, p) = (&r.scope().owner, &r.scope().actor, &r.scope().projects[0]);
    let n = seed::reinforce(&store, o, a, p, "first", Some(&n), 3, 1, "strategy").await;
    let c = seed::candidate(&n);
    store
        .purge(o, p, &[SourceId::new("lesson/first-0/1").unwrap()])
        .await
        .unwrap();
    assert_eq!(
        store
            .save_skill_draft(o, p, &c, "fixture", n.direct_sources)
            .await,
        Err(RuntimeError::Conflict)
    );
}
