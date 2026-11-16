//! Service-only skill review contracts. No filesystem installation here.
use super::*;
use crate::domain_models::skills::{SkillCandidate, SkillDecision, SkillStatus};
fn status(value: &str) -> Result<SkillStatus, CallError> {
    SkillStatus::parse(value).ok_or(CallError::Params)
}
fn id(args: &Map<String, Value>) -> Result<MemoryId, CallError> {
    MemoryId::new(text(args, "candidate_id")?).map_err(|_| CallError::Params)
}
fn value(c: SkillCandidate, detail: bool) -> Value {
    let mut v = json!({"candidate_id":c.candidate_id.as_str(),"scope":c.scope,"slug":c.slug,"status":c.status.as_str(),"memory_id":c.memory_id.as_str(),"revision_id":c.revision_id.as_str(),"sessions_at_draft":c.sessions_at_draft,"created_at_ms":c.created_at_ms,"decided_at_ms":c.decided_at_ms,"install_paths":c.install_paths});
    if detail {
        v["draft"] = json!(c.draft);
        v["install_hashes"] = json!(c.install_hashes);
    }
    v
}
pub(super) async fn list(
    runtime: &MemoryRuntime,
    args: &Map<String, Value>,
) -> Result<Value, CallError> {
    keys(args, &["project"], &["status"])?;
    let filter = args
        .get("status")
        .map(|v| v.as_str().ok_or(CallError::Params).and_then(status))
        .transpose()?;
    Ok(
        json!({"candidates":runtime.skill_candidates(&project(args)?,filter).await?.into_iter().map(|c|value(c,false)).collect::<Vec<_>>()}),
    )
}
pub(super) async fn read(
    runtime: &MemoryRuntime,
    args: &Map<String, Value>,
) -> Result<Value, CallError> {
    keys(args, &["project", "candidate_id"], &[])?;
    Ok(value(
        runtime.skill_candidate(&project(args)?, &id(args)?).await?,
        true,
    ))
}
pub(super) async fn decide(
    runtime: &MemoryRuntime,
    args: &Map<String, Value>,
) -> Result<Value, CallError> {
    keys(
        args,
        &["project", "candidate_id", "revision_id", "decision"],
        &["install_paths", "install_hashes"],
    )?;
    let decision = status(text(args, "decision")?)?;
    if !matches!(
        decision,
        SkillStatus::Approved | SkillStatus::Rejected | SkillStatus::Installed
    ) {
        return Err(CallError::Params);
    }
    let strings = |key| -> Result<Vec<String>, CallError> {
        match args.get(key) {
            None => Ok(Vec::new()),
            Some(Value::Array(v)) if v.len() <= 4 => v
                .iter()
                .map(|s| s.as_str().map(str::to_owned).ok_or(CallError::Params))
                .collect(),
            _ => Err(CallError::Params),
        }
    };
    let paths = strings("install_paths")?;
    let hashes = strings("install_hashes")?;
    Ok(value(
        runtime
            .skill_decide(
                &project(args)?,
                &id(args)?,
                &RevisionId::new(text(args, "revision_id")?).map_err(|_| CallError::Params)?,
                SkillDecision {
                    decision,
                    install_paths: paths,
                    install_hashes: hashes,
                },
            )
            .await?,
        true,
    ))
}
