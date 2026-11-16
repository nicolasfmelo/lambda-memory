use super::*;
use crate as lm;
use crate::domain_models::memory_tree::TreeNode;
use crate::domain_models::runtime_diagnostics::*;
use crate::domain_models::{ActorId, OwnerId};
use crate::integrations::database::embedded_capture::EmbeddedCaptureStore;
use crate::integrations::external_apis::router_runtime::RouterRuntimeProviders;
#[path = "skill_seed.rs"]
#[allow(clippy::duplicate_mod)]
mod seed;
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    io::{Read, Write},
    net::TcpListener,
    sync::{Arc, Mutex as StdMutex, atomic::AtomicUsize},
};
static NEXT: AtomicUsize = AtomicUsize::new(0);
struct Fake {
    base: String,
    replies: Arc<StdMutex<VecDeque<(u16, Value)>>>,
    inputs: Arc<StdMutex<Vec<Value>>>,
}
impl Fake {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let replies: Arc<StdMutex<VecDeque<(u16, Value)>>> = Arc::default();
        let inputs: Arc<StdMutex<Vec<Value>>> = Arc::default();
        let fake = Self {
            base: format!("http://{}", listener.local_addr().unwrap()),
            replies: replies.clone(),
            inputs: inputs.clone(),
        };
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut socket) = stream else {
                    break;
                };
                let mut bytes = Vec::new();
                let mut buf = [0; 4096];
                let mut end = None;
                while end.is_none_or(|n| bytes.len() < n) {
                    let n = socket.read(&mut buf).unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    bytes.extend_from_slice(&buf[..n]);
                    if end.is_none()
                        && let Some(i) = bytes.windows(4).position(|w| w == b"\r\n\r\n")
                    {
                        let head = String::from_utf8_lossy(&bytes[..i]).to_lowercase();
                        let len = head
                            .lines()
                            .find_map(|l| {
                                l.strip_prefix("content-length: ")
                                    .and_then(|s| s.parse::<usize>().ok())
                            })
                            .unwrap_or(0);
                        end = Some(i + 4 + len);
                    }
                }
                let start = bytes.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4;
                inputs
                    .lock()
                    .unwrap()
                    .push(serde_json::from_slice(&bytes[start..]).unwrap());
                let (status, answer) = replies
                    .lock()
                    .unwrap()
                    .pop_front()
                    .expect("unexpected provider call");
                if status == 0 {
                    continue;
                }
                let body=json!({"model":"anthropic/claude-haiku-5.5","usage":{"prompt_tokens":10,"completion_tokens":5,"cost":0.000001},"choices":[{"finish_reason":"stop","message":{"content":answer.to_string()}}]}).to_string();
                let _ = write!(
                    socket,
                    "HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
            }
        });
        fake
    }
    fn reply(&self, status: u16, answer: Value) {
        self.replies.lock().unwrap().push_back((status, answer));
    }
    fn calls(&self) -> usize {
        self.inputs.lock().unwrap().len()
    }
}
fn answer() -> Value {
    json!({"name":"test-storage","description":"Use when changing storage","steps":["Run targeted tests.","Inspect the result."],"pitfalls":["Avoid untested migrations."]})
}
async fn setup(fake: &Fake) -> MemoryRuntime {
    setup_project(fake, "project").await
}
async fn setup_project(fake: &Fake, project: &str) -> MemoryRuntime {
    let path = format!(
        "/tmp/lambda-skills-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    );
    MemoryRuntime::with_openrouter(
        EmbeddedRuntimeStore::new(EmbeddedCaptureStore::open(&path).await.unwrap()),
        RuntimeScope {
            owner: OwnerId::new("owner").unwrap(),
            actor: ActorId::new("actor").unwrap(),
            projects: vec![ProjectId::new(project).unwrap()],
        },
        RuntimeLimits {
            idle_ms: 1,
            ..Default::default()
        },
        RouterRuntimeProviders::mock(&fake.base, 128),
    )
    .unwrap()
}
async fn add(
    r: &MemoryRuntime,
    tag: &str,
    node: Option<&TreeNode>,
    start: usize,
    count: usize,
    kind: &str,
) -> TreeNode {
    seed::reinforce(
        &r.store,
        &r.scope.owner,
        &r.scope.actor,
        &r.scope.projects[0],
        tag,
        node,
        start,
        count,
        kind,
    )
    .await
}
async fn tick(r: &MemoryRuntime) -> RecordedOperation<Option<DreamReceipt>> {
    tokio::time::sleep(std::time::Duration::from_millis(3)).await;
    let p = &r.scope.projects[0];
    let work = r.skill_work(p).await.unwrap();
    let mut trace = r
        .begin_trace(
            std::slice::from_ref(p),
            ActivityOperation::Dream,
            ActivityPhase::Derivation,
            true,
        )
        .await
        .unwrap();
    let result = r.skill_tick(p, work, &mut trace).await;
    r.finish_trace(trace, result).await
}
async fn row(r: &MemoryRuntime, n: &TreeNode) -> SkillCandidate {
    r.skill_candidate(&r.scope.projects[0], &n.memory)
        .await
        .unwrap()
}
async fn decide(r: &MemoryRuntime, n: &TreeNode, status: SkillStatus) -> SkillCandidate {
    r.skill_decide(
        &r.scope.projects[0],
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
async fn draft_job_renders_skill_md() {
    let f = Fake::start();
    f.reply(200, answer());
    let r = setup(&f).await;
    let n = add(&r, "strategy", None, 0, 3, "strategy").await;
    add(&r, "pitfall", None, 0, 1, "pitfall").await;
    tick(&r).await.result.unwrap();
    let c = row(&r, &n).await;
    assert_eq!(c.status, SkillStatus::Drafted);
    assert_eq!(c.slug, "test-storage");
    assert_eq!(c.sessions_at_draft, 3);
    let d = c.draft.unwrap();
    assert!(d.starts_with(
        "---\nname: test-storage\ndescription: \"Use when changing storage\"\n---\n\n# Test storage"
    ));
    assert!(d.contains("## Steps\n1. Run targeted tests.\n2. Inspect the result."));
    assert!(d.contains("## Pitfalls\n- Avoid untested migrations."));
    assert!(d.contains(&format!("Lambda Memory project lesson node {} (revision {}), reinforced by 3 sessions: strategy-0, strategy-1, strategy-2.",n.memory,n.revision)));
    let req = &f.inputs.lock().unwrap()[0];
    assert_eq!(
        req["response_format"]["json_schema"]["name"],
        "runtime_skill_draft_v1"
    );
    assert_eq!(req["response_format"]["json_schema"]["strict"], true);
    let input: Value =
        serde_json::from_str(req["messages"][1]["content"].as_str().unwrap()).unwrap();
    assert_eq!(input["pitfalls"].as_array().unwrap().len(), 1);
}
#[tokio::test]
async fn changed_or_retired_pitfall_revision_rejects_draft_and_records_failure() {
    for retire in [false, true] {
        let f = Fake::start();
        let r = setup(&f).await;
        let n = add(&r, "strategy", None, 0, 3, "strategy").await;
        let pitfall = add(&r, "pitfall", None, 0, 1, "pitfall").await;
        let (o, a, p) = (&r.scope.owner, &r.scope.actor, &r.scope.projects[0]);
        let work = r.skill_work(p).await.unwrap();
        assert_eq!(work.len(), 1);
        assert_eq!(work[0].0.pitfalls.len(), 1);
        if retire {
            r.store
                .retire_node(o, a, p, &pitfall.memory, now_ms().unwrap())
                .await
                .unwrap();
        } else {
            add(&r, "pitfall", Some(&pitfall), 1, 1, "pitfall").await;
        }
        f.reply(200, answer());
        tokio::time::sleep(std::time::Duration::from_millis(3)).await;
        let mut trace = r
            .begin_trace(
                std::slice::from_ref(p),
                ActivityOperation::Dream,
                ActivityPhase::Derivation,
                true,
            )
            .await
            .unwrap();
        let result = r.skill_tick(p, work, &mut trace).await;
        r.finish_trace(trace, result).await.result.unwrap();
        assert_eq!(f.calls(), 1);
        assert!(r.skill_candidates(p, None).await.unwrap().is_empty());
        assert!(
            r.store
                .skill_failed(o, p, &n.memory, &n.revision)
                .await
                .unwrap()
        );
        assert!(r.skill_work(p).await.unwrap().is_empty());
        assert!(
            r.activity(ActivityRequest {
                project: p.clone(),
                limit: 32,
                operations: vec![],
                outcomes: vec![],
                cursor: None
            })
            .await
            .unwrap()
            .entries
            .iter()
            .any(|e| e.reason == Some(ActivityReason::ProviderInvalidOutput))
        );
        assert!(!r.background_stopped().unwrap());
    }
}

#[tokio::test]
async fn purging_older_pitfall_source_during_drafting_rejects_surviving_revision() {
    let f = Fake::start();
    let r = setup(&f).await;
    let n = add(&r, "strategy", None, 0, 3, "strategy").await;
    let pitfall = add(&r, "pitfall", None, 0, 1, "pitfall").await;
    let pitfall = add(&r, "pitfall", Some(&pitfall), 1, 1, "pitfall").await;
    let (o, p) = (&r.scope.owner, &r.scope.projects[0]);
    let work = r.skill_work(p).await.unwrap();
    assert_eq!(work[0].0.pitfalls[0].direct_sources, 2);
    r.store
        .purge(o, p, &[SourceId::new("lesson/pitfall-0/1").unwrap()])
        .await
        .unwrap();
    let surviving = r
        .store
        .tree_nodes(o, p)
        .await
        .unwrap()
        .into_iter()
        .find(|node| node.memory == pitfall.memory)
        .unwrap();
    assert_eq!(surviving.revision, pitfall.revision);
    assert_eq!(surviving.direct_sources, 1);
    f.reply(200, answer());
    tokio::time::sleep(std::time::Duration::from_millis(3)).await;
    let mut trace = r
        .begin_trace(
            std::slice::from_ref(p),
            ActivityOperation::Dream,
            ActivityPhase::Derivation,
            true,
        )
        .await
        .unwrap();
    let result = r.skill_tick(p, work, &mut trace).await;
    r.finish_trace(trace, result).await.result.unwrap();
    assert_eq!(f.calls(), 1);
    assert!(r.skill_candidates(p, None).await.unwrap().is_empty());
    assert!(
        r.store
            .skill_failed(o, p, &n.memory, &n.revision)
            .await
            .unwrap()
    );
    assert!(r.skill_work(p).await.unwrap().is_empty());
    assert!(!r.background_stopped().unwrap());
    assert!(
        r.activity(ActivityRequest {
            project: p.clone(),
            limit: 32,
            operations: vec![],
            outcomes: vec![],
            cursor: None
        })
        .await
        .unwrap()
        .entries
        .iter()
        .any(|e| e.reason == Some(ActivityReason::ProviderInvalidOutput))
    );
}

#[tokio::test]
async fn rebuilt_pitfall_guard_counts_distinct_sources_not_link_rows() {
    use crate::domain_models::memory_tree::{LinkOperation, MaintenanceCommit};
    let f = Fake::start();
    let r = setup(&f).await;
    let n = add(&r, "strategy", None, 0, 3, "strategy").await;
    let pitfall = add(&r, "pitfall", None, 0, 1, "pitfall").await;
    let (o, a, p) = (&r.scope.owner, &r.scope.actor, &r.scope.projects[0]);
    let sources = r
        .store
        .tree_recent_sources(o, p, &pitfall.memory)
        .await
        .unwrap()
        .into_iter()
        .map(|event| event.source)
        .collect();
    let rebuilt = r
        .store
        .commit_tree_maintenance(
            o,
            a,
            p,
            MaintenanceCommit {
                operation_id: "rebuild-pitfall-evidence".into(),
                operation: LinkOperation::Rebuild,
                rewrites: vec![(
                    pitfall.memory.clone(),
                    pitfall.revision.clone(),
                    pitfall.title.clone(),
                    pitfall.content.clone(),
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
    r.store
        .review(
            o,
            a,
            &ReviewRequest {
                project: p.clone(),
                revision: rebuilt.revision.id,
                action: ReviewAction::Promote,
                expected_active: rebuilt.revision.predecessor,
                conflict_with: None,
            },
            501,
        )
        .await
        .unwrap();
    assert_eq!(
        r.store
            .node_sources(o, p, &pitfall.memory, None, 32)
            .await
            .unwrap()
            .0
            .len(),
        2
    );
    f.reply(200, answer());
    tick(&r).await.result.unwrap();
    let c = row(&r, &n).await;
    assert_eq!(c.status, SkillStatus::Drafted);
    assert_eq!(c.pitfall_source_counts, vec![1]);
}

#[tokio::test]
async fn reinstall_elsewhere_keeps_earlier_pitfall_provenance() {
    let f = Fake::start();
    let r = setup(&f).await;
    let n = add(&r, "strategy", None, 0, 3, "strategy").await;
    let pitfall = add(&r, "pitfall", None, 0, 1, "pitfall").await;
    f.reply(200, answer());
    tick(&r).await.result.unwrap();
    decide(&r, &n, SkillStatus::Approved).await;
    let p = &r.scope.projects[0];
    let install = |path: &str| SkillDecision {
        decision: SkillStatus::Installed,
        install_paths: vec![path.into()],
        install_hashes: vec!["a".repeat(64)],
    };
    r.skill_decide(p, &n.memory, &n.revision, install("/p1/SKILL.md"))
        .await
        .unwrap();
    let (o, a) = (&r.scope.owner, &r.scope.actor);
    r.store
        .retire_node(o, a, p, &pitfall.memory, now_ms().unwrap())
        .await
        .unwrap();
    let n = add(&r, "strategy", Some(&n), 3, 1, "strategy").await;
    let mut updated = answer();
    updated["pitfalls"] = json!([]);
    f.reply(200, updated);
    tick(&r).await.result.unwrap();
    decide(&r, &n, SkillStatus::Approved).await;
    let c = r
        .skill_decide(p, &n.memory, &n.revision, install("/p2/SKILL.md"))
        .await
        .unwrap();
    assert_eq!(c.install_paths, ["/p2/SKILL.md", "/p1/SKILL.md"]);
    assert_eq!(c.installed_pitfall_memories, vec![pitfall.memory.clone()]);
    let receipt = r
        .store
        .purge(o, p, &[SourceId::new("lesson/pitfall-0/1").unwrap()])
        .await
        .unwrap();
    assert!(
        receipt
            .installed_skills_with_purged_evidence
            .contains(&"/p1/SKILL.md".to_string())
    );
}

#[tokio::test]
async fn installed_pitfall_provenance_survives_redraft_until_reinstalled() {
    for reinstall in [false, true] {
        let f = Fake::start();
        let r = setup(&f).await;
        let n = add(&r, "strategy", None, 0, 3, "strategy").await;
        let pitfall = add(&r, "pitfall", None, 0, 1, "pitfall").await;
        f.reply(200, answer());
        tick(&r).await.result.unwrap();
        assert_eq!(
            row(&r, &n).await.pitfall_memories,
            vec![pitfall.memory.clone()]
        );
        decide(&r, &n, SkillStatus::Approved).await;
        let installed = decide(&r, &n, SkillStatus::Installed).await;
        assert_eq!(
            installed.installed_pitfall_memories,
            vec![pitfall.memory.clone()]
        );
        let (o, a, p) = (&r.scope.owner, &r.scope.actor, &r.scope.projects[0]);
        // Retirement leaves B's evidence links intact but removes B from new draft inputs.
        r.store
            .retire_node(o, a, p, &pitfall.memory, now_ms().unwrap())
            .await
            .unwrap();
        let n = add(&r, "strategy", Some(&n), 3, 1, "strategy").await;
        let mut updated = answer();
        updated["pitfalls"] = json!([]);
        f.reply(200, updated);
        tick(&r).await.result.unwrap();
        let redrafted = row(&r, &n).await;
        assert!(redrafted.pitfall_memories.is_empty());
        assert_eq!(redrafted.install_paths, installed.install_paths);
        assert_eq!(
            redrafted.installed_pitfall_memories,
            installed.installed_pitfall_memories
        );
        if reinstall {
            let approved = decide(&r, &n, SkillStatus::Approved).await;
            assert_eq!(
                approved.installed_pitfall_memories,
                installed.installed_pitfall_memories
            );
            let reinstalled = decide(&r, &n, SkillStatus::Installed).await;
            assert!(reinstalled.installed_pitfall_memories.is_empty());
        }
        let receipt = r
            .store
            .purge(o, p, &[SourceId::new("lesson/pitfall-0/1").unwrap()])
            .await
            .unwrap();
        let c = row(&r, &n).await;
        if reinstall {
            assert!(receipt.installed_skills_with_purged_evidence.is_empty());
            assert_eq!(c.status, SkillStatus::Installed);
            assert!(c.draft.is_some());
            assert!(c.installed_pitfall_memories.is_empty());
        } else {
            assert_eq!(
                c.installed_pitfall_memories,
                installed.installed_pitfall_memories
            );
            assert_eq!(
                receipt.installed_skills_with_purged_evidence,
                installed.install_paths
            );
            assert_eq!(c.status, SkillStatus::Stale);
            assert!(c.draft.is_none());
        }
        assert_eq!(c.install_paths, installed.install_paths);
        assert_eq!(c.install_hashes, installed.install_hashes);
    }
}

#[tokio::test]
async fn purge_of_pitfall_evidence_invalidates_drafts_and_preserves_installs() {
    for installed in [false, true] {
        let f = Fake::start();
        let r = setup(&f).await;
        let n = add(&r, "strategy", None, 0, 3, "strategy").await;
        let pitfall = add(&r, "pitfall", None, 0, 1, "pitfall").await;
        f.reply(200, answer());
        tick(&r).await.result.unwrap();
        let c = row(&r, &n).await;
        assert_eq!(c.pitfall_memories, vec![pitfall.memory]);
        assert_eq!(c.pitfall_revisions, vec![pitfall.revision]);
        assert_eq!(c.pitfall_source_counts, vec![1]);
        if installed {
            decide(&r, &n, SkillStatus::Approved).await;
            decide(&r, &n, SkillStatus::Installed).await;
        }
        let receipt = r
            .store
            .purge(
                &r.scope.owner,
                &r.scope.projects[0],
                &[SourceId::new("lesson/pitfall-0/1").unwrap()],
            )
            .await
            .unwrap();
        if installed {
            assert_eq!(
                receipt.installed_skills_with_purged_evidence,
                vec!["/skills/test-storage/SKILL.md"]
            );
            let c = row(&r, &n).await;
            assert_eq!(c.status, SkillStatus::Stale);
            assert!(c.draft.is_none());
            assert_eq!(c.install_paths, vec!["/skills/test-storage/SKILL.md"]);
            assert_eq!(c.install_hashes, vec!["a".repeat(64)]);
        } else {
            assert!(receipt.installed_skills_with_purged_evidence.is_empty());
            assert!(
                r.skill_candidates(&r.scope.projects[0], None)
                    .await
                    .unwrap()
                    .is_empty()
            );
        }
        assert!(
            r.store
                .tree_nodes(&r.scope.owner, &r.scope.projects[0])
                .await
                .unwrap()
                .iter()
                .any(|node| node.memory == n.memory && node.revision == n.revision)
        );
    }
}

#[tokio::test]
async fn newer_non_lesson_sources_do_not_hide_lesson_admission_events() {
    use crate::domain_models::memory_tree::{CurationCommit, LinkOperation};
    let f = Fake::start();
    let r = setup_project(&f, "user:owner").await;
    let n = add(&r, "strategy", None, 0, 3, "strategy").await;
    let (o, a, p) = (&r.scope.owner, &r.scope.actor, &r.scope.projects[0]);
    let mut sources = Vec::new();
    for i in 0..32 {
        let source = SourceId::new(format!("newer-decision-{i}")).unwrap();
        r.store
            .capture(
                CaptureEvent {
                    owner: o.clone(),
                    actor: a.clone(),
                    project: p.clone(),
                    source: source.clone(),
                    recorded_at_ms: 1000 + i,
                    kind: WorkEventKind::Decision,
                    outcome: WorkOutcome::Unknown,
                    origin: "fixture://decision".into(),
                    content: "Keep the established testing procedure.".into(),
                },
                &r.limits,
            )
            .await
            .unwrap();
        sources.push(source);
    }
    let item = r
        .store
        .commit_curation(
            o,
            a,
            p,
            CurationCommit {
                links: vec![],
                operation_id: "mixed-general-lesson".into(),
                sources: sources.clone(),
                promotes: vec![],
                creates: vec![],
                supersedes: vec![],
                merges: vec![(
                    n.memory.clone(),
                    n.revision.clone(),
                    n.title.clone(),
                    n.content.clone(),
                    sources,
                )],
                parked: vec![],
                stale: vec![],
                split: vec![],
                operation: LinkOperation::Merge,
            },
            2000,
        )
        .await
        .unwrap()
        .pop()
        .unwrap();
    r.store
        .review(
            o,
            a,
            &ReviewRequest {
                project: p.clone(),
                revision: item.revision.id,
                action: ReviewAction::Promote,
                expected_active: item.revision.predecessor,
                conflict_with: None,
            },
            2001,
        )
        .await
        .unwrap();
    assert_eq!(
        r.store
            .tree_recent_sources(o, p, &n.memory)
            .await
            .unwrap()
            .len(),
        32
    );
    let events = r
        .store
        .skill_recent_lesson_sources(o, p, &n.memory)
        .await
        .unwrap();
    assert_eq!(events.len(), 3);
    assert!(
        events
            .iter()
            .all(|event| event.kind == WorkEventKind::Lesson)
    );
    assert_eq!(
        events
            .iter()
            .map(|event| event.source.as_str())
            .collect::<Vec<_>>(),
        vec![
            "lesson/strategy-2/1",
            "lesson/strategy-1/1",
            "lesson/strategy-0/1"
        ]
    );
    f.reply(200, answer());
    tick(&r).await.result.unwrap();
    assert_eq!(f.calls(), 1);
    assert_eq!(row(&r, &n).await.sessions_at_draft, 3);
}

#[tokio::test]
async fn brief_counts_pending_drafts() {
    let f = Fake::start();
    f.reply(200, answer());
    let r = setup(&f).await;
    let p = &r.scope.projects[0];
    assert_eq!(r.brief(p).await.unwrap().skill_drafts_pending, 0);
    let n = add(&r, "brief", None, 0, 3, "strategy").await;
    tick(&r).await.result.unwrap();
    assert_eq!(r.brief(p).await.unwrap().skill_drafts_pending, 1);
    decide(&r, &n, SkillStatus::Approved).await;
    assert_eq!(r.brief(p).await.unwrap().skill_drafts_pending, 0);
}
#[tokio::test]
async fn invalid_draft_records_failure_and_waits_for_new_revision() {
    let f = Fake::start();
    let mut a = answer();
    a["description"] = json!("Use when testing\n---\ninjected: true");
    f.reply(200, a);
    let r = setup(&f).await;
    let n = add(&r, "invalid", None, 0, 3, "strategy").await;
    tick(&r).await.result.unwrap();
    let (o, p) = (&r.scope.owner, &r.scope.projects[0]);
    assert!(
        r.store
            .skill_failed(o, p, &n.memory, &n.revision)
            .await
            .unwrap()
    );
    assert!(r.skill_candidates(p, None).await.unwrap().is_empty());
    tick(&r).await.result.unwrap();
    assert_eq!(f.calls(), 1);
    let n = add(&r, "invalid", Some(&n), 3, 1, "strategy").await;
    f.reply(200, answer());
    tick(&r).await.result.unwrap();
    assert_eq!(row(&r, &n).await.status, SkillStatus::Drafted);
    assert_eq!(f.calls(), 2);
}
#[tokio::test]
async fn uncertain_draft_admission_does_not_block_other_nodes() {
    let f = Fake::start();
    let r = setup(&f).await;
    let a = add(&r, "uncertain", None, 0, 3, "strategy").await;
    let b = add(&r, "other", None, 0, 3, "strategy").await;
    let (o, p) = (&r.scope.owner, &r.scope.projects[0]);
    let op = format!(
        "skill-draft-job:{}",
        stable_id(&[
            skill_policy::PROMPT_VERSION,
            o.as_str(),
            p.as_str(),
            a.revision.as_str(),
            "0"
        ])
    );
    let events = r.store.tree_recent_sources(o, p, &a.memory).await.unwrap();
    r.store
        .admit_skill_operation(o, p, &op, &events, &a.revision)
        .await
        .unwrap();
    f.reply(200, answer());
    tick(&r).await.result.unwrap();
    assert!(
        r.store
            .skill_failed(o, p, &a.memory, &a.revision)
            .await
            .unwrap()
    );
    assert_eq!(row(&r, &b).await.status, SkillStatus::Drafted);
    assert_eq!(f.calls(), 1);
    assert!(!r.background_stopped().unwrap());
    assert!(
        r.activity(ActivityRequest {
            project: p.clone(),
            limit: 32,
            operations: vec![],
            outcomes: vec![],
            cursor: None
        })
        .await
        .unwrap()
        .entries
        .iter()
        .any(|e| e.reason == Some(ActivityReason::UncertainAdmission))
    );
}
#[tokio::test]
async fn rejected_candidate_needs_two_more_sessions() {
    let f = Fake::start();
    f.reply(200, answer());
    let r = setup(&f).await;
    let n = add(&r, "reject", None, 0, 3, "strategy").await;
    tick(&r).await.result.unwrap();
    let c = decide(&r, &n, SkillStatus::Rejected).await;
    assert_eq!(c.sessions_at_decision, Some(3));
    assert!(c.decided_at_ms.is_some());
    let n = add(&r, "reject", Some(&n), 3, 1, "strategy").await;
    tick(&r).await.result.unwrap();
    assert_eq!(f.calls(), 1);
    assert_eq!(row(&r, &n).await.status, SkillStatus::Rejected);
    let n = add(&r, "reject", Some(&n), 4, 1, "strategy").await;
    f.reply(200, answer());
    tick(&r).await.result.unwrap();
    assert_eq!(row(&r, &n).await.status, SkillStatus::Drafted);
    assert_eq!(f.calls(), 2);
}
#[tokio::test]
async fn new_revision_marks_candidate_stale_and_redrafts() {
    let f = Fake::start();
    f.reply(200, answer());
    let r = setup(&f).await;
    let n = add(&r, "stale", None, 0, 3, "strategy").await;
    tick(&r).await.result.unwrap();
    decide(&r, &n, SkillStatus::Approved).await;
    let installed = decide(&r, &n, SkillStatus::Installed).await;
    let n = add(&r, "stale", Some(&n), 3, 1, "strategy").await;
    let work = r.skill_work(&r.scope.projects[0]).await.unwrap();
    assert_eq!(work.len(), 1);
    let stale = row(&r, &n).await;
    assert_eq!(stale.status, SkillStatus::Stale);
    assert_eq!(stale.install_paths, installed.install_paths);
    f.reply(200, answer());
    tick(&r).await.result.unwrap();
    let c = row(&r, &n).await;
    assert_eq!(c.status, SkillStatus::Drafted);
    assert_eq!(c.revision_id, n.revision);
    assert_eq!(c.install_hashes, installed.install_hashes);
    assert_eq!(c.sessions_at_decision, Some(3));
}
#[tokio::test]
async fn user_scope_and_no_provider_or_unreinforced_nodes_do_not_draft() {
    let f = Fake::start();
    let mut r = setup_project(&f, "user:owner").await;
    let n = add(&r, "user", None, 0, 2, "strategy").await;
    tick(&r).await.result.unwrap();
    assert_eq!(f.calls(), 0);
    let n = add(&r, "user", Some(&n), 2, 1, "strategy").await;
    let providers = r.providers.take();
    assert!(r.skill_work(&r.scope.projects[0]).await.unwrap().is_empty());
    r.providers = providers;
    f.reply(200, answer());
    tick(&r).await.result.unwrap();
    assert_eq!(row(&r, &n).await.scope, "user");
    assert!(
        row(&r, &n)
            .await
            .draft
            .unwrap()
            .contains("Lambda Memory user lesson node")
    );
}
#[tokio::test]
async fn purge_deleted_candidate_uses_a_fresh_admission_for_the_same_revision() {
    let f = Fake::start();
    let r = setup(&f).await;
    let n = add(&r, "purge-count", None, 0, 3, "strategy").await;
    let n = add(&r, "purge-count", Some(&n), 3, 1, "strategy").await;
    f.reply(200, answer());
    tick(&r).await.result.unwrap();
    let (o, p) = (&r.scope.owner, &r.scope.projects[0]);
    assert_eq!(
        r.store
            .skill_admission_count(o, p, &n.revision)
            .await
            .unwrap(),
        1
    );
    r.store
        .purge(o, p, &[SourceId::new("lesson/purge-count-0/1").unwrap()])
        .await
        .unwrap();
    assert!(r.skill_candidates(p, None).await.unwrap().is_empty());
    assert_eq!(
        r.store
            .skill_admission_count(o, p, &n.revision)
            .await
            .unwrap(),
        1
    );
    f.reply(200, answer());
    tick(&r).await.result.unwrap();
    assert_eq!(row(&r, &n).await.status, SkillStatus::Drafted);
    assert_eq!(
        r.store
            .skill_admission_count(o, p, &n.revision)
            .await
            .unwrap(),
        2
    );
    assert_eq!(f.calls(), 2);
}

#[tokio::test]
async fn idle_tick_drafts_at_most_one_candidate() {
    let f = Fake::start();
    let r = setup(&f).await;
    add(&r, "first", None, 0, 3, "strategy").await;
    add(&r, "second", None, 0, 3, "strategy").await;
    f.reply(200, answer());
    r.dream_once().await.result.unwrap();
    assert_eq!(f.calls(), 1);
    assert_eq!(
        r.skill_candidates(&r.scope.projects[0], Some(SkillStatus::Drafted))
            .await
            .unwrap()
            .len(),
        1
    );
    f.reply(200, answer());
    r.dream_once().await.result.unwrap();
    assert_eq!(f.calls(), 2);
    assert_eq!(
        r.skill_candidates(&r.scope.projects[0], Some(SkillStatus::Drafted))
            .await
            .unwrap()
            .len(),
        2
    );
}

#[tokio::test]
async fn provider_rejection_releases_admission_and_unavailability_stops_worker() {
    let f = Fake::start();
    let r = setup(&f).await;
    add(&r, "provider", None, 0, 3, "strategy").await;
    f.reply(429, answer());
    assert_eq!(
        r.dream_once().await.result,
        Err(RuntimeError::ProviderRejected)
    );
    assert!(!r.background_stopped().unwrap());
    f.reply(0, answer());
    assert_eq!(
        r.dream_once().await.result,
        Err(RuntimeError::ProviderUnavailable)
    );
    assert!(r.background_stopped().unwrap());
    assert_eq!(f.calls(), 2);
}
