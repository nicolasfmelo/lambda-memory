use crate::domain_models::MemoryError;
use crate::domain_models::embeddings::{Embedding, EmbeddingSpace};
use crate::integrations::embedding_ports::EmbeddingClient;
#[derive(Clone, Debug)]
pub struct OpenRouterEmbeddingConfig {
    pub dimensions: usize,
    pub index_revision: String,
}
impl OpenRouterEmbeddingConfig {
    pub fn space(&self) -> Result<EmbeddingSpace, MemoryError> {
        if !(128..=3072).contains(&self.dimensions) || self.index_revision.trim().is_empty() {
            return Err(MemoryError::InvalidInput(
                "invalid Gemini dimensions/index revision",
            ));
        }
        Ok(EmbeddingSpace {
            provider: "openrouter".into(),
            model: "google/gemini-embedding-2".into(),
            revision: self.index_revision.clone(),
            dimensions: self.dimensions,
        })
    }
}
#[derive(Debug)]
struct EmbeddingRequest {
    model: String,
    input: String,
    dimensions: usize,
    encoding_format: &'static str,
}
#[derive(Debug)]
struct EmbeddingResponse {
    model: String,
    index: usize,
    values: Vec<f64>,
}
fn dress_request(
    text: &str,
    config: &OpenRouterEmbeddingConfig,
) -> Result<EmbeddingRequest, MemoryError> {
    config.space()?;
    // Byte guard is an offline limit, not Gemini's 8192-token accounting.
    if text.trim().is_empty() || text.len() > 16000 {
        return Err(MemoryError::InvalidInput(
            "embedding input outside byte guard",
        ));
    }
    Ok(EmbeddingRequest {
        model: "google/gemini-embedding-2".into(),
        input: text.into(),
        dimensions: config.dimensions,
        encoding_format: "float",
    })
}
fn dress_response(
    response: EmbeddingResponse,
    space: EmbeddingSpace,
) -> Result<Embedding, MemoryError> {
    if response.model != space.model
        || response.index != 0
        || response.values.len() != space.dimensions
        || response.values.iter().any(|v| !v.is_finite())
        || response.values.iter().all(|v| *v == 0.0)
    {
        return Err(MemoryError::InvalidInput("invalid embedding response"));
    }
    Ok(Embedding {
        space,
        values: response.values,
    })
}
/// No live transport. Request preparation can be tested; every call stays disabled.
pub struct DisabledOpenRouterEmbeddingClient {
    pub config: OpenRouterEmbeddingConfig,
}
impl EmbeddingClient for DisabledOpenRouterEmbeddingClient {
    async fn embed(&self, text: &str) -> Result<Embedding, MemoryError> {
        let request = dress_request(text, &self.config)?;
        let _ = (
            request.model,
            request.input,
            request.dimensions,
            request.encoding_format,
        );
        Err(MemoryError::PaidCallsDisabled)
    }
}
/// Disclosed toy concept buckets for tests, no Gemini output.
#[cfg(any(test, feature = "test-support"))]
pub struct MockEmbeddingClient {
    pub config: OpenRouterEmbeddingConfig,
}
#[cfg(any(test, feature = "test-support"))]
impl EmbeddingClient for MockEmbeddingClient {
    async fn embed(&self, text: &str) -> Result<Embedding, MemoryError> {
        let request = dress_request(text, &self.config)?;
        if request.encoding_format != "float" {
            return Err(MemoryError::InvalidInput("mock encoding"));
        }
        let mut values = vec![0.0; request.dimensions];
        for word in request
            .input
            .to_lowercase()
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| !w.is_empty())
        {
            let bucket = match word {
                "database" | "storage" | "surrealdb" | "durable" | "persist" | "persists" => {
                    Some(0)
                }
                "memory" | "remembered" | "knowledge" => Some(1),
                "luna" | "selection" | "selector" | "decisions" => Some(2),
                "deploy" | "release" | "ship" | "delivery" => Some(3),
                _ => None,
            };
            if let Some(index) = bucket {
                values[index] += 1.0;
            }
        }
        if values.iter().all(|v| *v == 0.0) {
            values[4] = 1.0;
        }
        let norm = values.iter().map(|v| v * v).sum::<f64>().sqrt();
        for value in &mut values {
            *value /= norm;
        }
        dress_response(
            EmbeddingResponse {
                model: request.model,
                index: 0,
                values,
            },
            self.config.space()?,
        )
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn private_wire_validates_model_shape_and_finiteness() {
        let config = OpenRouterEmbeddingConfig {
            dimensions: 128,
            index_revision: "mock-concepts-v1".into(),
        };
        let request = dress_request("query", &config).unwrap();
        assert_eq!(request.model, "google/gemini-embedding-2");
        assert_eq!(request.encoding_format, "float");
        for (model, index, mut values) in [
            ("wrong", 0, vec![1.0; 128]),
            ("google/gemini-embedding-2", 1, vec![1.0; 128]),
            ("google/gemini-embedding-2", 0, vec![1.0; 127]),
            ("google/gemini-embedding-2", 0, vec![0.0; 128]),
        ] {
            assert!(
                dress_response(
                    EmbeddingResponse {
                        model: model.into(),
                        index,
                        values: values.clone()
                    },
                    config.space().unwrap()
                )
                .is_err()
            );
            values[0] = f64::NAN;
            assert!(
                dress_response(
                    EmbeddingResponse {
                        model: "google/gemini-embedding-2".into(),
                        index: 0,
                        values
                    },
                    config.space().unwrap()
                )
                .is_err()
            );
        }
    }
}
