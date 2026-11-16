use crate::domain_models::runtime_diagnostics::{ActivityCounts, ActivityPhase};

#[test]
fn curation_activity_counts_accumulate_and_phases_round_trip() {
    let mut counts = ActivityCounts::default();
    counts.accumulate(&ActivityCounts {
        sources_curated: Some(2),
        sources_discarded: Some(1),
        nodes_created: Some(1),
        nodes_merged: Some(2),
        sources_parked: Some(1),
        ..Default::default()
    });
    counts.accumulate(&counts.clone());
    assert_eq!(
        (
            counts.sources_curated,
            counts.sources_discarded,
            counts.nodes_created,
            counts.nodes_merged,
            counts.sources_parked
        ),
        (Some(4), Some(2), Some(2), Some(4), Some(2))
    );
    for phase in [
        ActivityPhase::Curation,
        ActivityPhase::TreeRefresh,
        ActivityPhase::TreeSplit,
        ActivityPhase::TreeRebuild,
    ] {
        assert_eq!(ActivityPhase::parse(phase.as_str()), Some(phase));
    }
}

#[cfg(feature = "router-http")]
mod integration {
    mod reflection {
        include!("../../../../tests/support/reflection_runtime.rs");
    }
    mod lessons {
        include!("../../../../tests/support/lesson_curation.rs");
    }
    mod links {
        include!("../../../../tests/support/tree_links_runtime.rs");
    }
    mod validity {
        include!("../../../../tests/support/tree_validity.rs");
    }
    use super::super::super::*;
    use crate::domain_models::memory_tree::{
        CurationCommit, LinkOperation, MaintenanceCommit, ParentRef, TreeRoot,
    };
    use crate::domain_models::{ActorId, OwnerId};
    use crate::integrations::database::embedded_capture::EmbeddedCaptureStore;
    use crate::integrations::external_apis::router_runtime::RouterRuntimeProviders;
    use std::{
        collections::VecDeque,
        io::{Read, Write},
        net::TcpListener,
        sync::{
            Arc, Mutex,
            atomic::{AtomicUsize, Ordering},
        },
    };

