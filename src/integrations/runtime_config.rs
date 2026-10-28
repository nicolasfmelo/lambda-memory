//! Operator configuration, never populated from MCP caller arguments.
use crate::domain_models::runtime::{RuntimeError, RuntimeLimits, RuntimeScope};
use crate::domain_models::{ActorId, OwnerId, ProjectId};

pub struct RuntimeConfig {
    pub path: String,
    pub token: String,
    pub scope: RuntimeScope,
    pub limits: RuntimeLimits,
    pub embedding_dimensions: usize,
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
fn positive_count(name: &'static str, default: usize) -> Result<usize, RuntimeError> {
    let value = count(name, default)?;
    if value == 0 {
        Err(RuntimeError::InvalidInput(name))
    } else {
        Ok(value)
    }
}
impl RuntimeConfig {
    pub fn from_environment() -> Result<Self, RuntimeError> {
        if std::env::var_os("LAMBDA_MEMORY_CONTEXT_TOKENS").is_some() {
            return Err(RuntimeError::InvalidInput(
                "LAMBDA_MEMORY_CONTEXT_TOKENS removed; use LAMBDA_MEMORY_CONTEXT_BYTES",
            ));
        }
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
            context_bytes: count("LAMBDA_MEMORY_CONTEXT_BYTES", defaults.context_bytes)?,
            dream_batch: count("LAMBDA_MEMORY_DREAM_BATCH", defaults.dream_batch)?,
            tree_max_children: positive_count(
                "LAMBDA_MEMORY_TREE_MAX_CHILDREN",
                defaults.tree_max_children,
            )?,
            tree_max_depth: u8::try_from(positive_count(
                "LAMBDA_MEMORY_TREE_MAX_DEPTH",
                defaults.tree_max_depth as usize,
            )?)
            .map_err(|_| RuntimeError::InvalidInput("LAMBDA_MEMORY_TREE_MAX_DEPTH"))?,
            tree_split_sources: positive_count(
                "LAMBDA_MEMORY_TREE_SPLIT_SOURCES",
                defaults.tree_split_sources,
            )?,
            tree_map_bytes: positive_count(
                "LAMBDA_MEMORY_TREE_MAP_BYTES",
                defaults.tree_map_bytes,
            )?,
            curation_batch: positive_count(
                "LAMBDA_MEMORY_CURATION_BATCH",
                defaults.curation_batch,
            )?,
            idle_ms: number("LAMBDA_MEMORY_IDLE_MS", defaults.idle_ms)?,
            tick_ms: number("LAMBDA_MEMORY_TICK_MS", defaults.tick_ms)?,
            retention_ms,
        };
        let embedding_dimensions = count("LAMBDA_MEMORY_EMBEDDING_DIMENSIONS", 1536)?;
        if !(128..=3072).contains(&embedding_dimensions) {
            return Err(RuntimeError::InvalidInput(
                "LAMBDA_MEMORY_EMBEDDING_DIMENSIONS must be 128..3072",
            ));
        }
        Ok(Self {
            path,
            token,
            scope: RuntimeScope {
                owner,
                actor,
                projects,
            },
            limits,
            embedding_dimensions,
        })
    }
}
