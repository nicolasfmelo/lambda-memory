//! UI reads deliberately bypass foreground accounting and model work, like `brief`.
use super::*;
use crate::domain_models::memory_tree::{StoreKind, roots};
use crate::domain_models::ui::{MemoryOverview, NodesPage, NodesQuery};
impl MemoryRuntime {
    pub async fn overview(
        &self,
        project: &ProjectId,
        utc_offset_minutes: i32,
    ) -> Result<MemoryOverview, RuntimeError> {
        self.authorize(project)?;
        if !(-720..=840).contains(&utc_offset_minutes) {
            return Err(RuntimeError::InvalidInput("UTC offset bounds"));
        }
        self.store
            .ui_overview(&self.scope.owner, project, utc_offset_minutes, now_ms()?)
            .await
    }
    pub async fn nodes(&self, query: NodesQuery) -> Result<NodesPage, RuntimeError> {
        self.authorize(&query.project)?;
        if !roots(StoreKind::of(&query.project)).contains(&query.root)
            || !(1..=32).contains(&query.limit)
            || query.query.len() > 256
        {
            return Err(RuntimeError::InvalidInput("nodes query bounds"));
        }
        self.store.ui_nodes(&self.scope.owner, &query).await
    }
}
