use lambda_memory::domain_models::embeddings::*;
use lambda_memory::domain_models::*;
use lambda_memory::engines::hybrid_policy::fuse;
use lambda_memory::integrations::database::mock_memory_repository::{
    MockMemoryRepository, fixture,
};
use lambda_memory::integrations::embedding_ports::{EmbeddingClient, VectorRepository};
use lambda_memory::integrations::external_apis::{
    mock_luna_decisions_client::MockLunaDecisionsClient, mock_query_planner::MockQueryPlanner,
};
use lambda_memory::integrations::mock_vector_repository::MockVectorRepository;
use lambda_memory::integrations::openrouter_embeddings::*;
use lambda_memory::integrations::provider_config::ProviderConfig;
use lambda_memory::services::hybrid_recall_service::HybridRecallService;
mod support;
fn config() -> OpenRouterEmbeddingConfig {
    OpenRouterEmbeddingConfig {
        dimensions: 128,
        index_revision: "mock-concepts-v1".into(),
    }
}
fn request(task: &str) -> RecallRequest {
    RecallRequest {
        task: task.into(),
        owner: OwnerId::new("fixture-owner").unwrap(),
        actor: ActorId::new("fixture-actor").unwrap(),
        project: ProjectId::new("demo-project").unwrap(),
        budget_tokens: 500,
    }
}
fn plan(task: &str) -> MemoryQueryPlan {
    let req = request(task);
    MemoryQueryPlan {
        terms: vec![task.into()],
        owner: req.owner,
        actor: req.actor,
        project: req.project,
        relations: vec![],
    }
}
fn corpus() -> Vec<IndexedEmbedding> {
    let client = MockEmbeddingClient { config: config() };
    fixture()
        .into_iter()
        .map(|memory| {
            let embedding = support::ready_mock(client.embed(&memory.text)).unwrap();
            IndexedEmbedding { memory, embedding }
        })
        .collect()
}
#[test]
fn live_adapter_stays_disabled_and_dimensions_are_explicit() {
    let client = DisabledOpenRouterEmbeddingClient { config: config() };
    assert_eq!(
        support::ready_mock(client.embed("query")).unwrap_err(),
        MemoryError::PaidCallsDisabled
    );
    for dimensions in [0, 127, 3073] {
        assert!(
            OpenRouterEmbeddingConfig {
                dimensions,
                index_revision: "v1".into()
            }
            .space()
            .is_err()
        );
    }
    assert!(support::ready_mock(client.embed(" ")).is_err());
}
#[test]
fn vector_search_filters_scope_lifecycle_and_incompatible_spaces() {
    let client = MockEmbeddingClient { config: config() };
    let query = support::ready_mock(client.embed("durable database")).unwrap();
    let mut records = corpus();
    records[0].embedding.space.revision = "old-index".into();
    let store = MockVectorRepository {
        corpus: records,
        minimum_cosine: 0.6,
    };
    let hits = support::ready_mock(store.search_vector(&plan("database"), &query, 4)).unwrap();
    assert!(hits.is_empty()); // matching db is stale; other-project and inactive evidence cannot rescue it.
}
#[test]
fn malformed_vectors_and_source_restrictions_are_enforced() {
    let client = MockEmbeddingClient { config: config() };
    let mut query = support::ready_mock(client.embed("database")).unwrap();
    let store = MockVectorRepository {
        corpus: corpus(),
        minimum_cosine: 0.6,
    };
    let mut restricted = plan("database");
    restricted.relations = vec![MemoryRelationFilter::Sources(vec![
        SourceId::new("fixture://luna").unwrap(),
    ])];
    assert!(
        support::ready_mock(store.search_vector(&restricted, &query, 4))
            .unwrap()
            .is_empty()
    );
    query.values[0] = f64::INFINITY;
    assert!(support::ready_mock(store.search_vector(&plan("database"), &query, 4)).is_err());
}
#[test]
fn fusion_is_bounded_deduplicated_and_rescopes_untrusted_streams() {
    let input = request("memory");
    let policy = HybridPolicy {
        stream_limit: 8,
        fusion_limit: 2,
        rrf_k: 60,
    };
    let records = fixture();
    let mut others = records.clone();
    others[0].owner = OwnerId::new("other-owner").unwrap();
    let hits = fuse(
        &input,
        &plan("memory"),
        vec![records[0].clone(), records[0].clone(), records[1].clone()],
        others,
        &policy,
    );
    assert_eq!(hits.len(), 2);
    assert_eq!(hits[0].id.as_str(), "luna");
    assert!(
        hits.iter()
            .all(|m| m.owner == input.owner && m.active && !m.restricted)
    );
}
#[test]
fn hybrid_candidate_gain_does_not_imply_mock_selector_gain() {
    let service = HybridRecallService::new(
        MockMemoryRepository::new(fixture()),
        MockVectorRepository {
            corpus: corpus(),
            minimum_cosine: 0.6,
        },
        MockEmbeddingClient { config: config() },
        MockLunaDecisionsClient::new(ProviderConfig::default()),
        MockQueryPlanner::new(true),
        RecallPolicy::default(),
        HybridPolicy {
            stream_limit: 8,
            fusion_limit: 4,
            rrf_k: 60,
        },
    );
    let output = support::ready_mock(service.recall(request("ship"))).unwrap();
    assert!(
        output
            .recall
            .candidate_ids
            .iter()
            .any(|id| id.as_str() == "deploy")
    );
    assert!(output.recall.evidence.selected.is_empty()); // selector is lexical overlap, not Luna.
}
#[test]
fn disabled_embedding_degrades_to_lexical_with_explicit_status() {
    let service = HybridRecallService::new(
        MockMemoryRepository::new(fixture()),
        MockVectorRepository {
            corpus: corpus(),
            minimum_cosine: 0.6,
        },
        DisabledOpenRouterEmbeddingClient { config: config() },
        MockLunaDecisionsClient::new(ProviderConfig::default()),
        MockQueryPlanner::new(true),
        RecallPolicy::default(),
        HybridPolicy {
            stream_limit: 8,
            fusion_limit: 4,
            rrf_k: 60,
        },
    );
    let output = support::ready_mock(service.recall(request("SurrealDB storage"))).unwrap();
    assert_eq!(output.vector_failure, Some(MemoryError::PaidCallsDisabled));
    assert!(
        output
            .recall
            .evidence
            .selected
            .iter()
            .any(|m| m.id.as_str() == "db")
    );
}
