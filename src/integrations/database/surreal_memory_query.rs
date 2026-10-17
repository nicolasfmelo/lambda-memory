//! Parametrized SurrealDB query draft. No connection, execution or schema assumed.
use crate::domain_models::{MemoryError, MemoryQueryPlan, MemoryRelationFilter};
use std::collections::BTreeMap;
#[derive(Debug, PartialEq)]
enum BoundValue {
    Text(String),
    Texts(Vec<String>),
    Limit(usize),
}
#[derive(Debug)]
struct SurrealQuery {
    statement: String,
    parameters: BTreeMap<String, BoundValue>,
}
fn build(plan: &MemoryQueryPlan, limit: usize) -> Result<SurrealQuery, MemoryError> {
    if limit == 0 || limit > 99 || plan.terms.is_empty() || plan.terms.len() > 12 {
        return Err(MemoryError::InvalidInput("query bounds"));
    }
    let mut statement = "SELECT memory_id AS id, actor_id AS actor, owner_id AS owner, project_id AS project, content AS text, source_id AS source, status = 'active' AS active, restricted FROM memory_item WHERE owner_id = $owner AND ((scope = 'project' AND project_id = $project) OR scope = 'global') AND status = 'active' AND superseded_by = NONE AND restricted = false AND (expires_at = NONE OR expires_at > time::now()) AND content @0@ $terms".to_string();
    let mut parameters = BTreeMap::from([
        ("owner".into(), BoundValue::Text(plan.owner.to_string())),
        ("project".into(), BoundValue::Text(plan.project.to_string())),
        ("terms".into(), BoundValue::Text(plan.terms.join(" "))),
        ("limit".into(), BoundValue::Limit(limit)),
    ]);
    for (index, filter) in plan.relations.iter().enumerate() {
        match filter {
            MemoryRelationFilter::Sources(ids) => {
                let key = format!("sources_{index}");
                statement.push_str(&format!(" AND source_id IN ${key}"));
                parameters.insert(
                    key,
                    BoundValue::Texts(ids.iter().map(ToString::to_string).collect()),
                );
            }
        }
    }
    statement.push_str(" LIMIT $limit;");
    Ok(SurrealQuery {
        statement,
        parameters,
    })
}
/// Validate bindings without exposing DB query DTOs to a service.
pub fn validate_query_plan(plan: &MemoryQueryPlan, limit: usize) -> Result<(), MemoryError> {
    let query = build(plan, limit)?;
    if query.statement.is_empty() || query.parameters.is_empty() {
        return Err(MemoryError::StorageUnavailable);
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain_models::{ActorId, OwnerId, ProjectId, SourceId};
    #[test]
    fn values_are_bound_not_interpolated() {
        let value = "'; DELETE memory; --";
        let plan = MemoryQueryPlan {
            terms: vec![value.into()],
            owner: OwnerId::new("owner").unwrap(),
            actor: ActorId::new("actor").unwrap(),
            project: ProjectId::new(value).unwrap(),
            relations: vec![MemoryRelationFilter::Sources(vec![
                SourceId::new("fixture://db").unwrap(),
            ])],
        };
        let query = build(&plan, 4).unwrap();
        assert!(!query.statement.contains("DELETE"));
        assert_eq!(query.parameters["terms"], BoundValue::Text(value.into()));
        assert!(query.statement.contains("source_id IN $sources_0"));
    }
}
