//! OpenRouter DTOs are dressed here; runtime scope and review remain application-owned.
use super::router_http::{
    DEFAULT_BASE, INVALID_PROVIDER_RESPONSE, RUNTIME_SOURCE_SLOTS, RouterEmbeddingClient,
    RouterHttp, RouterLunaSelector, RouterQueryPlanner, validate_base_url,
};
use crate::domain_models::consolidation::JobSnapshot;
use crate::domain_models::embeddings::{Embedding, EmbeddingSpace};
use crate::domain_models::procedural::{FactKind, ProceduralFact};
use crate::domain_models::runtime::{RuntimeError, RuntimeHit};
use crate::domain_models::selection_context::{
    CandidateValidity, SelectionContext, SelectionIntent,
};
use crate::domain_models::{
    Memory, MemoryError, RecallRequest, SelectionRequest, SourceId, SupportedSelectionOutcome,
};
use crate::integrations::embedding_ports::EmbeddingClient;
use crate::integrations::memory_ports::MemoryQueryPlanner;
use crate::integrations::openrouter_embeddings::OpenRouterEmbeddingConfig;
use crate::integrations::provider_config::ProviderConfig;
use serde_json::{Value, json};

pub struct RouterRuntimeProviders {
    http: RouterHttp,
    planner: RouterQueryPlanner,
    embedder: RouterEmbeddingClient,
    selector: RouterLunaSelector,
}
impl RouterRuntimeProviders {
    pub fn new(
        secret: &str,
        models: ProviderConfig,
        dimensions: usize,
    ) -> Result<Self, RuntimeError> {
        Self::with_base(None, secret, models, dimensions)
    }
    /// `base` is the optional `LAMBDA_MEMORY_ROUTER_BASE_URL`; `None` keeps the OpenRouter default.
    pub fn with_base(
        base: Option<&str>,
        secret: &str,
        models: ProviderConfig,
        dimensions: usize,
    ) -> Result<Self, RuntimeError> {
        if !(128..=3072).contains(&dimensions) {
            return Err(RuntimeError::InvalidInput(
                "runtime embedding dimensions must be 128..3072",
            ));
        }
        if models.generator_candidates != ["anthropic/claude-haiku-5.5"]
            || models.embedding_model != "google/gemini-embedding-2"
            || models.selector_model != "openai/gpt-6-luna-decisions"
        {
            return Err(RuntimeError::InvalidInput(
                "unreviewed runtime model profile",
            ));
        }
        // Construction never contacts the provider: a failure here is local configuration.
        let base = base.unwrap_or(DEFAULT_BASE);
        validate_base_url(base)
            .map_err(|_| RuntimeError::InvalidInput("LAMBDA_MEMORY_ROUTER_BASE_URL rejected"))?;
        let http = RouterHttp::runtime_with_base(base, secret).map_err(|_| {
            RuntimeError::InvalidInput("OpenRouter key or client configuration rejected")
        })?;
        let planner =
            RouterQueryPlanner::new(http.clone(), models.generator_candidates[0].clone(), 256)
                .map_err(|_| {
                    RuntimeError::InvalidInput("OpenRouter planner configuration rejected")
                })?;
        Ok(Self {
            http: http.clone(),
            planner,
            embedder: RouterEmbeddingClient {
                http: http.clone(),
                config: OpenRouterEmbeddingConfig {
                    dimensions,
                    index_revision: "runtime-gemini-retrieval-v2".into(),
                },
            },
            selector: RouterLunaSelector { http },
        })
    }
    /// Same wiring as `new`, but over a loopback fake; compiled only for in-crate tests.
    #[cfg(test)]
    pub(crate) fn mock(base: &str, dimensions: usize) -> Self {
        let http = RouterHttp::mock_runtime(base);
        Self {
            http: http.clone(),
            planner: RouterQueryPlanner::new(
                http.clone(),
                "anthropic/claude-haiku-5.5".into(),
                256,
            )
            .unwrap(),
            embedder: RouterEmbeddingClient {
                http: http.clone(),
                config: OpenRouterEmbeddingConfig {
                    dimensions,
                    index_revision: "runtime-gemini-retrieval-v2".into(),
                },
            },
            selector: RouterLunaSelector { http },
        }
    }
    pub fn space(&self) -> Result<EmbeddingSpace, RuntimeError> {
        self.embedder
            .config
            .space()
            .map_err(|_| RuntimeError::ProviderUnavailable)
    }
    pub async fn derive(
        &self,
        snapshot: &JobSnapshot,
    ) -> Result<Vec<(SourceId, String)>, RuntimeError> {
        if snapshot.sources.is_empty() || snapshot.sources.len() > RUNTIME_SOURCE_SLOTS.len() {
            return Err(RuntimeError::InvalidInput("source-bound batch size"));
        }
        let input = json!({"sources": snapshot.sources.iter().enumerate().map(|(index, event)| json!({
            "source": RUNTIME_SOURCE_SLOTS[index], "kind": format!("{:?}", event.kind),
            "outcome": format!("{:?}", event.outcome), "content": event.content
        })).collect::<Vec<_>>()});
        let raw = self
            .http
            .consolidate_runtime(input)
            .await
            .map_err(|error| {
                if error.uncharged() {
                    RuntimeError::ProviderRejected
                } else {
                    RuntimeError::ProviderUnavailable
                }
            })?;
        decode_claims(&raw, snapshot)
    }

