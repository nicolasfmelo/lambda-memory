//! Operator JSON consent gate. No MCP method can dispatch these commands.
use lambda_memory::domain_models::embeddings::EmbeddingSpace;
use lambda_memory::domain_models::runtime::RuntimeError;
use lambda_memory::domain_models::runtime_embedding::{
    EmbeddingRecoveryItem, EmbeddingRecoveryPlan,
};
use lambda_memory::domain_models::{JobId, ProjectId, RevisionId};
use serde_json::{Value, json};
use std::io::Read;

fn invalid() -> RuntimeError {
    RuntimeError::InvalidInput("exact embedding maintenance consent required")
}
fn fields(value: &Value, names: &[&str]) -> Result<(), RuntimeError> {
    let object = value.as_object().ok_or_else(invalid)?;
    if object.len() != names.len() || names.iter().any(|name| !object.contains_key(*name)) {
        return Err(invalid());
    }
    Ok(())
}
fn text(value: &Value) -> Result<&str, RuntimeError> {
    value.as_str().filter(|s| !s.is_empty()).ok_or_else(invalid)
}
fn space(value: &Value) -> Result<EmbeddingSpace, RuntimeError> {
    fields(
        value,
        &["provider", "model", "index_revision", "dimensions"],
    )?;
    let dimensions = usize::try_from(value["dimensions"].as_u64().ok_or_else(invalid)?)
        .map_err(|_| invalid())?;
    if !(128..=3072).contains(&dimensions) {
        return Err(invalid());
    }
    Ok(EmbeddingSpace {
        provider: text(&value["provider"])?.into(),
        model: text(&value["model"])?.into(),
        revision: text(&value["index_revision"])?.into(),
        dimensions,
    })
}
fn space_json(space: &EmbeddingSpace) -> Value {
    json!({"provider":space.provider,"model":space.model,"index_revision":space.revision,"dimensions":space.dimensions})
}
fn recovery(value: &Value) -> Result<(ProjectId, EmbeddingRecoveryPlan), RuntimeError> {
    fields(
        value,
        &["project", "space", "items", "accept_duplicate_charge"],
    )?;
    if value["accept_duplicate_charge"].as_bool() != Some(true) {
        return Err(invalid());
    }
    let project = ProjectId::new(text(&value["project"])?).map_err(|_| invalid())?;
    let space = space(&value["space"])?;
    let rows = value["items"]
        .as_array()
        .filter(|a| !a.is_empty() && a.len() <= 16)
        .ok_or_else(invalid)?;
    let mut items = Vec::with_capacity(rows.len());
    for row in rows {
        fields(row, &["revision", "snapshot", "previous_operations"])?;
        let revision = RevisionId::new(text(&row["revision"])?).map_err(|_| invalid())?;
        if items
            .iter()
            .any(|item: &EmbeddingRecoveryItem| item.revision == revision)
        {
            return Err(invalid());
        }
        let snapshot = text(&row["snapshot"])?;
        if snapshot.len() != 64 || !snapshot.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(invalid());
        }
        let previous = row["previous_operations"]
            .as_array()
            .filter(|a| !a.is_empty() && a.len() <= 32)
            .ok_or_else(invalid)?;
        let mut previous_operations = Vec::with_capacity(previous.len());
        for id in previous {
            let id = JobId::new(text(id)?).map_err(|_| invalid())?;
            if !id.as_str().starts_with("embedding:") || previous_operations.contains(&id) {
                return Err(invalid());
            }
            previous_operations.push(id);
        }
        items.push(EmbeddingRecoveryItem {
            revision,
            snapshot: snapshot.into(),
            previous_operations,
        });
    }
    if items
        .iter()
        .map(|item| item.previous_operations.len())
        .sum::<usize>()
        > 32
    {
        return Err(invalid());
    }
    Ok((project, EmbeddingRecoveryPlan { space, items }))
}
fn preparation(value: &Value) -> Result<(ProjectId, EmbeddingSpace, usize), RuntimeError> {
    fields(value, &["project", "space", "limit", "accept_charges"])?;
    if value["accept_charges"].as_bool() != Some(true) {
        return Err(invalid());
    }
    let limit =
        usize::try_from(value["limit"].as_u64().ok_or_else(invalid)?).map_err(|_| invalid())?;
    if !(1..=16).contains(&limit) {
        return Err(invalid());
    }
    Ok((
        ProjectId::new(text(&value["project"])?).map_err(|_| invalid())?,
        space(&value["space"])?,
        limit,
    ))
}
fn input() -> Result<Value, RuntimeError> {
    let mut bytes = Vec::new();
    std::io::stdin()
        .take(32769)
        .read_to_end(&mut bytes)
        .map_err(|_| invalid())?;
    if bytes.len() > 32768 {
        return Err(invalid());
    }
    serde_json::from_slice(&bytes).map_err(|_| invalid())
}
pub async fn run(command: &str) -> Result<(), RuntimeError> {
    if std::env::args().count() != 2 {
        return Err(invalid());
    }
    // Parse explicit consent before opening the store or constructing provider clients.
    let recovery_request = if command == "--recover-embeddings" {
        Some(recovery(&input()?)?)
    } else {
        None
    };
    let preparation_request = if command == "--prepare-embedding-index" {
        Some(preparation(&input()?)?)
    } else {
        None
    };
    let (runtime, _) = super::runtime_launcher::configured_runtime().await?;
    if runtime.scope().projects.len() != 1 {
        return Err(invalid());
    }
    let project = &runtime.scope().projects[0];
    let current_space = runtime.embedding_space(project)?;
    match command {
        "--inspect-embedding-recovery" => {
            let plan = runtime.embedding_recovery_plan(project).await?;
            println!(
                "{}",
                json!({"project":project.as_str(),"space":space_json(&plan.space),
                "items":plan.items.iter().map(|i| json!({"revision":i.revision.as_str(),"snapshot":i.snapshot,
                    "previous_operations":i.previous_operations.iter().map(JobId::as_str).collect::<Vec<_>>()})).collect::<Vec<_>>(),"accept_duplicate_charge":false})
            );
        }
        "--inspect-embedding-index" => println!(
            "{}",
            json!({"project":project.as_str(),"space":space_json(&current_space),"limit":16,"accept_charges":false})
        ),
        "--recover-embeddings" => {
            let (requested, plan) = recovery_request.ok_or_else(invalid)?;
            if &requested != project {
                return Err(RuntimeError::Unauthorized);
            }
            if plan.space != current_space {
                return Err(RuntimeError::Conflict);
            }
            runtime.initialize_diagnostics().await?;
            let recorded = runtime.recover_embeddings(project, &plan).await;
            println!(
                "{}",
                json!({"project":project.as_str(),"outcome":"succeeded","revisions_indexed":recorded.result?,"diagnostics_warning":recorded.diagnostics_warning.is_some()})
            );
        }
        "--prepare-embedding-index" => {
            let (requested, space, limit) = preparation_request.ok_or_else(invalid)?;
            if &requested != project {
                return Err(RuntimeError::Unauthorized);
            }
            if space != current_space {
                return Err(RuntimeError::Conflict);
            }
            runtime.initialize_diagnostics().await?;
            let recorded = runtime
                .prepare_embedding_index(project, &space, limit)
                .await;
            println!(
                "{}",
                json!({"project":project.as_str(),"outcome":"succeeded","revisions_indexed":recorded.result?,"diagnostics_warning":recorded.diagnostics_warning.is_some()})
            );
        }
        _ => return Err(invalid()),
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    fn plan() -> Value {
        json!({"project":"project","space":{"provider":"openrouter","model":"google/gemini-embedding-2","index_revision":"runtime-gemini-retrieval-v2","dimensions":1536},"items":[{"revision":"r","snapshot":"a".repeat(64),"previous_operations":["embedding:old"]}],"accept_duplicate_charge":true})
    }
    #[test]
    fn consent_is_explicit_and_exact() {
        assert!(recovery(&plan()).is_ok());
        for consent in [json!(false), json!("true"), Value::Null] {
            let mut p = plan();
            p["accept_duplicate_charge"] = consent;
            assert!(recovery(&p).is_err());
        }
        let mut p = plan();
        p["owner"] = json!("foreign");
        assert!(recovery(&p).is_err());
        let mut p = plan();
        p["items"][0]["previous_operations"] = json!([]);
        assert!(recovery(&p).is_err());
        let mut p = plan();
        p["space"]["dimensions"] = json!(3073);
        assert!(recovery(&p).is_err());
        let mut p = plan();
        p["items"][0]["snapshot"] = json!("");
        assert!(recovery(&p).is_err());
    }
}
