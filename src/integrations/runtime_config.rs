//! Operator configuration, never populated from MCP caller arguments.
use crate::domain_models::runtime::{RuntimeError, RuntimeLimits, RuntimeScope};
use crate::domain_models::{ActorId, OwnerId, ProjectId};

pub struct RuntimeConfig {
    pub path: String,
    pub token: String,
    pub scope: RuntimeScope,
    pub limits: RuntimeLimits,
}
fn required(name: &'static str) -> Result<String, RuntimeError> {
    std::env::var(name)
        .map_err(|_| RuntimeError::InvalidInput(name))
        .and_then(|value| {
            if value.trim().is_empty() {
                Err(RuntimeError::InvalidInput(name))
            } else {
                Ok(value)
            }
        })
}
fn number(name: &'static str, default: u64) -> Result<u64, RuntimeError> {
    match std::env::var(name) {
        Ok(value) => value.parse().map_err(|_| RuntimeError::InvalidInput(name)),
        Err(std::env::VarError::NotPresent) => Ok(default),
        Err(_) => Err(RuntimeError::InvalidInput(name)),
    }
}
fn count(name: &'static str, default: usize) -> Result<usize, RuntimeError> {
    usize::try_from(number(name, default as u64)?).map_err(|_| RuntimeError::InvalidInput(name))
}
impl RuntimeConfig {
    pub fn from_environment() -> Result<Self, RuntimeError> {
        let path = required("LAMBDA_MEMORY_DATA_DIR")?;
        let token = required("LAMBDA_MEMORY_TOKEN")?;
        if !(32..=1024).contains(&token.len())
            || !token.bytes().all(|byte| (0x21..=0x7e).contains(&byte))
        {
            return Err(RuntimeError::InvalidInput(
                "LAMBDA_MEMORY_TOKEN must be 32..1024 printable non-whitespace ASCII bytes",
            ));
        }
        let owner = OwnerId::new(required("LAMBDA_MEMORY_OWNER")?)
            .map_err(|_| RuntimeError::InvalidInput("LAMBDA_MEMORY_OWNER"))?;
        let actor = ActorId::new(required("LAMBDA_MEMORY_ACTOR")?)
            .map_err(|_| RuntimeError::InvalidInput("LAMBDA_MEMORY_ACTOR"))?;
        let project = ProjectId::new(required("LAMBDA_MEMORY_PROJECT")?)
            .map_err(|_| RuntimeError::InvalidInput("LAMBDA_MEMORY_PROJECT"))?;
        let mut projects = vec![project];
        match std::env::var("LAMBDA_MEMORY_ALLOWED_PROJECTS") {
            Ok(value) => {
                if value.trim().is_empty() {
                    return Err(RuntimeError::InvalidInput("LAMBDA_MEMORY_ALLOWED_PROJECTS"));
                }
                for value in value.split(',') {
                    projects.push(ProjectId::new(value.trim()).map_err(|_| {
                        RuntimeError::InvalidInput("LAMBDA_MEMORY_ALLOWED_PROJECTS")
                    })?);
                }
            }
            Err(std::env::VarError::NotPresent) => {}
            Err(_) => return Err(RuntimeError::InvalidInput("LAMBDA_MEMORY_ALLOWED_PROJECTS")),
        }
        let defaults = RuntimeLimits::default();
        let retention_ms = match std::env::var("LAMBDA_MEMORY_RETENTION_MS") {
            Ok(value) => Some(
                value
                    .parse()
                    .map_err(|_| RuntimeError::InvalidInput("LAMBDA_MEMORY_RETENTION_MS"))?,
            ),
            Err(std::env::VarError::NotPresent) => None,
            Err(_) => return Err(RuntimeError::InvalidInput("LAMBDA_MEMORY_RETENTION_MS")),
        };
        let limits = RuntimeLimits {
            max_content_bytes: count(
                "LAMBDA_MEMORY_MAX_CONTENT_BYTES",
                defaults.max_content_bytes,
            )?,
            max_pending: count("LAMBDA_MEMORY_MAX_PENDING", defaults.max_pending)?,
            max_candidates: count("LAMBDA_MEMORY_MAX_CANDIDATES", defaults.max_candidates)?,
            context_tokens: count("LAMBDA_MEMORY_CONTEXT_TOKENS", defaults.context_tokens)?,
            dream_batch: count("LAMBDA_MEMORY_DREAM_BATCH", defaults.dream_batch)?,
            idle_ms: number("LAMBDA_MEMORY_IDLE_MS", defaults.idle_ms)?,
            tick_ms: number("LAMBDA_MEMORY_TICK_MS", defaults.tick_ms)?,
            retention_ms,
        };
        Ok(Self {
            path,
            token,
            scope: RuntimeScope {
                owner,
                actor,
                projects,
            },
            limits,
        })
    }
}
