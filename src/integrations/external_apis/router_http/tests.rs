use super::*;
use crate::domain_models::{ActorId, OwnerId, ProjectId};
use crate::integrations::database::mock_memory_repository::fixture;
use std::{
    io::{Read, Write},
    net::TcpListener,
    thread,
};
fn server(
    responses: Vec<(u16, Value)>,
    expected: &'static str,
) -> (String, thread::JoinHandle<()>) {
    server_verified(responses, expected, |_| {})
}
fn server_verified(
    responses: Vec<(u16, Value)>,
    expected: &'static str,
    verify: fn(&Value),
) -> (String, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = format!("http://{}", listener.local_addr().unwrap());
    let worker = thread::spawn(move || {
        for (status, body) in responses {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut bytes = Vec::new();
            let mut buffer = [0; 4096];
            let mut required = None;
            loop {
                let n = socket.read(&mut buffer).unwrap();
                assert!(n > 0);
                bytes.extend_from_slice(&buffer[..n]);
                if let Some(pos) = bytes.windows(4).position(|v| v == b"\r\n\r\n") {
                    let headers = std::str::from_utf8(&bytes[..pos]).unwrap();
                    assert!(headers.starts_with(&format!("POST {expected} HTTP/1.1")));
                    assert!(
                        headers
                            .to_lowercase()
                            .contains("authorization: bearer synthetic-test-token")
                    );
                    let length = headers
                        .lines()
                        .find_map(|line| {
                            line.to_lowercase()
                                .strip_prefix("content-length: ")
                                .map(|v| v.parse::<usize>().unwrap())
                        })
                        .unwrap();
                    required = Some(pos + 4 + length);
                }
                if required.is_some_and(|n| bytes.len() >= n) {
                    break;
                }
            }
            let pos = bytes.windows(4).position(|v| v == b"\r\n\r\n").unwrap();
            verify(&serde_json::from_slice::<Value>(&bytes[pos + 4..]).unwrap());
            let text = body.to_string();
            write!(socket,"HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",text.len(),text).unwrap();
        }
    });
    (address, worker)
}
fn client(base: &str) -> RouterHttp {
    RouterHttp::mock(base, Duration::from_secs(2))
}
fn usage() -> Value {
    json!({"prompt_tokens":12,"completion_tokens":3,"cost":0.000001})
}
fn request() -> RecallRequest {
    RecallRequest {
        task: "synthetic database task".into(),
        owner: OwnerId::new("o").unwrap(),
        actor: ActorId::new("a").unwrap(),
        project: ProjectId::new("p").unwrap(),
        budget_bytes: 200,
    }
}
#[tokio::test]
async fn embedding_real_http_and_validation() {
    let response = json!({"model":"google/gemini-embedding-2","usage":usage(),"data":[{"index":0,"embedding":vec![1.0;128]}]});
    let (base, worker) = server(vec![(200, response)], "/api/v1/embeddings");
    let http = client(&base);
    let embedding = RouterEmbeddingClient {
        http,
        config: OpenRouterEmbeddingConfig {
            dimensions: 128,
            index_revision: "synthetic-v1".into(),
        },
    }
    .embed("synthetic")
    .await
    .unwrap();
    assert_eq!(embedding.values.len(), 128);
    worker.join().unwrap();
}
#[tokio::test]
async fn planner_preserves_caller_scope() {
    let (base, worker) = server(
        vec![(
            200,
            json!({"model":"synthetic/planner","usage":usage(),"choices":[{"finish_reason":"stop","message":{"content":"{\"terms\":[\"Database\",\"database\"]}"}}]}),
        )],
        "/api/v1/chat/completions",
    );
    let planner = RouterQueryPlanner::new(client(&base), "synthetic/planner".into(), 128).unwrap();
    let input = request();
    let plan = planner.plan(&input).await.unwrap();
    assert_eq!(plan.terms, vec!["database"]);
    assert_eq!(plan.owner, input.owner);
    assert_eq!(plan.project, input.project);
    worker.join().unwrap();
}
#[tokio::test]
async fn luna_named_answers_real_http() {
    let candidate = fixture()[0].clone();
    let id = candidate.id.to_string();
    let response = json!({"model":"openai/gpt-6-luna-decisions","usage":{"input_tokens":12,"output_tokens":0,"cost":0.000001},"answers":{"best":{"type":"choice","choice":id,"confidence":0.9,"probabilities":{id.clone():0.9,"none":0.1}},format!("relevant:{id}"):{"type":"noul","noul":0.95},format!("utility:{id}"):{"type":"score","score":2.0,"confidence":0.9}}});
    let (base, worker) = server(vec![(200, response)], "/api/alpha/decisions");
    let outcome = RouterLunaSelector {
        http: client(&base),
    }
    .evaluate(&SelectionRequest {
        task: "synthetic".into(),
        retrieval_terms: vec![],
        candidates: vec![candidate.clone()],
    })
    .await
    .unwrap();
    assert_eq!(outcome.best, Some(candidate.id));
    worker.join().unwrap();
}
#[tokio::test]
async fn authentication_failure_is_not_retried() {
    let (base, worker) = server(
        vec![(401, json!({"error":"secret should never surface"}))],
        "/api/v1/embeddings",
    );
    let http = client(&base);
    assert_eq!(
        http.post("/api/v1/embeddings", json!({"model":"m"})).await,
        Err(RouterError::Http(401))
    );
    worker.join().unwrap();
}
#[tokio::test]
async fn provider_error_body_is_rejected() {
    let (base, worker) = server(
        vec![(200, json!({"error":"synthetic"}))],
        "/api/v1/embeddings",
    );
    assert_eq!(
        client(&base)
            .post("/api/v1/embeddings", json!({"model":"m"}))
            .await,
        Err(RouterError::Response)
    );
    worker.join().unwrap();
}
#[tokio::test]
async fn timeout_is_bounded_and_not_retried() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let worker = thread::spawn(move || {
        let (_socket, _) = listener.accept().unwrap();
        thread::sleep(Duration::from_millis(200));
    });
    let http = RouterHttp::mock(&base, Duration::from_millis(30));
    assert_eq!(
        http.post("/api/v1/embeddings", json!({"model":"m"})).await,
        Err(RouterError::Timeout)
    );
    worker.join().unwrap();
}
#[tokio::test]
async fn malformed_embedding_dimensions_are_rejected() {
    let (base, worker) = server(
        vec![(
            200,
            json!({"model":"google/gemini-embedding-2","usage":usage(),"data":[{"index":0,"embedding":[1.0]}]}),
        )],
        "/api/v1/embeddings",
    );
    let output = RouterEmbeddingClient {
        http: client(&base),
        config: OpenRouterEmbeddingConfig {
            dimensions: 128,
            index_revision: "test".into(),
        },
    }
    .embed("synthetic")
    .await;
    assert!(output.is_err());
    worker.join().unwrap();
}
#[tokio::test]
async fn truncated_planner_output_is_rejected() {
    let (base, worker) = server(
        vec![(
            200,
            json!({"model":"synthetic/planner","usage":usage(),"choices":[{"finish_reason":"length","message":{"content":"{\"terms\":[\"storage\"]}"}}]}),
        )],
        "/api/v1/chat/completions",
    );
    assert!(
        RouterQueryPlanner::new(client(&base), "synthetic/planner".into(), 128)
            .unwrap()
            .plan(&request())
            .await
            .is_err()
    );
    worker.join().unwrap();
}
/// Planner result for one canned model content/finish_reason over the loopback HTTP mock.
async fn planned(content: &str, finish: &str) -> Result<Vec<String>, MemoryError> {
    let (base, worker) = server(
        vec![(
            200,
            json!({"model":"synthetic/planner","usage":usage(),"choices":[{"finish_reason":finish,"message":{"content":content}}]}),
        )],
        "/api/v1/chat/completions",
    );
    let result = RouterQueryPlanner::new(client(&base), "synthetic/planner".into(), 128)
        .unwrap()
        .plan(&request())
        .await
        .map(|plan| plan.terms);
    worker.join().unwrap();
    result
}
#[tokio::test]
async fn planner_accepts_fenced_json_with_or_without_closing_fence() {
    assert_eq!(
        planned("```json\n{\"terms\":[\"a\",\"b\"]}\n```", "stop")
            .await
            .unwrap(),
        vec!["a", "b"]
    );
    assert_eq!(
        planned("```\n{\"terms\":[\"a\",\"b\"]}\n```\n", "stop")
            .await
            .unwrap(),
        vec!["a", "b"]
    );
    assert_eq!(
        planned("```\n{\"terms\":[\"a\"]}", "stop").await.unwrap(),
        vec!["a"]
    );
    assert_eq!(
        planned("```json\n{\"terms\":[\"a\"]} ", "stop")
            .await
            .unwrap(),
        vec!["a"]
    );
}
#[tokio::test]
async fn planner_keeps_first_eight_valid_terms_and_drops_unusable_ones() {
    let ten: Vec<String> = (0..10).map(|i| format!("t{i}")).collect();
    assert_eq!(
        planned(&json!({"terms":ten}).to_string(), "stop")
            .await
            .unwrap(),
        ten[..8].to_vec()
    );
    let long = "x".repeat(129);
    assert_eq!(
        planned(&json!({"terms":["a","",long,"A"]}).to_string(), "stop")
            .await
            .unwrap(),
        vec!["a"]
    );
    assert_eq!(
        planned(
            &json!({"terms":[1,null,"  Spaced Out ","bad\nterm",["x"],"b"]}).to_string(),
            "stop"
        )
        .await
        .unwrap(),
        vec!["spaced out", "b"]
    );
    // Invalid entries do not consume the eight slots.
    let mixed: Vec<Value> = (0..6)
        .map(|_| json!(""))
        .chain((0..10).map(|i| json!(format!("t{i}"))))
        .collect();
    assert_eq!(
        planned(&json!({"terms":mixed}).to_string(), "stop")
            .await
            .unwrap()
            .len(),
        8
    );
}
#[tokio::test]
async fn planner_still_rejects_unusable_structure() {
    for content in [
        "{\"terms\":[]}",
        "{\"terms\":[\"\",1]}",
        "{\"terms\":\"a\"}",
        "{\"terms\":[\"a\"],\"x\":1}",
        "texto {\"terms\":[\"a\"]}",
        "```json\n```",
        "```{\"terms\":[\"a\"]}```",
        "[\"a\"]",
    ] {
        assert_eq!(
            planned(content, "stop").await,
            Err(MemoryError::InvalidInput(INVALID_PROVIDER_RESPONSE)),
            "{content}"
        );
    }
    assert_eq!(
        planned("{\"terms\":[\"a\"]}", "length").await,
        Err(MemoryError::InvalidInput(INVALID_PROVIDER_RESPONSE))
    );
    let oversized = format!("{{\"terms\":[\"a\"],\"pad\":\"{}\"}}", "p".repeat(4096));
    assert!(planned(&oversized, "stop").await.is_err());
}
#[tokio::test]
async fn incomplete_luna_answers_are_rejected() {
    let (base, worker) = server(
        vec![(
            200,
            json!({"model":"openai/gpt-6-luna-decisions","usage":usage(),"answers":{}}),
        )],
        "/api/alpha/decisions",
    );
    assert!(
        RouterLunaSelector {
            http: client(&base)
        }
        .evaluate(&SelectionRequest {
            task: "synthetic".into(),
            retrieval_terms: vec![],
            candidates: vec![fixture()[0].clone()]
        })
        .await
        .is_err()
    );
    worker.join().unwrap();
}
#[tokio::test]
async fn native_gemini_model_alias_retains_canonical_embedding_space() {
    let (base, worker) = server(
        vec![(
            200,
            json!({"model":"gemini-embedding-2","usage":usage(),"data":[{"index":0,"embedding":vec![1.0;128]}]}),
        )],
        "/api/v1/embeddings",
    );
    let output = RouterEmbeddingClient {
        http: client(&base),
        config: OpenRouterEmbeddingConfig {
            dimensions: 128,
            index_revision: "native-alias-test".into(),
        },
    }
    .embed("synthetic")
    .await
    .unwrap();
    assert_eq!(output.space.model, "google/gemini-embedding-2");
    assert_eq!(output.values.len(), 128);
    worker.join().unwrap();
}
#[test]
fn alias_mapping_is_exact() {
    assert!(response_model_matches(
        "google/gemini-embedding-2",
        Some("gemini-embedding-2")
    ));
    assert!(!response_model_matches(
        "google/gemini-embedding-2",
        Some("other/gemini-embedding-2")
    ));
    assert!(!response_model_matches(
        "other/model",
        Some("gemini-embedding-2")
    ));
    assert!(response_model_matches(
        "anthropic/claude-haiku-5.5",
        Some("anthropic/claude-haiku-5.5-20261007")
    ));
    assert!(!response_model_matches(
        "anthropic/claude-haiku-5.5",
        Some("anthropic/claude-haiku-5.5-20261008")
    ));
}
#[tokio::test]
async fn documented_dated_luna_answers_preserve_validation() {
    let candidate = fixture()[0].clone();
    let id = candidate.id.to_string();
    let response = json!({"model":"openai/gpt-6-luna-decisions-20261006","usage":{"input_tokens":12,"output_tokens":0,"cost":0.000001},"answers":{"best":{"type":"choice","choice":id,"confidence":0.9,"probabilities":{id.clone():0.9,"none":0.1}},format!("relevant:{id}"):{"type":"noul","noul":0.95},format!("utility:{id}"):{"type":"score","score":2.0,"confidence":0.9}}});
    let (base, worker) = server(vec![(200, response)], "/api/alpha/decisions");
    let outcome = RouterLunaSelector {
        http: client(&base),
    }
    .evaluate(&SelectionRequest {
        task: "synthetic".into(),
        retrieval_terms: vec![],
        candidates: vec![candidate.clone()],
    })
    .await
    .unwrap();
    assert_eq!(outcome.best, Some(candidate.id));
    worker.join().unwrap();
}
#[test]
fn luna_alias_rejects_other_dates_and_models() {
    assert!(response_model_matches(
        "openai/gpt-6-luna-decisions",
        Some("openai/gpt-6-luna-decisions-20261006")
    ));
    for model in [
        "openai/gpt-6-luna-decisions-20261007",
        "other/gpt-6-luna-decisions-20261006",
        "openai/gpt-6-luna",
        "gemini-embedding-2",
    ] {
        assert!(!response_model_matches(
            "openai/gpt-6-luna-decisions",
            Some(model)
        ));
    }
}
#[tokio::test]
async fn direct_support_is_typed_and_missing_predicate_fails_closed() {
    let candidate = fixture()[0].clone();
    let id = candidate.id.to_string();
    let response = json!({"model":"openai/gpt-6-luna-decisions","usage":{"input_tokens":12,"output_tokens":0,"cost":0.000001},"answers":{"best":{"type":"choice","choice":id,"confidence":0.9,"probabilities":{id.clone():0.9,"none":0.1}},format!("relevant:{id}"):{"type":"noul","noul":0.95},format!("utility:{id}"):{"type":"score","score":1.8,"confidence":0.63},format!("supports:{id}"):{"type":"noul","noul":0.96}}});
    let mut missing = response.clone();
    missing["answers"]
        .as_object_mut()
        .unwrap()
        .remove(&format!("supports:{id}"));
    let (base, worker) = server(
        vec![(200, response), (200, missing)],
        "/api/alpha/decisions",
    );
    let selector = RouterLunaSelector {
        http: client(&base),
    };
    let request = SelectionRequest {
        task: "synthetic".into(),
        retrieval_terms: vec![],
        candidates: vec![candidate.clone()],
    };
    let result = selector.evaluate_supported(&request).await.unwrap();
    assert_eq!(result.direct_support[&candidate.id], 0.96);
    assert_eq!(result.outcome.candidates[0].confidence, 0.63);
    assert!(selector.evaluate_supported(&request).await.is_err());
    worker.join().unwrap();
}