    /// Phrases deterministic facts as a playbook. The text is untrusted until the pure validator
    /// accepts it; errors map like `derive`.
    pub async fn playbook(
        &self,
        facts: &[ProceduralFact],
        current: Option<&str>,
    ) -> Result<String, RuntimeError> {
        if facts.is_empty() || facts.len() > 30 {
            return Err(RuntimeError::InvalidInput("playbook fact bounds"));
        }
        let input = json!({
            "current_playbook": current,
            "facts": facts.iter().map(|fact| json!({
                "id": fact.id, "kind": fact_kind(fact.kind), "key": fact.key, "detail": fact.detail,
                "successes": fact.successes, "failures": fact.failures, "sessions": fact.sessions
            })).collect::<Vec<_>>()
        });
        self.http.generate_playbook(input).await.map_err(|error| {
            if error.uncharged() {
                RuntimeError::ProviderRejected
            } else {
                RuntimeError::ProviderUnavailable
            }
        })
    }

    /// Search terms for one recall. `ProviderInvalidOutput` means the call completed and was
    /// billed but the model output was unusable (the only error the caller may degrade on);
    /// every other failure maps like the other provider calls.
    pub async fn plan(&self, request: &RecallRequest) -> Result<Vec<String>, RuntimeError> {
        self.planner
            .plan(request)
            .await
            .map(|plan| plan.terms)
            .map_err(|error| match error {
                MemoryError::InvalidInput(reason) if reason == INVALID_PROVIDER_RESPONSE => {
                    RuntimeError::ProviderInvalidOutput
                }
                other => provider_error(other),
            })
    }
    pub async fn embed_document(&self, content: &str) -> Result<Embedding, RuntimeError> {
        self.embedder
            .embed(&document_input(content))
            .await
            .map_err(provider_error)
    }
    pub async fn embed_query(&self, content: &str) -> Result<Embedding, RuntimeError> {
        self.embedder
            .embed(&query_input(content))
            .await
            .map_err(provider_error)
    }
    pub async fn select(
        &self,
        request: &RecallRequest,
        terms: &[String],
        hits: &[RuntimeHit],
        now_ms: u64,
    ) -> Result<(Vec<Memory>, SupportedSelectionOutcome), RuntimeError> {
        let candidates = hits
            .iter()
            .map(|hit| {
                let revision = &hit.item.revision;
                Ok(Memory {
                    id: crate::domain_models::MemoryId::new(revision.id.as_str())
                        .map_err(|_| RuntimeError::StorageUnavailable)?,
                    owner: revision.owner.clone(),
                    actor: request.actor.clone(),
                    project: revision.project.clone(),
                    text: revision.content.clone(),
                    source: revision
                        .sources
                        .first()
                        .ok_or(RuntimeError::StorageUnavailable)?
                        .clone(),
                    active: true,
                    restricted: false,
                })
            })
            .collect::<Result<Vec<_>, RuntimeError>>()?;
        let validity = candidates
            .iter()
            .zip(hits)
            .map(|(memory, hit)| CandidateValidity {
                memory: memory.id.clone(),
                source: memory.source.clone(),
                owner: memory.owner.clone(),
                project: memory.project.clone(),
                valid_from_ms: hit
                    .item
                    .sources
                    .iter()
                    .map(|source| source.recorded_at_ms)
                    .max()
                    .unwrap_or(now_ms),
                expires_at_ms: None,
            })
            .collect();
        let context = SelectionContext {
            intent: SelectionIntent::TopicRecall,
            owner: request.owner.clone(),
            project: request.project.clone(),
            as_of_ms: now_ms,
            validity,
        };
        let selection = SelectionRequest {
            task: request.task.clone(),
            retrieval_terms: terms.to_vec(),
            candidates,
        };
        let answer = self
            .selector
            .evaluate_contractual(&selection, &context)
            .await
            .map_err(provider_error)?;
        Ok((selection.candidates, answer))
    }
}
fn fact_kind(kind: FactKind) -> &'static str {
    match kind {
        FactKind::Procedure => "procedure",
        FactKind::Pitfall => "pitfall",
        FactKind::HotFile => "hot_file",
    }
}
fn query_input(content: &str) -> String {
    format!("task: search result | query: {content}")
}
fn document_input(content: &str) -> String {
    format!("title: none | text: {content}")
}
fn provider_error(error: MemoryError) -> RuntimeError {
    if error == MemoryError::ProviderRejected {
        RuntimeError::ProviderRejected
    } else {
        RuntimeError::ProviderUnavailable
    }
}
fn decode_claims(
    raw: &str,
    snapshot: &JobSnapshot,
) -> Result<Vec<(SourceId, String)>, RuntimeError> {
    if snapshot.sources.is_empty() || snapshot.sources.len() > RUNTIME_SOURCE_SLOTS.len() {
        return Err(RuntimeError::ProviderUnavailable);
    }
    let Value::Object(mut value) =
        serde_json::from_str(raw).map_err(|_| RuntimeError::ProviderUnavailable)?
    else {
        return Err(RuntimeError::ProviderUnavailable);
    };
    if value.len() != 1 {
        return Err(RuntimeError::ProviderUnavailable);
    }
    let Some(Value::Object(mut claims)) = value.remove("claims") else {
        return Err(RuntimeError::ProviderUnavailable);
    };
    if claims.len() != snapshot.sources.len() {
        return Err(RuntimeError::ProviderUnavailable);
    }
    let mut output = Vec::with_capacity(claims.len());
    for (index, event) in snapshot.sources.iter().enumerate() {
        let Some(Value::String(content)) = claims.remove(RUNTIME_SOURCE_SLOTS[index]) else {
            return Err(RuntimeError::ProviderUnavailable);
        };
        if content.trim().is_empty() || content.len() > 8192 {
            return Err(RuntimeError::ProviderUnavailable);
        }
        output.push((event.source.clone(), content));
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain_models::capture::{CaptureEvent, WorkEventKind, WorkOutcome};
    use crate::domain_models::{ActorId, JobId, OwnerId, ProjectId};
    #[test]
    fn retrieval_inputs_and_dimension_profile_are_explicit() {
        assert_eq!(
            query_input("storage?"),
            "task: search result | query: storage?"
        );
        assert_eq!(
            document_input("Use SQLite."),
            "title: none | text: Use SQLite."
        );
        for dimensions in [128, 1536, 3072] {
            let providers = RouterRuntimeProviders::new(
                "synthetic-unused-key",
                ProviderConfig::default(),
                dimensions,
            )
            .unwrap();
            let space = providers.space().unwrap();
            assert_eq!(space.dimensions, dimensions);
            assert_eq!(space.revision, "runtime-gemini-retrieval-v2");
        }
        assert!(
            RouterRuntimeProviders::with_base(
                Some("http://example.com"),
                "synthetic-unused-key",
                ProviderConfig::default(),
                128
            )
            .is_err()
        );
        assert!(
            RouterRuntimeProviders::with_base(
                Some("https://proxy.example.com:8443/"),
                "synthetic-unused-key",
                ProviderConfig::default(),
                128
            )
            .is_ok()
        );
        for dimensions in [0, 127, 3073] {
            assert!(
                RouterRuntimeProviders::new(
                    "synthetic-unused-key",
                    ProviderConfig::default(),
                    dimensions
                )
                .is_err()
            );
        }
    }
    #[test]
    fn model_claims_require_exact_slots_and_map_to_native_source_identity() {
        let snapshot = JobSnapshot {
            id: JobId::new("job").unwrap(),
            owner: OwnerId::new("owner").unwrap(),
            project: ProjectId::new("project").unwrap(),
            watermark_ms: 1,
            sources: ["one", "two"]
                .into_iter()
                .map(|source| CaptureEvent {
                    source: SourceId::new(source).unwrap(),
                    owner: OwnerId::new("owner").unwrap(),
                    actor: ActorId::new("actor").unwrap(),
                    project: ProjectId::new("project").unwrap(),
                    recorded_at_ms: 1,
                    kind: WorkEventKind::Error,
                    outcome: WorkOutcome::Failed,
                    origin: "fixture://failed".into(),
                    content: "Migration failed; rollback succeeded.".into(),
                })
                .collect(),
        };
        let valid = r#"{"claims":{"s1":"Migration failed.","s0":"Rollback succeeded after the failed migration."}}"#;
        let claims = decode_claims(valid, &snapshot).unwrap();
        assert_eq!(
            claims,
            vec![
                (
                    snapshot.sources[0].source.clone(),
                    "Rollback succeeded after the failed migration.".into()
                ),
                (
                    snapshot.sources[1].source.clone(),
                    "Migration failed.".into()
                ),
            ]
        );
        for invalid in [
            r#"{"claims":{"s0":"Claim.","foreign":"Another."}}"#,
            r#"{"claims":{"s0":"Claim.","s2":"Another."}}"#,
            r#"{"claims":{"s0":"Claim."}}"#,
            r#"{"claims":{"s0":"","s1":"Another."}}"#,
            r#"{"claims":{"s0":["Claim.","Another."],"s1":"Another."}}"#,
            r#"{"claims":{"s0":"Claim.","s1":"Another.","s2":"Extra."}}"#,
            r#"{"claims":[{"source":"one","content":"Claim."},{"source":"two","content":"Another."}]}"#,
        ] {
            assert_eq!(
                decode_claims(invalid, &snapshot).unwrap_err(),
                RuntimeError::ProviderUnavailable
            );
        }
    }
}
