//! Operator-only CLI: list, then reject, the legacy per-event tool-metadata revisions.
//! No MCP method can dispatch it. Retirement moves revisions to `rejected`; sources stay.
use lambda_memory::domain_models::runtime::RuntimeError;
use lambda_memory::integrations::database::embedded_capture::{
    EmbeddedCaptureStore, EmbeddedRuntimeStore,
};
use lambda_memory::integrations::runtime_config::RuntimeConfig;
use lambda_memory::services::runtime_service::MemoryRuntime;
use serde_json::json;

const BATCH: usize = 32;
const SAMPLES: usize = 5;

fn invalid() -> RuntimeError {
    RuntimeError::InvalidInput("usage: --reject-tool-metadata [--apply]")
}

pub async fn run() -> Result<(), RuntimeError> {
    let args: Vec<String> = std::env::args().skip(2).collect();
    let apply = match args.as_slice() {
        [] => false,
        [flag] if flag == "--apply" => true,
        _ => return Err(invalid()),
    };
    // Same environment as the other operator commands; no provider is needed to reject.
    let config = RuntimeConfig::from_environment()?;
    let capture = EmbeddedCaptureStore::open(&config.path)
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?;
    let store = EmbeddedRuntimeStore::new(capture);
    let runtime = MemoryRuntime::new(store.clone(), config.scope, config.limits)?;
    if runtime.scope().projects.len() != 1 {
        return Err(RuntimeError::InvalidInput(
            "tool-metadata rejection requires one configured project",
        ));
    }
    let owner = runtime.scope().owner.clone();
    let project = runtime.scope().projects[0].clone();
    if !apply {
        let count = store.tool_metadata_count(&owner, &project).await?;
        let mut samples = Vec::new();
        for id in store
            .tool_metadata_revisions(&owner, &project, BATCH)
            .await?
            .into_iter()
            .take(SAMPLES)
        {
            let item = store.runtime_revision(&owner, &project, &id).await?;
            samples.push(json!({"revision": id.as_str(), "content": item.revision.content}));
        }
        println!(
            "{}",
            json!({"project": project.as_str(), "count": count, "samples": samples})
        );
        return Ok(());
    }
    let actor = runtime.scope().actor.clone();
    let mut rejected = 0usize;
    loop {
        let batch = store
            .tool_metadata_revisions(&owner, &project, BATCH)
            .await?;
        if batch.is_empty() {
            break;
        }
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| RuntimeError::StorageUnavailable)?
            .as_millis() as u64;
        let retired = store
            .retire_tool_metadata(&owner, &actor, &project, &batch, now_ms)
            .await?;
        if retired == 0 {
            // The listing and the re-check disagree; stop instead of looping.
            return Err(RuntimeError::Conflict);
        }
        rejected += retired;
    }
    println!("{}", json!({"rejected": rejected}));
    Ok(())
}