fn opaque_response(choice: &str, support: f64) -> Value {
    json!({"model":"openai/gpt-6-luna-decisions","usage":usage(),"answers":{"best":{"type":"choice","choice":choice,"confidence":0.95,"probabilities":{"c0":0.02,"c1":0.96,"none":0.02}},"relevant:c0":{"type":"noul","noul":0.9},"utility:c0":{"type":"score","score":1.7,"confidence":0.8},"supports:c0":{"type":"noul","noul":support},"relevant:c1":{"type":"noul","noul":0.99},"utility:c1":{"type":"score","score":1.9,"confidence":0.8},"supports:c1":{"type":"noul","noul":support}}})
}
fn opaque_fixture() -> (
    SelectionRequest,
    crate::domain_models::selection_context::SelectionContext,
) {
    use crate::domain_models::selection_context::*;
    let mut first = fixture()[0].clone();
    first.id = MemoryId::new("foreign-owner-r").unwrap();
    first.source = SourceId::new("shadow-owner-source").unwrap();
    first.text = "Aster cache endpoint is amber.".into();
    let mut second = first.clone();
    second.id = MemoryId::new("do-not-select-this-id").unwrap();
    second.source = SourceId::new("foreign-source-claim").unwrap();
    second.text = "Aster cache timeout is 17 seconds.".into();
    let candidates = vec![first, second];
    let context = SelectionContext {
        intent: SelectionIntent::TopicRecall,
        owner: candidates[0].owner.clone(),
        project: candidates[0].project.clone(),
        as_of_ms: 30,
        validity: candidates
            .iter()
            .map(|m| CandidateValidity {
                memory: m.id.clone(),
                source: m.source.clone(),
                owner: m.owner.clone(),
                project: m.project.clone(),
                valid_from_ms: 10,
                expires_at_ms: Some(50),
            })
            .collect(),
    };
    (
        SelectionRequest {
            task: "Aster cache".into(),
            retrieval_terms: vec!["cache".into()],
            candidates,
        },
        context,
    )
}
#[tokio::test]
async fn contractual_wire_hides_semantic_ids_and_restores_second_candidate() {
    let (request, context) = opaque_fixture();
    let (base, worker) = server_verified(
        vec![(200, opaque_response("c1", 0.97))],
        "/api/alpha/decisions",
        |body| {
            let encoded = body.to_string();
            for sensitive_label in [
                "foreign-owner-r",
                "shadow-owner-source",
                "do-not-select-this-id",
                "foreign-source-claim",
            ] {
                assert!(!encoded.contains(sensitive_label));
            }
            assert!(body["state"]["application_validity"].get("owner").is_none());
        },
    );
    let answer = RouterLunaSelector {
        http: client(&base),
    }
    .evaluate_contractual(&request, &context)
    .await
    .unwrap();
    worker.join().unwrap();
    assert_eq!(
        answer.outcome.best.as_ref().unwrap(),
        &request.candidates[1].id
    );
    assert_eq!(
        answer.outcome.candidates[1].memory_id,
        request.candidates[1].id
    );
    assert_eq!(answer.direct_support[&request.candidates[1].id], 0.97);
    assert!(
        !answer
            .direct_support
            .contains_key(&MemoryId::new("c1").unwrap())
    );
}
#[tokio::test]
async fn contractual_rejects_raw_id_choice_and_missing_alias_answers() {
    let (request, context) = opaque_fixture();
    let bad = opaque_response(request.candidates[1].id.as_str(), 0.97);
    let mut missing = opaque_response("c1", 0.97);
    missing["answers"]
        .as_object_mut()
        .unwrap()
        .remove("supports:c1");
    let (base, worker) = server(vec![(200, bad), (200, missing)], "/api/alpha/decisions");
    let selector = RouterLunaSelector {
        http: client(&base),
    };
    assert!(
        selector
            .evaluate_contractual(&request, &context)
            .await
            .is_err()
    );
    assert!(
        selector
            .evaluate_contractual(&request, &context)
            .await
            .is_err()
    );
    worker.join().unwrap();
}
#[tokio::test]
async fn contractual_factual_intent_and_negative_veto_preserve_existing_cuts() {
    use crate::domain_models::selection_context::*;
    let (request, mut context) = opaque_fixture();
    context.intent = SelectionIntent::FactualQuestion;
    let mut abstain = opaque_response("none", 0.1);
    abstain["answers"]["best"]["probabilities"] = json!({"c0":0.02,"c1":0.02,"none":0.96});
    let (base, worker) = server(
        vec![(200, opaque_response("c1", 0.89)), (200, abstain)],
        "/api/alpha/decisions",
    );
    let selector = RouterLunaSelector {
        http: client(&base),
    };
    for index in 0..2 {
        let answer = selector
            .evaluate_contractual(&request, &context)
            .await
            .unwrap();
        if index == 0 {
            assert_eq!(answer.direct_support[&request.candidates[1].id], 0.89);
        } else {
            assert!(answer.outcome.best.is_none());
        }
    }
    worker.join().unwrap();
}

