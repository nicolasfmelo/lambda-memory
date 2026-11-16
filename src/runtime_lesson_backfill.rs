//! Free operator bookkeeping; exact consent authorizes the later lesson job's calls.
use lambda_memory::domain_models::runtime::RuntimeError;
use lambda_memory::domain_models::session::LessonBackfillPlan;
use lambda_memory::integrations::database::embedded_capture::{
    EmbeddedCaptureStore, EmbeddedRuntimeStore,
};
use lambda_memory::integrations::runtime_config::RuntimeConfig;
use lambda_memory::services::runtime_service::MemoryRuntime;
use serde_json::{Value, json};
use std::io::Read;
fn invalid() -> RuntimeError {
    RuntimeError::InvalidInput("exact lesson backfill consent required")
}
fn plan_json(project: &str, plan: &LessonBackfillPlan) -> Value {
    json!({"project":project,"sessions":plan.sessions.iter().map(|key| key.as_str()).collect::<Vec<_>>(),"estimated_calls":plan.estimated_calls(),"accept_charges":false})
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
    // Deliberately do not construct providers, initialize diagnostics or run idle work.
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
    let plan = runtime.inspect_lesson_backfill(project).await?;
    let expected = plan_json(project.as_str(), &plan);
    if let Some(request) = request {
        if !approved(&request, &expected) {
            return Err(invalid());
        }
        let count = runtime.lesson_backfill(project, &plan).await?;
        println!("{}", json!({"project":project.as_str(),"sessions":count}));
    } else {
        println!("{expected}");
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_lesson_backfill_consent() {
        let plan = json!({"project":"p","sessions":["0123456789abcdef"],"estimated_calls":1,"accept_charges":false});
        assert!(!approved(&plan, &plan));
        let mut consent = plan.clone();
        consent["accept_charges"] = json!(true);
        assert!(approved(&consent, &plan));
        for (key, value) in [
            ("project", json!("q")),
            ("sessions", json!([])),
            ("estimated_calls", json!(0)),
            ("extra", json!(true)),
        ] {
            let mut changed = consent.clone();
            changed[key] = value;
            assert!(!approved(&changed, &plan));
        }
    }
}
