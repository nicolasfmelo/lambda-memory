//! Consented historical reflection: free exact-plan inspection and watermark adjustment only.
use lambda_memory::domain_models::reflection::ReflectionBackfillPlan;
use lambda_memory::domain_models::runtime::RuntimeError;
use lambda_memory::integrations::database::embedded_capture::{
    EmbeddedCaptureStore, EmbeddedRuntimeStore,
};
use lambda_memory::integrations::runtime_config::RuntimeConfig;
use lambda_memory::services::runtime_service::MemoryRuntime;
use serde_json::{Value, json};
use std::io::Read;
fn invalid() -> RuntimeError {
    RuntimeError::InvalidInput("exact reflection backfill consent required")
}
fn plan_json(project: &str, plan: &ReflectionBackfillPlan) -> Value {
    json!({"project":project,"branches":plan.branches.iter().map(|b|b.as_str()).collect::<Vec<_>>(),"estimated_calls":plan.branches.len(),"accept_charges":false})
}
fn approved(request: &Value, expected: &Value) -> bool {
    let mut expected = expected.clone();
    expected["accept_charges"] = json!(true);
    request == &expected
}
pub async fn run(inspect: bool) -> Result<(), RuntimeError> {
    if std::env::args().count() != 2 {
        return Err(invalid());
    }
    let request = if inspect {
        None
    } else {
        let mut bytes = Vec::new();
        std::io::stdin()
            .take(1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| invalid())?;
        if bytes.len() > 1024 * 1024 {
            return Err(invalid());
        }
        Some(serde_json::from_slice::<Value>(&bytes).map_err(|_| invalid())?)
    };
    let config = RuntimeConfig::from_environment()?;
    if config.scope.projects.len() != 1 {
        return Err(invalid());
    }
    let capture = EmbeddedCaptureStore::open(&config.path)
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?;
    let runtime = MemoryRuntime::new(
        EmbeddedRuntimeStore::new(capture),
        config.scope,
        config.limits,
    )?;
    let project = &runtime.scope().projects[0];
    let plan = runtime.inspect_reflection_backfill(project).await?;
    let expected = plan_json(project.as_str(), &plan);
    if let Some(request) = request {
        if !approved(&request, &expected) {
            return Err(invalid());
        }
        let count = runtime.reflection_backfill(project, &plan).await?;
        println!("{}", json!({"project":project.as_str(),"branches":count}));
    } else {
        println!("{expected}");
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reflection_backfill_requires_consent() {
        let plan =
            json!({"project":"p","branches":["branch"],"estimated_calls":1,"accept_charges":false});
        assert!(!approved(&plan, &plan));
        let mut request = plan.clone();
        request["accept_charges"] = json!(true);
        assert!(approved(&request, &plan));
        for (key, value) in [
            ("project", json!("q")),
            ("branches", json!([])),
            ("estimated_calls", json!(0)),
            ("extra", json!(true)),
        ] {
            let mut changed = request.clone();
            changed[key] = value;
            assert!(!approved(&changed, &plan));
        }
    }
}
