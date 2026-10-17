//! Domain-only async contracts. Implementations dress backend data and errors here.
use crate::domain_models::{
    Memory, MemoryError, MemoryQueryPlan, RecallRequest, SelectionOutcome, SelectionRequest,
};
use std::future::Future;

pub trait MemoryRepository {
    fn search(
        &self,
        plan: &MemoryQueryPlan,
        limit: usize,
    ) -> impl Future<Output = Result<Vec<Memory>, MemoryError>>;
}
pub trait MemoryQueryPlanner {
    fn plan(
        &self,
        request: &RecallRequest,
    ) -> impl Future<Output = Result<MemoryQueryPlan, MemoryError>>;
}
pub trait MemorySelector {
    fn evaluate(
        &self,
        request: &SelectionRequest,
    ) -> impl Future<Output = Result<SelectionOutcome, MemoryError>>;
}
