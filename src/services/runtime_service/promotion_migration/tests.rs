use super::*;
use crate::domain_models::memory_tree::*;
use crate::domain_models::{ActorId, OwnerId};
use crate::integrations::database::embedded_capture::EmbeddedCaptureStore;
use crate::integrations::external_apis::router_runtime::RouterRuntimeProviders;
use std::collections::VecDeque;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex as StdMutex, atomic::AtomicBool};

static NEXT: AtomicUsize = AtomicUsize::new(0);
struct Fake {
    base: String,
    replies: Arc<StdMutex<VecDeque<(u16, String)>>>,
    requests: Arc<StdMutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl Fake {
    fn new() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let replies = Arc::new(StdMutex::new(VecDeque::<(u16, String)>::new()));
        let requests = Arc::new(StdMutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let (r, q, s) = (replies.clone(), requests.clone(), stop.clone());
        let thread = std::thread::spawn(move || {
            while !s.load(Ordering::SeqCst) {
                let Ok((mut socket, _)) = listener.accept() else {
                    std::thread::sleep(std::time::Duration::from_millis(2));
                    continue;
                };
                socket
                    .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                    .unwrap();
                let mut data = Vec::new();
                let mut buffer = [0; 4096];
                let mut end = None;
                let mut start = 0;
                while end.is_none_or(|end| data.len() < end) {
                    let n = socket.read(&mut buffer).unwrap();
                    if n == 0 {
                        break;
                    }
                    data.extend_from_slice(&buffer[..n]);
                    if end.is_none()
                        && let Some(i) = data.windows(4).position(|w| w == b"\r\n\r\n")
                    {
                        start = i + 4;
                        let head = String::from_utf8_lossy(&data[..i]).to_lowercase();
                        let len = head
                            .lines()
                            .find_map(|l| {
                                l.strip_prefix("content-length: ")
                                    .and_then(|s| s.parse::<usize>().ok())
                            })
                            .unwrap();
                        end = Some(start + len);
                    }
                }
                q.lock()
                    .unwrap()
                    .push(String::from_utf8(data[start..].to_vec()).unwrap());
                let (status, content) = r
                    .lock()
                    .unwrap()
                    .pop_front()
                    .unwrap_or((500, "unexpected request".into()));
                let body = format!(
                    r#"{{"model":"anthropic/claude-haiku-5.5","usage":{{"prompt_tokens":12,"completion_tokens":3,"cost":0.000001}},"choices":[{{"finish_reason":"stop","message":{{"content":{content:?}}}}}]}}"#
                );
                let _ = write!(
                    socket,
                    "HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
            }
        });
        Self {
            base,
            replies,
            requests,
            stop,
            thread: Some(thread),
        }
    }
    fn reply(&self, status: u16, content: &str) {
        self.replies
            .lock()
            .unwrap()
            .push_back((status, content.into()));
    }
    fn calls(&self) -> usize {
        self.requests.lock().unwrap().len()
    }
}
impl Drop for Fake {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.thread.take().unwrap().join().unwrap();
    }
}
struct Fixture {
    runtime: Option<MemoryRuntime>,
    path: String,
}
impl std::ops::Deref for Fixture {
    type Target = MemoryRuntime;
    fn deref(&self) -> &MemoryRuntime {
        self.runtime.as_ref().unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        drop(self.runtime.take());
        let _ = std::fs::remove_dir_all(&self.path);
    }
}
async fn setup(fake: &Fake) -> Fixture {
    let path = format!(
        "{}/.cache/lambda-memory/promotion-runtime-{}-{}",
        std::env::var("HOME").unwrap(),
        std::process::id(),
        NEXT.fetch_add(1, Ordering::SeqCst)
    );
    let store = EmbeddedRuntimeStore::new(EmbeddedCaptureStore::open(&path).await.unwrap());
    let runtime = MemoryRuntime::with_openrouter(
        store,
        RuntimeScope {
            owner: OwnerId::new("owner").unwrap(),
            actor: ActorId::new("actor").unwrap(),
            projects: vec![ProjectId::new("project").unwrap()],
        },
        RuntimeLimits::default(),
        RouterRuntimeProviders::mock(&fake.base, 128),
    )
    .unwrap();
    Fixture {
        runtime: Some(runtime),
        path,
    }
}
async fn node(runtime: &MemoryRuntime, id: &str, root: TreeRoot, sources: &[&str]) -> TreeNode {
    let project = &runtime.scope.projects[0];
    for source in sources {
        runtime
            .store
            .capture(
                CaptureEvent {
                    source: SourceId::new(*source).unwrap(),
                    owner: runtime.scope.owner.clone(),
                    actor: runtime.scope.actor.clone(),
                    project: project.clone(),
                    origin: "fixture://test".into(),
                    recorded_at_ms: 100,
                    kind: WorkEventKind::Decision,
                    outcome: WorkOutcome::Unknown,
                    content: "Delegate only coding tasks.".into(),
                },
                &runtime.limits,
            )
            .await
            .unwrap();
    }
    let sources: Vec<_> = sources.iter().map(|s| SourceId::new(*s).unwrap()).collect();
    let items = runtime
        .store
        .commit_curation(
            &runtime.scope.owner,
            &runtime.scope.actor,
            project,
            CurationCommit {
                links: vec![],
                operation_id: id.into(),
                supersedes: vec![],
                sources: sources.clone(),
                promotes: vec![],
                merges: vec![],
                creates: vec![(
                    ParentRef::Root(root),
                    id.into(),
                    "Delegate only coding tasks.".into(),
                    sources,
                )],
                parked: vec![],
                stale: vec![],
                split: vec![],
                operation: LinkOperation::Migrate,
            },
            1000,
        )
        .await
        .unwrap();
    publish(runtime, &items).await;
    runtime
        .store
        .tree_nodes(&runtime.scope.owner, project)
        .await
        .unwrap()
        .into_iter()
        .find(|n| n.revision == items[0].revision.id)
        .unwrap()
}
async fn publish(runtime: &MemoryRuntime, items: &[RuntimeRevision]) {
    for item in items {
        runtime
            .store
            .review(
                &runtime.scope.owner,
                &runtime.scope.actor,
                &ReviewRequest {
                    project: item.revision.project.clone(),
                    revision: item.revision.id.clone(),
                    action: ReviewAction::Promote,
                    expected_active: item.revision.predecessor.clone(),
                    conflict_with: None,
                },
                1001,
            )
            .await
            .unwrap();
    }
}
#[tokio::test]
async fn promote_existing_requires_exact_consent() {
    let fake = Fake::new();
    let r = setup(&fake).await;
    let p = &r.scope.projects[0];
    node(&r, "node", TreeRoot::Preferences, &["s"]).await;
    let plan = r.inspect_promotions(p).await.unwrap();
    assert!(matches!(
        r.promote_existing(p, &plan, false).await,
        Err(RuntimeError::InvalidInput(
            "exact promotion consent required"
        ))
    ));
    let mut changed = plan.clone();
    changed.linked_sources += 1;
    assert!(matches!(
        r.promote_existing(p, &changed, true).await,
        Err(RuntimeError::InvalidInput(
            "exact promotion consent required"
        ))
    ));
    assert_eq!(fake.calls(), 0);
    assert!(
        r.store
            .promotion_reviews(&r.scope.owner, p)
            .await
            .unwrap()
            .is_empty()
    );
}
#[tokio::test]
async fn promote_existing_records_reviews_and_forwards() {
    let fake = Fake::new();
    let r = setup(&fake).await;
    let p = &r.scope.projects[0];
    let a = node(&r, "a", TreeRoot::Preferences, &["s1", "s2"]).await;
    let b = node(&r, "b", TreeRoot::Decisions, &["s4"]).await;
    let c = node(&r, "c", TreeRoot::Decisions, &["s3"]).await;
    r.store
        .commit_curation(
            &r.scope.owner,
            &r.scope.actor,
            p,
            CurationCommit {
                links: vec![],
                operation_id: "already-forwarded".into(),
                supersedes: vec![],
                sources: vec![SourceId::new("s1").unwrap()],
                promotes: vec![SourceId::new("s1").unwrap()],
                merges: vec![],
                creates: vec![],
                parked: vec![],
                stale: vec![],
                split: vec![],
                operation: LinkOperation::Create,
            },
            1500,
        )
        .await
        .unwrap();
    let candidates = r.promotion_candidates(p).await.unwrap();
    let output = candidates
        .iter()
        .enumerate()
        .map(|(i, n)| {
            format!(
                "\"n{i}\":\"{}\"",
                if n.node.memory == c.memory {
                    "project"
                } else {
                    "user"
                }
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    fake.reply(200, &format!("{{\"results\":{{{output}}}}}"));
    let report = r
        .promote_existing(p, &r.inspect_promotions(p).await.unwrap(), true)
        .await
        .unwrap();
    assert_eq!(report.classified, 3);
    assert_eq!(report.forwards, 2);
    assert!(report.user_nodes.contains(&a.memory));
    assert!(report.user_nodes.contains(&b.memory));
    assert_eq!(
        r.store
            .promotion_reviews(&r.scope.owner, p)
            .await
            .unwrap()
            .len(),
        3
    );
    assert_eq!(r.forwards(p, 64).await.unwrap().len(), 3);
    assert_eq!(r.inspect_promotions(p).await.unwrap().nodes.len(), 0);
    r.promote_existing(p, &r.inspect_promotions(p).await.unwrap(), true)
        .await
        .unwrap();
    assert_eq!(fake.calls(), 1);
    let requests = fake.requests.lock().unwrap();
    let request = &requests[0];
    assert!(request.contains("\"name\":\"runtime_promotion_v1\""));
    assert!(request.contains("\"strict\":true"));
    assert!(request.to_string().contains("When unsure, answer project"));
}
#[tokio::test]
async fn promote_existing_invalid_output_commits_nothing() {
    let fake = Fake::new();
    let r = setup(&fake).await;
    let p = &r.scope.projects[0];
    node(&r, "a", TreeRoot::Preferences, &["s1"]).await;
    node(&r, "b", TreeRoot::Decisions, &["s2"]).await;
    let plan = r.inspect_promotions(p).await.unwrap();
    for answer in [
        r#"{"results":{"n0":"user"}}"#,
        r#"{"results":{"n0":"user","n1":"project","n2":"user"}}"#,
        r#"{"results":{"n0":"user","n1":"maybe"}}"#,
        r#"{"results":{"n0":"user","n1":"project"},"extra":true}"#,
        r#"{"results":{"n0":"project","n0":"user","n1":"project"}}"#,
    ] {
        fake.reply(200, answer);
        assert_eq!(
            r.promote_existing(p, &plan, true).await.unwrap_err(),
            RuntimeError::ProviderInvalidOutput
        );
        assert!(
            r.store
                .promotion_reviews(&r.scope.owner, p)
                .await
                .unwrap()
                .is_empty()
        );
        assert!(r.forwards(p, 64).await.unwrap().is_empty());
    }
}
#[tokio::test]
async fn promote_existing_rerun_after_invalid_output_is_a_new_request() {
    let fake = Fake::new();
    let r = setup(&fake).await;
    let p = &r.scope.projects[0];
    node(&r, "a", TreeRoot::Preferences, &["s1"]).await;
    let plan = r.inspect_promotions(p).await.unwrap();
    fake.reply(200, r#"{"results":{}}"#);
    assert_eq!(
        r.promote_existing(p, &plan, true).await.unwrap_err(),
        RuntimeError::ProviderInvalidOutput
    );
    fake.reply(200, r#"{"results":{"n0":"user"}}"#);
    assert_eq!(
        r.promote_existing(p, &plan, true).await.unwrap().classified,
        1
    );
    assert_eq!(
        r.store
            .promotion_admitted_sources(&r.scope.owner, p)
            .await
            .unwrap()
            .len(),
        2
    );
    assert_eq!(fake.calls(), 2);
}
#[tokio::test]
async fn promote_existing_admits_the_most_recently_linked_source() {
    let fake = Fake::new();
    let r = setup(&fake).await;
    let p = &r.scope.projects[0];
    let n = node(&r, "a", TreeRoot::Preferences, &["s"]).await;
    let mut event = r
        .store
        .promotion_sources(&r.scope.owner, p, &n.memory)
        .await
        .unwrap()[0]
        .clone();
    event.source = SourceId::new("late-link").unwrap();
    event.recorded_at_ms = 99; // Older evidence, linked to the node more recently.
    r.store.capture(event.clone(), &r.limits).await.unwrap();
    let items = r
        .store
        .commit_curation(
            &r.scope.owner,
            &r.scope.actor,
            p,
            CurationCommit {
                links: vec![],
                operation_id: "late-link".into(),
                supersedes: vec![],
                sources: vec![event.source.clone()],
                promotes: vec![],
                merges: vec![(
                    n.memory,
                    n.revision,
                    "Updated".into(),
                    "A general working rule.".into(),
                    vec![event.source.clone()],
                )],
                creates: vec![],
                parked: vec![],
                stale: vec![],
                split: vec![],
                operation: LinkOperation::Create,
            },
            4000,
        )
        .await
        .unwrap();
    publish(&r, &items).await;
    fake.reply(200, r#"{"results":{"n0":"user"}}"#);
    r.promote_existing(p, &r.inspect_promotions(p).await.unwrap(), true)
        .await
        .unwrap();
    assert_eq!(
        r.store
            .promotion_admitted_sources(&r.scope.owner, p)
            .await
            .unwrap(),
        vec![event.source]
    );
}

#[tokio::test]
async fn promote_existing_releases_rejected_admission() {
    let fake = Fake::new();
    let r = setup(&fake).await;
    let p = &r.scope.projects[0];
    node(&r, "a", TreeRoot::Preferences, &["s"]).await;
    let plan = r.inspect_promotions(p).await.unwrap();
    fake.reply(402, "rejected");
    assert_eq!(
        r.promote_existing(p, &plan, true).await.unwrap_err(),
        RuntimeError::ProviderRejected
    );
    assert!(
        r.store
            .promotion_admitted_sources(&r.scope.owner, p)
            .await
            .unwrap()
            .is_empty()
    );
}
#[tokio::test]
async fn promote_existing_without_sources_is_project_without_a_call() {
    let fake = Fake::new();
    let r = setup(&fake).await;
    let p = &r.scope.projects[0];
    let owner = &r.scope.owner;
    let parent = node(&r, "parent", TreeRoot::Preferences, &["s"]).await;
    let source = SourceId::new("s").unwrap();
    let items = r
        .store
        .commit_tree_maintenance(
            owner,
            &r.scope.actor,
            p,
            MaintenanceCommit {
                operation_id: "move".into(),
                operation: LinkOperation::Split,
                rewrites: vec![(
                    parent.memory.clone(),
                    parent.revision.clone(),
                    "Parent".into(),
                    "General preference".into(),
                    vec![source.clone()],
                )],
                creates: vec![(
                    ParentRef::Node(parent.memory.clone()),
                    "Child".into(),
                    "General preference".into(),
                    vec![source.clone()],
                )],
                reparent: vec![],
                expected_reparent: vec![],
                move_sources: vec![(source, parent.memory.clone(), 0)],
                stale: vec![],
                queue_item: None,
            },
            2000,
        )
        .await
        .unwrap();
    publish(&r, &items).await;
    let child = items
        .iter()
        .find(|n| n.revision.memory != parent.memory)
        .unwrap();
    r.store
        .retire_node(owner, &r.scope.actor, p, &child.revision.memory, 3000)
        .await
        .unwrap();
    let plan = r.inspect_promotions(p).await.unwrap();
    assert_eq!(plan.nodes, vec![parent.memory]);
    assert_eq!(plan.linked_sources, 0);
    let report = r.promote_existing(p, &plan, true).await.unwrap();
    assert_eq!(report.classified, 1);
    assert_eq!(report.forwards, 0);
    assert!(report.user_nodes.is_empty());
    assert_eq!(fake.calls(), 0);
    assert_eq!(
        r.store.promotion_reviews(owner, p).await.unwrap()[0].verdict,
        PromotionVerdict::Project
    );
}
