//! Explicit operator consent for migrating flat knowledge into a tree.
use lambda_memory::domain_models::memory_tree::TreeMode;
use lambda_memory::domain_models::runtime::RuntimeError;
use serde_json::{Value, json};
use std::io::Read;

fn invalid() -> RuntimeError {
    RuntimeError::InvalidInput("exact tree migration consent required")
}
fn plan_json(
    project: &str,
    plan: &lambda_memory::services::runtime_service::tree_migration::TreeMigrationPlan,
) -> Value {
    json!({"project":project,"mode":match plan.mode { TreeMode::Flat => "flat", TreeMode::Tree => "tree" },"flat_memories":plan.flat_memories,"sources":plan.sources.iter().map(|s| s.as_str()).collect::<Vec<_>>(),"batches":plan.batches,"accept_charges":false})
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
        let value: Value = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
        Some(value)
    };
    let (runtime, _) = super::runtime_launcher::configured_runtime().await?;
    if runtime.scope().projects.len() != 1 {
        return Err(invalid());
    }
    let project = &runtime.scope().projects[0];
    let plan = runtime.inspect_tree(project).await?;
    let expected = plan_json(project.as_str(), &plan);
    // Resuming an apply: a project already in tree mode is done, and no paid work is possible.
    if request.is_some() && plan.mode == TreeMode::Tree {
        println!(
            "{}",
            json!({"project":project.as_str(),"outcome":"succeeded","already_migrated":true,"batches":0,"sources":0})
        );
        return Ok(());
    }
    if let Some(request) = request {
        if !approved(&request, &expected) {
            return Err(invalid());
        }
        let report = runtime.curate_existing(project, plan.sources).await?;
        println!(
            "{}",
            json!({"project":project.as_str(),"outcome":"succeeded","batches":report.batches,"sources":report.sources})
        );
    } else {
        println!("{expected}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn curate_existing_requires_exact_consent() {
        let plan = json!({"project":"p","mode":"flat","flat_memories":1,"sources":["s"],"batches":1,"accept_charges":false});
        assert!(!approved(&plan, &plan));
        let mut consent = plan.clone();
        consent["accept_charges"] = json!(true);
        assert!(approved(&consent, &plan));
        for (key, value) in [
            ("project", json!("other")),
            ("sources", json!([])),
            ("mode", json!("tree")),
            ("batches", json!(2)),
        ] {
            let mut changed = consent.clone();
            changed[key] = value;
            assert!(!approved(&changed, &plan));
        }
    }
}
