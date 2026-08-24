use crate::outline_markdown::{
    outline_document_links_to_obsidian, outline_to_obsidian_markdown,
    rewrite_markdown_link_destinations,
};
use crate::publish::outline::{OutlineApi, OutlineRemoteDocument};
use crate::AppError;
use fs2::FileExt;
use pulldown_cmark::{Event, Options as MarkdownOptions, Parser, Tag, TagEnd};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::sync::LazyLock;
use unicode_normalization::UnicodeNormalization;
use vulcan_core::paths::{secure_read, secure_read_to_string, secure_write};
use vulcan_core::VaultPaths;

const STATE_VERSION: u32 = 1;
pub const DEFAULT_ATTACHMENT_MAX_BYTES: usize = 25 * 1024 * 1024;
pub const DEFAULT_REMOTE_CONTENT_MAX_BYTES: usize = 256 * 1024 * 1024;
pub const DEFAULT_ATTACHMENT_COUNT_MAX: usize = 10_000;
pub const DEFAULT_TOTAL_ATTACHMENT_MAX_BYTES: usize = 1024 * 1024 * 1024;

static OUTLINE_ATTACHMENT_DESTINATION: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?:https?://[^/]+)?/api/attachments\.redirect(?:[/?#].*)?$")
        .expect("Outline attachment destination regex should compile")
});

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OutlinePullConflictResolution {
    OverwriteLocal,
    ConflictMarkers,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OutlinePullConflictPolicy {
    resolutions: BTreeMap<String, OutlinePullConflictResolution>,
    default: Option<OutlinePullConflictResolution>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutlinePullOptions {
    pub apply_remote_moves: bool,
    pub missing_policy: OutlinePullMissingPolicy,
    pub confirmed_delete_count: Option<usize>,
    pub scope: OutlinePullScope,
    pub stale_attachment_policy: OutlinePullStaleAttachmentPolicy,
    pub confirmed_stale_attachment_delete_count: Option<usize>,
    pub connector_identity: Option<String>,
    pub max_remote_documents: usize,
    pub max_remote_content_bytes: usize,
    pub max_attachments: usize,
    pub max_attachment_bytes: usize,
    pub max_total_attachment_bytes: usize,
}

impl Default for OutlinePullOptions {
    fn default() -> Self {
        Self {
            apply_remote_moves: false,
            missing_policy: OutlinePullMissingPolicy::default(),
            confirmed_delete_count: None,
            scope: OutlinePullScope::default(),
            stale_attachment_policy: OutlinePullStaleAttachmentPolicy::default(),
            confirmed_stale_attachment_delete_count: None,
            connector_identity: None,
            max_remote_documents: 10_000,
            max_remote_content_bytes: DEFAULT_REMOTE_CONTENT_MAX_BYTES,
            max_attachments: DEFAULT_ATTACHMENT_COUNT_MAX,
            max_attachment_bytes: DEFAULT_ATTACHMENT_MAX_BYTES,
            max_total_attachment_bytes: DEFAULT_TOTAL_ATTACHMENT_MAX_BYTES,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum OutlinePullStaleAttachmentPolicy {
    #[default]
    Retain,
    Archive {
        directory: String,
    },
    Delete,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OutlinePullScope {
    pub root_document_ids: BTreeSet<String>,
    pub excluded_document_ids: BTreeSet<String>,
    pub max_depth: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutlinePullMissingResolution {
    Retain,
    Archive { directory: String },
    Delete,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutlinePullMissingPolicy {
    resolutions: BTreeMap<String, OutlinePullMissingResolution>,
    default: OutlinePullMissingResolution,
}

impl Default for OutlinePullMissingPolicy {
    fn default() -> Self {
        Self::retain()
    }
}

impl OutlinePullMissingPolicy {
    #[must_use]
    pub fn retain() -> Self {
        Self {
            resolutions: BTreeMap::new(),
            default: OutlinePullMissingResolution::Retain,
        }
    }

    #[must_use]
    pub fn archive_all(directory: String) -> Self {
        Self {
            resolutions: BTreeMap::new(),
            default: OutlinePullMissingResolution::Archive { directory },
        }
    }

    #[must_use]
    pub fn delete_all() -> Self {
        Self {
            resolutions: BTreeMap::new(),
            default: OutlinePullMissingResolution::Delete,
        }
    }

    #[must_use]
    pub fn selected(
        resolutions: impl IntoIterator<Item = (String, OutlinePullMissingResolution)>,
    ) -> Self {
        Self {
            resolutions: resolutions.into_iter().collect(),
            default: OutlinePullMissingResolution::Retain,
        }
    }

    fn resolution(&self, remote_id: &str) -> &OutlinePullMissingResolution {
        self.resolutions.get(remote_id).unwrap_or(&self.default)
    }
}

impl OutlinePullConflictPolicy {
    #[must_use]
    pub fn abort() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn overwrite_all() -> Self {
        Self {
            resolutions: BTreeMap::new(),
            default: Some(OutlinePullConflictResolution::OverwriteLocal),
        }
    }

    #[must_use]
    pub fn markers_all() -> Self {
        Self {
            resolutions: BTreeMap::new(),
            default: Some(OutlinePullConflictResolution::ConflictMarkers),
        }
    }

    #[must_use]
    pub fn selected(
        resolutions: impl IntoIterator<Item = (String, OutlinePullConflictResolution)>,
    ) -> Self {
        Self {
            resolutions: resolutions.into_iter().collect(),
            default: None,
        }
    }

    fn resolution(&self, path: &str) -> Option<OutlinePullConflictResolution> {
        self.resolutions.get(path).copied().or(self.default)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OutlinePullActionKind {
    Create,
    Update,
    Move,
    Unchanged,
    Conflict,
    WriteConflictMarkers,
    AutoMerge,
    RemoteMissing,
    ArchiveMissing,
    DeleteMissing,
    OutOfScope,
    StaleAttachment,
    ArchiveStaleAttachment,
    DeleteStaleAttachment,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[allow(clippy::struct_excessive_bools)]
pub struct OutlinePullAction {
    pub kind: OutlinePullActionKind,
    pub remote_document_id: String,
    pub local_path: String,
    pub reason: String,
    pub local_changed: bool,
    pub remote_changed: bool,
    pub source_local_path: Option<String>,
    pub rewritten_local_paths: Vec<String>,
    pub attachment_paths: Vec<String>,
    pub downloaded_attachment_paths: Vec<String>,
    pub preserves_local_changes: bool,
    pub conflict_markers_available: bool,
    #[serde(skip)]
    desired_content: Option<String>,
    #[serde(skip)]
    merged_content: Option<String>,
    #[serde(skip)]
    local_content: Option<String>,
    #[serde(skip)]
    attachments: Vec<OutlinePullAttachmentPlan>,
    #[serde(skip)]
    stale_attachment_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OutlinePullReport {
    pub profile: String,
    pub collection_id: String,
    pub destination: String,
    pub dry_run: bool,
    pub applied: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub operation_id: Option<String>,
    pub resumed_operation: bool,
    pub conflicts: usize,
    pub created: usize,
    pub updated: usize,
    pub moved: usize,
    pub unchanged: usize,
    pub conflict_markers_written: usize,
    pub auto_merged: usize,
    pub remote_missing: usize,
    pub archived_missing: usize,
    pub deleted_missing: usize,
    pub out_of_scope: usize,
    pub attachments_planned: usize,
    pub attachments_downloaded: usize,
    pub stale_attachments: usize,
    pub archived_stale_attachments: usize,
    pub deleted_stale_attachments: usize,
    pub actions: Vec<OutlinePullAction>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OutlinePullPhase {
    ListingRemote,
    Planning,
    Applying,
    DownloadingAttachments,
    Scanning,
    Completed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OutlinePullProgress {
    pub phase: OutlinePullPhase,
    pub processed: usize,
    pub total: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub operation_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct OutlinePullState {
    version: u32,
    profile: String,
    collection_id: String,
    destination: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    connector_identity: Option<String>,
    #[serde(default)]
    documents: BTreeMap<String, OutlinePullMapping>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    incomplete_operation: Option<OutlinePullOperationJournal>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_completed_operation_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct OutlinePullOperationJournal {
    operation_id: String,
    pending_actions: BTreeSet<String>,
    completed_actions: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct OutlinePullMapping {
    local_path: String,
    last_remote_content_hash: String,
    #[serde(default)]
    last_remote_source_hash: Option<String>,
    #[serde(default)]
    last_remote_source: Option<String>,
    #[serde(default)]
    last_remote_revision: Option<u64>,
    #[serde(default)]
    last_remote_updated_at: Option<String>,
    last_remote_title: String,
    last_remote_parent_id: Option<String>,
    last_materialized_local_hash: String,
    base_content: String,
    #[serde(default)]
    attachments: BTreeMap<String, OutlinePullAttachmentMapping>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct OutlinePullAttachmentMapping {
    local_path: String,
    content_hash: String,
    content_type: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutlinePulledBinding {
    pub local_path: String,
    pub remote_document_id: String,
    pub last_remote_source_hash: String,
    pub last_remote_title: String,
    pub last_remote_parent_id: Option<String>,
    pub attachments: Vec<OutlinePulledAttachmentBinding>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutlinePulledAttachmentBinding {
    pub local_path: String,
    pub remote_url: String,
    pub content_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct OutlinePullAttachmentPlan {
    remote_url: String,
    local_path: String,
    needs_download: bool,
    local_changed: bool,
    unmanaged_collision: bool,
}

impl OutlinePullState {
    fn empty(
        profile: &str,
        collection_id: &str,
        destination: &str,
        connector_identity: Option<&str>,
    ) -> Self {
        Self {
            version: STATE_VERSION,
            profile: profile.to_string(),
            collection_id: collection_id.to_string(),
            destination: destination.to_string(),
            connector_identity: connector_identity.map(str::to_string),
            documents: BTreeMap::new(),
            incomplete_operation: None,
            last_completed_operation_id: None,
        }
    }

    fn validate(
        &self,
        profile: &str,
        collection_id: &str,
        destination: &str,
        connector_identity: Option<&str>,
    ) -> Result<(), AppError> {
        if self.version != STATE_VERSION
            || self.profile != profile
            || self.collection_id != collection_id
            || self.destination != destination
        {
            return Err(AppError::operation(
                "Outline pull state belongs to a different route, collection, or destination",
            ));
        }
        if let (Some(stored), Some(requested)) =
            (self.connector_identity.as_deref(), connector_identity)
        {
            if stored != requested {
                return Err(AppError::operation(
                    "Outline pull state belongs to a different connector server",
                ));
            }
        }
        if self
            .connector_identity
            .as_deref()
            .is_some_and(str::is_empty)
        {
            return Err(AppError::operation(
                "Outline pull state contains an empty connector identity",
            ));
        }
        if self.documents.iter().any(|(remote_id, mapping)| {
            remote_id.is_empty()
                || mapping.local_path.is_empty()
                || mapping.last_remote_content_hash.is_empty()
                || mapping.last_materialized_local_hash.is_empty()
        }) {
            return Err(AppError::operation(
                "Outline pull state contains an incomplete document mapping",
            ));
        }
        let mut local_paths = BTreeSet::new();
        for mapping in self.documents.values() {
            if mapping.last_remote_source.as_deref().is_some_and(|source| {
                mapping.last_remote_source_hash.as_deref() != Some(content_hash(source).as_str())
            }) {
                return Err(AppError::operation(
                    "Outline pull state remote source does not match its recorded hash",
                ));
            }
            validate_managed_path(destination, &mapping.local_path)?;
            if !local_paths.insert(portable_path_key(&mapping.local_path)) {
                return Err(AppError::operation(
                    "Outline pull state maps multiple documents to the same local path",
                ));
            }
            for (remote_url, attachment) in &mapping.attachments {
                if remote_url.is_empty() || attachment.content_hash.is_empty() {
                    return Err(AppError::operation(
                        "Outline pull state contains an incomplete attachment mapping",
                    ));
                }
                validate_managed_asset_path(destination, &attachment.local_path)?;
                if !local_paths.insert(portable_path_key(&attachment.local_path)) {
                    return Err(AppError::operation(
                        "Outline pull state maps multiple objects to the same local path",
                    ));
                }
            }
        }
        if self.incomplete_operation.as_ref().is_some_and(|operation| {
            operation.operation_id.is_empty()
                || operation.pending_actions.iter().any(String::is_empty)
        }) {
            return Err(AppError::operation(
                "Outline pull state contains an invalid operation journal",
            ));
        }
        Ok(())
    }
}

#[allow(clippy::too_many_arguments)]
pub fn pull_outline(
    paths: &VaultPaths,
    api: &dyn OutlineApi,
    profile: &str,
    collection_id: &str,
    destination: &str,
    dry_run: bool,
    conflict_policy: &OutlinePullConflictPolicy,
) -> Result<OutlinePullReport, AppError> {
    pull_outline_with_options_and_write_authorizer(
        paths,
        api,
        profile,
        collection_id,
        destination,
        dry_run,
        conflict_policy,
        &OutlinePullOptions::default(),
        &|_| Ok(()),
    )
}

/// Pulls an Outline collection while authorizing every path from the fresh live plan before any
/// note is written.
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
pub fn pull_outline_with_write_authorizer(
    paths: &VaultPaths,
    api: &dyn OutlineApi,
    profile: &str,
    collection_id: &str,
    destination: &str,
    dry_run: bool,
    conflict_policy: &OutlinePullConflictPolicy,
    authorize_write: &dyn Fn(&str) -> Result<(), AppError>,
) -> Result<OutlinePullReport, AppError> {
    pull_outline_with_options_and_write_authorizer(
        paths,
        api,
        profile,
        collection_id,
        destination,
        dry_run,
        conflict_policy,
        &OutlinePullOptions::default(),
        authorize_write,
    )
}

/// Pulls an Outline collection with explicit reconciliation options and live-plan authorization.
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
pub fn pull_outline_with_options_and_write_authorizer(
    paths: &VaultPaths,
    api: &dyn OutlineApi,
    profile: &str,
    collection_id: &str,
    destination: &str,
    dry_run: bool,
    conflict_policy: &OutlinePullConflictPolicy,
    options: &OutlinePullOptions,
    authorize_write: &dyn Fn(&str) -> Result<(), AppError>,
) -> Result<OutlinePullReport, AppError> {
    pull_outline_with_options_progress_and_write_authorizer(
        paths,
        api,
        profile,
        collection_id,
        destination,
        dry_run,
        conflict_policy,
        options,
        authorize_write,
        &mut |_| {},
        &|| false,
    )
}

/// Pulls an Outline collection with progress events and cooperative cancellation.
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
pub fn pull_outline_with_options_progress_and_write_authorizer(
    paths: &VaultPaths,
    api: &dyn OutlineApi,
    profile: &str,
    collection_id: &str,
    destination: &str,
    dry_run: bool,
    conflict_policy: &OutlinePullConflictPolicy,
    options: &OutlinePullOptions,
    authorize_write: &dyn Fn(&str) -> Result<(), AppError>,
    on_progress: &mut dyn FnMut(&OutlinePullProgress),
    is_cancelled: &dyn Fn() -> bool,
) -> Result<OutlinePullReport, AppError> {
    let destination = validate_destination(destination)?;
    emit_pull_progress(
        on_progress,
        OutlinePullPhase::ListingRemote,
        0,
        0,
        None,
        None,
    );
    if dry_run {
        let state = load_state(
            paths,
            profile,
            collection_id,
            &destination,
            options.connector_identity.as_deref(),
        )?;
        let remote = api.list_collection_documents(collection_id)?;
        validate_remote_work_limit(&remote, options)?;
        ensure_pull_not_cancelled(is_cancelled, None)?;
        emit_pull_progress(
            on_progress,
            OutlinePullPhase::Planning,
            0,
            remote.len(),
            None,
            None,
        );
        let actions = plan_pull(paths, &remote, &state, conflict_policy, options, false)?;
        validate_attachment_work_limit(&actions, options)?;
        let operation_id = state
            .incomplete_operation
            .as_ref()
            .map(|operation| operation.operation_id.clone());
        let report = report(
            profile,
            collection_id,
            &destination,
            true,
            false,
            operation_id,
            state.incomplete_operation.is_some(),
            actions,
        );
        emit_pull_progress(
            on_progress,
            OutlinePullPhase::Completed,
            report.actions.len(),
            report.actions.len(),
            None,
            report.operation_id.as_deref(),
        );
        return Ok(report);
    }

    let _write_lock =
        vulcan_core::write_lock::acquire_write_lock(paths).map_err(AppError::operation)?;
    let lock = StateLock::acquire(paths, profile)?;
    let mut state = load_state(
        paths,
        profile,
        collection_id,
        &destination,
        options.connector_identity.as_deref(),
    )?;
    let remote = api.list_collection_documents(collection_id)?;
    validate_remote_work_limit(&remote, options)?;
    ensure_pull_not_cancelled(is_cancelled, None)?;
    emit_pull_progress(
        on_progress,
        OutlinePullPhase::Planning,
        0,
        remote.len(),
        None,
        None,
    );
    let mut actions = plan_pull(paths, &remote, &state, conflict_policy, options, true)?;
    validate_attachment_work_limit(&actions, options)?;
    if actions
        .iter()
        .any(|action| action.kind == OutlinePullActionKind::Conflict)
    {
        let operation_id = state
            .incomplete_operation
            .as_ref()
            .map(|operation| operation.operation_id.clone());
        return Ok(report(
            profile,
            collection_id,
            &destination,
            false,
            false,
            operation_id,
            state.incomplete_operation.is_some(),
            actions,
        ));
    }
    let delete_count = actions
        .iter()
        .filter(|action| action.kind == OutlinePullActionKind::DeleteMissing)
        .count();
    if delete_count > 0 && options.confirmed_delete_count != Some(delete_count) {
        return Err(AppError::operation(format!(
            "Outline pull planned {delete_count} permanent deletion(s); confirm that exact live count"
        )));
    }
    let stale_attachment_delete_count = actions
        .iter()
        .filter(|action| action.kind == OutlinePullActionKind::DeleteStaleAttachment)
        .count();
    if stale_attachment_delete_count > 0
        && options.confirmed_stale_attachment_delete_count != Some(stale_attachment_delete_count)
    {
        return Err(AppError::operation(format!(
            "Outline pull planned {stale_attachment_delete_count} permanent stale attachment deletion(s); confirm that exact live count"
        )));
    }
    for action in &actions {
        if matches!(
            action.kind,
            OutlinePullActionKind::Create
                | OutlinePullActionKind::Update
                | OutlinePullActionKind::Move
                | OutlinePullActionKind::WriteConflictMarkers
                | OutlinePullActionKind::AutoMerge
                | OutlinePullActionKind::ArchiveMissing
                | OutlinePullActionKind::DeleteMissing
                | OutlinePullActionKind::ArchiveStaleAttachment
                | OutlinePullActionKind::DeleteStaleAttachment
        ) {
            if let Some(source_path) = action.source_local_path.as_deref() {
                authorize_write(source_path)?;
            }
            authorize_write(&action.local_path)?;
            for rewritten_path in &action.rewritten_local_paths {
                authorize_write(rewritten_path)?;
            }
            for attachment_path in &action.attachment_paths {
                authorize_write(attachment_path)?;
            }
        }
    }
    let resumed_operation = state.incomplete_operation.is_some();
    let operation_id = state.incomplete_operation.as_ref().map_or_else(
        || ulid::Ulid::new().to_string(),
        |journal| journal.operation_id.clone(),
    );
    let completed_actions = state
        .incomplete_operation
        .as_ref()
        .map_or(0, |journal| journal.completed_actions);
    state.incomplete_operation = Some(OutlinePullOperationJournal {
        operation_id: operation_id.clone(),
        pending_actions: actions
            .iter()
            .filter(|action| pull_action_mutates(action.kind))
            .map(pull_action_journal_key)
            .collect(),
        completed_actions,
    });
    if state.connector_identity.is_none() {
        state
            .connector_identity
            .clone_from(&options.connector_identity);
    }
    lock.save(&state)?;
    let mutation_total = actions
        .iter()
        .filter(|action| pull_action_mutates(action.kind))
        .count();
    let attachment_total = actions
        .iter()
        .map(|action| {
            action
                .attachments
                .iter()
                .filter(|attachment| {
                    attachment.needs_download
                        || (attachment.local_changed && !action.preserves_local_changes)
                        || attachment.unmanaged_collision
                })
                .count()
        })
        .sum();
    let mut mutations_processed = 0usize;
    let mut attachments_processed = 0usize;
    let mut attachment_bytes_downloaded = 0usize;
    emit_pull_progress(
        on_progress,
        OutlinePullPhase::Applying,
        0,
        mutation_total,
        None,
        Some(&operation_id),
    );
    let remote_by_id = remote
        .iter()
        .map(|document| (document.id.as_str(), document))
        .collect::<BTreeMap<_, _>>();
    for action in &mut actions {
        if pull_action_mutates(action.kind) {
            ensure_pull_not_cancelled(is_cancelled, Some(&operation_id))?;
            emit_pull_progress(
                on_progress,
                OutlinePullPhase::Applying,
                mutations_processed,
                mutation_total,
                Some(&action.local_path),
                Some(&operation_id),
            );
        }
        if matches!(
            action.kind,
            OutlinePullActionKind::ArchiveStaleAttachment
                | OutlinePullActionKind::DeleteStaleAttachment
        ) {
            if action.kind == OutlinePullActionKind::ArchiveStaleAttachment {
                if let Some(source_path) = action.source_local_path.as_deref() {
                    let bytes = secure_read(paths.vault_root(), Path::new(source_path))
                        .map_err(AppError::operation)?;
                    secure_write(paths.vault_root(), Path::new(&action.local_path), &bytes)
                        .map_err(AppError::operation)?;
                    remove_managed_file(paths, source_path)?;
                }
            } else {
                remove_managed_file(paths, &action.local_path)?;
            }
            let remote_url = action.stale_attachment_url.as_deref().ok_or_else(|| {
                AppError::operation("stale Outline attachment action omitted its remote URL")
            })?;
            if let Some(mapping) = state.documents.get_mut(&action.remote_document_id) {
                mapping.attachments.remove(remote_url);
            }
            complete_journal_action(&mut state, action);
            lock.save(&state)?;
            mutations_processed += 1;
            emit_pull_progress(
                on_progress,
                OutlinePullPhase::Applying,
                mutations_processed,
                mutation_total,
                None,
                Some(&operation_id),
            );
            continue;
        }
        if matches!(
            action.kind,
            OutlinePullActionKind::ArchiveMissing | OutlinePullActionKind::DeleteMissing
        ) {
            if action.kind == OutlinePullActionKind::ArchiveMissing {
                if let Some(source_path) = action.source_local_path.as_deref() {
                    let moved = vulcan_core::move_rewrite::move_note_unlocked(
                        paths,
                        source_path,
                        &action.local_path,
                        false,
                    )
                    .map_err(AppError::operation)?;
                    action.rewritten_local_paths = moved
                        .rewritten_files
                        .into_iter()
                        .map(|file| file.path)
                        .collect();
                }
            } else {
                remove_managed_file(paths, &action.local_path)?;
                for attachment_path in &action.attachment_paths {
                    remove_managed_file(paths, attachment_path)?;
                }
            }
            state.documents.remove(&action.remote_document_id);
            complete_journal_action(&mut state, action);
            lock.save(&state)?;
            mutations_processed += 1;
            emit_pull_progress(
                on_progress,
                OutlinePullPhase::Applying,
                mutations_processed,
                mutation_total,
                None,
                Some(&operation_id),
            );
            continue;
        }
        if !matches!(
            action.kind,
            OutlinePullActionKind::Create
                | OutlinePullActionKind::Update
                | OutlinePullActionKind::Move
                | OutlinePullActionKind::WriteConflictMarkers
                | OutlinePullActionKind::AutoMerge
        ) {
            continue;
        }
        let remote = remote_by_id
            .get(action.remote_document_id.as_str())
            .ok_or_else(|| AppError::operation("planned Outline pull document disappeared"))?;
        if action.kind == OutlinePullActionKind::Move {
            let source_path = action
                .source_local_path
                .as_deref()
                .ok_or_else(|| AppError::operation("Outline pull move omitted its source path"))?;
            let moved = vulcan_core::move_rewrite::move_note_unlocked(
                paths,
                source_path,
                &action.local_path,
                false,
            )
            .map_err(AppError::operation)?;
            action.rewritten_local_paths = moved
                .rewritten_files
                .into_iter()
                .map(|file| file.path)
                .collect();
        }
        let desired = action
            .desired_content
            .as_deref()
            .ok_or_else(|| AppError::operation("Outline pull action omitted desired content"))?;
        let mut attachment_mappings = state
            .documents
            .get(&action.remote_document_id)
            .map_or_else(BTreeMap::new, |mapping| mapping.attachments.clone());
        if action.kind == OutlinePullActionKind::WriteConflictMarkers {
            complete_journal_action(&mut state, action);
            lock.save(&state)?;
        } else {
            for attachment in &action.attachments {
                if attachment.needs_download
                    || (attachment.local_changed && !action.preserves_local_changes)
                    || attachment.unmanaged_collision
                {
                    ensure_pull_not_cancelled(is_cancelled, Some(&operation_id))?;
                    emit_pull_progress(
                        on_progress,
                        OutlinePullPhase::DownloadingAttachments,
                        attachments_processed,
                        attachment_total,
                        Some(&attachment.local_path),
                        Some(&operation_id),
                    );
                    let downloaded = api.download_attachment(
                        &attachment.remote_url,
                        options.max_attachment_bytes,
                    )?;
                    attachment_bytes_downloaded = attachment_bytes_downloaded
                        .checked_add(downloaded.bytes.len())
                        .filter(|total| *total <= options.max_total_attachment_bytes)
                        .ok_or_else(|| {
                            AppError::operation(format!(
                                "Outline pull attachment downloads exceed the configured total byte limit of {}",
                                options.max_total_attachment_bytes
                            ))
                        })?;
                    secure_write(
                        paths.vault_root(),
                        Path::new(&attachment.local_path),
                        &downloaded.bytes,
                    )
                    .map_err(AppError::operation)?;
                    attachment_mappings.insert(
                        attachment.remote_url.clone(),
                        OutlinePullAttachmentMapping {
                            local_path: attachment.local_path.clone(),
                            content_hash: bytes_hash(&downloaded.bytes),
                            content_type: downloaded.content_type,
                        },
                    );
                    action
                        .downloaded_attachment_paths
                        .push(attachment.local_path.clone());
                    attachments_processed += 1;
                    emit_pull_progress(
                        on_progress,
                        OutlinePullPhase::DownloadingAttachments,
                        attachments_processed,
                        attachment_total,
                        None,
                        Some(&operation_id),
                    );
                }
            }
        }
        let written = if matches!(
            action.kind,
            OutlinePullActionKind::WriteConflictMarkers | OutlinePullActionKind::AutoMerge
        ) {
            action.merged_content.clone().ok_or_else(|| {
                AppError::operation("Outline merge action omitted its merged content")
            })?
        } else {
            desired.to_string()
        };
        if !action.preserves_local_changes {
            secure_write(
                paths.vault_root(),
                Path::new(&action.local_path),
                written.as_bytes(),
            )
            .map_err(AppError::operation)?;
        }
        if action.kind != OutlinePullActionKind::WriteConflictMarkers {
            let previous_materialized_hash = state
                .documents
                .get(&action.remote_document_id)
                .map(|mapping| mapping.last_materialized_local_hash.clone());
            state.documents.insert(
                action.remote_document_id.clone(),
                OutlinePullMapping {
                    local_path: action.local_path.clone(),
                    last_remote_content_hash: content_hash(desired),
                    last_remote_source_hash: Some(content_hash(&remote.text)),
                    last_remote_source: Some(remote.text.clone()),
                    last_remote_revision: remote.revision,
                    last_remote_updated_at: remote.updated_at.clone(),
                    last_remote_title: remote.title.clone(),
                    last_remote_parent_id: remote.parent_document_id.clone(),
                    last_materialized_local_hash: if action.preserves_local_changes {
                        previous_materialized_hash.ok_or_else(|| {
                            AppError::operation(
                                "Outline pull move cannot preserve changes without a baseline",
                            )
                        })?
                    } else {
                        content_hash(&written)
                    },
                    base_content: desired.to_string(),
                    attachments: attachment_mappings,
                },
            );
            complete_journal_action(&mut state, action);
            lock.save(&state)?;
        }
        mutations_processed += 1;
        emit_pull_progress(
            on_progress,
            OutlinePullPhase::Applying,
            mutations_processed,
            mutation_total,
            None,
            Some(&operation_id),
        );
    }
    ensure_pull_not_cancelled(is_cancelled, Some(&operation_id))?;
    emit_pull_progress(
        on_progress,
        OutlinePullPhase::Scanning,
        0,
        1,
        None,
        Some(&operation_id),
    );
    vulcan_core::scan::scan_vault_unlocked(paths, vulcan_core::ScanMode::Incremental)
        .map_err(AppError::operation)?;
    state.last_completed_operation_id = Some(operation_id.clone());
    state.incomplete_operation = None;
    lock.save(&state)?;
    emit_pull_progress(
        on_progress,
        OutlinePullPhase::Scanning,
        1,
        1,
        None,
        Some(&operation_id),
    );
    let report = report(
        profile,
        collection_id,
        &destination,
        false,
        true,
        Some(operation_id),
        resumed_operation,
        actions,
    );
    emit_pull_progress(
        on_progress,
        OutlinePullPhase::Completed,
        report.actions.len(),
        report.actions.len(),
        None,
        report.operation_id.as_deref(),
    );
    Ok(report)
}

#[allow(clippy::too_many_lines)]
fn plan_pull(
    paths: &VaultPaths,
    remote: &[OutlineRemoteDocument],
    state: &OutlinePullState,
    conflict_policy: &OutlinePullConflictPolicy,
    options: &OutlinePullOptions,
    write_lock_held: bool,
) -> Result<Vec<OutlinePullAction>, AppError> {
    let mut remote_ids = BTreeSet::new();
    for document in remote {
        if document.id.is_empty() || !remote_ids.insert(document.id.as_str()) {
            return Err(AppError::operation(
                "Outline returned an empty or duplicate document ID",
            ));
        }
        if document.collection_id != state.collection_id {
            return Err(AppError::operation(
                "Outline returned a document outside the configured collection",
            ));
        }
    }
    let active = remote
        .iter()
        .filter(|document| document.archived_at.is_none())
        .cloned()
        .collect::<Vec<_>>();
    let selected_ids = select_remote_documents(&active, &options.scope)?;
    let selected = active
        .iter()
        .filter(|document| selected_ids.contains(&document.id))
        .cloned()
        .collect::<Vec<_>>();
    let generated_paths = generate_paths(&active, &selected_ids, &state.destination)?;
    let local_paths = active
        .iter()
        .filter_map(|document| {
            if !selected_ids.contains(&document.id) {
                return state
                    .documents
                    .get(&document.id)
                    .map(|mapping| (document.id.clone(), mapping.local_path.clone()));
            }
            let path = if options.apply_remote_moves {
                generated_paths[&document.id].clone()
            } else {
                state.documents.get(&document.id).map_or_else(
                    || generated_paths[&document.id].clone(),
                    |mapping| mapping.local_path.clone(),
                )
            };
            Some((document.id.clone(), path))
        })
        .collect::<BTreeMap<_, _>>();
    let mut seen_local_paths = BTreeMap::<String, String>::new();
    for (remote_id, local_path) in &local_paths {
        validate_managed_path(&state.destination, local_path)?;
        if let Some(existing) =
            seen_local_paths.insert(portable_path_key(local_path), remote_id.clone())
        {
            return Err(AppError::operation(format!(
                "Outline documents `{existing}` and `{remote_id}` map to the same portable local path `{local_path}`"
            )));
        }
    }
    let mut actions = Vec::with_capacity(selected.len() + state.documents.len());
    for document in &selected {
        let local_path = local_paths[&document.id].clone();
        let mapped_path = state
            .documents
            .get(&document.id)
            .map(|mapping| mapping.local_path.as_str());
        let move_source = mapped_path.filter(|path| *path != local_path);
        let translated = outline_document_links_to_obsidian(
            &outline_to_obsidian_markdown(&document.text),
            |remote_id| local_paths.get(remote_id).cloned(),
        );
        let mapping = state.documents.get(&document.id);
        let (desired, attachments) = plan_document_attachments(
            paths,
            &state.destination,
            document,
            &local_path,
            &translated,
            mapping,
        )?;
        let content_path = move_source.unwrap_or(&local_path);
        let local_content = match secure_read_to_string(paths.vault_root(), Path::new(content_path))
        {
            Ok(content) => Some(content),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(AppError::operation(error)),
        };
        let desired_hash = content_hash(&desired);
        let local_hash = local_content.as_deref().map(content_hash);
        let note_local_changed = match (mapping, local_hash.as_deref()) {
            (Some(mapping), Some(hash)) => hash != mapping.last_materialized_local_hash,
            (Some(_), None) | (None, Some(_)) => true,
            (None, None) => false,
        };
        let attachment_local_changed = attachments
            .iter()
            .any(|attachment| attachment.local_changed || attachment.unmanaged_collision);
        let local_changed = note_local_changed || attachment_local_changed;
        let local_note_missing = mapping.is_some() && local_content.is_none();
        let attachment_needs_download = attachments
            .iter()
            .any(|attachment| attachment.needs_download);
        let remote_content_changed = mapping.is_none_or(|mapping| {
            mapping.last_remote_source_hash.as_ref().map_or_else(
                || desired_hash != mapping.last_remote_content_hash,
                |hash| content_hash(&document.text) != *hash,
            )
        });
        let remote_changed = mapping.is_none_or(|mapping| {
            remote_content_changed
                || document.title != mapping.last_remote_title
                || document.parent_document_id != mapping.last_remote_parent_id
        });
        let desired_matches_local = local_hash.as_deref() == Some(desired_hash.as_str());
        let collision = (mapping.is_none() && local_content.is_some() && !desired_matches_local)
            || (move_source.is_some() && paths.vault_root().join(&local_path).exists())
            || attachments
                .iter()
                .any(|attachment| attachment.unmanaged_collision);
        let conflicted = collision
            || local_note_missing
            || (attachment_local_changed && remote_content_changed)
            || (note_local_changed && remote_content_changed && !desired_matches_local);
        let conflict_markers_available = !attachment_local_changed
            && !local_note_missing
            && move_source.is_none()
            && local_content.is_some();
        let reviewed_merge = (conflicted
            && conflict_policy.resolution(&local_path)
                == Some(OutlinePullConflictResolution::ConflictMarkers)
            && conflict_markers_available)
            .then(|| {
                three_way_merge(
                    &extract_local_from_diff3(local_content.as_deref().unwrap_or_default()),
                    mapping.map_or("", |mapping| mapping.base_content.as_str()),
                    &desired,
                    &document.id,
                )
            })
            .transpose()?;
        let (kind, reason) = if conflicted {
            match conflict_policy.resolution(&local_path) {
                Some(OutlinePullConflictResolution::OverwriteLocal) => (
                    if move_source.is_some() && local_content.is_some() {
                        OutlinePullActionKind::Move
                    } else if mapping.is_some() {
                        OutlinePullActionKind::Update
                    } else {
                        OutlinePullActionKind::Create
                    },
                    "overwrite the reviewed local conflict with Outline",
                ),
                Some(OutlinePullConflictResolution::ConflictMarkers)
                    if conflict_markers_available =>
                {
                    if reviewed_merge
                        .as_ref()
                        .is_some_and(|merge| merge.has_conflicts)
                    {
                        (
                            OutlinePullActionKind::WriteConflictMarkers,
                            "write localized diff3 markers for overlapping edits",
                        )
                    } else {
                        (
                            OutlinePullActionKind::AutoMerge,
                            "automatically merge non-overlapping local and Outline edits",
                        )
                    }
                }
                _ => (
                    OutlinePullActionKind::Conflict,
                    if collision {
                        "an unmanaged local note or attachment occupies an Outline destination"
                    } else if local_note_missing {
                        "the managed local note is missing while its Outline document still exists"
                    } else if attachment_local_changed {
                        "a local attachment changed while the Outline document also changed"
                    } else {
                        "local and Outline content changed since the last successful pull"
                    },
                ),
            }
        } else if mapping.is_none() {
            (
                OutlinePullActionKind::Create,
                if desired_matches_local {
                    "adopt an existing local file that already matches Outline"
                } else {
                    "materialize a new Outline document"
                },
            )
        } else if move_source.is_some() && local_content.is_some() {
            (
                OutlinePullActionKind::Move,
                "apply the reviewed Outline title or hierarchy path",
            )
        } else if remote_changed && (!local_changed || desired_matches_local) {
            (
                OutlinePullActionKind::Update,
                if desired_matches_local {
                    "adopt a resolved local file that matches the current Outline document"
                } else {
                    "Outline document changed"
                },
            )
        } else if attachment_needs_download {
            (
                OutlinePullActionKind::Update,
                "repair missing materialized Outline attachments",
            )
        } else {
            (
                OutlinePullActionKind::Unchanged,
                if local_changed {
                    "local content changed while Outline remained unchanged"
                } else {
                    "local and Outline content match the pull baseline"
                },
            )
        };
        let rewritten_local_paths = if kind == OutlinePullActionKind::Move {
            let source_path = move_source.expect("move action has source");
            plan_pull_move(paths, source_path, &local_path, write_lock_held)?
                .rewritten_files
                .into_iter()
                .map(|file| file.path)
                .collect()
        } else {
            Vec::new()
        };
        let preserves_local_changes =
            kind == OutlinePullActionKind::Move && local_changed && !remote_content_changed;
        let stale_attachment_actions =
            plan_stale_attachment_actions(paths, document, mapping, &attachments, options)?;
        actions.push(OutlinePullAction {
            kind,
            remote_document_id: document.id.clone(),
            local_path,
            reason: reason.to_string(),
            local_changed,
            remote_changed,
            source_local_path: (kind == OutlinePullActionKind::Move)
                .then(|| move_source.expect("move action has source").to_string()),
            rewritten_local_paths,
            attachment_paths: attachments
                .iter()
                .map(|attachment| attachment.local_path.clone())
                .collect(),
            downloaded_attachment_paths: Vec::new(),
            preserves_local_changes,
            conflict_markers_available,
            desired_content: Some(desired),
            merged_content: reviewed_merge.map(|merge| merge.content),
            local_content,
            attachments,
            stale_attachment_url: None,
        });
        actions.extend(stale_attachment_actions);
    }
    for (remote_id, mapping) in &state.documents {
        if !active.iter().any(|document| document.id == *remote_id) {
            let local_content =
                match secure_read_to_string(paths.vault_root(), Path::new(&mapping.local_path)) {
                    Ok(content) => Some(content),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                    Err(error) => return Err(AppError::operation(error)),
                };
            let local_changed = local_content.as_deref().map(content_hash).as_deref()
                != Some(mapping.last_materialized_local_hash.as_str());
            let (kind, local_path, source_local_path, rewritten_local_paths, reason) = match options
                .missing_policy
                .resolution(remote_id)
            {
                OutlinePullMissingResolution::Retain => (
                    OutlinePullActionKind::RemoteMissing,
                    mapping.local_path.clone(),
                    None,
                    Vec::new(),
                    "managed Outline document is no longer in scope; local file retained",
                ),
                OutlinePullMissingResolution::Archive { directory } => {
                    let directory = validate_destination(directory)?;
                    let archive_path =
                        missing_archive_path(&directory, remote_id, &mapping.local_path);
                    validate_managed_path(&directory, &archive_path)?;
                    let rewritten = if local_content.is_some() {
                        plan_pull_move(paths, &mapping.local_path, &archive_path, write_lock_held)?
                            .rewritten_files
                            .into_iter()
                            .map(|file| file.path)
                            .collect()
                    } else {
                        Vec::new()
                    };
                    (
                        OutlinePullActionKind::ArchiveMissing,
                        archive_path,
                        local_content.as_ref().map(|_| mapping.local_path.clone()),
                        rewritten,
                        "archive the missing remote document at a recoverable local path",
                    )
                }
                OutlinePullMissingResolution::Delete => (
                    OutlinePullActionKind::DeleteMissing,
                    mapping.local_path.clone(),
                    local_content.as_ref().map(|_| mapping.local_path.clone()),
                    Vec::new(),
                    "permanently delete the explicitly confirmed missing remote document",
                ),
            };
            actions.push(OutlinePullAction {
                kind,
                remote_document_id: remote_id.clone(),
                local_path,
                reason: reason.to_string(),
                local_changed,
                remote_changed: true,
                source_local_path,
                rewritten_local_paths,
                attachment_paths: mapping
                    .attachments
                    .values()
                    .map(|attachment| attachment.local_path.clone())
                    .collect(),
                downloaded_attachment_paths: Vec::new(),
                preserves_local_changes: false,
                conflict_markers_available: false,
                desired_content: None,
                merged_content: None,
                local_content: None,
                attachments: Vec::new(),
                stale_attachment_url: None,
            });
        } else if !selected_ids.contains(remote_id) {
            actions.push(OutlinePullAction {
                kind: OutlinePullActionKind::OutOfScope,
                remote_document_id: remote_id.clone(),
                local_path: mapping.local_path.clone(),
                reason: "managed Outline document is outside this pull's selected scope"
                    .to_string(),
                local_changed: false,
                remote_changed: false,
                source_local_path: None,
                rewritten_local_paths: Vec::new(),
                attachment_paths: Vec::new(),
                downloaded_attachment_paths: Vec::new(),
                preserves_local_changes: true,
                conflict_markers_available: false,
                desired_content: None,
                merged_content: None,
                local_content: None,
                attachments: Vec::new(),
                stale_attachment_url: None,
            });
        }
    }
    actions.sort_by(|left, right| left.local_path.cmp(&right.local_path));
    Ok(actions)
}

fn generate_paths(
    remote: &[OutlineRemoteDocument],
    selected_ids: &BTreeSet<String>,
    destination: &str,
) -> Result<BTreeMap<String, String>, AppError> {
    let by_id = remote
        .iter()
        .map(|document| (document.id.as_str(), document))
        .collect::<BTreeMap<_, _>>();
    let mut paths = BTreeMap::new();
    for document in remote
        .iter()
        .filter(|document| selected_ids.contains(&document.id))
    {
        let mut titles = vec![safe_title(&document.title, &document.id)];
        let mut parent = document.parent_document_id.as_deref();
        let mut seen = BTreeSet::from([document.id.as_str()]);
        while let Some(parent_id) = parent {
            if !seen.insert(parent_id) {
                return Err(AppError::operation(
                    "Outline hierarchy contains a parent cycle",
                ));
            }
            let Some(parent_document) = by_id.get(parent_id) else {
                return Err(AppError::operation(format!(
                    "Outline document `{}` references missing or archived parent `{parent_id}`",
                    document.id
                )));
            };
            titles.push(safe_title(&parent_document.title, &parent_document.id));
            parent = parent_document.parent_document_id.as_deref();
        }
        titles.reverse();
        let file = titles.pop().expect("document title exists");
        let mut path = PathBuf::from(destination);
        path.extend(titles);
        path.push(format!("{file}.md"));
        validate_portable_generated_path(&path)?;
        paths.insert(
            document.id.clone(),
            path.to_string_lossy().replace('\\', "/"),
        );
    }
    let mut seen = BTreeMap::<String, String>::new();
    for (remote_id, path) in &paths {
        if let Some(existing) = seen.insert(portable_path_key(path), remote_id.clone()) {
            return Err(AppError::operation(format!(
                "Outline hierarchy maps remote documents `{existing}` and `{remote_id}` to the same portable local path `{path}`"
            )));
        }
    }
    Ok(paths)
}

fn plan_pull_move(
    paths: &VaultPaths,
    source: &str,
    destination: &str,
    write_lock_held: bool,
) -> Result<vulcan_core::MoveSummary, AppError> {
    if write_lock_held {
        vulcan_core::move_rewrite::move_note_unlocked(paths, source, destination, true)
            .map_err(AppError::operation)
    } else {
        vulcan_core::move_note(paths, source, destination, true).map_err(AppError::operation)
    }
}

fn select_remote_documents(
    active: &[OutlineRemoteDocument],
    scope: &OutlinePullScope,
) -> Result<BTreeSet<String>, AppError> {
    let by_id = active
        .iter()
        .map(|document| (document.id.as_str(), document))
        .collect::<BTreeMap<_, _>>();
    for remote_id in scope
        .root_document_ids
        .iter()
        .chain(&scope.excluded_document_ids)
    {
        if !by_id.contains_key(remote_id.as_str()) {
            return Err(AppError::operation(format!(
                "Outline pull scope references missing or archived document `{remote_id}`"
            )));
        }
    }
    if scope.max_depth.is_some() && scope.root_document_ids.is_empty() {
        return Err(AppError::operation(
            "Outline pull max depth requires at least one root document",
        ));
    }

    let distance_to = |document: &OutlineRemoteDocument,
                       targets: &BTreeSet<String>|
     -> Result<Option<usize>, AppError> {
        let mut current = Some(document.id.as_str());
        let mut depth = 0usize;
        let mut seen = BTreeSet::new();
        while let Some(remote_id) = current {
            if !seen.insert(remote_id) {
                return Err(AppError::operation(
                    "Outline hierarchy contains a parent cycle",
                ));
            }
            if targets.contains(remote_id) {
                return Ok(Some(depth));
            }
            current = by_id
                .get(remote_id)
                .and_then(|parent| parent.parent_document_id.as_deref());
            depth += 1;
        }
        Ok(None)
    };

    let mut selected = BTreeSet::new();
    for document in active {
        let included = if scope.root_document_ids.is_empty() {
            true
        } else {
            distance_to(document, &scope.root_document_ids)?
                .is_some_and(|depth| scope.max_depth.is_none_or(|maximum| depth <= maximum))
        };
        let excluded = distance_to(document, &scope.excluded_document_ids)?.is_some();
        if included && !excluded {
            selected.insert(document.id.clone());
        }
    }
    Ok(selected)
}

fn plan_document_attachments(
    paths: &VaultPaths,
    destination: &str,
    document: &OutlineRemoteDocument,
    note_path: &str,
    source: &str,
    mapping: Option<&OutlinePullMapping>,
) -> Result<(String, Vec<OutlinePullAttachmentPlan>), AppError> {
    let labels = outline_attachment_links(source);
    let mut attachments = Vec::with_capacity(labels.len());
    let mut replacements = BTreeMap::new();
    for (remote_url, label) in labels {
        let existing = mapping.and_then(|mapping| mapping.attachments.get(&remote_url));
        let local_path = existing.map_or_else(
            || attachment_path(destination, &document.id, &remote_url, &label),
            |attachment| attachment.local_path.clone(),
        );
        validate_managed_asset_path(destination, &local_path)?;
        let (needs_download, local_changed, unmanaged_collision) =
            match secure_read(paths.vault_root(), Path::new(&local_path)) {
                Ok(bytes) => existing.map_or((true, false, true), |attachment| {
                    (false, bytes_hash(&bytes) != attachment.content_hash, false)
                }),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => (true, false, false),
                Err(error) => return Err(AppError::operation(error)),
            };
        replacements.insert(
            remote_url.clone(),
            relative_markdown_path(note_path, &local_path)?,
        );
        attachments.push(OutlinePullAttachmentPlan {
            remote_url,
            local_path,
            needs_download,
            local_changed,
            unmanaged_collision,
        });
    }
    let desired = rewrite_markdown_link_destinations(source, |destination| {
        replacements.get(destination).cloned()
    });
    Ok((desired, attachments))
}

fn outline_attachment_links(source: &str) -> BTreeMap<String, String> {
    let mut labels = BTreeMap::new();
    let mut current = None::<(String, String)>;
    for event in Parser::new_ext(source, MarkdownOptions::all()) {
        match event {
            Event::Start(Tag::Link { dest_url, .. } | Tag::Image { dest_url, .. })
                if OUTLINE_ATTACHMENT_DESTINATION.is_match(&dest_url) =>
            {
                current = Some((dest_url.to_string(), String::new()));
            }
            Event::Text(text) | Event::Code(text) => {
                if let Some((_, label)) = current.as_mut() {
                    label.push_str(&text);
                }
            }
            Event::End(TagEnd::Link | TagEnd::Image) => {
                if let Some((destination, label)) = current.take() {
                    labels.entry(destination).or_insert(label);
                }
            }
            _ => {}
        }
    }
    labels
}

fn plan_stale_attachment_actions(
    paths: &VaultPaths,
    document: &OutlineRemoteDocument,
    mapping: Option<&OutlinePullMapping>,
    selected: &[OutlinePullAttachmentPlan],
    options: &OutlinePullOptions,
) -> Result<Vec<OutlinePullAction>, AppError> {
    let Some(mapping) = mapping else {
        return Ok(Vec::new());
    };
    let selected_urls = selected
        .iter()
        .map(|attachment| attachment.remote_url.as_str())
        .collect::<BTreeSet<_>>();
    mapping
        .attachments
        .iter()
        .filter(|(remote_url, _)| !selected_urls.contains(remote_url.as_str()))
        .map(|(remote_url, attachment)| {
            let bytes = match secure_read(paths.vault_root(), Path::new(&attachment.local_path)) {
                Ok(bytes) => Some(bytes),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(error) => return Err(AppError::operation(error)),
            };
            let local_changed = bytes
                .as_deref()
                .is_some_and(|bytes| bytes_hash(bytes) != attachment.content_hash);
            let (kind, local_path, source_local_path, reason) =
                match &options.stale_attachment_policy {
                    OutlinePullStaleAttachmentPolicy::Retain => (
                        OutlinePullActionKind::StaleAttachment,
                        attachment.local_path.clone(),
                        None,
                        "remote document no longer references this managed attachment; local file retained",
                    ),
                    OutlinePullStaleAttachmentPolicy::Archive { directory } => {
                        let directory = validate_destination(directory)?;
                        let archive_path = stale_attachment_archive_path(
                            &directory,
                            &document.id,
                            &attachment.local_path,
                        );
                        validate_contained_file_path(&directory, &archive_path)?;
                        if bytes.is_some() && paths.vault_root().join(&archive_path).exists() {
                            return Err(AppError::operation(format!(
                                "stale Outline attachment archive destination `{archive_path}` already exists"
                            )));
                        }
                        (
                            OutlinePullActionKind::ArchiveStaleAttachment,
                            archive_path,
                            bytes.as_ref().map(|_| attachment.local_path.clone()),
                            "archive the stale managed attachment at a recoverable local path",
                        )
                    }
                    OutlinePullStaleAttachmentPolicy::Delete => (
                        OutlinePullActionKind::DeleteStaleAttachment,
                        attachment.local_path.clone(),
                        bytes.as_ref().map(|_| attachment.local_path.clone()),
                        "permanently delete the explicitly confirmed stale managed attachment",
                    ),
                };
            Ok(OutlinePullAction {
                kind,
                remote_document_id: document.id.clone(),
                local_path,
                reason: reason.to_string(),
                local_changed,
                remote_changed: true,
                source_local_path,
                rewritten_local_paths: Vec::new(),
                attachment_paths: Vec::new(),
                downloaded_attachment_paths: Vec::new(),
                preserves_local_changes: kind == OutlinePullActionKind::StaleAttachment,
                conflict_markers_available: false,
                desired_content: None,
                merged_content: None,
                local_content: None,
                attachments: Vec::new(),
                stale_attachment_url: Some(remote_url.clone()),
            })
        })
        .collect()
}

fn attachment_path(destination: &str, document_id: &str, remote_url: &str, label: &str) -> String {
    let document_hash = bytes_hash(document_id.as_bytes());
    let url_hash = bytes_hash(remote_url.as_bytes());
    let label = Path::new(label)
        .file_name()
        .and_then(|name| name.to_str())
        .map_or("attachment.bin", |name| name);
    let mut filename = safe_title(label, &url_hash);
    if !filename.contains('.') {
        filename.push_str(".bin");
    }
    let filename = truncate_filename_preserving_extension(&filename, 96);
    format!(
        "{destination}/_attachments/{}/{}-{filename}",
        &document_hash[..16],
        &url_hash[..12]
    )
}

fn missing_archive_path(directory: &str, remote_id: &str, source_path: &str) -> String {
    let remote_hash = bytes_hash(remote_id.as_bytes());
    let filename = Path::new(source_path)
        .file_name()
        .and_then(|filename| filename.to_str())
        .unwrap_or("missing.md");
    format!("{directory}/{}-{filename}", &remote_hash[..12])
}

fn stale_attachment_archive_path(directory: &str, remote_id: &str, source_path: &str) -> String {
    let remote_hash = bytes_hash(remote_id.as_bytes());
    let filename = Path::new(source_path)
        .file_name()
        .and_then(|filename| filename.to_str())
        .unwrap_or("attachment.bin");
    format!("{directory}/{}/{}", &remote_hash[..12], filename)
}

fn relative_markdown_path(note_path: &str, target_path: &str) -> Result<String, AppError> {
    let note_parent = Path::new(note_path)
        .parent()
        .ok_or_else(|| AppError::operation("Outline pull note path has no parent"))?;
    let source = note_parent.components().collect::<Vec<_>>();
    let target = Path::new(target_path).components().collect::<Vec<_>>();
    let common = source
        .iter()
        .zip(&target)
        .take_while(|(left, right)| left == right)
        .count();
    let mut relative = PathBuf::new();
    for _ in common..source.len() {
        relative.push("..");
    }
    for component in &target[common..] {
        relative.push(component.as_os_str());
    }
    Ok(relative.to_string_lossy().replace('\\', "/"))
}

fn safe_title(title: &str, remote_id: &str) -> String {
    let title = title
        .chars()
        .map(|character| {
            if character.is_control()
                || matches!(
                    character,
                    '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|'
                )
            {
                '-'
            } else {
                character
            }
        })
        .collect::<String>();
    let title = title.trim().trim_matches('.').trim_end_matches([' ', '.']);
    let title = if title.is_empty() {
        format!("untitled-{}", &remote_id[..remote_id.len().min(8)])
    } else {
        title.to_string()
    };
    let title = if is_windows_reserved_component(&title) {
        format!("_{title}")
    } else {
        title
    };
    truncate_filename_preserving_extension(&title, 120)
}

fn is_windows_reserved_component(component: &str) -> bool {
    let stem = component.split('.').next().unwrap_or(component);
    let upper = stem.to_ascii_uppercase();
    matches!(upper.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || upper
            .strip_prefix("COM")
            .or_else(|| upper.strip_prefix("LPT"))
            .is_some_and(|number| {
                matches!(number, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9")
            })
}

fn truncate_utf8_bytes(value: &str, maximum: usize) -> &str {
    if value.len() <= maximum {
        return value;
    }
    let mut boundary = maximum;
    while !value.is_char_boundary(boundary) {
        boundary -= 1;
    }
    value[..boundary].trim_end_matches([' ', '.'])
}

fn truncate_filename_preserving_extension(value: &str, maximum: usize) -> String {
    if value.len() <= maximum {
        return value.to_string();
    }
    let Some((stem, extension)) = value.rsplit_once('.') else {
        return truncate_utf8_bytes(value, maximum).to_string();
    };
    let suffix = format!(".{extension}");
    if suffix.len() >= maximum {
        return truncate_utf8_bytes(value, maximum).to_string();
    }
    format!(
        "{}{}",
        truncate_utf8_bytes(stem, maximum - suffix.len()),
        suffix
    )
}

fn portable_path_key(path: &str) -> String {
    path.nfkc().flat_map(char::to_lowercase).collect()
}

fn validate_portable_generated_path(path: &Path) -> Result<(), AppError> {
    let rendered = path.to_string_lossy().replace('\\', "/");
    if rendered.len() > 240 {
        return Err(AppError::operation(format!(
            "Outline hierarchy generates a local path longer than the portable 240-byte limit: `{rendered}`"
        )));
    }
    Ok(())
}

fn validate_destination(destination: &str) -> Result<String, AppError> {
    let normalized = destination.trim().replace('\\', "/");
    let destination = normalized.trim_end_matches('/');
    let path = Path::new(destination);
    if destination.is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
        || destination == ".vulcan"
        || destination.starts_with(".vulcan/")
    {
        return Err(AppError::operation(
            "Outline pull destination must be a non-internal relative vault directory",
        ));
    }
    Ok(destination.to_string())
}

fn validate_managed_path(destination: &str, local_path: &str) -> Result<(), AppError> {
    let path = Path::new(local_path);
    if local_path.contains('\\')
        || path.is_absolute()
        || path.extension().and_then(|extension| extension.to_str()) != Some("md")
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
        || !path.starts_with(Path::new(destination))
    {
        return Err(AppError::operation(
            "Outline pull state contains an unsafe local path",
        ));
    }
    Ok(())
}

fn validate_managed_asset_path(destination: &str, local_path: &str) -> Result<(), AppError> {
    let path = Path::new(local_path);
    if local_path.contains('\\')
        || path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
        || !path.starts_with(Path::new(destination).join("_attachments"))
    {
        return Err(AppError::operation(
            "Outline pull state contains an unsafe attachment path",
        ));
    }
    Ok(())
}

fn validate_contained_file_path(directory: &str, local_path: &str) -> Result<(), AppError> {
    let path = Path::new(local_path);
    if local_path.contains('\\')
        || path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
        || !path.starts_with(Path::new(directory))
        || path == Path::new(directory)
    {
        return Err(AppError::operation(
            "Outline pull planned an unsafe contained file path",
        ));
    }
    Ok(())
}

fn remove_managed_file(paths: &VaultPaths, local_path: &str) -> Result<(), AppError> {
    match secure_read(paths.vault_root(), Path::new(local_path)) {
        Ok(_) => fs::remove_file(paths.vault_root().join(local_path)).map_err(AppError::operation),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(AppError::operation(error)),
    }
}

fn pull_action_mutates(kind: OutlinePullActionKind) -> bool {
    matches!(
        kind,
        OutlinePullActionKind::Create
            | OutlinePullActionKind::Update
            | OutlinePullActionKind::Move
            | OutlinePullActionKind::WriteConflictMarkers
            | OutlinePullActionKind::AutoMerge
            | OutlinePullActionKind::ArchiveMissing
            | OutlinePullActionKind::DeleteMissing
            | OutlinePullActionKind::ArchiveStaleAttachment
            | OutlinePullActionKind::DeleteStaleAttachment
    )
}

fn pull_action_journal_key(action: &OutlinePullAction) -> String {
    format!(
        "{:?}:{}:{}",
        action.kind, action.remote_document_id, action.local_path
    )
}

fn complete_journal_action(state: &mut OutlinePullState, action: &OutlinePullAction) {
    if let Some(journal) = state.incomplete_operation.as_mut() {
        if journal
            .pending_actions
            .remove(&pull_action_journal_key(action))
        {
            journal.completed_actions += 1;
        }
    }
}

fn ensure_pull_not_cancelled(
    is_cancelled: &dyn Fn() -> bool,
    operation_id: Option<&str>,
) -> Result<(), AppError> {
    if is_cancelled() {
        Err(AppError::operation(operation_id.map_or_else(
            || "Outline pull cancelled before mutation".to_string(),
            |operation_id| {
                format!(
                    "Outline pull operation `{operation_id}` cancelled; its durable journal will be resumed by the next live pull"
                )
            },
        )))
    } else {
        Ok(())
    }
}

fn emit_pull_progress(
    on_progress: &mut dyn FnMut(&OutlinePullProgress),
    phase: OutlinePullPhase,
    processed: usize,
    total: usize,
    current_path: Option<&str>,
    operation_id: Option<&str>,
) {
    on_progress(&OutlinePullProgress {
        phase,
        processed,
        total,
        current_path: current_path.map(str::to_string),
        operation_id: operation_id.map(str::to_string),
    });
}

struct ThreeWayMerge {
    content: String,
    has_conflicts: bool,
}

fn three_way_merge(
    local: &str,
    base: &str,
    remote: &str,
    remote_id: &str,
) -> Result<ThreeWayMerge, AppError> {
    let mut local_file = tempfile::NamedTempFile::new().map_err(AppError::operation)?;
    let mut base_file = tempfile::NamedTempFile::new().map_err(AppError::operation)?;
    let mut remote_file = tempfile::NamedTempFile::new().map_err(AppError::operation)?;
    local_file
        .write_all(local.as_bytes())
        .map_err(AppError::operation)?;
    base_file
        .write_all(base.as_bytes())
        .map_err(AppError::operation)?;
    remote_file
        .write_all(remote.as_bytes())
        .map_err(AppError::operation)?;
    let output = Command::new("git")
        .arg("merge-file")
        .arg("--stdout")
        .arg("--diff3")
        .arg("-L")
        .arg("LOCAL")
        .arg("-L")
        .arg("BASE")
        .arg("-L")
        .arg(format!("OUTLINE {remote_id}"))
        .arg(local_file.path())
        .arg(base_file.path())
        .arg(remote_file.path())
        .output()
        .map_err(|error| {
            AppError::operation(format!(
                "failed to run `git merge-file` for Outline conflict resolution: {error}"
            ))
        })?;
    let has_conflicts = match output.status.code() {
        Some(0) => false,
        Some(1..=127) => true,
        _ => {
            let detail = String::from_utf8_lossy(&output.stderr);
            return Err(AppError::operation(format!(
                "`git merge-file` failed during Outline conflict resolution: {}",
                detail.trim()
            )));
        }
    };
    let content = String::from_utf8(output.stdout)
        .map_err(|_| AppError::operation("`git merge-file` returned non-UTF-8 Markdown"))?;
    Ok(ThreeWayMerge {
        content,
        has_conflicts,
    })
}

fn extract_local_from_diff3(content: &str) -> String {
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Section {
        Normal,
        Local,
        Base,
        Remote,
    }
    let mut section = Section::Normal;
    let mut extracted = String::with_capacity(content.len());
    let mut complete_markers = 0usize;
    for line in content.split_inclusive('\n') {
        let marker = line.trim_end_matches(['\r', '\n']);
        match section {
            Section::Normal if marker == "<<<<<<< LOCAL" => section = Section::Local,
            Section::Local if marker == "||||||| BASE" => section = Section::Base,
            Section::Base if marker == "=======" => section = Section::Remote,
            Section::Remote if marker.starts_with(">>>>>>> OUTLINE ") => {
                section = Section::Normal;
                complete_markers += 1;
            }
            Section::Normal | Section::Local => extracted.push_str(line),
            Section::Base | Section::Remote => {}
        }
    }
    if section == Section::Normal && complete_markers > 0 {
        extracted
    } else {
        content.to_string()
    }
}

#[allow(clippy::too_many_arguments)]
fn report(
    profile: &str,
    collection_id: &str,
    destination: &str,
    dry_run: bool,
    applied: bool,
    operation_id: Option<String>,
    resumed_operation: bool,
    actions: Vec<OutlinePullAction>,
) -> OutlinePullReport {
    let count = |kind| actions.iter().filter(|action| action.kind == kind).count();
    let attachments_planned = actions.iter().map(|action| action.attachments.len()).sum();
    let attachments_downloaded = actions
        .iter()
        .map(|action| action.downloaded_attachment_paths.len())
        .sum();
    OutlinePullReport {
        profile: profile.to_string(),
        collection_id: collection_id.to_string(),
        destination: destination.to_string(),
        dry_run,
        applied,
        operation_id,
        resumed_operation,
        conflicts: count(OutlinePullActionKind::Conflict),
        created: count(OutlinePullActionKind::Create),
        updated: count(OutlinePullActionKind::Update),
        moved: count(OutlinePullActionKind::Move),
        unchanged: count(OutlinePullActionKind::Unchanged),
        conflict_markers_written: count(OutlinePullActionKind::WriteConflictMarkers),
        auto_merged: count(OutlinePullActionKind::AutoMerge),
        remote_missing: count(OutlinePullActionKind::RemoteMissing),
        archived_missing: count(OutlinePullActionKind::ArchiveMissing),
        deleted_missing: count(OutlinePullActionKind::DeleteMissing),
        out_of_scope: count(OutlinePullActionKind::OutOfScope),
        attachments_planned,
        attachments_downloaded,
        stale_attachments: count(OutlinePullActionKind::StaleAttachment),
        archived_stale_attachments: count(OutlinePullActionKind::ArchiveStaleAttachment),
        deleted_stale_attachments: count(OutlinePullActionKind::DeleteStaleAttachment),
        actions,
    }
}

fn content_hash(content: &str) -> String {
    bytes_hash(content.as_bytes())
}

fn validate_remote_work_limit(
    remote: &[OutlineRemoteDocument],
    options: &OutlinePullOptions,
) -> Result<(), AppError> {
    if options.max_remote_documents == 0 || options.max_remote_content_bytes == 0 {
        return Err(AppError::operation(
            "Outline pull document and content byte limits must be greater than zero",
        ));
    }
    if remote.len() > options.max_remote_documents {
        return Err(AppError::operation(format!(
            "Outline collection contains {} documents, exceeding the configured pull limit of {}",
            remote.len(),
            options.max_remote_documents
        )));
    }
    let content_bytes = remote.iter().try_fold(0usize, |total, document| {
        total.checked_add(document.text.len())
    });
    if content_bytes.is_none_or(|total| total > options.max_remote_content_bytes) {
        return Err(AppError::operation(format!(
            "Outline collection content exceeds the configured pull byte limit of {}",
            options.max_remote_content_bytes
        )));
    }
    Ok(())
}

fn validate_attachment_work_limit(
    actions: &[OutlinePullAction],
    options: &OutlinePullOptions,
) -> Result<(), AppError> {
    if options.max_attachments == 0
        || options.max_attachment_bytes == 0
        || options.max_total_attachment_bytes == 0
    {
        return Err(AppError::operation(
            "Outline pull attachment limits must be greater than zero",
        ));
    }
    let count = actions
        .iter()
        .map(|action| action.attachments.len())
        .sum::<usize>();
    if count > options.max_attachments {
        return Err(AppError::operation(format!(
            "Outline pull references {count} attachments, exceeding the configured limit of {}",
            options.max_attachments
        )));
    }
    Ok(())
}

fn bytes_hash(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

fn state_path(paths: &VaultPaths, profile: &str) -> Result<PathBuf, AppError> {
    if profile.is_empty()
        || !profile
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
    {
        return Err(AppError::operation(
            "Outline profile names may contain only ASCII letters, digits, '-' and '_'",
        ));
    }
    Ok(paths
        .vulcan_dir()
        .join("integrations")
        .join("outline-pull")
        .join(format!("{profile}.json")))
}

fn load_state(
    paths: &VaultPaths,
    profile: &str,
    collection_id: &str,
    destination: &str,
    connector_identity: Option<&str>,
) -> Result<OutlinePullState, AppError> {
    let path = state_path(paths, profile)?;
    if !path.exists() {
        return Ok(OutlinePullState::empty(
            profile,
            collection_id,
            destination,
            connector_identity,
        ));
    }
    let bytes = fs::read(path).map_err(AppError::operation)?;
    let state: OutlinePullState = serde_json::from_slice(&bytes)
        .map_err(|_| AppError::operation("Outline pull state contains malformed JSON"))?;
    state.validate(profile, collection_id, destination, connector_identity)?;
    Ok(state)
}

/// Loads durable pull bindings for an explicit, fail-closed publisher adoption.
pub fn load_outline_pulled_bindings(
    paths: &VaultPaths,
    profile: &str,
    collection_id: &str,
) -> Result<Vec<OutlinePulledBinding>, AppError> {
    let path = state_path(paths, profile)?;
    if !path.exists() {
        return Ok(Vec::new());
    }
    let bytes = fs::read(path).map_err(AppError::operation)?;
    let state: OutlinePullState = serde_json::from_slice(&bytes)
        .map_err(|_| AppError::operation("Outline pull state contains malformed JSON"))?;
    let destination = state.destination.clone();
    state.validate(profile, collection_id, &destination, None)?;
    state
        .documents
        .into_iter()
        .map(|(remote_document_id, mapping)| {
            let last_remote_source_hash = mapping.last_remote_source_hash.ok_or_else(|| {
                AppError::operation(format!(
                    "pulled Outline document `{remote_document_id}` predates adoption-safe remote baselines; pull it again before adoption"
                ))
            })?;
            Ok(OutlinePulledBinding {
                local_path: mapping.local_path,
                remote_document_id,
                last_remote_source_hash,
                last_remote_title: mapping.last_remote_title,
                last_remote_parent_id: mapping.last_remote_parent_id,
                attachments: mapping
                    .attachments
                    .into_iter()
                    .map(|(remote_url, attachment)| OutlinePulledAttachmentBinding {
                        local_path: attachment.local_path,
                        remote_url,
                        content_hash: attachment.content_hash,
                    })
                    .collect(),
            })
        })
        .collect()
}

struct StateLock {
    file: File,
    state_path: PathBuf,
}

impl StateLock {
    fn acquire(paths: &VaultPaths, profile: &str) -> Result<Self, AppError> {
        let state_path = state_path(paths, profile)?;
        let directory = state_path.parent().expect("state path has a parent");
        fs::create_dir_all(directory).map_err(AppError::operation)?;
        let lock_path = directory.join(format!("{profile}.lock"));
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(lock_path)
            .map_err(AppError::operation)?;
        file.try_lock_exclusive()
            .map_err(|_| AppError::operation("Outline pull state is locked by another process"))?;
        Ok(Self { file, state_path })
    }

    fn save(&self, state: &OutlinePullState) -> Result<(), AppError> {
        let bytes = serde_json::to_vec_pretty(state).map_err(AppError::operation)?;
        let temporary = self.state_path.with_extension("json.tmp");
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&temporary)
            .map_err(AppError::operation)?;
        file.write_all(&bytes).map_err(AppError::operation)?;
        file.sync_all().map_err(AppError::operation)?;
        fs::rename(&temporary, &self.state_path).map_err(AppError::operation)?;
        File::open(self.state_path.parent().expect("state parent"))
            .and_then(|directory| directory.sync_all())
            .map_err(AppError::operation)
    }
}

impl Drop for StateLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::publish::outline::{OutlineDownloadedAttachment, OutlineRemoteAttachment};
    use std::cell::Cell;
    use tempfile::tempdir;
    use vulcan_core::initialize_vulcan_dir;

    struct MockApi {
        documents: Vec<OutlineRemoteDocument>,
        download_count: Cell<usize>,
    }

    impl OutlineApi for MockApi {
        fn list_collection_documents(
            &self,
            _collection_id: &str,
        ) -> Result<Vec<OutlineRemoteDocument>, AppError> {
            Ok(self.documents.clone())
        }

        fn document_info(&self, _id: &str) -> Result<OutlineRemoteDocument, AppError> {
            unreachable!()
        }

        fn create_document(
            &self,
            _id: &str,
            _collection_id: &str,
            _parent_document_id: Option<&str>,
            _title: &str,
            _text: &str,
        ) -> Result<OutlineRemoteDocument, AppError> {
            unreachable!()
        }

        fn update_document(
            &self,
            _id: &str,
            _title: &str,
            _text: &str,
        ) -> Result<OutlineRemoteDocument, AppError> {
            unreachable!()
        }

        fn move_document(
            &self,
            _id: &str,
            _collection_id: &str,
            _parent_document_id: Option<&str>,
        ) -> Result<OutlineRemoteDocument, AppError> {
            unreachable!()
        }

        fn archive_document(&self, _id: &str) -> Result<OutlineRemoteDocument, AppError> {
            unreachable!()
        }

        fn upload_attachment(
            &self,
            _document_id: &str,
            _name: &str,
            _content_type: &str,
            _bytes: &[u8],
        ) -> Result<OutlineRemoteAttachment, AppError> {
            unreachable!()
        }

        fn download_attachment(
            &self,
            _url: &str,
            max_bytes: usize,
        ) -> Result<OutlineDownloadedAttachment, AppError> {
            let bytes = b"downloaded image".to_vec();
            assert!(bytes.len() <= max_bytes);
            self.download_count.set(self.download_count.get() + 1);
            Ok(OutlineDownloadedAttachment {
                bytes,
                content_type: Some("image/png".to_string()),
            })
        }
    }

    fn api(documents: Vec<OutlineRemoteDocument>) -> MockApi {
        MockApi {
            documents,
            download_count: Cell::new(0),
        }
    }

    fn document(id: &str, title: &str, text: &str, parent: Option<&str>) -> OutlineRemoteDocument {
        OutlineRemoteDocument {
            id: id.to_string(),
            title: title.to_string(),
            text: text.to_string(),
            collection_id: "collection".to_string(),
            parent_document_id: parent.map(str::to_string),
            archived_at: None,
            revision: None,
            updated_at: None,
        }
    }

    #[test]
    fn pull_materializes_hierarchy_reverse_markdown_and_links_idempotently() {
        let temp = tempdir().expect("temp dir");
        let paths = VaultPaths::new(temp.path());
        initialize_vulcan_dir(&paths).expect("initialize vault");
        let api = api(vec![
            document(
                "parent",
                "THE ÒRÌSHÀ",
                ":::warning\nCareful\n:::\n\n[Yemoja](/doc/child)",
                None,
            ),
            document("child", "Yemoja", "# Water\n", Some("parent")),
        ]);

        let first = pull_outline(
            &paths,
            &api,
            "wiki",
            "collection",
            "Imported",
            false,
            &OutlinePullConflictPolicy::abort(),
        )
        .expect("initial pull");
        assert!(first.applied);
        assert_eq!(first.created, 2);
        let parent =
            fs::read_to_string(temp.path().join("Imported/THE ÒRÌSHÀ.md")).expect("parent note");
        assert!(parent.contains("> [!warning]"));
        assert!(parent.contains("[[Imported/THE ÒRÌSHÀ/Yemoja]]"));
        assert!(temp.path().join("Imported/THE ÒRÌSHÀ/Yemoja.md").is_file());
        let bindings = load_outline_pulled_bindings(&paths, "wiki", "collection")
            .expect("durable pulled bindings");
        assert_eq!(bindings.len(), 2);
        assert!(bindings.iter().any(|binding| {
            binding.remote_document_id == "child"
                && binding.local_path == "Imported/THE ÒRÌSHÀ/Yemoja.md"
        }));

        let second = pull_outline(
            &paths,
            &api,
            "wiki",
            "collection",
            "Imported",
            false,
            &OutlinePullConflictPolicy::abort(),
        )
        .expect("idempotent pull");
        assert!(second.applied);
        assert_eq!(second.unchanged, 2);
    }

    #[test]
    fn pull_materializes_referenced_attachments_and_repairs_missing_files() {
        let temp = tempdir().expect("temp dir");
        let paths = VaultPaths::new(temp.path());
        initialize_vulcan_dir(&paths).expect("initialize vault");
        let api = api(vec![document(
            "home",
            "Home",
            "![diagram.png](/api/attachments.redirect?id=asset)",
            None,
        )]);

        let first = pull_outline(
            &paths,
            &api,
            "wiki",
            "collection",
            "Imported",
            false,
            &OutlinePullConflictPolicy::abort(),
        )
        .expect("attachment pull");
        assert_eq!(first.attachments_planned, 1);
        assert_eq!(first.attachments_downloaded, 1);
        assert_eq!(api.download_count.get(), 1);
        let attachment_path = &first.actions[0].attachment_paths[0];
        assert_eq!(
            fs::read(temp.path().join(attachment_path)).unwrap(),
            b"downloaded image"
        );
        let note = fs::read_to_string(temp.path().join("Imported/Home.md")).unwrap();
        assert!(!note.contains("/api/attachments.redirect"));
        assert!(note.contains("![diagram.png](_attachments/"));

        let second = pull_outline(
            &paths,
            &api,
            "wiki",
            "collection",
            "Imported",
            false,
            &OutlinePullConflictPolicy::abort(),
        )
        .expect("idempotent attachment pull");
        assert_eq!(second.unchanged, 1);
        assert_eq!(second.attachments_downloaded, 0);
        assert_eq!(api.download_count.get(), 1);

        fs::remove_file(temp.path().join(attachment_path)).expect("remove attachment");
        let repaired = pull_outline(
            &paths,
            &api,
            "wiki",
            "collection",
            "Imported",
            false,
            &OutlinePullConflictPolicy::abort(),
        )
        .expect("repair attachment");
        assert_eq!(repaired.updated, 1);
        assert_eq!(repaired.attachments_downloaded, 1);
        assert_eq!(api.download_count.get(), 2);
    }

    #[test]
    fn pull_materializes_reference_style_and_parenthesized_attachment_links() {
        let temp = tempdir().expect("temp dir");
        let paths = VaultPaths::new(temp.path());
        initialize_vulcan_dir(&paths).expect("initialize vault");
        let api = api(vec![document(
            "home",
            "Home",
            "[diagram][asset]\n\n[asset]: </api/attachments.redirect?id=(asset)> \"diagram\"\n\n`![ignored](/api/attachments.redirect?id=ignored)`",
            None,
        )]);

        let report = pull_outline(
            &paths,
            &api,
            "wiki",
            "collection",
            "Imported",
            false,
            &OutlinePullConflictPolicy::abort(),
        )
        .expect("reference attachment pull");
        assert_eq!(report.attachments_planned, 1);
        assert_eq!(report.attachments_downloaded, 1);
        let note = fs::read_to_string(temp.path().join("Imported/Home.md")).unwrap();
        assert!(note.contains("[diagram][asset]"));
        assert!(note.contains("[asset]: <_attachments/"));
        assert!(note.contains("id=ignored"));
    }

    #[test]
    fn stale_managed_attachments_are_retained_or_recoverably_archived() {
        let temp = tempdir().expect("temp dir");
        let paths = VaultPaths::new(temp.path());
        initialize_vulcan_dir(&paths).expect("initialize vault");
        let with_attachment = api(vec![document(
            "home",
            "Home",
            "![diagram.png](/api/attachments.redirect?id=asset)",
            None,
        )]);
        let initial = pull_outline(
            &paths,
            &with_attachment,
            "wiki",
            "collection",
            "Imported",
            false,
            &OutlinePullConflictPolicy::abort(),
        )
        .expect("initial attachment pull");
        let attachment_path = initial.actions[0].attachment_paths[0].clone();
        let without_attachment = api(vec![document("home", "Home", "attachment removed\n", None)]);

        let retained = pull_outline(
            &paths,
            &without_attachment,
            "wiki",
            "collection",
            "Imported",
            false,
            &OutlinePullConflictPolicy::abort(),
        )
        .expect("retain stale attachment");
        assert_eq!(retained.stale_attachments, 1);
        assert!(temp.path().join(&attachment_path).is_file());
        let retained_state = load_state(&paths, "wiki", "collection", "Imported", None).unwrap();
        assert_eq!(retained_state.documents["home"].attachments.len(), 1);

        let archived = pull_outline_with_options_and_write_authorizer(
            &paths,
            &without_attachment,
            "wiki",
            "collection",
            "Imported",
            false,
            &OutlinePullConflictPolicy::abort(),
            &OutlinePullOptions {
                stale_attachment_policy: OutlinePullStaleAttachmentPolicy::Archive {
                    directory: "Archive/Outline Assets".to_string(),
                },
                ..OutlinePullOptions::default()
            },
            &|_| Ok(()),
        )
        .expect("archive stale attachment");
        assert_eq!(archived.archived_stale_attachments, 1);
        let archive_action = archived
            .actions
            .iter()
            .find(|action| action.kind == OutlinePullActionKind::ArchiveStaleAttachment)
            .unwrap();
        assert!(!temp.path().join(&attachment_path).exists());
        assert_eq!(
            fs::read(temp.path().join(&archive_action.local_path)).unwrap(),
            b"downloaded image"
        );
        let archived_state = load_state(&paths, "wiki", "collection", "Imported", None).unwrap();
        assert!(archived_state.documents["home"].attachments.is_empty());
    }

    #[test]
    fn stale_attachment_deletion_requires_an_exact_live_count() {
        let temp = tempdir().expect("temp dir");
        let paths = VaultPaths::new(temp.path());
        initialize_vulcan_dir(&paths).expect("initialize vault");
        let with_attachment = api(vec![document(
            "home",
            "Home",
            "![diagram.png](/api/attachments.redirect?id=asset)",
            None,
        )]);
        let initial = pull_outline(
            &paths,
            &with_attachment,
            "wiki",
            "collection",
            "Imported",
            false,
            &OutlinePullConflictPolicy::abort(),
        )
        .expect("initial attachment pull");
        let attachment_path = initial.actions[0].attachment_paths[0].clone();
        let without_attachment = api(vec![document("home", "Home", "removed\n", None)]);
        let options = OutlinePullOptions {
            stale_attachment_policy: OutlinePullStaleAttachmentPolicy::Delete,
            ..OutlinePullOptions::default()
        };
        assert!(pull_outline_with_options_and_write_authorizer(
            &paths,
            &without_attachment,
            "wiki",
            "collection",
            "Imported",
            false,
            &OutlinePullConflictPolicy::abort(),
            &options,
            &|_| Ok(()),
        )
        .is_err());
        assert!(temp.path().join(&attachment_path).is_file());

        let deleted = pull_outline_with_options_and_write_authorizer(
            &paths,
            &without_attachment,
            "wiki",
            "collection",
            "Imported",
            false,
            &OutlinePullConflictPolicy::abort(),
            &OutlinePullOptions {
                confirmed_stale_attachment_delete_count: Some(1),
                ..options
            },
            &|_| Ok(()),
        )
        .expect("confirmed stale attachment deletion");
        assert_eq!(deleted.deleted_stale_attachments, 1);
        assert!(!temp.path().join(&attachment_path).exists());
    }

    #[test]
    fn pull_can_apply_remote_hierarchy_changes_as_link_aware_moves() {
        let temp = tempdir().expect("temp dir");
        let paths = VaultPaths::new(temp.path());
        initialize_vulcan_dir(&paths).expect("initialize vault");
        let initial = api(vec![
            document("parent", "Parent", "parent\n", None),
            document("child", "Child", "child\n", Some("parent")),
        ]);
        pull_outline(
            &paths,
            &initial,
            "wiki",
            "collection",
            "Imported",
            false,
            &OutlinePullConflictPolicy::abort(),
        )
        .expect("initial pull");
        fs::write(
            temp.path().join("References.md"),
            "[[Imported/Parent/Child]]\n",
        )
        .expect("reference note");
        fs::write(
            temp.path().join("Imported/Parent.md"),
            "local parent edit\n",
        )
        .expect("local parent edit");
        crate::scan::refresh_cache_incrementally(&paths).expect("index reference note");

        let renamed = api(vec![
            document("parent", "Renamed", "parent\n", None),
            document("child", "Child", "child\n", Some("parent")),
        ]);
        let options = OutlinePullOptions {
            apply_remote_moves: true,
            ..OutlinePullOptions::default()
        };
        let plan = pull_outline_with_options_and_write_authorizer(
            &paths,
            &renamed,
            "wiki",
            "collection",
            "Imported",
            true,
            &OutlinePullConflictPolicy::abort(),
            &options,
            &|_| Ok(()),
        )
        .expect("move plan");
        assert_eq!(plan.moved, 2);
        assert!(plan
            .actions
            .iter()
            .any(|action| action.rewritten_local_paths == ["References.md"]));

        let applied = pull_outline_with_options_and_write_authorizer(
            &paths,
            &renamed,
            "wiki",
            "collection",
            "Imported",
            false,
            &OutlinePullConflictPolicy::abort(),
            &options,
            &|_| Ok(()),
        )
        .expect("apply remote moves");
        assert_eq!(applied.moved, 2);
        assert!(!temp.path().join("Imported/Parent.md").exists());
        assert!(!temp.path().join("Imported/Parent/Child.md").exists());
        assert!(temp.path().join("Imported/Renamed.md").is_file());
        assert!(temp.path().join("Imported/Renamed/Child.md").is_file());
        assert_eq!(
            fs::read_to_string(temp.path().join("Imported/Renamed.md")).unwrap(),
            "local parent edit\n"
        );
        let reference = fs::read_to_string(temp.path().join("References.md")).unwrap();
        assert!(
            reference.contains("Child"),
            "rewritten reference: {reference}"
        );
        assert!(!reference.contains("Parent/Child"));
    }

    #[test]
    fn missing_documents_can_be_archived_recoverably() {
        let archive_temp = tempdir().expect("archive temp dir");
        let archive_paths = VaultPaths::new(archive_temp.path());
        initialize_vulcan_dir(&archive_paths).expect("initialize archive vault");
        let initial = api(vec![document("home", "Home", "base\n", None)]);
        pull_outline(
            &archive_paths,
            &initial,
            "wiki",
            "collection",
            "Imported",
            false,
            &OutlinePullConflictPolicy::abort(),
        )
        .expect("initial archive pull");
        fs::write(archive_temp.path().join("Imported/Home.md"), "local edit\n")
            .expect("local edit");
        let missing = api(Vec::new());
        let archive_options = OutlinePullOptions {
            missing_policy: OutlinePullMissingPolicy::archive_all("Archive".to_string()),
            ..OutlinePullOptions::default()
        };
        let archived = pull_outline_with_options_and_write_authorizer(
            &archive_paths,
            &missing,
            "wiki",
            "collection",
            "Imported",
            false,
            &OutlinePullConflictPolicy::abort(),
            &archive_options,
            &|_| Ok(()),
        )
        .expect("archive missing document");
        assert_eq!(archived.archived_missing, 1);
        assert!(!archive_temp.path().join("Imported/Home.md").exists());
        let archive_path = &archived.actions[0].local_path;
        assert_eq!(
            fs::read_to_string(archive_temp.path().join(archive_path)).unwrap(),
            "local edit\n"
        );
        let rerun = pull_outline(
            &archive_paths,
            &missing,
            "wiki",
            "collection",
            "Imported",
            true,
            &OutlinePullConflictPolicy::abort(),
        )
        .expect("archived mapping is cleared");
        assert_eq!(rerun.remote_missing, 0);
    }

    #[test]
    fn missing_documents_require_exact_count_confirmation_before_delete() {
        let delete_temp = tempdir().expect("delete temp dir");
        let delete_paths = VaultPaths::new(delete_temp.path());
        initialize_vulcan_dir(&delete_paths).expect("initialize delete vault");
        let with_attachment = api(vec![document(
            "home",
            "Home",
            "![diagram.png](/api/attachments.redirect?id=asset)",
            None,
        )]);
        let pulled = pull_outline(
            &delete_paths,
            &with_attachment,
            "wiki",
            "collection",
            "Imported",
            false,
            &OutlinePullConflictPolicy::abort(),
        )
        .expect("initial delete pull");
        let attachment_path = pulled.actions[0].attachment_paths[0].clone();
        let missing = api(Vec::new());
        let unconfirmed = OutlinePullOptions {
            missing_policy: OutlinePullMissingPolicy::delete_all(),
            ..OutlinePullOptions::default()
        };
        assert!(pull_outline_with_options_and_write_authorizer(
            &delete_paths,
            &missing,
            "wiki",
            "collection",
            "Imported",
            false,
            &OutlinePullConflictPolicy::abort(),
            &unconfirmed,
            &|_| Ok(()),
        )
        .is_err());
        assert!(delete_temp.path().join("Imported/Home.md").is_file());

        let confirmed = OutlinePullOptions {
            missing_policy: OutlinePullMissingPolicy::delete_all(),
            confirmed_delete_count: Some(1),
            ..OutlinePullOptions::default()
        };
        let deleted = pull_outline_with_options_and_write_authorizer(
            &delete_paths,
            &missing,
            "wiki",
            "collection",
            "Imported",
            false,
            &OutlinePullConflictPolicy::abort(),
            &confirmed,
            &|_| Ok(()),
        )
        .expect("delete missing document");
        assert_eq!(deleted.deleted_missing, 1);
        assert!(!delete_temp.path().join("Imported/Home.md").exists());
        assert!(!delete_temp.path().join(attachment_path).exists());
    }

    #[test]
    fn pull_conflicts_support_overwrite_and_diff3_markers() {
        let temp = tempdir().expect("temp dir");
        let paths = VaultPaths::new(temp.path());
        initialize_vulcan_dir(&paths).expect("initialize vault");
        let initial = api(vec![document("home", "Home", "base\n", None)]);
        pull_outline(
            &paths,
            &initial,
            "wiki",
            "collection",
            "Imported",
            false,
            &OutlinePullConflictPolicy::abort(),
        )
        .expect("initial pull");
        fs::write(temp.path().join("Imported/Home.md"), "local edit\n").expect("local edit");
        let changed = api(vec![document("home", "Home", "remote edit\n", None)]);

        let conflict = pull_outline(
            &paths,
            &changed,
            "wiki",
            "collection",
            "Imported",
            false,
            &OutlinePullConflictPolicy::abort(),
        )
        .expect("conflict plan");
        assert!(!conflict.applied);
        assert_eq!(conflict.conflicts, 1);
        assert_eq!(
            fs::read_to_string(temp.path().join("Imported/Home.md")).unwrap(),
            "local edit\n"
        );

        let markers = pull_outline(
            &paths,
            &changed,
            "wiki",
            "collection",
            "Imported",
            false,
            &OutlinePullConflictPolicy::markers_all(),
        )
        .expect("marker pull");
        assert!(markers.applied);
        assert_eq!(markers.conflict_markers_written, 1);
        let merged = fs::read_to_string(temp.path().join("Imported/Home.md")).unwrap();
        assert!(merged.contains("<<<<<<< LOCAL\nlocal edit"));
        assert!(merged.contains("||||||| BASE\nbase"));
        assert!(merged.contains("=======\nremote edit"));

        pull_outline(
            &paths,
            &changed,
            "wiki",
            "collection",
            "Imported",
            false,
            &OutlinePullConflictPolicy::markers_all(),
        )
        .expect("repeat marker pull");
        let repeated = fs::read_to_string(temp.path().join("Imported/Home.md")).unwrap();
        assert_eq!(repeated.matches("<<<<<<< LOCAL").count(), 1);
        assert!(repeated.contains("<<<<<<< LOCAL\nlocal edit"));

        fs::write(temp.path().join("Imported/Home.md"), "remote edit\n").expect("resolve markers");
        let overwritten = pull_outline(
            &paths,
            &changed,
            "wiki",
            "collection",
            "Imported",
            false,
            &OutlinePullConflictPolicy::abort(),
        )
        .expect("adopt resolved local content");
        assert!(overwritten.applied);
        assert_eq!(
            fs::read_to_string(temp.path().join("Imported/Home.md")).unwrap(),
            "remote edit\n"
        );
    }

    #[test]
    fn missing_managed_local_note_conflicts_and_can_be_restored() {
        let temp = tempdir().expect("temp dir");
        let paths = VaultPaths::new(temp.path());
        initialize_vulcan_dir(&paths).expect("initialize vault");
        let remote = api(vec![document("home", "Home", "remote body\n", None)]);
        pull_outline(
            &paths,
            &remote,
            "wiki",
            "collection",
            "Imported",
            false,
            &OutlinePullConflictPolicy::abort(),
        )
        .expect("initial pull");
        fs::remove_file(temp.path().join("Imported/Home.md")).expect("remove managed note");

        let conflict = pull_outline(
            &paths,
            &remote,
            "wiki",
            "collection",
            "Imported",
            false,
            &OutlinePullConflictPolicy::abort(),
        )
        .expect("missing note conflict");
        assert_eq!(conflict.conflicts, 1);
        assert!(!conflict.actions[0].conflict_markers_available);
        assert!(conflict.actions[0].reason.contains("missing"));

        let restored = pull_outline(
            &paths,
            &remote,
            "wiki",
            "collection",
            "Imported",
            false,
            &OutlinePullConflictPolicy::overwrite_all(),
        )
        .expect("restore missing note");
        assert!(restored.applied);
        assert_eq!(
            fs::read_to_string(temp.path().join("Imported/Home.md")).unwrap(),
            "remote body\n"
        );
    }

    #[test]
    fn conflict_marker_policy_auto_merges_non_overlapping_line_edits() {
        let temp = tempdir().expect("temp dir");
        let paths = VaultPaths::new(temp.path());
        initialize_vulcan_dir(&paths).expect("initialize vault");
        pull_outline(
            &paths,
            &api(vec![document(
                "home",
                "Home",
                "local line\nshared line\nremote line\n",
                None,
            )]),
            "wiki",
            "collection",
            "Imported",
            false,
            &OutlinePullConflictPolicy::abort(),
        )
        .expect("initial pull");
        fs::write(
            temp.path().join("Imported/Home.md"),
            "local edit\nshared line\nremote line\n",
        )
        .expect("local edit");
        let remote = api(vec![document(
            "home",
            "Home",
            "local line\nshared line\nremote edit\n",
            None,
        )]);

        let merged = pull_outline(
            &paths,
            &remote,
            "wiki",
            "collection",
            "Imported",
            false,
            &OutlinePullConflictPolicy::markers_all(),
        )
        .expect("automatic three-way merge");
        assert!(merged.applied);
        assert_eq!(merged.auto_merged, 1);
        assert_eq!(merged.conflict_markers_written, 0);
        assert_eq!(
            fs::read_to_string(temp.path().join("Imported/Home.md")).unwrap(),
            "local edit\nshared line\nremote edit\n"
        );

        let rerun = pull_outline(
            &paths,
            &remote,
            "wiki",
            "collection",
            "Imported",
            false,
            &OutlinePullConflictPolicy::abort(),
        )
        .expect("merged local result remains stable");
        assert_eq!(rerun.unchanged, 1);
    }

    #[test]
    fn pull_rejects_case_collisions_and_internal_destinations() {
        let temp = tempdir().expect("temp dir");
        let paths = VaultPaths::new(temp.path());
        initialize_vulcan_dir(&paths).expect("initialize vault");
        let api = api(vec![
            document("one", "Home", "one", None),
            document("two", "home", "two", None),
        ]);
        assert!(pull_outline(
            &paths,
            &api,
            "wiki",
            "collection",
            "Imported",
            true,
            &OutlinePullConflictPolicy::abort(),
        )
        .is_err());
        assert!(pull_outline(
            &paths,
            &api,
            "wiki",
            "collection",
            ".vulcan/imported",
            true,
            &OutlinePullConflictPolicy::abort(),
        )
        .is_err());
        assert!(pull_outline(
            &paths,
            &api,
            "wiki",
            "collection",
            ".vulcan\\imported",
            true,
            &OutlinePullConflictPolicy::abort(),
        )
        .is_err());
    }

    #[test]
    fn pull_rejects_unicode_collisions_or_orphaned_hierarchy() {
        let temp = tempdir().expect("temp dir");
        let paths = VaultPaths::new(temp.path());
        initialize_vulcan_dir(&paths).expect("initialize vault");
        let collision = api(vec![
            document("one", "Café", "one", None),
            document("two", "Cafe\u{301}", "two", None),
        ]);
        let error = pull_outline(
            &paths,
            &collision,
            "wiki",
            "collection",
            "Imported",
            true,
            &OutlinePullConflictPolicy::abort(),
        )
        .expect_err("canonical Unicode collision must fail closed");
        assert!(error.to_string().contains("portable local path"));

        let orphan = api(vec![document("child", "Child", "body", Some("missing"))]);
        let error = pull_outline(
            &paths,
            &orphan,
            "wiki",
            "collection",
            "Imported",
            true,
            &OutlinePullConflictPolicy::abort(),
        )
        .expect_err("orphaned hierarchy must fail closed");
        assert!(error.to_string().contains("missing or archived parent"));
    }

    #[test]
    fn generated_names_are_windows_safe_and_byte_bounded() {
        assert_eq!(safe_title("CON", "remote"), "_CON");
        assert_eq!(safe_title("lpt9.txt", "remote"), "_lpt9.txt");
        assert_eq!(safe_title("name. ", "remote"), "name");
        let long = format!("{}🍵.png", "é".repeat(100));
        let truncated = truncate_filename_preserving_extension(&safe_title(&long, "remote"), 96);
        assert!(truncated.len() <= 96);
        assert_eq!(
            Path::new(&truncated)
                .extension()
                .and_then(|ext| ext.to_str()),
            Some("png")
        );
        assert!(std::str::from_utf8(truncated.as_bytes()).is_ok());
    }

    #[test]
    fn live_pull_authorizes_the_fresh_plan_before_writing_notes() {
        let temp = tempdir().expect("temp dir");
        let paths = VaultPaths::new(temp.path());
        initialize_vulcan_dir(&paths).expect("initialize vault");
        let api = api(vec![document("home", "Home", "remote\n", None)]);

        let error = pull_outline_with_write_authorizer(
            &paths,
            &api,
            "wiki",
            "collection",
            "Imported",
            false,
            &OutlinePullConflictPolicy::abort(),
            &|path| Err(AppError::operation(format!("denied {path}"))),
        )
        .expect_err("write authorization should fail");

        assert_eq!(error.message(), "denied Imported/Home.md");
        assert!(!temp.path().join("Imported/Home.md").exists());
    }

    #[test]
    fn cancelled_pull_keeps_a_resumable_operation_journal_and_reports_progress() {
        let temp = tempdir().expect("temp dir");
        let paths = VaultPaths::new(temp.path());
        initialize_vulcan_dir(&paths).expect("initialize vault");
        let api = api(vec![
            document("alpha", "Alpha", "alpha\n", None),
            document("beta", "Beta", "beta\n", None),
        ]);
        let cancellation_checks = Cell::new(0usize);
        let progress = std::cell::RefCell::new(Vec::new());
        let cancelled = pull_outline_with_options_progress_and_write_authorizer(
            &paths,
            &api,
            "wiki",
            "collection",
            "Imported",
            false,
            &OutlinePullConflictPolicy::abort(),
            &OutlinePullOptions::default(),
            &|_| Ok(()),
            &mut |event| progress.borrow_mut().push(event.clone()),
            &|| {
                let check = cancellation_checks.get();
                cancellation_checks.set(check + 1);
                check >= 2
            },
        )
        .expect_err("second document should observe cancellation");
        assert!(cancelled.message().contains("durable journal"));
        let interrupted =
            load_state(&paths, "wiki", "collection", "Imported", None).expect("interrupted state");
        let operation_id = interrupted
            .incomplete_operation
            .as_ref()
            .expect("operation journal")
            .operation_id
            .clone();
        assert_eq!(interrupted.documents.len(), 1);
        assert!(progress
            .borrow()
            .iter()
            .any(|event| event.phase == OutlinePullPhase::Applying));

        let resumed = pull_outline_with_options_and_write_authorizer(
            &paths,
            &api,
            "wiki",
            "collection",
            "Imported",
            false,
            &OutlinePullConflictPolicy::abort(),
            &OutlinePullOptions::default(),
            &|_| Ok(()),
        )
        .expect("resume pull");
        assert!(resumed.applied);
        assert!(resumed.resumed_operation);
        assert_eq!(resumed.operation_id.as_deref(), Some(operation_id.as_str()));
        assert!(temp.path().join("Imported/Alpha.md").is_file());
        assert!(temp.path().join("Imported/Beta.md").is_file());
        let completed =
            load_state(&paths, "wiki", "collection", "Imported", None).expect("completed state");
        assert!(completed.incomplete_operation.is_none());
        assert_eq!(
            completed.last_completed_operation_id.as_deref(),
            Some(operation_id.as_str())
        );
    }

    #[test]
    fn scoped_pull_selects_bounded_subtrees_without_treating_other_documents_as_missing() {
        let temp = tempdir().expect("temp dir");
        let paths = VaultPaths::new(temp.path());
        initialize_vulcan_dir(&paths).expect("initialize vault");
        let api = api(vec![
            document("root", "Root", "[Other](/doc/other)\n", None),
            document("child", "Child", "child\n", Some("root")),
            document("grandchild", "Grandchild", "grandchild\n", Some("child")),
            document("other", "Other", "other\n", None),
        ]);
        pull_outline(
            &paths,
            &api,
            "wiki",
            "collection",
            "Imported",
            false,
            &OutlinePullConflictPolicy::abort(),
        )
        .expect("initial complete pull");

        let bounded = OutlinePullOptions {
            missing_policy: OutlinePullMissingPolicy::delete_all(),
            confirmed_delete_count: Some(0),
            scope: OutlinePullScope {
                root_document_ids: BTreeSet::from(["root".to_string()]),
                excluded_document_ids: BTreeSet::new(),
                max_depth: Some(1),
            },
            ..OutlinePullOptions::default()
        };
        let report = pull_outline_with_options_and_write_authorizer(
            &paths,
            &api,
            "wiki",
            "collection",
            "Imported",
            true,
            &OutlinePullConflictPolicy::abort(),
            &bounded,
            &|_| Ok(()),
        )
        .expect("bounded pull");
        assert_eq!(report.unchanged, 2);
        assert_eq!(report.out_of_scope, 2);
        assert_eq!(report.deleted_missing, 0);
        assert!(report.actions.iter().any(|action| {
            action.remote_document_id == "root"
                && action
                    .desired_content
                    .as_deref()
                    .is_some_and(|content| content.contains("[[Imported/Other]]"))
        }));

        let excluded = OutlinePullOptions {
            scope: OutlinePullScope {
                root_document_ids: BTreeSet::from(["root".to_string()]),
                excluded_document_ids: BTreeSet::from(["child".to_string()]),
                max_depth: None,
            },
            ..OutlinePullOptions::default()
        };
        let report = pull_outline_with_options_and_write_authorizer(
            &paths,
            &api,
            "wiki",
            "collection",
            "Imported",
            true,
            &OutlinePullConflictPolicy::abort(),
            &excluded,
            &|_| Ok(()),
        )
        .expect("excluded subtree pull");
        assert_eq!(report.unchanged, 1);
        assert_eq!(report.out_of_scope, 3);

        let invalid = OutlinePullOptions {
            scope: OutlinePullScope {
                root_document_ids: BTreeSet::from(["missing".to_string()]),
                ..OutlinePullScope::default()
            },
            ..OutlinePullOptions::default()
        };
        assert!(pull_outline_with_options_and_write_authorizer(
            &paths,
            &api,
            "wiki",
            "collection",
            "Imported",
            true,
            &OutlinePullConflictPolicy::abort(),
            &invalid,
            &|_| Ok(()),
        )
        .is_err());
    }

    #[test]
    fn pull_state_rejects_unsafe_or_duplicate_managed_paths() {
        let mapping = |path: &str| OutlinePullMapping {
            local_path: path.to_string(),
            last_remote_content_hash: "remote".to_string(),
            last_remote_source_hash: Some("source".to_string()),
            last_remote_source: Some("source".to_string()),
            last_remote_revision: None,
            last_remote_updated_at: None,
            last_remote_title: "Home".to_string(),
            last_remote_parent_id: None,
            last_materialized_local_hash: "local".to_string(),
            base_content: "base".to_string(),
            attachments: BTreeMap::new(),
        };
        let mut unsafe_state = OutlinePullState::empty("wiki", "collection", "Imported", None);
        unsafe_state
            .documents
            .insert("one".to_string(), mapping(".vulcan/config.md"));
        assert!(unsafe_state
            .validate("wiki", "collection", "Imported", None)
            .is_err());

        let mut duplicate_state = OutlinePullState::empty("wiki", "collection", "Imported", None);
        duplicate_state
            .documents
            .insert("one".to_string(), mapping("Imported/Home.md"));
        duplicate_state
            .documents
            .insert("two".to_string(), mapping("Imported/home.md"));
        assert!(duplicate_state
            .validate("wiki", "collection", "Imported", None)
            .is_err());
    }

    #[test]
    fn pull_bounds_remote_work_and_persists_connector_revision_provenance() {
        let temp = tempdir().expect("temp dir");
        let paths = VaultPaths::new(temp.path());
        initialize_vulcan_dir(&paths).expect("initialize vault");
        let mut remote = document("home", "Home", "remote source\n", None);
        remote.revision = Some(7);
        remote.updated_at = Some("2026-08-24T12:00:00Z".to_string());
        let api = api(vec![remote]);
        let bounded = OutlinePullOptions {
            connector_identity: Some("https://outline.example/".to_string()),
            max_remote_documents: 1,
            ..OutlinePullOptions::default()
        };
        pull_outline_with_options_and_write_authorizer(
            &paths,
            &api,
            "wiki",
            "collection",
            "Imported",
            false,
            &OutlinePullConflictPolicy::abort(),
            &bounded,
            &|_| Ok(()),
        )
        .expect("bounded pull");

        let state = load_state(
            &paths,
            "wiki",
            "collection",
            "Imported",
            Some("https://outline.example/"),
        )
        .expect("state");
        assert_eq!(
            state.connector_identity.as_deref(),
            Some("https://outline.example/")
        );
        let mapping = &state.documents["home"];
        assert_eq!(
            mapping.last_remote_source.as_deref(),
            Some("remote source\n")
        );
        assert_eq!(mapping.last_remote_revision, Some(7));
        assert_eq!(
            mapping.last_remote_updated_at.as_deref(),
            Some("2026-08-24T12:00:00Z")
        );
        assert!(load_state(
            &paths,
            "wiki",
            "collection",
            "Imported",
            Some("https://other.example/"),
        )
        .is_err());

        let too_small = OutlinePullOptions {
            max_remote_documents: 0,
            ..bounded
        };
        assert!(pull_outline_with_options_and_write_authorizer(
            &paths,
            &api,
            "wiki",
            "collection",
            "Imported",
            true,
            &OutlinePullConflictPolicy::abort(),
            &too_small,
            &|_| Ok(()),
        )
        .is_err());
    }

    #[test]
    fn pull_enforces_content_attachment_count_and_total_byte_budgets() {
        let temp = tempdir().expect("temp dir");
        let paths = VaultPaths::new(temp.path());
        initialize_vulcan_dir(&paths).expect("initialize vault");
        let api = api(vec![document(
            "home",
            "Home",
            "![asset](/api/attachments.redirect?id=asset)",
            None,
        )]);

        let content_limited = OutlinePullOptions {
            max_remote_content_bytes: 8,
            ..OutlinePullOptions::default()
        };
        let error = pull_outline_with_options_and_write_authorizer(
            &paths,
            &api,
            "wiki",
            "collection",
            "Imported",
            true,
            &OutlinePullConflictPolicy::abort(),
            &content_limited,
            &|_| Ok(()),
        )
        .expect_err("remote content budget");
        assert!(error.to_string().contains("content exceeds"));

        let count_limited = OutlinePullOptions {
            max_attachments: 0,
            ..OutlinePullOptions::default()
        };
        let error = pull_outline_with_options_and_write_authorizer(
            &paths,
            &api,
            "wiki",
            "collection",
            "Imported",
            true,
            &OutlinePullConflictPolicy::abort(),
            &count_limited,
            &|_| Ok(()),
        )
        .expect_err("attachment count budget");
        assert!(error.to_string().contains("attachment limits"));

        let total_limited = OutlinePullOptions {
            max_total_attachment_bytes: 1,
            ..OutlinePullOptions::default()
        };
        let error = pull_outline_with_options_and_write_authorizer(
            &paths,
            &api,
            "wiki",
            "collection",
            "Imported",
            false,
            &OutlinePullConflictPolicy::abort(),
            &total_limited,
            &|_| Ok(()),
        )
        .expect_err("total attachment byte budget");
        assert!(error.to_string().contains("total byte limit"));
        assert!(!temp.path().join("Imported/Home.md").exists());
    }
}
