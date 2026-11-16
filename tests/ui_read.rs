#![cfg(feature = "memory-runtime")]
use lambda_memory as lm;
use lm::delivery::mcp::McpSession;
use lm::domain_models::{capture::*, memory_tree::*, runtime::*, skills::*, *};
use lm::integrations::database::embedded_capture::{EmbeddedCaptureStore, EmbeddedRuntimeStore};
use lm::services::runtime_service::MemoryRuntime;
use serde_json::{Value, json};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
static NEXT: AtomicUsize = AtomicUsize::new(0);
const TOKEN: &str = "ui-test-token-at-least-32-bytes-long";
struct Fixture {
    store: EmbeddedRuntimeStore,
    owner: OwnerId,
    actor: ActorId,
    project: ProjectId,
}
impl Fixture {
    async fn new(project: &str) -> Self {
        let path = std::env::var("HOME").unwrap()
            + &format!(
                "/.cache/lambda-memory/ui-read-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            );
        Self {
            store: EmbeddedRuntimeStore::new(EmbeddedCaptureStore::open(&path).await.unwrap()),
            owner: OwnerId::new("owner").unwrap(),
            actor: ActorId::new("actor").unwrap(),
            project: ProjectId::new(project).unwrap(),
        }
    }
    async fn source(&self, tag: &str, at: u64) -> SourceId {
        let source = SourceId::new(tag).unwrap();
        self.store
            .capture(
                CaptureEvent {
                    owner: self.owner.clone(),
                    actor: self.actor.clone(),
                    project: self.project.clone(),
                    source: source.clone(),
                    kind: WorkEventKind::Decision,
                    outcome: WorkOutcome::Unknown,
                    origin: "fixture://ui".into(),
                    recorded_at_ms: at,
                    content: format!("Evidence {tag}"),
                },
                &RuntimeLimits::default(),
            )
            .await
            .unwrap();
        source
    }
    async fn write(
        &self,
        tag: &str,
        parent: ParentRef,
        merge: Option<&TreeNode>,
        supersede: bool,
        at: u64,
    ) -> TreeNode {
        let source = self.source(tag, at).await;
        let mut plan = CurationCommit {
            links: vec![],
            operation_id: tag.into(),
            sources: vec![source.clone()],
            promotes: vec![],
            merges: vec![],
            supersedes: vec![],
            creates: vec![],
            parked: vec![],
            stale: vec![],
            split: vec![],
            operation: if supersede {
                LinkOperation::Supersede
            } else {
                LinkOperation::Create
            },
        };
        let content = "é🦀漢字".repeat(100);
        if let Some(n) = merge {
            plan.merges.push((
                n.memory.clone(),
                n.revision.clone(),
                tag.into(),
                content,
                vec![source],
            ));
            if supersede {
                plan.supersedes.push((n.memory.clone(), at));
            }
        } else {
            plan.creates
                .push((parent, tag.into(), content, vec![source]));
        }
        let item = self
            .store
            .commit_curation(&self.owner, &self.actor, &self.project, plan, at)
            .await
            .unwrap()
            .pop()
            .unwrap();
        self.publish(item, at).await
    }
    async fn publish(&self, item: RuntimeRevision, at: u64) -> TreeNode {
        self.store
            .review(
                &self.owner,
                &self.actor,
                &ReviewRequest {
                    project: self.project.clone(),
                    revision: item.revision.id,
                    action: ReviewAction::Promote,
                    expected_active: item.revision.predecessor,
                    conflict_with: None,
                },
                at,
            )
            .await
            .unwrap();
        self.store
            .tree_nodes(&self.owner, &self.project)
            .await
            .unwrap()
            .into_iter()
            .find(|n| n.memory == item.revision.memory)
            .unwrap()
    }
    async fn flat(&self, tag: &str, at: u64) {
        let source = self.source(&format!("source-{tag}"), at).await;
        let revision = RevisionId::new(format!("revision-{tag}")).unwrap();
        self.store
            .propose(
                &self.owner,
                &self.actor,
                RevisionProposal {
                    project: self.project.clone(),
                    memory: MemoryId::new(tag).unwrap(),
                    revision: revision.clone(),
                    sources: vec![source],
                    content: tag.into(),
                    predecessor: None,
                },
                at,
            )
            .await
            .unwrap();
        self.store
            .review(
                &self.owner,
                &self.actor,
                &ReviewRequest {
                    project: self.project.clone(),
                    revision,
                    action: ReviewAction::Promote,
                    expected_active: None,
                    conflict_with: None,
                },
                at,
            )
            .await
            .unwrap();
    }
    async fn session(&self) -> (Arc<MemoryRuntime>, McpSession) {
        let runtime = Arc::new(
            MemoryRuntime::new(
                self.store.clone(),
                RuntimeScope {
                    owner: self.owner.clone(),
                    actor: self.actor.clone(),
                    projects: vec![self.project.clone()],
                },
                RuntimeLimits {
                    idle_ms: 1,
                    ..Default::default()
                },
            )
            .unwrap(),
        );
        runtime.initialize_diagnostics().await.unwrap();
        let mut session = McpSession::new(runtime.clone(), TOKEN.into()).unwrap();
        rpc(&mut session,"initialize",json!({"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"ui-test","version":"1"},"_meta":{"lambdaMemoryToken":TOKEN}})).await;
        session
            .handle_frame(
                &json!({"jsonrpc":"2.0","method":"notifications/initialized"}).to_string(),
            )
            .await;
        (runtime, session)
    }
}
async fn rpc(session: &mut McpSession, method: &str, params: Value) -> Value {
    serde_json::from_str(
        &session
            .handle_frame(
                &json!({"jsonrpc":"2.0","id":1,"method":method,"params":params}).to_string(),
            )
            .await
            .unwrap(),
    )
    .unwrap()
}
async fn call(session: &mut McpSession, name: &str, args: Value) -> Value {
    let response = rpc(session, "tools/call", json!({"name":name,"arguments":args})).await;
    assert!(response.get("error").is_none(), "{response}");
    serde_json::from_str(response["result"]["content"][0]["text"].as_str().unwrap()).unwrap()
}
fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

#[tokio::test]
async fn node_additions_sources_replaced_and_skill_candidate_selection() {
    let f = Fixture::new("project").await;
    let original = f
        .write(
            "original",
            ParentRef::Root(TreeRoot::Decisions),
            None,
            false,
            1,
        )
        .await;
    let updated = f
        .write(
            "current",
            ParentRef::Root(TreeRoot::Decisions),
            Some(&original),
            true,
            2,
        )
        .await;
    let mut sources = vec![];
    for i in 0..32 {
        sources.push(f.source(&format!("extra-{i}"), 3).await);
    }
    let item = f
        .store
        .commit_curation(
            &f.owner,
            &f.actor,
            &f.project,
            CurationCommit {
                links: vec![],
                operation_id: "many-sources".into(),
                sources: sources.clone(),
                promotes: vec![],
                supersedes: vec![],
                merges: vec![(
                    updated.memory.clone(),
                    updated.revision.clone(),
                    "current refined".into(),
                    "current content".into(),
                    sources,
                )],
                creates: vec![],
                parked: vec![],
                stale: vec![],
                split: vec![],
                operation: LinkOperation::Merge,
            },
            3,
        )
        .await
        .unwrap()
        .pop()
        .unwrap();
    let current = f.publish(item, 4).await;
    let (_, mut s) = f.session().await;
    let detail = call(
        &mut s,
        "memory_node",
        json!({"project":"project","node":current.memory.as_str(),"sources":true}),
    )
    .await;
    assert_eq!(detail["source_total"], 34);
    assert_eq!(detail["sources"].as_array().unwrap().len(), 32);
    assert!(!detail["next_cursor"].is_null());
    assert_eq!(
        detail["replaced"],
        json!({"title":"original","valid_until_ms":2})
    );
    assert!(detail["skill_draft"].is_null());
    assert_eq!(detail["derived_memories"], json!([]));
    let second=call(&mut s,"memory_node",json!({"project":"project","node":current.memory.as_str(),"sources":true,"cursor":detail["next_cursor"]})).await;
    assert_eq!(second["sources"].as_array().unwrap().len(), 2);
    assert!(second["next_cursor"].is_null());
    assert_eq!(second["source_total"], 34);
    for (tag, status, at) in [
        ("older-stale", SkillStatus::Stale, 10),
        ("draft", SkillStatus::Drafted, 20),
        ("newer-stale", SkillStatus::Stale, 30),
    ] {
        f.candidate(&current, tag, status, at).await;
    }
    let detail = call(
        &mut s,
        "memory_node",
        json!({"project":"project","node":current.memory.as_str()}),
    )
    .await;
    assert_eq!(
        detail["skill_draft"],
        json!({"candidate":"draft","status":"drafted"})
    );
    let page = call(
        &mut s,
        "memory_nodes",
        json!({"project":"project","root":"decisions"}),
    )
    .await;
    assert_eq!(page["nodes"][0]["skill_draft"], detail["skill_draft"]);
    f.candidate(&current, "approved", SkillStatus::Approved, 40)
        .await;
    assert_eq!(
        call(
            &mut s,
            "memory_node",
            json!({"project":"project","node":current.memory.as_str()})
        )
        .await["skill_draft"],
        json!({"candidate":"approved","status":"approved"})
    );
}
impl Fixture {
    async fn candidate(&self, node: &TreeNode, id: &str, status: SkillStatus, at: u64) {
        self.store
            .save_skill_draft(
                &self.owner,
                &self.project,
                &SkillCandidate {
                    candidate_id: MemoryId::new(id).unwrap(),
                    scope: "project",
                    memory_id: node.memory.clone(),
                    revision_id: node.revision.clone(),
                    slug: id.into(),
                    status,
                    draft: Some("Draft steps".into()),
                    sessions_at_draft: 3,
                    sessions_at_decision: None,
                    created_at_ms: at,
                    decided_at_ms: None,
                    install_paths: vec![],
                    install_hashes: vec![],
                    pitfall_memories: vec![],
                    pitfall_revisions: vec![],
                    pitfall_source_counts: vec![],
                    installed_pitfall_memories: vec![],
                },
                id,
                node.direct_sources,
            )
            .await
            .unwrap();
    }
}
#[tokio::test]
async fn lesson_cards_complete_shape_counts_and_skill_fallback() {
    let f = Fixture::new("project").await;
    let mut sources = vec![];
    for (i, kind) in ["strategy", "pitfall", "strategy"].into_iter().enumerate() {
        let source = SourceId::new(format!("lesson/session-{i}/0")).unwrap();
        f.store
            .capture(
                CaptureEvent {
                    owner: f.owner.clone(),
                    actor: f.actor.clone(),
                    project: f.project.clone(),
                    source: source.clone(),
                    kind: WorkEventKind::Lesson,
                    outcome: WorkOutcome::Unknown,
                    origin: "fixture://lesson".into(),
                    recorded_at_ms: 10,
                    content: json!({"kind":kind,"title":"Test","guidance":"Run tests"}).to_string(),
                },
                &RuntimeLimits::default(),
            )
            .await
            .unwrap();
        sources.push(source);
    }
    let item = f
        .store
        .commit_curation(
            &f.owner,
            &f.actor,
            &f.project,
            CurationCommit {
                links: vec![],
                operation_id: "lesson-node".into(),
                sources: sources.clone(),
                promotes: vec![],
                merges: vec![],
                supersedes: vec![],
                creates: vec![(
                    ParentRef::Root(TreeRoot::Lessons),
                    "Test lessons".into(),
                    "Run tests".into(),
                    sources,
                )],
                parked: vec![],
                stale: vec![],
                split: vec![],
                operation: LinkOperation::Create,
            },
            11,
        )
        .await
        .unwrap()
        .pop()
        .unwrap();
    let n = f.publish(item, 12).await;
    for (i, status) in [
        SkillStatus::Drafted,
        SkillStatus::Approved,
        SkillStatus::Installed,
        SkillStatus::Stale,
        SkillStatus::Rejected,
    ]
    .into_iter()
    .enumerate()
    {
        f.candidate(&n, &format!("candidate-{i}"), status, 20 + i as u64)
            .await;
    }
    let (_, mut s) = f.session().await;
    assert_eq!(
        call(
            &mut s,
            "memory_nodes",
            json!({"project":"project","root":"lessons"})
        )
        .await,
        json!({
        "project":"project","root":"lessons","total":1,"topics":[{"node":n.memory.as_str(),"title":"Test lessons","count":1}],
        "nodes":[{"node":n.memory.as_str(),"title":"Test lessons","summary":"Run tests","path":["Lessons","Test lessons"],"topic":{"node":n.memory.as_str(),"title":"Test lessons"},"depth":1,"children":0,"basis":"observed","confidence":null,"derived_count":0,"valid_from_ms":null,"replaced":null,"source_count":3,"link_count":0,"updated_at_ms":11,"lesson":{"sessions":3,"strategies":2,"pitfalls":1},"skill_draft":{"candidate":"candidate-1","status":"approved"}}],"next_cursor":null})
    );
    let overview = call(&mut s, "memory_overview", json!({"project":"project"})).await;
    assert_eq!(overview["lessons"], json!({"nodes":1,"reinforced":1}));
    assert_eq!(
        overview["skill_drafts"],
        json!({"drafted":1,"approved":1,"installed":1,"stale":1,"rejected":1})
    );
    assert_eq!(
        call(
            &mut s,
            "memory_node",
            json!({"project":"project","node":n.memory.as_str()})
        )
        .await["lesson"],
        json!({"reinforced_sessions":3,"strategies":2,"pitfalls":1})
    );
    let other = f
        .write(
            "no pending candidates",
            ParentRef::Root(TreeRoot::Lessons),
            None,
            false,
            30,
        )
        .await;
    f.candidate(&other, "old-installed", SkillStatus::Installed, 31)
        .await;
    f.candidate(&other, "latest-rejected", SkillStatus::Rejected, 32)
        .await;
    assert_eq!(
        call(
            &mut s,
            "memory_node",
            json!({"project":"project","node":other.memory.as_str()})
        )
        .await["skill_draft"],
        json!({"candidate":"latest-rejected","status":"rejected"})
    );
}
#[tokio::test]
async fn concluded_nodes_derived_memories_and_link_counts() {
    use lm::domain_models::reflection::*;
    let f = Fixture::new("project").await;
    let a = f
        .write(
            "topic",
            ParentRef::Root(TreeRoot::Decisions),
            None,
            false,
            1,
        )
        .await;
    let b = f
        .write("child", ParentRef::Node(a.memory.clone()), None, false, 2)
        .await;
    let source = f.source("relation", 3).await;
    let item = f
        .store
        .commit_curation(
            &f.owner,
            &f.actor,
            &f.project,
            CurationCommit {
                links: vec![CurationLink {
                    from: LinkFrom::Node(a.memory.clone()),
                    to: b.memory.clone(),
                    relation: LinkRelation::Related,
                    sources: vec![source.clone()],
                }],
                operation_id: "link".into(),
                sources: vec![source.clone()],
                promotes: vec![],
                supersedes: vec![],
                merges: vec![(
                    a.memory.clone(),
                    a.revision.clone(),
                    a.title.clone(),
                    a.content.clone(),
                    vec![source],
                )],
                creates: vec![],
                parked: vec![],
                stale: vec![],
                split: vec![],
                operation: LinkOperation::Merge,
            },
            3,
        )
        .await
        .unwrap()
        .pop()
        .unwrap();
    let a = f.publish(item, 4).await;
    let input = ReflectionInput {
        branch: a.memory.clone(),
        nodes: vec![a.clone(), b.clone()]
            .into_iter()
            .map(|node| ReflectionNode {
                node,
                path: vec![],
                inside: true,
            })
            .collect(),
    };
    let item = f
        .store
        .commit_reflection(
            &f.owner,
            &f.actor,
            &f.project,
            &input,
            ReflectionAnswer {
                conclusions: vec![Conclusion {
                    title: "Conclusion".into(),
                    content: "Inferred".into(),
                    confidence: Confidence::Medium,
                    derived_from: vec![0, 1],
                    parent: 0,
                }],
                ..Default::default()
            },
            "conclude",
            5,
            lm::engines::reflection_policy::POLICY,
        )
        .await
        .unwrap()
        .pop()
        .unwrap();
    let c = f.publish(item, 6).await;
    let (_, mut s) = f.session().await;
    let detail = call(
        &mut s,
        "memory_node",
        json!({"project":"project","node":c.memory.as_str()}),
    )
    .await;
    assert_eq!(
        detail["derived_memories"],
        json!([{"node":a.memory.as_str(),"title":a.title},{"node":b.memory.as_str(),"title":b.title}])
    );
    assert!(detail["skill_draft"].is_null());
    let page = call(
        &mut s,
        "memory_nodes",
        json!({"project":"project","root":"decisions","basis":"concluded"}),
    )
    .await;
    assert_eq!(page["total"], 1);
    assert_eq!(page["nodes"][0]["derived_count"], 2);
    assert_eq!(page["nodes"][0]["confidence"], "medium");
    let page = call(
        &mut s,
        "memory_nodes",
        json!({"project":"project","root":"decisions","basis":"observed"}),
    )
    .await;
    assert_eq!(page["total"], 2);
    for node in page["nodes"].as_array().unwrap() {
        assert_eq!(node["link_count"], 1);
    }
    assert_eq!(
        call(&mut s, "memory_overview", json!({"project":"project"})).await["basis"],
        json!({"observed":2,"concluded":1})
    );
}
#[tokio::test]
async fn day_buckets_use_shifted_dates_and_floor_at_epoch() {
    let f = Fixture::new("project").await;
    f.source("boundary", 23 * 3_600_000 + 30 * 60_000).await;
    let view = f
        .store
        .ui_overview(&f.owner, &f.project, 60, 2 * 86_400_000)
        .await
        .unwrap();
    assert_eq!(view.days.last().unwrap().day, "1970-01-03");
    assert_eq!(
        view.days.iter().find(|d| d.captured == 1).unwrap().day,
        "1970-01-02"
    );
    let view = f
        .store
        .ui_overview(&f.owner, &f.project, -60, 0)
        .await
        .unwrap();
    assert_eq!(view.days.last().unwrap().day, "1969-12-31");
}

#[tokio::test]
async fn overview_empty_project_and_user_contracts_and_read_only() {
    for (project, root_names) in [
        (
            "project",
            vec![
                ("decisions", "Decisions"),
                ("preferences", "Preferences"),
                ("architecture", "Architecture"),
                ("open_items", "Open items"),
                ("lessons", "Lessons"),
            ],
        ),
        (
            "user:owner",
            vec![
                ("preferences", "Preferences"),
                ("working_rules", "Working rules"),
                ("general_lessons", "General lessons"),
            ],
        ),
    ] {
        let f = Fixture::new(project).await;
        let (runtime, mut session) = f.session().await;
        let before = call(&mut session, "memory_activity", json!({"project":project})).await;
        call(
            &mut session,
            "memory_nodes",
            json!({"project":project,"root":root_names[0].0}),
        )
        .await;
        let listed = rpc(&mut session, "tools/list", json!({})).await;
        for name in ["memory_nodes", "memory_overview"] {
            let tool = listed["result"]["tools"]
                .as_array()
                .unwrap()
                .iter()
                .find(|t| t["name"] == name)
                .unwrap();
            assert_eq!(tool["annotations"]["readOnlyHint"], true);
            let description = tool["description"].as_str().unwrap();
            assert!(
                description.starts_with("UI read:")
                    && description.contains("without model calls or idle postponement")
            );
            let error = call(
                &mut session,
                name,
                if name == "memory_nodes" {
                    json!({"project":"foreign","root":"decisions"})
                } else {
                    json!({"project":"foreign"})
                },
            )
            .await;
            assert!(error.get("error").is_some());
        }
        let got = call(&mut session, "memory_overview", json!({"project":project})).await;
        let days = got["days"].as_array().unwrap();
        assert_eq!(days.len(), 30);
        assert!(
            days.iter()
                .all(|d| d["captured"] == 0 && d["published"] == 0)
        );
        assert!(
            days.windows(2)
                .all(|w| w[0]["day"].as_str().unwrap() < w[1]["day"].as_str().unwrap())
        );
        assert_eq!(
            got,
            json!({"project":project,"mode":"tree","roots":root_names.into_iter().map(|(root,label)|json!({"root":root,"label":label,"nodes":0,"latest":null})).collect::<Vec<_>>(),"flat_active":0,"basis":{"observed":0,"concluded":0},"lessons":{"nodes":0,"reinforced":0},"skill_drafts":{"drafted":0,"approved":0,"installed":0,"stale":0,"rejected":0},"days":days})
        );
        assert_eq!(
            before["entries"],
            call(&mut session, "memory_activity", json!({"project":project})).await["entries"]
        );
        tokio::time::sleep(std::time::Duration::from_millis(3)).await;
        assert!(runtime.dream_once().await.result.is_ok());
    }
}
#[tokio::test]
async fn flat_overview_nodes_and_list_exclusion_paging() {
    let f = Fixture::new("project").await;
    for (i, tag) in ["flat-a", "flat-b", "procedural:playbook"]
        .into_iter()
        .enumerate()
    {
        f.flat(tag, 100 + i as u64).await;
    }
    let (_, mut s) = f.session().await;
    let overview = call(&mut s, "memory_overview", json!({"project":"project"})).await;
    assert_eq!(overview["mode"], "flat");
    assert_eq!(overview["flat_active"], 2);
    assert!(
        overview["roots"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["nodes"] == 0 && r["latest"].is_null())
    );
    assert_eq!(
        call(
            &mut s,
            "memory_nodes",
            json!({"project":"project","root":"decisions"})
        )
        .await,
        json!({"project":"project","root":"decisions","total":0,"topics":[],"nodes":[],"next_cursor":null})
    );
    f.store
        .set_tree_mode(&f.owner, &f.project, TreeMode::Tree, 200)
        .await
        .unwrap();
    f.write(
        "tree",
        ParentRef::Root(TreeRoot::Decisions),
        None,
        false,
        201,
    )
    .await;
    let all = call(
        &mut s,
        "memory_list",
        json!({"project":"project","states":["active"]}),
    )
    .await;
    assert_eq!(all["revisions"].as_array().unwrap().len(), 4);
    assert_eq!(
        all,
        call(
            &mut s,
            "memory_list",
            json!({"project":"project","states":["active"],"exclude_tree":false})
        )
        .await
    );
    let mut args = json!({"project":"project","states":["active"],"exclude_tree":true,"limit":1});
    let mut ids = vec![];
    loop {
        let page = call(&mut s, "memory_list", args.clone()).await;
        ids.push(page["revisions"][0]["memory"].clone());
        if page["next_cursor"].is_null() {
            break;
        }
        args["cursor"] = page["next_cursor"].clone();
    }
    assert_eq!(ids, vec![json!("flat-a"), json!("flat-b")]);
    assert_eq!(
        call(&mut s, "memory_overview", json!({"project":"project"})).await["flat_active"],
        2
    );
}
#[tokio::test]
async fn overview_counts_latest_ties_and_day_offset() {
    let f = Fixture::new("project").await;
    let day = now() / 86_400_000 - 1;
    let at = day * 86_400_000 + 23 * 3_600_000 + 30 * 60_000;
    let a = f
        .write("a", ParentRef::Root(TreeRoot::Decisions), None, false, at)
        .await;
    let b = f
        .write("b", ParentRef::Root(TreeRoot::Decisions), None, false, at)
        .await;
    let (_, mut s) = f.session().await;
    let zero = call(&mut s, "memory_overview", json!({"project":"project"})).await;
    let shifted = call(
        &mut s,
        "memory_overview",
        json!({"project":"project","utc_offset_minutes":60}),
    )
    .await;
    let expected = if a.memory < b.memory { a } else { b };
    assert_eq!(
        zero["roots"][0],
        json!({"root":"decisions","label":"Decisions","nodes":2,"latest":{"node":expected.memory.as_str(),"title":expected.title}})
    );
    assert_eq!(zero["basis"], json!({"observed":2,"concluded":0}));
    let day0 = zero["days"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["captured"] == 2)
        .unwrap()["day"]
        .as_str()
        .unwrap();
    let day1 = shifted["days"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["captured"] == 2)
        .unwrap()["day"]
        .as_str()
        .unwrap();
    assert!(day1 > day0);
    assert_eq!(
        shifted["days"]
            .as_array()
            .unwrap()
            .iter()
            .find(|d| d["captured"] == 2)
            .unwrap()["published"],
        2
    );
    let latest = f
        .write(
            "latest child",
            ParentRef::Node(expected.memory.clone()),
            None,
            false,
            at + 1,
        )
        .await;
    let preference = f
        .write(
            "preference",
            ParentRef::Root(TreeRoot::Preferences),
            None,
            false,
            at + 2,
        )
        .await;
    let overview = call(&mut s, "memory_overview", json!({"project":"project"})).await;
    assert_eq!(
        overview["roots"][0],
        json!({"root":"decisions","label":"Decisions","nodes":3,"latest":{"node":latest.memory.as_str(),"title":"latest child"}})
    );
    assert_eq!(
        overview["roots"][1],
        json!({"root":"preferences","label":"Preferences","nodes":1,"latest":{"node":preference.memory.as_str(),"title":"preference"}})
    );
}
#[tokio::test]
async fn topics_follow_edges_through_inactive_ancestors() {
    let f = Fixture::new("project").await;
    let a = f
        .write("A", ParentRef::Root(TreeRoot::Decisions), None, false, 1)
        .await;
    let b = f
        .write("B", ParentRef::Node(a.memory.clone()), None, false, 2)
        .await;
    let c = f
        .write("C", ParentRef::Node(b.memory.clone()), None, false, 3)
        .await;
    f.store
        .purge(&f.owner, &f.project, &[SourceId::new("B").unwrap()])
        .await
        .unwrap();
    let active = f.store.tree_nodes(&f.owner, &f.project).await.unwrap();
    assert!(!active.iter().any(|n| n.memory == b.memory));
    assert_eq!(
        active.iter().find(|n| n.memory == c.memory).unwrap().parent,
        Some(b.memory.clone())
    );
    let geometry = f
        .store
        .tree_rebuild_node(&f.owner, &f.project, &b.memory)
        .await
        .unwrap();
    assert_eq!(geometry.parent, Some(a.memory.clone()));
    let (_, mut session) = f.session().await;
    let page = call(
        &mut session,
        "memory_nodes",
        json!({"project":"project","root":"decisions"}),
    )
    .await;
    assert_eq!(page["total"], 2);
    assert_eq!(
        page["topics"],
        json!([{"node":a.memory.as_str(),"title":"A","count":2}])
    );
    for node in page["nodes"].as_array().unwrap() {
        assert_eq!(node["topic"], json!({"node":a.memory.as_str(),"title":"A"}));
    }
    let filtered = call(
        &mut session,
        "memory_nodes",
        json!({"project":"project","root":"decisions","topic":a.memory.as_str()}),
    )
    .await;
    assert_eq!(filtered, page);
    let mut ids: Vec<_> = filtered["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["node"].as_str().unwrap())
        .collect();
    ids.sort_unstable();
    let mut expected = vec![a.memory.as_str(), c.memory.as_str()];
    expected.sort_unstable();
    assert_eq!(ids, expected);

    // The edges still lead to A after its own head disappears, but an inactive
    // first-level ancestor must not be advertised as a topic.
    f.store
        .purge(&f.owner, &f.project, &[SourceId::new("A").unwrap()])
        .await
        .unwrap();
    let page = call(
        &mut session,
        "memory_nodes",
        json!({"project":"project","root":"decisions"}),
    )
    .await;
    assert_eq!(page["total"], 1);
    assert_eq!(page["topics"], json!([]));
    assert_eq!(page["nodes"][0]["node"], c.memory.as_str());
    assert!(page["nodes"][0]["topic"].is_null());
}
#[tokio::test]
async fn nodes_filters_stable_paging_topics_sort_updated_and_utf8() {
    let f = Fixture::new("project").await;
    let topic = f
        .write(
            "Topic",
            ParentRef::Root(TreeRoot::Decisions),
            None,
            false,
            10,
        )
        .await;
    let mut nodes = vec![];
    for i in 0..5 {
        nodes.push(
            f.write(
                &format!("MATCH-{i}"),
                ParentRef::Node(topic.memory.clone()),
                None,
                false,
                20 + i,
            )
            .await,
        );
    }
    f.write(
        "non-target",
        ParentRef::Node(topic.memory.clone()),
        None,
        false,
        30,
    )
    .await;
    f.write(
        "other topic",
        ParentRef::Root(TreeRoot::Decisions),
        None,
        false,
        31,
    )
    .await;
    let updated = f
        .write(
            "MATCH-updated",
            ParentRef::Root(TreeRoot::Decisions),
            Some(&nodes[0]),
            true,
            40,
        )
        .await;
    let (_, mut s) = f.session().await;
    let mut args = json!({"project":"project","root":"decisions","topic":topic.memory.as_str(),"basis":"observed","query":"match","limit":2});
    let mut ids = vec![];
    let mut sizes = vec![];
    let mut topics = None;
    loop {
        let page = call(&mut s, "memory_nodes", args.clone()).await;
        assert_eq!(page["total"], 5);
        if let Some(ref topics) = topics {
            assert_eq!(topics, &page["topics"]);
        } else {
            topics = Some(page["topics"].clone());
        }
        let entries = page["nodes"].as_array().unwrap();
        sizes.push(entries.len());
        for n in entries {
            let summary = n["summary"].as_str().unwrap();
            assert!(summary.len() <= 280);
            ids.push(n["node"].as_str().unwrap().to_owned());
            assert_eq!(n["topic"]["node"], topic.memory.as_str());
        }
        if page["next_cursor"].is_null() {
            break;
        }
        args["cursor"] = page["next_cursor"].clone();
    }
    assert_eq!(sizes, vec![2, 2, 1]);
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), 5);
    let unfiltered = call(
        &mut s,
        "memory_nodes",
        json!({"project":"project","root":"decisions"}),
    )
    .await;
    assert_eq!(topics.unwrap(), unfiltered["topics"]);
    assert_eq!(
        unfiltered["topics"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["node"] == topic.memory.as_str())
            .unwrap()["count"],
        7
    );
    let recent = call(
        &mut s,
        "memory_nodes",
        json!({"project":"project","root":"decisions","updated":true}),
    )
    .await;
    assert_eq!(recent["total"], 1);
    assert_eq!(recent["nodes"][0]["node"], updated.memory.as_str());
    assert_eq!(
        recent["nodes"][0]["replaced"],
        json!({"title":"MATCH-0","valid_until_ms":40})
    );
    let sorted = call(
        &mut s,
        "memory_nodes",
        json!({"project":"project","root":"decisions","sort":"sources"}),
    )
    .await;
    assert_eq!(sorted["nodes"][0]["node"], updated.memory.as_str());
    assert_eq!(sorted["nodes"][0]["source_count"], 2);
    let mut args = json!({"project":"project","root":"decisions","sort":"sources","limit":2});
    let mut paged = vec![];
    loop {
        let page = call(&mut s, "memory_nodes", args.clone()).await;
        paged.extend(
            page["nodes"]
                .as_array()
                .unwrap()
                .iter()
                .map(|n| n["node"].clone()),
        );
        if page["next_cursor"].is_null() {
            break;
        }
        args["cursor"] = page["next_cursor"].clone();
    }
    assert_eq!(
        paged,
        sorted["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|n| n["node"].clone())
            .collect::<Vec<_>>()
    );
}
#[tokio::test]
async fn strict_arguments_invalid_cursors_and_filter_mismatches() {
    let f = Fixture::new("project").await;
    let topic = f
        .write(
            "topic",
            ParentRef::Root(TreeRoot::Decisions),
            None,
            false,
            1,
        )
        .await;
    let child = f
        .write(
            "child",
            ParentRef::Node(topic.memory.clone()),
            None,
            false,
            2,
        )
        .await;
    let (_, mut s) = f.session().await;
    let page = call(
        &mut s,
        "memory_nodes",
        json!({"project":"project","root":"decisions","limit":1}),
    )
    .await;
    assert!(!page["next_cursor"].is_null());
    for changed in [
        json!({"root":"preferences"}),
        json!({"basis":"observed"}),
        json!({"updated":true}),
        json!({"topic":topic.memory.as_str()}),
        json!({"sort":"sources"}),
    ] {
        let mut args = json!({"project":"project","root":"decisions","cursor":page["next_cursor"]});
        args.as_object_mut()
            .unwrap()
            .extend(changed.as_object().unwrap().clone());
        let error = call(&mut s, "memory_nodes", args).await;
        assert!(
            error["error"]
                .as_str()
                .unwrap()
                .contains("invalid runtime input"),
            "{error}"
        );
    }
    for extra in [
        json!({"root":"working_rules"}),
        json!({"topic":child.memory.as_str()}),
        json!({"topic":"missing"}),
        json!({"limit":0}),
        json!({"limit":33}),
        json!({"limit":1.5}),
        json!({"query":"é".repeat(129)}),
        json!({"basis":"other"}),
        json!({"sort":"other"}),
        json!({"updated":1}),
        json!({"cursor":"bad"}),
        json!({"query":"changed","cursor":page["next_cursor"]}),
        json!({"unknown":true}),
    ] {
        let mut args = json!({"project":"project","root":"decisions"});
        args.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        let error = call(&mut s, "memory_nodes", args).await;
        assert!(
            error["error"]
                .as_str()
                .unwrap()
                .contains("invalid runtime input"),
            "{error}"
        );
    }
    for extra in [
        json!({"utc_offset_minutes":-721}),
        json!({"utc_offset_minutes":841}),
        json!({"utc_offset_minutes":1.5}),
        json!({"unknown":true}),
    ] {
        let mut args = json!({"project":"project"});
        args.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        let e = call(&mut s, "memory_overview", args).await;
        assert!(
            e["error"]
                .as_str()
                .unwrap()
                .contains("invalid runtime input"),
            "{e}"
        );
    }
}