    static NEXT: AtomicUsize = AtomicUsize::new(0);
    #[derive(Clone)]
    struct Fake {
        base: String,
        replies: Arc<Mutex<VecDeque<(u16, String)>>>,
        embeddings: Arc<Mutex<Vec<String>>>,
        chats: Arc<AtomicUsize>,
        chat_bodies: Arc<Mutex<Vec<String>>>,
    }
    impl Fake {
        fn start() -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let fake = Self {
                base: format!("http://{}", listener.local_addr().unwrap()),
                replies: Arc::default(),
                embeddings: Arc::default(),
                chats: Arc::default(),
                chat_bodies: Arc::default(),
            };
            let shared = fake.clone();
            std::thread::spawn(move || {
                for socket in listener.incoming() {
                    let Ok(mut socket) = socket else { break };
                    let fake = shared.clone();
                    std::thread::spawn(move || {
                        let mut bytes = vec![];
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
                                            .and_then(|v| v.parse::<usize>().ok())
                                    })
                                    .unwrap_or(0);
                                end = Some(i + 4 + len);
                            }
                        }
                        let text = String::from_utf8_lossy(&bytes);
                        let path = text.split_whitespace().nth(1).unwrap_or("");
                        let usage = r#"{"prompt_tokens":12,"completion_tokens":3,"cost":0.000001}"#;
                        let (status, body) = if path == "/api/v1/embeddings" {
                            fake.embeddings.lock().unwrap().push(text.to_string());
                            (
                                200,
                                format!(
                                    r#"{{"model":"google/gemini-embedding-2","usage":{usage},"data":[{{"index":0,"embedding":[{}]}}]}}"#,
                                    vec!["1.0"; 128].join(",")
                                ),
                            )
                        } else {
                            fake.chat_bodies
                                .lock()
                                .unwrap()
                                .push(text.split_once("\r\n\r\n").unwrap().1.to_owned());
                            fake.chats.fetch_add(1, Ordering::SeqCst);
                            let (status, content) =
                                fake.replies.lock().unwrap().pop_front().unwrap();
                            if status == 0 {
                                std::thread::sleep(std::time::Duration::from_millis(20));
                                return;
                            }
                            let content = content
                                .replace('\\', "\\\\")
                                .replace('"', "\\\"")
                                .replace('\n', "\\n");
                            (
                                status,
                                format!(
                                    r#"{{"model":"anthropic/claude-haiku-5.5","usage":{usage},"choices":[{{"finish_reason":"stop","message":{{"content":"{content}"}}}}]}}"#
                                ),
                            )
                        };
                        let _ = write!(
                            socket,
                            "HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                            body.len()
                        );
                    });
                }
            });
            fake
        }
        fn reply(&self, status: u16, text: &str) {
            self.replies
                .lock()
                .unwrap()
                .push_back((status, text.into()));
        }
    }
    async fn setup(fake: &Fake) -> MemoryRuntime {
        let path = format!(
            "/tmp/lambda-curation-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        );
        let store = EmbeddedRuntimeStore::new(EmbeddedCaptureStore::open(&path).await.unwrap());
        MemoryRuntime::with_openrouter(
            store,
            RuntimeScope {
                owner: OwnerId::new("owner").unwrap(),
                actor: ActorId::new("actor").unwrap(),
                projects: vec![ProjectId::new("project").unwrap()],
            },
            RuntimeLimits {
                idle_ms: 1,
                ..Default::default()
            },
            RouterRuntimeProviders::mock(&fake.base, 128),
        )
        .unwrap()
    }
    async fn capture(runtime: &MemoryRuntime, id: &str, kind: WorkEventKind) {
        runtime
            .capture(
                &runtime.scope.projects[0],
                SourceId::new(id).unwrap(),
                kind,
                WorkOutcome::Unknown,
                "fixture://test".into(),
                "Use SQLite for durable storage.".into(),
            )
            .await
            .result
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(3)).await;
    }
    async fn curate_user_answer(answer: &str) -> (MemoryRuntime, ProjectId) {
        let fake = Fake::start();
        fake.reply(200, answer);
        let mut runtime = setup(&fake).await;
        // The user store is deliberately not the first authorized project.
        let project = ProjectId::new("user:nicolas").unwrap();
        runtime.scope.projects.push(project.clone());
        runtime
            .capture(
                &project,
                SourceId::new("user-source").unwrap(),
                WorkEventKind::Decision,
                WorkOutcome::Unknown,
                "fixture://user".into(),
                "Delegate only coding tasks.".into(),
            )
            .await
            .result
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(3)).await;
        runtime.dream_once().await.result.unwrap();
        (runtime, project)
    }

    #[tokio::test]
    async fn user_store_rejects_project_root() {
        let (runtime, project) = curate_user_answer(r#"{"results":{"s0":[
            {"op":"create","node":null,"parent":"decisions","title":"Project fact","content":"Use SQLite."},
            {"op":"create","node":null,"parent":"working_rules","title":"Delegation","content":"Delegate only coding tasks."}
        ]}}"#).await;
        let nodes = runtime
            .store
            .tree_nodes(&runtime.scope.owner, &project)
            .await
            .unwrap();
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].root, TreeRoot::WorkingRules);
        assert_eq!(nodes[0].title, "Delegation");
        assert!(
            runtime
                .store
                .tree_nodes(&runtime.scope.owner, &runtime.scope.projects[0])
                .await
                .unwrap()
                .is_empty()
        );
        let activity = runtime
            .activity(crate::domain_models::runtime_diagnostics::ActivityRequest {
                project,
                limit: 8,
                operations: vec![],
                outcomes: vec![],
                cursor: None,
            })
            .await
            .unwrap();
        assert!(
            activity
                .entries
                .iter()
                .any(|entry| entry.counts.operations_dropped == Some(1))
        );
    }

    #[tokio::test]
    async fn user_store_create_under_working_rules() {
        let (runtime, project) = curate_user_answer(r#"{"results":{"s0":[
            {"op":"create","node":null,"parent":"working_rules","title":"Delegation","content":"Delegate only coding tasks."}
        ]}}"#).await;
        let nodes = runtime
            .store
            .tree_nodes(&runtime.scope.owner, &project)
            .await
            .unwrap();
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].root, TreeRoot::WorkingRules);
        assert_eq!(nodes[0].depth, 1);
        assert_eq!(nodes[0].parent, Some(TreeRoot::WorkingRules.memory_id()));
        let view = runtime
            .node(&project, &nodes[0].memory, false, false, None)
            .await
            .unwrap();
        assert_eq!(view.path, vec!["Working rules", "Delegation"]);
        let roots = runtime
            .store
            .tree_root_children(&runtime.scope.owner, &project)
            .await
            .unwrap();
        assert_eq!(
            roots,
            vec![
                (TreeRoot::Preferences, 0),
                (TreeRoot::WorkingRules, 1),
                (TreeRoot::GeneralLessons, 0)
            ]
        );
    }

    const PROMOTE: &str = r#"{"results":{"s0":[{"op":"promote","node":null,"parent":null,"title":null,"content":null}]}}"#;

    async fn promoted_runtime() -> MemoryRuntime {
        let fake = Fake::start();
        fake.reply(200, PROMOTE);
        let runtime = setup(&fake).await;
        capture(&runtime, "promoted", WorkEventKind::Decision).await;
        runtime.dream_once().await.result.unwrap();
        runtime
    }

    #[tokio::test]
    async fn promote_only_source_is_consumed_without_node() {
        let runtime = promoted_runtime().await;
        let (owner, project) = (&runtime.scope.owner, &runtime.scope.projects[0]);
        assert!(
            runtime
                .store
                .tree_nodes(owner, project)
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            runtime
                .store
                .eligible_sources(owner, project, 32, true, None, None, true)
                .await
                .unwrap()
                .is_empty()
        );
        let forwards = runtime.forwards(project, 32).await.unwrap();
        assert_eq!(forwards.len(), 1);
        assert_eq!(forwards[0].forward_id, "capture:promoted");
        assert_eq!(forwards[0].action, "capture");
        assert_eq!(
            forwards[0].source.as_ref().unwrap().source.as_str(),
            "promoted"
        );
    }

    #[tokio::test]
    async fn promote_with_create_links_and_forwards() {
        let fake = Fake::start();
        fake.reply(200, r#"{"results":{"s0":[{"op":"promote","node":null,"parent":null,"title":null,"content":null},{"op":"create","node":null,"parent":"decisions","title":"Database","content":"Use SQLite."}]}}"#);
        let runtime = setup(&fake).await;
        capture(&runtime, "mixed", WorkEventKind::Decision).await;
        runtime.dream_once().await.result.unwrap();
        let (owner, project) = (&runtime.scope.owner, &runtime.scope.projects[0]);
        let nodes = runtime.store.tree_nodes(owner, project).await.unwrap();
        assert_eq!(nodes.len(), 1);
        let (linked, _) = runtime
            .store
            .node_sources(owner, project, &nodes[0].memory, None, 32)
            .await
            .unwrap();
        assert_eq!(linked.len(), 1);
        assert_eq!(linked[0].0.source.as_str(), "mixed");
        assert_eq!(
            runtime.forwards(project, 32).await.unwrap()[0].forward_id,
            "capture:mixed"
        );
        assert_eq!(
            runtime
                .store
                .node_history(owner, project, &nodes[0].memory, 32)
                .await
                .unwrap()[0]
                .revision
                .prompt_version,
            "tree-curation-v6"
        );
    }

    async fn assert_invalid_promotion(user: bool, answer: &str) {
        let fake = Fake::start();
        fake.reply(200, answer);
        let mut runtime = setup(&fake).await;
        if user {
            runtime.scope.projects[0] = ProjectId::new("user:nicolas").unwrap();
        }
        capture(&runtime, "invalid", WorkEventKind::Decision).await;
        assert!(matches!(
            runtime.dream_once().await.result,
            Err(RuntimeError::ProviderInvalidOutput)
        ));
        let (owner, project) = (&runtime.scope.owner, &runtime.scope.projects[0]);
        assert!(
            runtime
                .store
                .tree_nodes(owner, project)
                .await
                .unwrap()
                .is_empty()
        );
        assert!(runtime.forwards(project, 32).await.unwrap().is_empty());
        assert_eq!(
            runtime
                .store
                .eligible_sources(owner, project, 32, true, None, None, true)
                .await
                .unwrap()
                .len(),
            1
        );
        let activity = runtime
            .activity(crate::domain_models::runtime_diagnostics::ActivityRequest {
                project: project.clone(),
                limit: 32,
                operations: vec![],
                outcomes: vec![],
                cursor: None,
            })
            .await
            .unwrap();
        assert!(activity.entries.iter().any(|e| e.reason
            == Some(
                crate::domain_models::runtime_diagnostics::ActivityReason::TreeInvalidPromotion
            )));
    }

    #[tokio::test]
    async fn promote_in_user_store_is_invalid() {
        // A create before the forbidden promotion must not be committed either.
        assert_invalid_promotion(true, r#"{"results":{"s0":[{"op":"create","node":null,"parent":"preferences","title":"Preference","content":"No emojis."},{"op":"promote","node":null,"parent":null,"title":null,"content":null}]}}"#).await;
    }

    #[tokio::test]
    async fn double_promote_is_invalid() {
        assert_invalid_promotion(false, r#"{"results":{"s0":[{"op":"promote","node":null,"parent":null,"title":null,"content":null},{"op":"promote","node":null,"parent":null,"title":null,"content":null}]}}"#).await;
    }

    #[tokio::test]
    async fn purge_after_done_enqueues_purge_forward() {
        for retention in [false, true] {
            let runtime = promoted_runtime().await;
            let project = &runtime.scope.projects[0];
            assert_eq!(
                runtime
                    .forward_done(project, &["capture:promoted".into()], &[])
                    .await
                    .unwrap(),
                (1, 0)
            );
            if retention {
                let receipt = runtime
                    .store
                    .retain(&runtime.scope.owner, project, now_ms().unwrap() + 1, 32)
                    .await
                    .unwrap();
                assert_eq!(receipt.sources, 1);
            } else {
                assert_eq!(
                    runtime
                        .purge(project, &[SourceId::new("promoted").unwrap()])
                        .await
                        .result
                        .unwrap()
                        .sources,
                    1
                );
            }
            let forwards = runtime.forwards(project, 32).await.unwrap();
            assert_eq!(forwards.len(), 1);
            assert_eq!(forwards[0].action, "purge");
            assert_eq!(forwards[0].source_id.as_str(), "promoted");
            assert!(forwards[0].forward_id.starts_with("purge:promoted:"));
            assert!(forwards[0].source.is_none());
        }
    }

    #[tokio::test]
    async fn purge_cancels_pending_forward_and_enqueues_purge() {
        let runtime = promoted_runtime().await;
        let project = &runtime.scope.projects[0];
        runtime
            .purge(project, &[SourceId::new("promoted").unwrap()])
            .await
            .result
            .unwrap();
        let forwards = runtime.forwards(project, 32).await.unwrap();
        assert_eq!(forwards.len(), 1);
        assert_eq!(forwards[0].action, "purge");
        assert_eq!(forwards[0].source_id.as_str(), "promoted");
        assert_eq!(
            runtime
                .forward_done(project, &["capture:promoted".into()], &[])
                .await
                .unwrap(),
            (0, 0)
        );
    }

    #[tokio::test]
    async fn purge_clears_terminal_capture_forwards() {
        for failed in [false, true] {
            let fake = Fake::start();
            let runtime = setup(&fake).await;
            for cycle in 0..2 {
                fake.reply(200, PROMOTE);
                capture(&runtime, "same", WorkEventKind::Decision).await;
                runtime.dream_once().await.result.unwrap();
                let project = &runtime.scope.projects[0];
                assert!(
                    runtime
                        .forwards(project, 32)
                        .await
                        .unwrap()
                        .iter()
                        .any(|f| if cycle == 0 {
                            f.forward_id == "capture:same"
                        } else {
                            f.forward_id.starts_with("recapture:")
                                && f.forward_id.ends_with(":same")
                        })
                );
                if cycle == 0 {
                    let ids = vec!["capture:same".into()];
                    let (done, failures) = if failed {
                        (&[][..], ids.as_slice())
                    } else {
                        (ids.as_slice(), &[][..])
                    };
                    runtime.forward_done(project, done, failures).await.unwrap();
                    runtime
                        .purge(project, &[SourceId::new("same").unwrap()])
                        .await
                        .result
                        .unwrap();
                }
            }
        }
    }

    #[tokio::test]
    async fn forward_done_marks_failed_as_terminal() {
        let runtime = promoted_runtime().await;
        let project = &runtime.scope.projects[0];
        assert_eq!(
            runtime
                .forward_done(project, &[], &["capture:promoted".into()])
                .await
                .unwrap(),
            (0, 1)
        );
        assert!(runtime.forwards(project, 32).await.unwrap().is_empty());
        assert_eq!(
            runtime
                .forward_done(project, &["capture:promoted".into()], &[])
                .await
                .unwrap(),
            (0, 0)
        );
        assert_eq!(runtime.status(project).await.unwrap().forwards_failed, 1);
    }

    const CREATE: &str = r#"{"results":{"s0":[{"op":"create","node":null,"parent":"decisions","title":"Database","content":"Use SQLite."}]}}"#;
    const DISCARD: &str = r#"{"results":{"s0":[{"op":"discard","node":null,"parent":null,"title":null,"content":null}]}}"#;
    #[tokio::test]
    async fn exchange_source_reaches_curation() {
        let fake = Fake::start();
        fake.reply(200, CREATE);
        let runtime = setup(&fake).await;
        runtime
            .capture(
                &runtime.scope.projects[0],
                SourceId::new("exchange").unwrap(),
                WorkEventKind::Exchange,
                WorkOutcome::Unknown,
                "claude-code://exchange".into(),
                r#"{"v":1,"prompt":"Which database?","reply":"Use SQLite."}"#.into(),
            )
            .await
            .result
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(3)).await;
        runtime.dream_once().await.result.unwrap();
        let nodes = runtime
            .store
            .tree_nodes(&runtime.scope.owner, &runtime.scope.projects[0])
            .await
            .unwrap();
        assert_eq!(nodes.len(), 1);
    }
    #[tokio::test]
    async fn discard_consumes_without_embedding() {
        let fake = Fake::start();
        fake.reply(200, DISCARD);
        let runtime = setup(&fake).await;
        capture(&runtime, "discard", WorkEventKind::Decision).await;
        runtime.dream_once().await.result.unwrap();
        assert_eq!(fake.embeddings.lock().unwrap().len(), 0);
        assert!(
            runtime
                .store
                .eligible_sources(
                    &runtime.scope.owner,
                    &runtime.scope.projects[0],
                    4,
                    true,
                    None,
                    None,
                    true
                )
                .await
                .unwrap()
                .is_empty()
        );
    }
    #[tokio::test]
    async fn decision_source_becomes_a_node_and_is_recallable_text() {
        let fake = Fake::start();
        fake.reply(200, CREATE);
        let runtime = setup(&fake).await;
        capture(&runtime, "decision", WorkEventKind::Decision).await;
        runtime.dream_once().await.result.unwrap();
        let nodes = runtime
            .store
            .tree_nodes(&runtime.scope.owner, &runtime.scope.projects[0])
            .await
            .unwrap();
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].title, "Database");
        let embeddings = fake.embeddings.lock().unwrap();
        assert_eq!(embeddings.len(), 1);
        assert!(embeddings[0].contains("Database\\nUse SQLite."));
    }
    #[tokio::test]
    async fn provider_rejected_releases_and_keeps_sources_pending() {
        let fake = Fake::start();
        fake.reply(402, "{}");
        let runtime = setup(&fake).await;
        capture(&runtime, "rejected", WorkEventKind::Decision).await;
        assert!(matches!(
            runtime.dream_once().await.result,
            Err(RuntimeError::ProviderRejected)
        ));
        let sources = runtime
            .store
            .eligible_sources(
                &runtime.scope.owner,
                &runtime.scope.projects[0],
                4,
                true,
                None,
                None,
                true,
            )
            .await
            .unwrap();
        assert_eq!(sources.len(), 1);
        assert!(
            runtime
                .store
                .tree_admitted_sources(&runtime.scope.owner, &runtime.scope.projects[0])
                .await
                .unwrap()
                .is_empty()
        );
    }
    #[tokio::test]
    async fn action_sources_never_reach_curation() {
        let fake = Fake::start();
        let runtime = setup(&fake).await;
        capture(&runtime, "action", WorkEventKind::Action).await;
        runtime.dream_once().await.result.unwrap();
        assert!(fake.embeddings.lock().unwrap().is_empty());
    }
    #[tokio::test]
    async fn two_merges_into_one_node_make_one_revision_with_both_sources() {
        let fake = Fake::start();
        fake.reply(200, CREATE);
        let runtime = setup(&fake).await;
        capture(&runtime, "first", WorkEventKind::Decision).await;
        runtime.dream_once().await.result.unwrap();
        fake.reply(200, r#"{"results":{"s0":[{"op":"merge","node":"n0","parent":null,"title":"Database","content":"Use SQLite."}],"s1":[{"op":"merge","node":"n0","parent":null,"title":"Database","content":"Use SQLite with WAL."}]}}"#);
        capture(&runtime, "second", WorkEventKind::Decision).await;
        capture(&runtime, "third", WorkEventKind::Decision).await;
        runtime.dream_once().await.result.unwrap();
        let (owner, project) = (&runtime.scope.owner, &runtime.scope.projects[0]);
        let nodes = runtime.store.tree_nodes(owner, project).await.unwrap();
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].content, "Use SQLite with WAL.");
        assert_eq!(
            runtime
                .store
                .node_history(owner, project, &nodes[0].memory, 32)
                .await
                .unwrap()
                .len(),
            2
        );
        let (linked, _) = runtime
            .store
            .node_sources(owner, project, &nodes[0].memory, None, 32)
            .await
            .unwrap();
        let mut ids: Vec<_> = linked.iter().map(|(e, ..)| e.source.to_string()).collect();
        ids.sort();
        assert_eq!(ids, vec!["first", "second", "third"]);
    }
    #[tokio::test]
    async fn undecodable_answer_keeps_the_worker_running() {
        let fake = Fake::start();
        fake.reply(200, r#"{"results":{"s0":[{"op":"merge","node":"n7","parent":null,"title":"T","content":"C"}]}}"#);
        let runtime = setup(&fake).await;
        capture(&runtime, "bad", WorkEventKind::Decision).await;
        let _ = runtime.dream_once().await;
        assert!(!runtime.background_stopped().unwrap());
    }
    #[tokio::test]
    async fn secret_bearing_summary_is_never_stored() {
        let fake = Fake::start();
        fake.reply(200, &format!(r#"{{"results":{{"s0":[{{"op":"create","node":null,"parent":"decisions","title":"Key","content":"Use key sk-ant-{}"}}]}}}}"#, "a".repeat(24)));
        let runtime = setup(&fake).await;
        capture(&runtime, "leaky", WorkEventKind::Decision).await;
        runtime.dream_once().await.result.unwrap();
        assert!(!runtime.background_stopped().unwrap());
        let (owner, project) = (&runtime.scope.owner, &runtime.scope.projects[0]);
        assert!(
            runtime
                .store
                .tree_nodes(owner, project)
                .await
                .unwrap()
                .is_empty()
        );
        let activity = runtime
            .activity(crate::domain_models::runtime_diagnostics::ActivityRequest {
                project: project.clone(),
                limit: 8,
                operations: vec![],
                outcomes: vec![],
                cursor: None,
            })
            .await
            .unwrap();
        assert!(
            activity
                .entries
                .iter()
                .any(|e| e.counts.operations_dropped == Some(1))
        );
    }
    #[tokio::test]
    async fn contradiction_merge_supersedes_with_lineage() {
        let fake = Fake::start();
        fake.reply(200, CREATE);
        let runtime = setup(&fake).await;
        capture(&runtime, "first", WorkEventKind::Decision).await;
        runtime.dream_once().await.result.unwrap();
        fake.reply(200, r#"{"results":{"s0":[{"op":"merge","node":"n0","parent":null,"title":"Database","content":"Use PostgreSQL; earlier SQLite."}]}}"#);
        capture(&runtime, "second", WorkEventKind::Decision).await;
        runtime.dream_once().await.result.unwrap();
        let nodes = runtime
            .store
            .tree_nodes(&runtime.scope.owner, &runtime.scope.projects[0])
            .await
            .unwrap();
        let history = runtime
            .store
            .node_history(
                &runtime.scope.owner,
                &runtime.scope.projects[0],
                &nodes[0].memory,
                32,
            )
            .await
            .unwrap();
        assert_eq!(history.len(), 2);
        assert!(
            history
                .iter()
                .any(|v| v.state == LifecycleState::Superseded)
        );
        assert!(history.iter().any(|v| v.revision.predecessor.is_some()));
    }
    #[tokio::test]
    async fn status_reports_tree_block() {
        let fake = Fake::start();
        let runtime = setup(&fake).await;
        let status = runtime.status(&runtime.scope.projects[0]).await.unwrap();
        assert_eq!(status.stored.tree.mode, "tree");
        assert_eq!(status.stored.tree.nodes_active, 0);
        assert_eq!(status.stored.tree.parked_sources, 0);
    }
    #[tokio::test]
    async fn uncertain_admission_is_not_replayed() {
        let fake = Fake::start();
        fake.reply(0, "");
        let runtime = setup(&fake).await;
        capture(&runtime, "uncertain", WorkEventKind::Decision).await;
        assert!(matches!(
            runtime.dream_once().await.result,
            Err(RuntimeError::ProviderUnavailable)
        ));
        let restarted = MemoryRuntime::with_openrouter(
            runtime.store.clone(),
            runtime.scope.clone(),
            runtime.limits.clone(),
            RouterRuntimeProviders::mock(&fake.base, 128),
        )
        .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(3)).await;
        restarted.dream_once().await.result.unwrap();
        assert_eq!(fake.chats.load(Ordering::SeqCst), 1);
    }
    #[tokio::test]
    async fn flat_mode_project_still_uses_derivation() {
        let fake = Fake::start();
        fake.reply(200, r#"{"claims":{"s0":"Use SQLite for storage."}}"#);
        let runtime = setup(&fake).await;
        capture(&runtime, "baseline", WorkEventKind::Decision).await;
        let baseline = runtime
            .store
            .propose(
                &runtime.scope.owner,
                &runtime.scope.actor,
                RevisionProposal {
                    project: runtime.scope.projects[0].clone(),
                    memory: MemoryId::new("flat-memory").unwrap(),
                    revision: RevisionId::new("flat-revision").unwrap(),
                    sources: vec![SourceId::new("baseline").unwrap()],
                    content: "Prior flat memory".into(),
                    predecessor: None,
                },
                now_ms().unwrap(),
            )
            .await
            .unwrap();
        runtime
            .store
            .review(
                &runtime.scope.owner,
                &runtime.scope.actor,
                &ReviewRequest {
                    project: runtime.scope.projects[0].clone(),
                    revision: baseline.revision.id,
                    action: ReviewAction::Promote,
                    expected_active: None,
                    conflict_with: None,
                },
                now_ms().unwrap(),
            )
            .await
            .unwrap();
        capture(&runtime, "flat", WorkEventKind::Decision).await;
        let pending = runtime
            .store
            .eligible_sources(
                &runtime.scope.owner,
                &runtime.scope.projects[0],
                8,
                true,
                None,
                None,
                true,
            )
            .await
            .unwrap();
        assert_eq!(pending.len(), 1);
        runtime.dream_once().await.result.unwrap();
        assert_eq!(fake.chats.load(Ordering::SeqCst), 1);
        assert_eq!(fake.embeddings.lock().unwrap().len(), 1);
        assert!(
            runtime
                .store
                .tree_nodes(&runtime.scope.owner, &runtime.scope.projects[0])
                .await
                .unwrap()
                .is_empty()
        );
    }
    async fn seed_root(runtime: &MemoryRuntime, count: usize, queue_split: bool) -> MemoryId {
        let mut sources = Vec::new();
        for i in 0..count {
            let id = format!("seed-{i:02}");
            capture(runtime, &id, WorkEventKind::Decision).await;
            sources.push(SourceId::new(id).unwrap());
        }
        let plan = CurationCommit {
            links: vec![],
            operation_id: "seed-op".into(),
            supersedes: vec![],
            promotes: vec![],
            sources: sources.clone(),
            merges: vec![],
            creates: vec![(
                ParentRef::Root(TreeRoot::Decisions),
                "Parent".into(),
                "Parent summary".into(),
                sources,
            )],
            parked: vec![],
            stale: vec![],
            split: vec![],
            operation: LinkOperation::Create,
        };
        let revisions = runtime
            .store
            .commit_curation(
                &runtime.scope.owner,
                &runtime.scope.actor,
                &runtime.scope.projects[0],
                plan,
                now_ms().unwrap(),
            )
            .await
            .unwrap();
        let memory = revisions[0].revision.memory.clone();
        if queue_split {
            // The queue update is atomic with a no-op source consumption plan.
            runtime
                .store
                .test_queue_split(&runtime.scope.owner, &runtime.scope.projects[0], &memory)
                .await
                .unwrap();
        }
        runtime.dream_once().await.result.unwrap();
        memory
    }
    #[tokio::test]
    async fn node_over_split_bound_is_split() {
        let fake = Fake::start();
        let runtime = setup(&fake).await;
        let memory = seed_root(&runtime, 13, true).await;
        fake.reply(200, r#"{"node_title":"Parent","node_content":"Overview","children":[{"title":"A","content":"Group A","sources":["s0","s1","s2","s3"]},{"title":"B","content":"Group B","sources":["s4","s5","s6","s7"]},{"title":"C","content":"Group C","sources":["s8","s9","s10","s11","s12"]}]}"#);
        runtime.dream_once().await.result.unwrap();
        let children = runtime
            .store
            .tree_children(&runtime.scope.owner, &runtime.scope.projects[0], &memory)
            .await
            .unwrap();
        assert_eq!(children.len(), 3);
        assert_eq!(children.iter().map(|n| n.direct_sources).sum::<u32>(), 13);
        let mut partition = std::collections::HashSet::new();
        for child in &children {
            for (event, _, _, _) in runtime
                .store
                .node_sources(
                    &runtime.scope.owner,
                    &runtime.scope.projects[0],
                    &child.memory,
                    None,
                    32,
                )
                .await
                .unwrap()
                .0
            {
                assert!(partition.insert(event.source));
            }
        }
        assert_eq!(partition.len(), 13);
    }

    #[tokio::test]
    async fn root_split_groups_children() {
        let fake = Fake::start();
        let runtime = setup(&fake).await;
        for batch in 0..2 {
            let mut sources = Vec::new();
            let mut creates = Vec::new();
            for i in 0..32 {
                let id = format!("root-{batch}-{i:02}");
                capture(&runtime, &id, WorkEventKind::Decision).await;
                let source = SourceId::new(id).unwrap();
                sources.push(source.clone());
                creates.push((
                    ParentRef::Root(TreeRoot::Decisions),
                    format!("Node {batch}-{i}"),
                    "A summary".into(),
                    vec![source],
                ));
            }
            let plan = CurationCommit {
                links: vec![],
                operation_id: format!("seed-root-{batch}"),
                supersedes: vec![],
                promotes: vec![],
                sources,
                merges: vec![],
                creates,
                parked: vec![],
                stale: vec![],
                split: vec![],
                operation: LinkOperation::Create,
            };
            runtime
                .store
                .commit_curation(
                    &runtime.scope.owner,
                    &runtime.scope.actor,
                    &runtime.scope.projects[0],
                    plan,
                    now_ms().unwrap(),
                )
                .await
                .unwrap();
            for _ in 0..4 {
                runtime.dream_once().await.result.unwrap();
            }
        }
        capture(&runtime, "overflow", WorkEventKind::Decision).await;
        fake.reply(200, CREATE);
        runtime.dream_once().await.result.unwrap();
        assert_eq!(
            runtime
                .status(&runtime.scope.projects[0])
                .await
                .unwrap()
                .stored
                .pending
                .blocked_sources,
            1
        );
        let groups: Vec<String> = (0..4)
            .map(|g| {
                let slots = (g * 16..g * 16 + 16)
                    .map(|n| format!("\"n{n}\""))
                    .collect::<Vec<_>>()
                    .join(",");
                format!(r#"{{"title":"Group {g}","content":"Group summary","sources":[{slots}]}}"#)
            })
            .collect();
        fake.reply(
            200,
            &format!(
                r#"{{"node_title":"Root","node_content":"Overview","children":[{}]}}"#,
                groups.join(",")
            ),
        );
        runtime.dream_once().await.result.unwrap();
        let nodes = runtime
            .store
            .tree_nodes(&runtime.scope.owner, &runtime.scope.projects[0])
            .await
            .unwrap();
        assert_eq!(nodes.iter().filter(|n| n.depth == 1).count(), 4);
        assert_eq!(nodes.iter().filter(|n| n.depth == 2).count(), 64);
        fake.reply(200, CREATE);
        runtime.dream_once().await.result.unwrap();
        assert_eq!(
            runtime
                .store
                .tree_status(&runtime.scope.owner, &runtime.scope.projects[0])
                .await
                .unwrap()
                .parked_sources,
            0
        );
    }
    #[tokio::test]
    async fn merge_marks_parent_stale_and_refresh_rewrites_it() {
        let fake = Fake::start();
        let runtime = setup(&fake).await;
        let parent = seed_root(&runtime, 1, false).await;
        capture(&runtime, "child-source", WorkEventKind::Decision).await;
        let source = SourceId::new("child-source").unwrap();
        let plan = CurationCommit {
            links: vec![],
            operation_id: "seed-child".into(),
            supersedes: vec![],
            promotes: vec![],
            sources: vec![source.clone()],
            merges: vec![],
            creates: vec![(
                ParentRef::Node(parent.clone()),
                "SQLite child".into(),
                "SQLite durable storage".into(),
                vec![source],
            )],
            parked: vec![],
            stale: vec![],
            split: vec![],
            operation: LinkOperation::Create,
        };
        let created = runtime
            .store
            .commit_curation(
                &runtime.scope.owner,
                &runtime.scope.actor,
                &runtime.scope.projects[0],
                plan,
                now_ms().unwrap(),
            )
            .await
            .unwrap();
        let child = created[0].revision.memory.clone();
        runtime.dream_once().await.result.unwrap();
        let nodes = runtime
            .store
            .tree_nodes(&runtime.scope.owner, &runtime.scope.projects[0])
            .await
            .unwrap();
        let map = crate::engines::tree_policy::tree_map(&nodes, runtime.limits.tree_map_bytes);
        let slot = map.iter().position(|n| n.memory == child).unwrap();
        fake.reply(200, &format!(r#"{{"results":{{"s0":[{{"op":"merge","node":"n{slot}","parent":null,"title":"SQLite child","content":"SQLite current decision"}}]}}}}"#));
        capture(&runtime, "merge-source", WorkEventKind::Decision).await;
        runtime.dream_once().await.result.unwrap();
        assert_eq!(
            runtime
                .store
                .tree_status(&runtime.scope.owner, &runtime.scope.projects[0])
                .await
                .unwrap()
                .stale,
            1
        );
        fake.reply(
            200,
            r#"{"title":"Updated parent","content":"Current decision summary"}"#,
        );
        runtime.dream_once().await.result.unwrap();
        let nodes = runtime
            .store
            .tree_nodes(&runtime.scope.owner, &runtime.scope.projects[0])
            .await
            .unwrap();
        assert_eq!(
            nodes.iter().find(|n| n.memory == parent).unwrap().title,
            "Updated parent"
        );
    }
    #[tokio::test]
    async fn rebuild_after_purge_rewrites_or_retires() {
        let fake = Fake::start();
        let runtime = setup(&fake).await;
        let memory = seed_root(&runtime, 2, false).await;
        let recent = runtime
            .store
            .tree_recent_sources(&runtime.scope.owner, &runtime.scope.projects[0], &memory)
            .await
            .unwrap();
        assert_eq!(recent[0].source.as_str(), "seed-01");
        runtime
            .purge(
                &runtime.scope.projects[0],
                &[SourceId::new("seed-00").unwrap()],
            )
            .await
            .result
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(3)).await;
        fake.reply(
            200,
            r#"{"title":"Rebuilt","content":"Remaining evidence summary"}"#,
        );
        runtime.dream_once().await.result.unwrap();
        let nodes = runtime
            .store
            .tree_nodes(&runtime.scope.owner, &runtime.scope.projects[0])
            .await
            .unwrap();
        assert_eq!(
            nodes.iter().find(|n| n.memory == memory).unwrap().title,
            "Rebuilt"
        );
        runtime
            .purge(
                &runtime.scope.projects[0],
                &[SourceId::new("seed-01").unwrap()],
            )
            .await
            .result
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(3)).await;
        runtime.dream_once().await.result.unwrap();
        assert!(
            runtime
                .store
                .tree_nodes(&runtime.scope.owner, &runtime.scope.projects[0])
                .await
                .unwrap()
                .is_empty()
        );
    }
    #[tokio::test]
    async fn maintenance_conflict_keeps_the_active_revision() {
        let fake = Fake::start();
        let runtime = setup(&fake).await;
        let memory = seed_root(&runtime, 1, false).await;
        let node = runtime
            .store
            .tree_nodes(&runtime.scope.owner, &runtime.scope.projects[0])
            .await
            .unwrap()
            .into_iter()
            .find(|n| n.memory == memory)
            .unwrap();
        let plan = MaintenanceCommit {
            operation_id: "conflict".into(),
            operation: LinkOperation::Merge,
            rewrites: vec![(
                memory.clone(),
                RevisionId::new("stale-revision").unwrap(),
                "New".into(),
                "New summary".into(),
                vec![SourceId::new("seed-00").unwrap()],
            )],
            creates: vec![],
            reparent: vec![],
            expected_reparent: vec![],
            move_sources: vec![],
            stale: vec![],
            queue_item: None,
        };
        assert!(matches!(
            runtime
                .store
                .commit_tree_maintenance(
                    &runtime.scope.owner,
                    &runtime.scope.actor,
                    &runtime.scope.projects[0],
                    plan,
                    now_ms().unwrap()
                )
                .await,
            Err(RuntimeError::Conflict)
        ));
        let after = runtime
            .store
            .tree_nodes(&runtime.scope.owner, &runtime.scope.projects[0])
            .await
            .unwrap();
        assert_eq!(
            after.iter().find(|n| n.memory == memory).unwrap().revision,
            node.revision
        );
    }
    #[tokio::test]
    async fn uncertain_parked_source_is_not_replayed_after_restart() {
        let fake = Fake::start();
        let runtime = setup(&fake).await;
        seed_root(&runtime, 1, false).await;
        capture(&runtime, "expand-source", WorkEventKind::Decision).await;
        fake.reply(200, r#"{"results":{"s0":[{"op":"expand","node":"n0","parent":null,"title":null,"content":null}]}}"#);
        runtime.dream_once().await.result.unwrap();
        fake.reply(0, "");
        assert!(matches!(
            runtime.dream_once().await.result,
            Err(RuntimeError::ProviderUnavailable)
        ));
        let restarted = MemoryRuntime::with_openrouter(
            runtime.store.clone(),
            runtime.scope.clone(),
            runtime.limits.clone(),
            RouterRuntimeProviders::mock(&fake.base, 128),
        )
        .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(3)).await;
        restarted.dream_once().await.result.unwrap();
        assert_eq!(fake.chats.load(Ordering::SeqCst), 2);
    }
    #[tokio::test]
    async fn admitted_sources_do_not_starve_new_curation() {
        let fake = Fake::start();
        let runtime = setup(&fake).await;
        for i in 0..33 {
            capture(
                &runtime,
                &format!("blocked-{i:02}"),
                WorkEventKind::Decision,
            )
            .await;
        }
        let oldest = runtime
            .store
            .eligible_sources(
                &runtime.scope.owner,
                &runtime.scope.projects[0],
                32,
                true,
                None,
                None,
                true,
            )
            .await
            .unwrap();
        for (i, event) in oldest.iter().enumerate() {
            runtime
                .store
                .admit_runtime_operation(
                    &runtime.scope.owner,
                    &runtime.scope.projects[0],
                    &format!("curation-job:test-{i}"),
                    std::slice::from_ref(event),
                )
                .await
                .unwrap();
        }
        fake.reply(200, DISCARD);
        runtime.dream_once().await.result.unwrap();
        assert_eq!(fake.chats.load(Ordering::SeqCst), 1);
        let pending = runtime
            .store
            .eligible_sources(
                &runtime.scope.owner,
                &runtime.scope.projects[0],
                32,
                true,
                None,
                None,
                true,
            )
            .await
            .unwrap();
        assert_eq!(pending.len(), 32);
    }
    #[tokio::test]
    async fn concluded_matches_do_not_crowd_observed_detail_out_of_curation() {
        use crate::domain_models::memory_tree::{Basis, Confidence};
        use crate::domain_models::reflection::{
            Conclusion, ReflectionAnswer, ReflectionInput, ReflectionNode,
        };
        use crate::engines::reflection_policy::POLICY;

        async fn create(
            runtime: &MemoryRuntime,
            id: &str,
            parent: ParentRef,
            title: &str,
            content: &str,
        ) -> MemoryId {
            capture(runtime, id, WorkEventKind::Decision).await;
            let source = SourceId::new(id).unwrap();
            let plan = CurationCommit {
                links: vec![],
                operation_id: id.into(),
                sources: vec![source.clone()],
                promotes: vec![],
                merges: vec![],
                supersedes: vec![],
                creates: vec![(parent, title.into(), content.into(), vec![source])],
                parked: vec![],
                stale: vec![],
                split: vec![],
                operation: LinkOperation::Create,
            };
            let item = runtime
                .store
                .commit_curation(
                    &runtime.scope.owner,
                    &runtime.scope.actor,
                    &runtime.scope.projects[0],
                    plan,
                    now_ms().unwrap(),
                )
                .await
                .unwrap()
                .remove(0);
            runtime
                .store
                .review(
                    &runtime.scope.owner,
                    &runtime.scope.actor,
                    &ReviewRequest {
                        project: runtime.scope.projects[0].clone(),
                        revision: item.revision.id.clone(),
                        action: ReviewAction::Promote,
                        expected_active: None,
                        conflict_with: None,
                    },
                    now_ms().unwrap(),
                )
                .await
                .unwrap();
            item.revision.memory
        }
        let fake = Fake::start();
        let runtime = setup(&fake).await;
        let project = &runtime.scope.projects[0];
        let a = create(
            &runtime,
            "detail-root",
            ParentRef::Root(TreeRoot::Decisions),
            "Root",
            "Root context",
        )
        .await;
        let b = create(
            &runtime,
            "detail-middle",
            ParentRef::Node(a.clone()),
            "Middle",
            "Middle context",
        )
        .await;
        let observed = create(
            &runtime,
            "detail-leaf",
            ParentRef::Node(b.clone()),
            "Unique observed leaf",
            "SQLite",
        )
        .await;
        for batch in 0..3 {
            let nodes = runtime
                .store
                .tree_nodes(&runtime.scope.owner, project)
                .await
                .unwrap();
            let input = ReflectionInput {
                branch: a.clone(),
                nodes: [a.clone(), b.clone()]
                    .iter()
                    .map(|id| ReflectionNode {
                        node: nodes.iter().find(|n| &n.memory == id).unwrap().clone(),
                        path: vec![],
                        inside: true,
                    })
                    .collect(),
            };
            let conclusions = (0..if batch == 2 { 2 } else { 3 })
                .map(|i| Conclusion {
                    title: format!("Inference {batch}-{i}"),
                    content: "Use SQLite for durable storage".into(),
                    confidence: Confidence::High,
                    derived_from: vec![0, 1],
                    parent: 0,
                })
                .collect();
            let items = runtime
                .store
                .commit_reflection(
                    &runtime.scope.owner,
                    &runtime.scope.actor,
                    project,
                    &input,
                    ReflectionAnswer {
                        conclusions,
                        ..Default::default()
                    },
                    &format!("detail-reflection-{batch}"),
                    now_ms().unwrap(),
                    POLICY,
                )
                .await
                .unwrap();
            for item in items {
                runtime
                    .store
                    .review(
                        &runtime.scope.owner,
                        &runtime.scope.actor,
                        &ReviewRequest {
                            project: project.clone(),
                            revision: item.revision.id,
                            action: ReviewAction::Promote,
                            expected_active: None,
                            conflict_with: None,
                        },
                        now_ms().unwrap(),
                    )
                    .await
                    .unwrap();
            }
        }
        capture(&runtime, "detail-source", WorkEventKind::Decision).await;
        let events = runtime
            .store
            .eligible_sources(&runtime.scope.owner, project, 32, true, None, None, true)
            .await
            .unwrap();
        let event = events
            .into_iter()
            .find(|e| e.source.as_str() == "detail-source")
            .unwrap();
        let terms = crate::engines::tree_policy::source_terms(&event.content);
        let ranked = runtime
            .store
            .tree_lexical_candidates(&runtime.scope.owner, project, &terms, 32)
            .await
            .unwrap();
        assert_eq!(ranked.len(), 9);
        assert!(ranked[..8].iter().all(|n| n.basis == Basis::Concluded));
        assert_eq!(ranked[8].memory, observed);
        assert_eq!(ranked[8].depth, 3);
        fake.reply(200, DISCARD);
        let mut trace = runtime
            .begin_trace(
                std::slice::from_ref(project),
                crate::domain_models::runtime_diagnostics::ActivityOperation::Dream,
                ActivityPhase::Curation,
                true,
            )
            .await
            .unwrap();
        let result = runtime
            .curate_batch(project, vec![event], LinkOperation::Create, &mut trace)
            .await;
        runtime.finish_trace(trace, result).await.result.unwrap();
        let body = fake.chat_bodies.lock().unwrap().last().unwrap().clone();
        let detailed = body
            .split_once("\\\"detailed\\\":[")
            .unwrap()
            .1
            .split_once("],\\\"map\\\"")
            .unwrap()
            .0;
        assert!(
            detailed.contains("Unique observed leaf"),
            "observed detail missing: {body}"
        );
        assert!(!body.contains("Inference 0-0"));
    }
}
