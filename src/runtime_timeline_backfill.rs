//! Free, single-project operator backfill; no providers or paid admission.
use lambda_memory::domain_models::runtime::RuntimeError;
use lambda_memory::integrations::database::embedded_capture::{
    EmbeddedCaptureStore, EmbeddedRuntimeStore,
};
use lambda_memory::integrations::runtime_config::RuntimeConfig;
use lambda_memory::services::runtime_service::MemoryRuntime;
use serde_json::json;

pub async fn run() -> Result<(), RuntimeError> {
    let args: Vec<String> = std::env::args().skip(2).collect();
    let apply = match args.as_slice() {
        [] => false,
        [flag] if flag == "--apply" => true,
        _ => {
            return Err(RuntimeError::InvalidInput(
                "usage: --backfill-timeline [--apply]",
            ));
        }
    };
    let config = RuntimeConfig::from_environment()?;
    let capture = EmbeddedCaptureStore::open(&config.path)
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?;
    let store = EmbeddedRuntimeStore::new(capture).with_timeline_policy(
        lambda_memory::engines::timeline_policy::AUTO_POLICY,
        config.limits.max_content_bytes,
    );
    let summary_limit = config.limits.max_content_bytes;
    let runtime = MemoryRuntime::new(store.clone(), config.scope, config.limits)?;
    if runtime.scope().projects.len() != 1 {
        return Err(RuntimeError::InvalidInput(
            "timeline backfill requires one configured project",
        ));
    }
    let scope = runtime.scope();
    let project = &scope.projects[0];
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| RuntimeError::StorageUnavailable)?
        .as_millis() as u64;
    let (eligible, created) = store
        .backfill_timeline(
            &scope.owner,
            &scope.actor,
            project,
            apply,
            now_ms,
            summary_limit,
        )
        .await?;
    println!(
        "{}",
        json!({"project": project.as_str(), "eligible": eligible, "created": created})
    );
    Ok(())
}
