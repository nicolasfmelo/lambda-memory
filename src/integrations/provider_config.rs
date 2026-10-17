//! Infrastructure coordinates and candidate models; no secrets or live transports.
#[derive(Clone, Debug)]
pub struct ProviderConfig {
    pub surreal_endpoint: String,
    pub namespace: String,
    pub database: String,
    pub selector_model: String,
    pub embedding_model: String,
    pub generator_candidates: Vec<String>,
}
impl Default for ProviderConfig {
    fn default() -> Self {
        Self {
            surreal_endpoint: "surrealkv:///var/lib/lambda-memory/store".into(),
            namespace: "lambda_memory".into(),
            database: "experiment".into(),
            selector_model: "openai/gpt-6-luna-decisions".into(),
            embedding_model: "google/gemini-embedding-2".into(),
            generator_candidates: vec!["anthropic/claude-haiku-5.5".into()],
        }
    }
}

/// Model selection is explicit but pinned: any value other than the fixed profile is rejected.
impl ProviderConfig {
    pub fn from_lookup(
        mut lookup: impl FnMut(&str) -> Option<String>,
    ) -> Result<Self, &'static str> {
        let mut config = Self::default();
        for (name, expected) in [
            ("LAMBDA_PLANNER_MODEL", "anthropic/claude-haiku-5.5"),
            ("LAMBDA_EMBEDDING_MODEL", "google/gemini-embedding-2"),
            ("LAMBDA_SELECTOR_MODEL", "openai/gpt-6-luna-decisions"),
        ] {
            let value = lookup(name).unwrap_or_else(|| expected.into());
            if value != expected {
                return Err("model environment override does not match the pinned model profile");
            }
            if name == "LAMBDA_PLANNER_MODEL" {
                config.generator_candidates = vec![value];
            } else if name == "LAMBDA_SELECTOR_MODEL" {
                config.selector_model = value;
            } else {
                config.embedding_model = value;
            }
        }
        Ok(config)
    }
    pub fn from_environment() -> Result<Self, &'static str> {
        Self::from_lookup(|name| std::env::var(name).ok())
    }
}
