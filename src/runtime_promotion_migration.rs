//! Single-store operator boundary. Cross-store transport belongs to promote-all.mjs.
use lambda_memory::domain_models::capture::{WorkEventKind, WorkOutcome};
use lambda_memory::domain_models::promotion::PromotionPlan;
use lambda_memory::domain_models::runtime::RuntimeError;
use lambda_memory::domain_models::{MemoryId, SourceId};
use lambda_memory::engines::runtime_policy;
use lambda_memory::integrations::database::embedded_capture::{
    EmbeddedCaptureStore, EmbeddedRuntimeStore,
};
use lambda_memory::integrations::runtime_config::RuntimeConfig;
use lambda_memory::services::runtime_service::MemoryRuntime;
use serde_json::{Value, json};
use std::io::{Read, Write};

fn invalid() -> RuntimeError {
    RuntimeError::InvalidInput("invalid promotion operator input")
}
fn consent() -> RuntimeError {
    RuntimeError::InvalidInput("exact promotion consent required")
}
fn plan_json(project: &str, plan: &PromotionPlan) -> Value {
    json!({"project":project,"nodes":plan.nodes.iter().map(|n| n.as_str()).collect::<Vec<_>>(),"batches":plan.batches,"linked_sources":plan.linked_sources,"accept_charges":false})
}
fn ids(value: &Value) -> Result<Vec<String>, RuntimeError> {
    value
        .as_array()
        .ok_or_else(invalid)?
        .iter()
        .map(|v| v.as_str().map(str::to_owned).ok_or_else(invalid))
        .collect()
}
fn now() -> Result<u64, RuntimeError> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|t| t.as_millis() as u64)
        .map_err(|_| invalid())
}
pub async fn run(command: &str) -> Result<(), RuntimeError> {
    let args: Vec<_> = std::env::args().skip(2).collect();
    let apply = command == "--retire-promoted" && args == ["--apply"];
    if !args.is_empty() && !apply {
        return Err(invalid());
    }
    let input = if matches!(
        command,
        "--promote-existing" | "--forward-done" | "--inspect-copies" | "--retire-promoted"
    ) {
        let mut bytes = Vec::new();
        let error = || {
            if command == "--promote-existing" {
                consent()
            } else {
                invalid()
            }
        };
        // Copy inspection accepts 10000 maximum-length identities (including JSON escapes).
        let bound = if command == "--promote-existing" {
            1024 * 1024
        } else {
            64 * 1024 * 1024
        };
        std::io::stdin()
            .take(bound as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| error())?;
        if bytes.len() > bound {
            return Err(error());
        }
        if bytes.is_empty() && command == "--retire-promoted" && !apply {
            None
        } else {
            Some(serde_json::from_slice::<Value>(&bytes).map_err(|_| error())?)
        }
    } else {
        None
    };
    let config = RuntimeConfig::from_environment()?;
    runtime_policy::validate_config(&config.scope, &config.limits)?;
    if config.scope.projects.len() != 1 {
        return Err(invalid());
    }
    let project = &config.scope.projects[0];
    if command == "--promote-existing" {
        #[cfg(feature = "router-http")]
        {
            let (runtime, _) = super::runtime_launcher::configured_runtime().await?;
            let plan = runtime.inspect_promotions(project).await?;
            let mut expected = plan_json(project.as_str(), &plan);
            expected["accept_charges"] = json!(true);
            if input.as_ref() != Some(&expected) {
                return Err(consent());
            }
            let report = runtime.promote_existing(project, &plan, true).await?;
            println!(
                "{}",
                json!({"project":project.as_str(),"outcome":"succeeded","classified":report.classified,"user_nodes":report.user_nodes.iter().map(|n| n.as_str()).collect::<Vec<_>>(),"forwards":report.forwards})
            );
            return Ok(());
        }
        #[cfg(not(feature = "router-http"))]
        {
            return Err(RuntimeError::InvalidInput(
                "promotion classifier requires memory-runtime and router-http features",
            ));
        }
    }
    let store = EmbeddedRuntimeStore::new(
        EmbeddedCaptureStore::open(&config.path)
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?,
    );
    let store = store.with_timeline_policy(
        lambda_memory::engines::timeline_policy::AUTO_POLICY,
        config.limits.max_content_bytes,
    );
    let owner = &config.scope.owner;
    match command {
        "--inspect-promotions" => {
            let runtime = MemoryRuntime::new(store, config.scope.clone(), config.limits)?;
            println!(
                "{}",
                plan_json(
                    project.as_str(),
                    &runtime.inspect_promotions(project).await?
                )
            );
        }
        "--export-forwards" => {
            let mut out = std::io::BufWriter::new(std::io::stdout().lock());
            for forward in store.export_capture_forwards(owner, project).await? {
                let Some(s) = forward.source else { continue };
                let kind = match s.kind {
                    WorkEventKind::Attempt => "attempt",
                    WorkEventKind::Result => "result",
                    WorkEventKind::Error => "error",
                    WorkEventKind::Decision => "decision",
                    WorkEventKind::Action => "action",
                    WorkEventKind::Exchange => "exchange",
                    WorkEventKind::Lesson => "lesson",
                };
                let outcome = match s.outcome {
                    WorkOutcome::Unknown => "unknown",
                    WorkOutcome::Succeeded => "succeeded",
                    WorkOutcome::Failed => "failed",
                };
                writeln!(out,"{}", json!({"forward_id":forward.forward_id,"source_id":forward.source_id.as_str(),"source":{"source":s.source.as_str(),"origin":s.origin,"recorded_at_ms":s.recorded_at_ms,"kind":kind,"outcome":outcome,"content":s.content}})).map_err(|_| RuntimeError::StorageUnavailable)?;
            }
            out.flush().map_err(|_| RuntimeError::StorageUnavailable)?;
        }
        "--forward-done" => {
            let map = input
                .as_ref()
                .and_then(Value::as_object)
                .ok_or_else(invalid)?;
            if map.len() != 2 {
                return Err(invalid());
            }
            let done = ids(map.get("done").ok_or_else(invalid)?)?;
            let failed = ids(map.get("failed").ok_or_else(invalid)?)?;
            let (done, failed) = store
                .forward_done(owner, project, &done, &failed, now()?)
                .await?;
            println!("{}", json!({"done":done,"failed":failed}));
        }
        "--inspect-copies" => {
            let ids = ids(input.as_ref().ok_or_else(invalid)?)?
                .into_iter()
                .map(|s| SourceId::new(s).map_err(|_| invalid()))
                .collect::<Result<Vec<_>, _>>()?;
            let result = store.inspect_copies(owner, project, &ids).await?;
            let strings =
                |ids: Vec<SourceId>| ids.into_iter().map(|s| s.to_string()).collect::<Vec<_>>();
            println!(
                "{}",
                json!({"active":strings(result.active),"consumed":strings(result.consumed),"pending":strings(result.pending),"missing":strings(result.missing)})
            );
        }
        "--retire-promoted" => {
            let selected = input
                .as_ref()
                .map(|v| {
                    ids(v).and_then(|ids| {
                        ids.into_iter()
                            .map(|id| MemoryId::new(id).map_err(|_| invalid()))
                            .collect::<Result<Vec<_>, _>>()
                    })
                })
                .transpose()?;
            if apply && selected.is_none() {
                return Err(invalid());
            }
            let result = store
                .retire_promoted(
                    owner,
                    &config.scope.actor,
                    project,
                    if apply { selected.as_deref() } else { None },
                    now()?,
                )
                .await?;
            println!(
                "{}",
                json!({"project":project.as_str(),"eligible":result.eligible.iter().map(|e| json!({"node":e.node.as_str(),"sources":e.sources.iter().map(|s| s.as_str()).collect::<Vec<_>>()})).collect::<Vec<_>>(),"blocked":result.blocked.iter().map(|n| n.as_str()).collect::<Vec<_>>(),"retired":result.retired})
            );
        }
        _ => return Err(invalid()),
    }
    Ok(())
}
