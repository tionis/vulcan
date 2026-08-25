//! One-way Outline publication primitives.

#[cfg(feature = "web")]
mod client;
mod collections;
mod planner;
mod publisher;
mod state;

#[cfg(feature = "web")]
pub use client::HttpOutlineClient;
pub use collections::{
    bind_outline_profile_collection, provision_outline_profile_collection,
    validate_outline_collection_create, validate_outline_collection_update,
    OutlineCollectionProvisionReport, OutlineCollectionProvisionStatus,
};
pub use planner::{
    plan_outline_reconciliation, plan_outline_reconciliation_with_policy, OutlineConflictDetail,
    OutlineConflictField, OutlineConflictKind, OutlineConflictPolicy, OutlineConflictSide,
    OutlineConflictSideState, OutlinePublishAction, OutlinePublishActionKind, OutlinePublishPlan,
};
pub use publisher::{
    publish_outline, publish_outline_with_options_and_progress, publish_outline_with_progress,
    OutlinePublishOptions, OutlinePublishPhase, OutlinePublishProgress, OutlinePublishProjection,
    OutlinePublishReport,
};
pub use state::{
    load_outline_state, lock_outline_state, outline_state_collection_id, OutlineAttachmentMapping,
    OutlineDocumentMapping, OutlinePublishState, OutlineRemoteSnapshot, OutlineStateLock,
};

use crate::AppError;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub(crate) fn deterministic_remote_uuid(
    profile: &str,
    collection_id: &str,
    source_seed: &str,
) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"vulcan-outline-document-v2\0");
    hasher.update(profile.as_bytes());
    hasher.update(b"\0");
    hasher.update(collection_id.as_bytes());
    hasher.update(b"\0");
    hasher.update(source_seed.as_bytes());
    let mut bytes = *hasher.finalize().as_bytes();
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        bytes[8], bytes[9], bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15]
    )
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OutlineRemoteDocument {
    pub id: String,
    pub title: String,
    pub text: String,
    pub collection_id: String,
    pub parent_document_id: Option<String>,
    #[serde(default)]
    pub archived_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deleted_at: Option<String>,
    #[serde(default)]
    pub revision: Option<u64>,
    #[serde(default)]
    pub updated_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OutlineRemoteCollection {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub url_id: Option<String>,
    #[serde(default)]
    pub icon: Option<String>,
    #[serde(default)]
    pub color: Option<String>,
    #[serde(default)]
    pub permission: Option<String>,
    #[serde(default)]
    pub sharing: Option<bool>,
    #[serde(default)]
    pub commenting: Option<bool>,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub updated_at: Option<String>,
    #[serde(default)]
    pub archived_at: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OutlineCollectionCreate {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub permission: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sharing: Option<bool>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OutlineCollectionUpdate {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub permission: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sharing: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutlineRemoteAttachment {
    pub id: String,
    pub url: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutlineDownloadedAttachment {
    pub bytes: Vec<u8>,
    pub content_type: Option<String>,
}

pub trait OutlineApi {
    fn list_collections(
        &self,
        _query: Option<&str>,
        _archived: bool,
    ) -> Result<Vec<OutlineRemoteCollection>, AppError> {
        Err(AppError::operation(
            "this Outline connector does not support collection listing",
        ))
    }
    fn collection_info(&self, _id: &str) -> Result<OutlineRemoteCollection, AppError> {
        Err(AppError::operation(
            "this Outline connector does not support collection inspection",
        ))
    }
    fn create_collection(
        &self,
        _request: &OutlineCollectionCreate,
    ) -> Result<OutlineRemoteCollection, AppError> {
        Err(AppError::operation(
            "this Outline connector does not support collection creation",
        ))
    }
    fn update_collection(
        &self,
        _id: &str,
        _request: &OutlineCollectionUpdate,
    ) -> Result<OutlineRemoteCollection, AppError> {
        Err(AppError::operation(
            "this Outline connector does not support collection updates",
        ))
    }
    fn archive_collection(&self, _id: &str) -> Result<OutlineRemoteCollection, AppError> {
        Err(AppError::operation(
            "this Outline connector does not support collection archiving",
        ))
    }
    fn restore_collection(&self, _id: &str) -> Result<OutlineRemoteCollection, AppError> {
        Err(AppError::operation(
            "this Outline connector does not support collection restoration",
        ))
    }
    fn list_collection_documents(
        &self,
        collection_id: &str,
    ) -> Result<Vec<OutlineRemoteDocument>, AppError>;
    fn document_info(&self, id: &str) -> Result<OutlineRemoteDocument, AppError>;
    fn document_info_optional(&self, id: &str) -> Result<Option<OutlineRemoteDocument>, AppError> {
        self.document_info(id).map(Some)
    }
    fn create_document(
        &self,
        id: &str,
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
    fn upload_attachment(
        &self,
        document_id: &str,
        name: &str,
        content_type: &str,
        bytes: &[u8],
    ) -> Result<OutlineRemoteAttachment, AppError>;

    fn download_attachment(
        &self,
        _url: &str,
        _max_bytes: usize,
    ) -> Result<OutlineDownloadedAttachment, AppError> {
        Err(AppError::operation(
            "this Outline connector does not support attachment downloads",
        ))
    }
}