#[tokio::test]
async fn contractual_scope_and_reserved_identity_fail_before_transport() {
    let (mut request, mut context) = opaque_fixture();
    let http = client("http://127.0.0.1:1");
    let selector = RouterLunaSelector { http };
    context.validity[1].owner = OwnerId::new("other-owner").unwrap();
    assert!(matches!(
        selector.evaluate_contractual(&request, &context).await,
        Err(MemoryError::InvalidSelection)
    ));
    context.validity[1].owner = context.owner.clone();
    request.candidates[0].id = MemoryId::new("none").unwrap();
    context.validity[0].memory = request.candidates[0].id.clone();
    assert!(matches!(
        selector.evaluate_contractual(&request, &context).await,
        Err(MemoryError::InvalidSelection)
    ));
}
#[tokio::test]
async fn resident_calls_have_no_cost_ceiling() {
    let (base, worker) = server(
        vec![
            (200, json!({"usage":{"prompt_tokens":3,"cost":10.0}})),
            (200, json!({"usage":{"prompt_tokens":3,"cost":20.0}})),
        ],
        "/api/v1/embeddings",
    );
    let http = client(&base);
    http.post(
        "/api/v1/embeddings",
        json!({"model":"google/gemini-embedding-2"}),
    )
    .await
    .unwrap();
    http.post(
        "/api/v1/embeddings",
        json!({"model":"google/gemini-embedding-2"}),
    )
    .await
    .unwrap();
    worker.join().unwrap();
}
#[tokio::test]
async fn refused_connection_is_classified_uncharged() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    drop(listener);
    let http = client(&base);
    let error = http
        .post("/api/v1/embeddings", json!({"model":"m"}))
        .await
        .unwrap_err();
    assert_eq!(error, RouterError::Connect);
    assert!(error.uncharged());
}
#[test]
fn only_pre_billing_failures_are_uncharged() {
    for error in [
        RouterError::Connect,
        RouterError::Configuration,
        RouterError::Http(401),
        RouterError::Http(402),
        RouterError::Http(403),
        RouterError::Http(429),
    ] {
        assert!(error.uncharged(), "{error:?}");
    }
    for error in [
        RouterError::Timeout,
        RouterError::Transport,
        RouterError::Http(500),
        RouterError::Http(503),
        RouterError::Response,
    ] {
        assert!(!error.uncharged(), "{error:?}");
    }
}
#[tokio::test]
async fn credit_refusal_surfaces_as_a_rejected_embedding() {
    let (base, worker) = server(vec![(402, json!({"error":"credit"}))], "/api/v1/embeddings");
    let output = RouterEmbeddingClient {
        http: client(&base),
        config: OpenRouterEmbeddingConfig {
            dimensions: 128,
            index_revision: "test".into(),
        },
    }
    .embed("synthetic")
    .await;
    assert_eq!(output.unwrap_err(), MemoryError::ProviderRejected);
    worker.join().unwrap();
}
fn playbook_response(model: &str, finish: &str, text: &str) -> Value {
    json!({"model":model,"usage":usage(),"choices":[{"finish_reason":finish,"message":{"content":json!({"playbook":text}).to_string()}}]})
}
#[tokio::test]
async fn generate_playbook_requires_schema_and_model() {
    fn verify(body: &Value) {
        assert_eq!(body["model"], json!("anthropic/claude-haiku-5.5"));
        assert_eq!(body["response_format"]["type"], json!("json_schema"));
        assert_eq!(
            body["response_format"]["json_schema"]["strict"],
            json!(true)
        );
        assert_eq!(
            body["response_format"]["json_schema"]["schema"]["required"],
            json!(["playbook"])
        );
        assert_eq!(
            body["response_format"]["json_schema"]["schema"]["additionalProperties"],
            json!(false)
        );
        let system = body["messages"][0]["content"].as_str().unwrap();
        assert!(system.contains("[F#]") && system.contains("1536"));
        assert!(
            body["messages"][1]["content"]
                .as_str()
                .unwrap()
                .contains("\"F1\"")
        );
    }
    let text = "- Run tests with cargo test [F1]";
    let responses = vec![
        (
            200,
            playbook_response("anthropic/claude-haiku-5.5", "stop", text),
        ),
        (200, playbook_response("other/model", "stop", text)),
        (
            200,
            playbook_response("anthropic/claude-haiku-5.5", "length", text),
        ),
        (
            200,
            json!({"model":"anthropic/claude-haiku-5.5","usage":usage(),"choices":[{"finish_reason":"stop","message":{"content":"{\"playbook\":\"x\",\"extra\":1}"}}]}),
        ),
        (
            200,
            json!({"model":"anthropic/claude-haiku-5.5","usage":usage(),"choices":[{"finish_reason":"stop","message":{"content":"{\"playbook\":1}"}}]}),
        ),
    ];
    let (base, worker) = server_verified(responses, "/api/v1/chat/completions", verify);
    let http = client(&base);
    let input = json!({"current_playbook":null,"facts":[{"id":"F1","kind":"procedure","key":"bash cargo test"}]});
    assert_eq!(http.generate_playbook(input.clone()).await.unwrap(), text);
    for _ in 0..4 {
        assert!(http.generate_playbook(input.clone()).await.is_err());
    }
    worker.join().unwrap();
}
#[test]
fn base_url_validation_table() {
    for (input, expected) in [
        ("https://openrouter.ai", "https://openrouter.ai"),
        (
            "https://proxy.example.com:8443/",
            "https://proxy.example.com:8443",
        ),
        ("http://127.0.0.1:58080", "http://127.0.0.1:58080"),
        ("http://[::1]:9", "http://[::1]:9"),
        ("http://localhost:8080/", "http://localhost:8080"),
        ("https://[::1]:443", "https://[::1]:443"),
    ] {
        assert_eq!(validate_base_url(input), Ok(expected.into()), "{input}");
    }
    let long = format!("https://{}.com", "a".repeat(243));
    assert_eq!(long.len(), 255);
    assert!(validate_base_url(&long).is_ok());
    let too_long = format!("https://{}.com", "a".repeat(244));
    assert_eq!(too_long.chars().count(), 256);
    for input in [
        "http://example.com",
        "http://127.0.0.1",
        "http://[::1]",
        "http://localhost",
        "http://127.0.0.2:80",
        "http://10.0.0.1:80",
        "https://u:p@x.com",
        "https://u@x.com",
        "https://x.com/api",
        "https://x.com//",
        "https://x.com?a=1",
        "https://x.com/?a=1",
        "https://x.com#f",
        "ftp://x",
        "",
        "https://",
        "https:///",
        "https://x.com:",
        "https://x.com:0",
        "https://x.com:65536",
        "https://x.com:8a",
        "https://x..com",
        "https://-x.com",
        "https://x.com.:1/x",
        "https://x .com",
        "https://x.com\\",
        "HTTPS://x.com",
        "//x.com",
        "x.com",
        "https://[::1",
        "https://[zz]:1",
        "https://x.com:1:2",
        "https://é.com",
        &too_long,
    ] {
        assert_eq!(
            validate_base_url(input),
            Err(RouterError::Configuration),
            "{input}"
        );
    }
}
#[tokio::test]
async fn runtime_with_base_sends_all_three_routes_with_bearer_to_custom_base() {
    for (path, response) in [
        ("/api/v1/chat/completions", json!({"ok":1})),
        ("/api/v1/embeddings", json!({"ok":2})),
        ("/api/alpha/decisions", json!({"ok":3})),
    ] {
        let (base, worker) = server(vec![(200, response.clone())], path);
        // A trailing slash is stripped, so the request path is exactly the route.
        let http =
            RouterHttp::runtime_with_base(&format!("{base}/"), "synthetic-test-token").unwrap();
        assert_eq!(http.post(path, json!({"model":"m"})).await, Ok(response));
        worker.join().unwrap();
    }
    assert!(RouterHttp::runtime_with_base("http://example.com", "synthetic-test-token").is_err());
    assert!(RouterHttp::runtime_with_base("https://x.com/api", "synthetic-test-token").is_err());
    assert!(RouterHttp::runtime_with_base("https://openrouter.ai", "").is_err());
}
#[tokio::test]
async fn custom_base_does_not_follow_redirects() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let worker = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        let mut buffer = [0; 4096];
        let _ = socket.read(&mut buffer);
        write!(socket,"HTTP/1.1 307 Temporary Redirect\r\nLocation: http://127.0.0.1:1/\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
    });
    let http = RouterHttp::runtime_with_base(&base, "synthetic-test-token").unwrap();
    assert_eq!(
        http.post("/api/v1/embeddings", json!({"model":"m"})).await,
        Err(RouterError::Http(307))
    );
    worker.join().unwrap();
}
