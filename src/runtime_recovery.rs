//! Operator-only CLI: inspect exact blocked identities, then execute one explicitly approved batch.
use lambda_memory::domain_models::runtime::RuntimeError;
use lambda_memory::domain_models::{JobId, ProjectId, SourceId};
use serde_json::{Value, json};
use std::io::Read;

const INPUT_LIMIT: u64 = 32 * 1024;

pub async fn run(inspect: bool) -> Result<(), RuntimeError> {
    if std::env::args().count() != 2 {
        return Err(RuntimeError::InvalidInput("recovery command arguments"));
    }
    // Reject missing consent before opening the store or constructing providers.
    let request = if inspect { None } else { Some(request()?) };
    let (runtime, _) = super::runtime_launcher::configured_runtime().await?;
    if runtime.scope().projects.len() != 1 {
        return Err(RuntimeError::InvalidInput(
            "recovery requires one configured project",
        ));
    }
    let project = &runtime.scope().projects[0];
    if inspect {
        let batch = runtime.dream_recovery_plan(project).await?;
        println!(
            "{}",
            json!({
                "project": project.as_str(),
                "sources": batch.sources.iter().map(|s| s.source.as_str()).collect::<Vec<_>>(),
                "previous_operations": batch.previous_operations.iter().map(JobId::as_str).collect::<Vec<_>>(),
                "accept_duplicate_charge": false,
            })
        );
    } else if let Some((requested_project, sources, previous)) = request {
        if &requested_project != project {
            return Err(RuntimeError::Unauthorized);
        }
        runtime.initialize_diagnostics().await?;
        let recorded = runtime
            .recover_dream_sources(project, &sources, &previous)
            .await;
        let receipt = recorded.result?;
        if receipt.sources != sources.len() || receipt.batches != 1 {
            return Err(RuntimeError::Conflict);
        }
        println!(
            "{}",
            json!({
                "project": project.as_str(), "outcome": "succeeded",
                "batches": receipt.batches, "sources_consumed": receipt.sources,
                "diagnostics_warning": recorded.diagnostics_warning.is_some(),
            })
        );
    }
    Ok(())
}

type RecoveryRequest = (ProjectId, Vec<SourceId>, Vec<JobId>);

fn request() -> Result<RecoveryRequest, RuntimeError> {
    let mut bytes = Vec::new();
    std::io::stdin()
        .take(INPUT_LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| RuntimeError::InvalidInput("recovery input"))?;
    parse_request(&bytes)
}

/// The consent gate is the only guard against an unapproved duplicate paid call.
fn parse_request(bytes: &[u8]) -> Result<RecoveryRequest, RuntimeError> {
    if bytes.len() as u64 > INPUT_LIMIT {
        return Err(RuntimeError::InvalidInput("recovery input bound"));
    }
    let value: Value =
        serde_json::from_slice(bytes).map_err(|_| RuntimeError::InvalidInput("recovery JSON"))?;
    let object = value
        .as_object()
        .ok_or(RuntimeError::InvalidInput("recovery object"))?;
    if object.len() != 4
        || object.keys().any(|key| {
            !matches!(
                key.as_str(),
                "project" | "sources" | "previous_operations" | "accept_duplicate_charge"
            )
        })
        || object
            .get("accept_duplicate_charge")
            .and_then(Value::as_bool)
            != Some(true)
    {
        return Err(RuntimeError::InvalidInput(
            "explicit duplicate-charge consent required",
        ));
    }
    let project = object
        .get("project")
        .and_then(Value::as_str)
        .ok_or(RuntimeError::InvalidInput("recovery project"))?;
    let project =
        ProjectId::new(project).map_err(|_| RuntimeError::InvalidInput("recovery project"))?;
    let sources = object
        .get("sources")
        .and_then(Value::as_array)
        .filter(|ids| !ids.is_empty() && ids.len() <= 16)
        .ok_or(RuntimeError::InvalidInput("recovery sources"))?
        .iter()
        .map(|id| {
            SourceId::new(
                id.as_str()
                    .ok_or(RuntimeError::InvalidInput("recovery source identity"))?,
            )
            .map_err(|_| RuntimeError::InvalidInput("recovery source identity"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let previous = object
        .get("previous_operations")
        .and_then(Value::as_array)
        .filter(|ids| !ids.is_empty() && ids.len() <= 32)
        .ok_or(RuntimeError::InvalidInput("recovery admissions"))?
        .iter()
        .map(|id| {
            JobId::new(
                id.as_str()
                    .ok_or(RuntimeError::InvalidInput("recovery admission identity"))?,
            )
            .map_err(|_| RuntimeError::InvalidInput("recovery admission identity"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok((project, sources, previous))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan(consent: Value) -> Value {
        json!({"project":"project","sources":["s1"],"previous_operations":["dream-job:a"],"accept_duplicate_charge":consent})
    }
    fn parse(value: &Value) -> Result<RecoveryRequest, RuntimeError> {
        parse_request(value.to_string().as_bytes())
    }

    #[test]
    fn only_an_exact_plan_with_explicit_consent_is_accepted() {
        let (project, sources, previous) = parse(&plan(json!(true))).unwrap();
        assert_eq!(project.as_str(), "project");
        assert_eq!(sources[0].as_str(), "s1");
        assert_eq!(previous[0].as_str(), "dream-job:a");
        for consent in [json!(false), json!("true"), json!(1), Value::Null] {
            assert!(parse(&plan(consent)).is_err());
        }
        let mut extra = plan(json!(true));
        extra["owner"] = json!("foreign");
        assert!(parse(&extra).is_err());
        let mut missing = plan(json!(true));
        missing
            .as_object_mut()
            .unwrap()
            .remove("previous_operations");
        assert!(parse(&missing).is_err());
    }

    #[test]
    fn batch_and_input_bounds_are_enforced() {
        let ids = |n: usize, prefix: &str| {
            (0..n)
                .map(|i| json!(format!("{prefix}{i}")))
                .collect::<Vec<_>>()
        };
        for (sources, previous, ok) in [
            (16, 32, true),
            (17, 1, false),
            (1, 33, false),
            (0, 1, false),
            (1, 0, false),
        ] {
            let mut value = plan(json!(true));
            value["sources"] = json!(ids(sources, "s"));
            value["previous_operations"] = json!(ids(previous, "dream-job:"));
            assert_eq!(
                parse(&value).is_ok(),
                ok,
                "{sources} sources, {previous} admissions"
            );
        }
        let oversized = vec![b' '; INPUT_LIMIT as usize + 1];
        assert_eq!(
            parse_request(&oversized).unwrap_err(),
            RuntimeError::InvalidInput("recovery input bound")
        );
        assert!(parse_request(b"not json").is_err());
    }
}
