//! Opt-in resident composition root; never configures or installs OMP.
use lambda_memory::domain_models::runtime::RuntimeError;
use lambda_memory::engines::runtime_policy;
use lambda_memory::integrations::database::embedded_capture::{
    EmbeddedCaptureStore, EmbeddedRuntimeStore,
};
use lambda_memory::integrations::runtime_config::RuntimeConfig;
use lambda_memory::services::runtime_service::MemoryRuntime;
use std::sync::Arc;

pub(crate) async fn configured_runtime() -> Result<(Arc<MemoryRuntime>, String), RuntimeError> {
    let config = RuntimeConfig::from_environment()?;
    runtime_policy::validate_config(&config.scope, &config.limits)?;
    let capture = EmbeddedCaptureStore::open(&config.path)
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?;
    let store = EmbeddedRuntimeStore::new(capture);
    let mode = match std::env::var("LAMBDA_MEMORY_MODE") {
        Ok(mode) => mode,
        Err(std::env::VarError::NotPresent) => "local".into(),
        Err(_) => return Err(RuntimeError::InvalidInput("LAMBDA_MEMORY_MODE")),
    };
    let runtime = Arc::new(match mode.as_str() {
        "local" => MemoryRuntime::new(store, config.scope, config.limits)?,
        "openrouter" => {
            #[cfg(feature = "router-http")]
            {
                let models =
                    lambda_memory::integrations::provider_config::ProviderConfig::from_environment(
                    )
                    .map_err(RuntimeError::InvalidInput)?;
                let secret = std::env::var("OPEN_ROUTER_KEY").map_err(|_| {
                    RuntimeError::InvalidInput("OPEN_ROUTER_KEY required for openrouter mode")
                })?;
                // Optional validated base URL; an invalid value fails startup without being echoed.
                let base = match std::env::var("LAMBDA_MEMORY_ROUTER_BASE_URL") {
                    Ok(base) => Some(base),
                    Err(std::env::VarError::NotPresent) => None,
                    Err(_) => {
                        return Err(RuntimeError::InvalidInput("LAMBDA_MEMORY_ROUTER_BASE_URL"));
                    }
                };
                let providers = lambda_memory::integrations::external_apis::router_runtime::RouterRuntimeProviders::with_base(base.as_deref(), &secret, models, config.embedding_dimensions)?;
                MemoryRuntime::with_openrouter(store, config.scope, config.limits, providers)?
            }
            #[cfg(not(feature = "router-http"))]
            {
                return Err(RuntimeError::InvalidInput(
                    "openrouter runtime requires router-http feature",
                ));
            }
        }
        _ => {
            return Err(RuntimeError::InvalidInput(
                "LAMBDA_MEMORY_MODE must be local or openrouter",
            ));
        }
    });
    Ok((runtime, config.token))
}

pub async fn run() -> Result<(), RuntimeError> {
    let (runtime, token) = configured_runtime().await?;
    runtime.initialize_diagnostics().await?;
    let (stop, mut stopped) = tokio::sync::watch::channel(false);
    let worker_runtime = Arc::clone(&runtime);
    let worker = tokio::spawn(async move {
        // Background failures never end the MCP session; consecutive failures back off up to 64 ticks.
        let mut failures: u32 = 0;
        loop {
            let tick_ms = worker_runtime.limits().tick_ms;
            let delay_ms = tick_ms.saturating_mul(1 << failures.min(6));
            tokio::select! {
                changed = stopped.changed() => {
                    if changed.is_err() || *stopped.borrow() { return Ok::<(), RuntimeError>(()); }
                }
                _ = tokio::time::sleep(std::time::Duration::from_millis(delay_ms)) => {}
            }
            // A native batch is bounded and finishes before the worker accepts shutdown.
            let mut failed = false;
            if let Err(error) = worker_runtime.maintain_diagnostics().await {
                eprintln!(
                    "Memory diagnostics maintenance failed ({error}); MCP stays available and maintenance backs off."
                );
                failed = true;
            }
            match worker_runtime.dream_once().await.result {
                Ok(_) => {}
                Err(RuntimeError::ProviderRejected) => {
                    eprintln!(
                        "Memory provider rejected work without charge; sources stay pending and work backs off."
                    );
                    failed = true;
                }
                Err(RuntimeError::ProviderUnavailable | RuntimeError::UncertainAdmission) => {
                    eprintln!(
                        "Memory provider work stopped; admitted originals stay blocked until operator recovery and paid operations are not automatically retried."
                    );
                }
                Err(error) => {
                    eprintln!(
                        "Memory background work failed ({error}); MCP stays available and work backs off."
                    );
                    failed = true;
                }
            }
            failures = if failed {
                failures.saturating_add(1)
            } else {
                0
            };
        }
    });
    let mut worker = worker;
    let result = tokio::select! {
        result = lambda_memory::delivery::mcp::serve(runtime, token) => {
            let _ = stop.send(true);
            let worker_result = worker.await.map_err(|_| RuntimeError::StorageUnavailable)?;
            result.and(worker_result)
        }
        worker_result = &mut worker => {
            worker_result.map_err(|_| RuntimeError::StorageUnavailable)?
        }
    };
    result
}
