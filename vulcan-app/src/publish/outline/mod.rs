//! One-way Outline publication primitives.

#[cfg(feature = "web")]
mod client;
mod planner;
mod state;

#[cfg(feature = "web")]
pub use client::HttpOutlineClient;
pub use planner::{
    plan_outline_reconciliation, OutlinePublishAction, OutlinePublishActionKind, OutlinePublishPlan,
};
pub use state::{
    load_outline_state, lock_outline_state, OutlineDocumentMapping, OutlinePublishState,
    OutlineStateLock,
};

use crate::AppError;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OutlineRemoteDocument {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub text: String,
    pub collection_id: String,
    pub parent_document_id: Option<String>,
    #[serde(default)]
    pub archived_at: Option<String>,
}

pub trait OutlineApi {
    fn list_collection_documents(
        &self,
        collection_id: &str,
    ) -> Result<Vec<OutlineRemoteDocument>, AppError>;
    fn document_info(&self, id: &str) -> Result<OutlineRemoteDocument, AppError>;
    fn create_document(
        &self,
        collection_id: &str,
        parent_document_id: Option<&str>,
        title: &str,
        text: &str,
    ) -> Result<OutlineRemoteDocument, AppError>;
    fn update_document(
        &self,
        id: &str,
        title: &str,
        text: &str,
    ) -> Result<OutlineRemoteDocument, AppError>;
    fn move_document(
        &self,
        id: &str,
        collection_id: &str,
        parent_document_id: Option<&str>,
    ) -> Result<OutlineRemoteDocument, AppError>;
    fn archive_document(&self, id: &str) -> Result<OutlineRemoteDocument, AppError>;
}
