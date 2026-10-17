use crate::domain_models::{MemoryError, retrieval::RevisionRetrievalRequest};
pub fn validate(request: &RevisionRetrievalRequest) -> Result<(), MemoryError> {
    if request.terms.is_empty()
        || request.terms.len() > 12
        || request.limit == 0
        || request.limit > 32
        || request
            .terms
            .iter()
            .any(|term| term.trim().is_empty() || term.len() > 128)
    {
        return Err(MemoryError::InvalidInput(
            "invalid deterministic retrieval bounds",
        ));
    }
    Ok(())
}

pub fn normalize(
    mut request: RevisionRetrievalRequest,
) -> Result<RevisionRetrievalRequest, MemoryError> {
    validate(&request)?;
    request.terms = request
        .terms
        .into_iter()
        .map(|term| term.trim().to_lowercase())
        .collect();
    request.terms.sort();
    request.terms.dedup();
    Ok(request)
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain_models::{OwnerId, ProjectId};
    #[test]
    fn repeated_terms_cannot_inflate_lexical_score() {
        let request = RevisionRetrievalRequest {
            owner: OwnerId::new("owner").unwrap(),
            project: ProjectId::new("project").unwrap(),
            terms: vec![" Database ".into(), "database".into()],
            as_of_ms: 10,
            limit: 1,
        };
        assert_eq!(normalize(request).unwrap().terms, vec!["database"]);
    }
}
