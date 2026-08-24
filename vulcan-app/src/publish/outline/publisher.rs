use super::{
    deterministic_remote_uuid, load_outline_state, lock_outline_state,
    plan_outline_reconciliation_with_policy, OutlineApi, OutlineAttachmentMapping,
    OutlineConflictPolicy, OutlineDocumentMapping, OutlinePublishActionKind, OutlinePublishPlan,
    OutlineRemoteDocument, OutlineRemoteSnapshot,
};
use crate::export::outline::{
    planned_document_references_attachment, OutlineDiagnostic, OutlineLinkTransform,
    OutlinePublicationPlan, OutlineRemoteRenderIndex,
};
use crate::pull::outline::OutlinePulledBinding;
use crate::AppError;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use ulid::Ulid;
use vulcan_core::VaultPaths;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OutlinePublishReport {
    pub dry_run: bool,
    pub applied: bool,
    pub conflicts: usize,
    pub adopted_pull_bindings: usize,
    pub diagnostics: Vec<OutlineDiagnostic>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub link_transform: Option<OutlineLinkTransform>,
    #[serde(flatten)]
    pub plan: OutlinePublishPlan,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OutlinePublishOptions {
    pub adopt_pull_bindings: Vec<OutlinePulledBinding>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OutlinePublishPhase {
    Planning,
    ReconcilingDocuments,
    UploadingAttachments,
    UpdatingDocuments,
    ArchivingDocuments,
    Completed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OutlinePublishProgress {
    pub phase: OutlinePublishPhase,
    pub processed: usize,
    pub total: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_path: Option<String>,
}

#[allow(clippy::too_many_lines)]
pub fn publish_outline(
    paths: &VaultPaths,
    api: &dyn OutlineApi,
    profile: &str,
    collection_id: &str,
    publication: &OutlinePublicationPlan,
    dry_run: bool,
    overwrite_conflicts: bool,
) -> Result<OutlinePublishReport, AppError> {
    let conflict_policy = if overwrite_conflicts {
        OutlineConflictPolicy::overwrite_all()
    } else {
        OutlineConflictPolicy::abort()
    };
    publish_outline_with_progress(
        paths,
        api,
        profile,
        collection_id,
        publication,
        dry_run,
        &conflict_policy,
        |_| {},
    )
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
pub fn publish_outline_with_progress<F>(
    paths: &VaultPaths,
    api: &dyn OutlineApi,
    profile: &str,
    collection_id: &str,
    publication: &OutlinePublicationPlan,
    dry_run: bool,
    conflict_policy: &OutlineConflictPolicy,
    on_progress: F,
) -> Result<OutlinePublishReport, AppError>
where
    F: FnMut(&OutlinePublishProgress),
{
    publish_outline_with_options_and_progress(
        paths,
        api,
        profile,
        collection_id,
        publication,
        dry_run,
        conflict_policy,
        &OutlinePublishOptions::default(),
        on_progress,
    )
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
pub fn publish_outline_with_options_and_progress<F>(
    paths: &VaultPaths,
    api: &dyn OutlineApi,
    profile: &str,
    collection_id: &str,
    publication: &OutlinePublicationPlan,
    dry_run: bool,
    conflict_policy: &OutlineConflictPolicy,
    options: &OutlinePublishOptions,
    mut on_progress: F,
) -> Result<OutlinePublishReport, AppError>
where
    F: FnMut(&OutlinePublishProgress),
{
    emit_progress(
        &mut on_progress,
        OutlinePublishPhase::Planning,
        0,
        publication.documents.len(),
        None,
    );
    if dry_run {
        let mut state = load_outline_state(paths, profile, collection_id)?;
        let adopted_pull_bindings = apply_pull_adoptions(
            api,
            collection_id,
            publication,
            &mut state,
            &options.adopt_pull_bindings,
        )?;
        let plan = plan_outline_reconciliation_with_policy(
            api,
            profile,
            collection_id,
            publication,
            &state,
            conflict_policy,
        )?;
        emit_progress(
            &mut on_progress,
            OutlinePublishPhase::Completed,
            plan.actions.len(),
            plan.actions.len(),
            None,
        );
        return Ok(report(
            plan,
            publication.diagnostics.clone(),
            publication.link_transform.clone(),
            true,
            false,
            adopted_pull_bindings,
        ));
    }

    let lock = lock_outline_state(paths, profile)?;
    let mut state = load_outline_state(paths, profile, collection_id)?;
    let adopted_pull_bindings = apply_pull_adoptions(
        api,
        collection_id,
        publication,
        &mut state,
        &options.adopt_pull_bindings,
    )?;
    let mut plan = plan_outline_reconciliation_with_policy(
        api,
        profile,
        collection_id,
        publication,
        &state,
        conflict_policy,
    )?;
    plan.dry_run = false;
    if plan.has_conflicts() {
        emit_progress(
            &mut on_progress,
            OutlinePublishPhase::Completed,
            0,
            plan.actions.len(),
            None,
        );
        return Ok(report(
            plan,
            publication.diagnostics.clone(),
            publication.link_transform.clone(),
            false,
            false,
            adopted_pull_bindings,
        ));
    }

    let document_actions = plan
        .actions
        .iter()
        .filter(|action| {
            !matches!(
                action.kind,
                OutlinePublishActionKind::UploadAttachment | OutlinePublishActionKind::Archive
            )
        })
        .filter_map(|action| Some((action.source_path.as_deref()?, action)))
        .collect::<BTreeMap<_, _>>();
    let mut identity_by_path = state
        .documents
        .iter()
        .map(|(identity, mapping)| (mapping.source_path.clone(), identity.clone()))
        .collect::<BTreeMap<_, _>>();

    if !publication.documents.is_empty() {
        emit_progress(
            &mut on_progress,
            OutlinePublishPhase::ReconcilingDocuments,
            0,
            publication.documents.len(),
            None,
        );
    }
    for (index, document) in publication.documents.iter().enumerate() {
        emit_progress(
            &mut on_progress,
            OutlinePublishPhase::ReconcilingDocuments,
            index,
            publication.documents.len(),
            Some(&document.source_path),
        );
        let action = document_actions
            .get(document.source_path.as_str())
            .copied()
            .ok_or_else(|| AppError::operation("Outline plan omitted a local document"))?;
        let desired_parent = document
            .parent_source_path
            .as_deref()
            .and_then(|path| identity_by_path.get(path))
            .and_then(|identity| state.documents.get(identity))
            .map(|mapping| mapping.remote_document_id.clone());
        let source_identity = if action.kind == OutlinePublishActionKind::Create {
            let source_identity = action
                .source_identity
                .clone()
                .unwrap_or_else(|| Ulid::new().to_string());
            let requested_remote_id = action
                .remote_document_id
                .clone()
                .unwrap_or_else(|| deterministic_remote_uuid(&source_identity));
            let mapping = state
                .documents
                .entry(source_identity.clone())
                .or_insert_with(|| OutlineDocumentMapping {
                    source_path: document.source_path.clone(),
                    source_document_id: document.source_document_id.clone(),
                    remote_document_id: requested_remote_id.clone(),
                    last_published_content_hash: content_hash(&document.content),
                    last_published_title: document.title.clone(),
                    remote_parent_id: desired_parent.clone(),
                    last_observed_remote: None,
                    pending_create: true,
                    pending_archive: false,
                    attachments: BTreeMap::new(),
                });
            mapping.source_path.clone_from(&document.source_path);
            mapping
                .source_document_id
                .clone_from(&document.source_document_id);
            mapping.remote_document_id.clone_from(&requested_remote_id);
            mapping.pending_create = true;
            lock.save(&state)?;
            let remote = api.create_document(
                &requested_remote_id,
                collection_id,
                desired_parent.as_deref(),
                &document.title,
                &document.content,
            )?;
            let mapping = state
                .documents
                .get_mut(&source_identity)
                .expect("provisional mapping should exist");
            mapping.remote_document_id.clone_from(&remote.id);
            mapping.last_observed_remote = Some(remote_snapshot(&remote));
            mapping.pending_create = false;
            lock.save(&state)?;
            source_identity
        } else {
            action.source_identity.clone().ok_or_else(|| {
                AppError::operation("managed Outline action has no source identity")
            })?
        };
        let mapping = state
            .documents
            .get_mut(&source_identity)
            .ok_or_else(|| AppError::operation("Outline mapping disappeared during publication"))?;
        let previous_path = mapping.source_path.clone();
        if matches!(
            action.kind,
            OutlinePublishActionKind::Move | OutlinePublishActionKind::UpdateAndMove
        ) {
            let remote = api.move_document(
                &mapping.remote_document_id,
                collection_id,
                desired_parent.as_deref(),
            )?;
            mapping.last_observed_remote = Some(remote_snapshot(&remote));
        }
        mapping.source_path.clone_from(&document.source_path);
        mapping
            .source_document_id
            .clone_from(&document.source_document_id);
        mapping.remote_parent_id = desired_parent;
        if previous_path != document.source_path {
            identity_by_path.remove(&previous_path);
        }
        identity_by_path.insert(document.source_path.clone(), source_identity);
        lock.save(&state)?;
        emit_progress(
            &mut on_progress,
            OutlinePublishPhase::ReconcilingDocuments,
            index + 1,
            publication.documents.len(),
            None,
        );
    }

    if !publication.attachments.is_empty() {
        emit_progress(
            &mut on_progress,
            OutlinePublishPhase::UploadingAttachments,
            0,
            publication.attachments.len(),
            None,
        );
    }
    for (index, attachment) in publication.attachments.iter().enumerate() {
        emit_progress(
            &mut on_progress,
            OutlinePublishPhase::UploadingAttachments,
            index,
            publication.attachments.len(),
            Some(&attachment.source_path),
        );
        let existing = state.documents.values().find_map(|mapping| {
            mapping
                .attachments
                .get(&attachment.source_path)
                .filter(|mapped| mapped.content_hash == attachment.content_hash)
                .cloned()
        });
        if existing.is_some() {
            emit_progress(
                &mut on_progress,
                OutlinePublishPhase::UploadingAttachments,
                index + 1,
                publication.attachments.len(),
                None,
            );
            continue;
        }
        let owner = publication
            .documents
            .iter()
            .find(|document| planned_document_references_attachment(document, attachment))
            .ok_or_else(|| AppError::operation("planned attachment has no owning document"))?;
        let owner_identity = identity_by_path
            .get(&owner.source_path)
            .cloned()
            .ok_or_else(|| AppError::operation("attachment owner has no remote mapping"))?;
        let owner_mapping = state
            .documents
            .get(&owner_identity)
            .ok_or_else(|| AppError::operation("attachment owner mapping disappeared"))?;
        let owner_remote_id = owner_mapping.remote_document_id.clone();
        let bytes = fs::read(paths.vault_root().join(&attachment.source_path))
            .map_err(AppError::operation)?;
        let remote = api.upload_attachment(
            &owner_remote_id,
            attachment
                .source_path
                .rsplit('/')
                .next()
                .unwrap_or(&attachment.source_path),
            attachment_content_type(&attachment.source_path),
            &bytes,
        )?;
        for mapping in state.documents.values_mut() {
            mapping.attachments.remove(&attachment.source_path);
        }
        state
            .documents
            .get_mut(&owner_identity)
            .expect("attachment owner mapping should exist")
            .attachments
            .insert(
                attachment.source_path.clone(),
                OutlineAttachmentMapping {
                    remote_attachment_id: remote.id,
                    remote_url: remote.url,
                    content_hash: attachment.content_hash.clone(),
                    owner_remote_document_id: owner_remote_id,
                },
            );
        lock.save(&state)?;
        emit_progress(
            &mut on_progress,
            OutlinePublishPhase::UploadingAttachments,
            index + 1,
            publication.attachments.len(),
            None,
        );
    }

    let selected_attachments = publication
        .attachments
        .iter()
        .map(|attachment| attachment.source_path.as_str())
        .collect::<BTreeSet<_>>();
    for mapping in state.documents.values_mut() {
        mapping
            .attachments
            .retain(|path, _| selected_attachments.contains(path.as_str()));
    }
    let remote_urls = state
        .documents
        .values()
        .flat_map(|mapping| &mapping.attachments)
        .map(|(path, attachment)| (path.clone(), attachment.remote_url.clone()))
        .collect::<BTreeMap<_, _>>();
    let remote_document_ids = state
        .documents
        .values()
        .map(|mapping| {
            (
                mapping.source_path.clone(),
                mapping.remote_document_id.clone(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let render_index = OutlineRemoteRenderIndex::new(
        &publication.documents,
        &remote_document_ids,
        &publication.attachments,
        &remote_urls,
    );

    if !publication.documents.is_empty() {
        emit_progress(
            &mut on_progress,
            OutlinePublishPhase::UpdatingDocuments,
            0,
            publication.documents.len(),
            None,
        );
    }
    for (index, document) in publication.documents.iter().enumerate() {
        emit_progress(
            &mut on_progress,
            OutlinePublishPhase::UpdatingDocuments,
            index,
            publication.documents.len(),
            Some(&document.source_path),
        );
        let source_identity = identity_by_path
            .get(&document.source_path)
            .cloned()
            .ok_or_else(|| AppError::operation("local document has no remote mapping"))?;
        let mapping = state
            .documents
            .get(&source_identity)
            .ok_or_else(|| AppError::operation("local document mapping disappeared"))?;
        let remote_id = mapping.remote_document_id.clone();
        let desired = render_index.render(document);
        let action_kind = document_actions
            .get(document.source_path.as_str())
            .map(|action| action.kind)
            .ok_or_else(|| AppError::operation("Outline plan omitted a local document"))?;
        let desired_hash = content_hash(&desired);
        let needs_update = matches!(
            action_kind,
            OutlinePublishActionKind::Update | OutlinePublishActionKind::UpdateAndMove
        ) || (action_kind != OutlinePublishActionKind::AdoptRemoteResult
            && (desired_hash != mapping.last_published_content_hash
                || document.title != mapping.last_published_title));
        let updated_remote = needs_update
            .then(|| api.update_document(&remote_id, &document.title, &desired))
            .transpose()?;
        let mapping = state
            .documents
            .get_mut(&source_identity)
            .expect("document mapping should exist");
        mapping.last_published_content_hash = desired_hash;
        mapping.last_published_title.clone_from(&document.title);
        if let Some(remote) = updated_remote {
            mapping.last_observed_remote = Some(remote_snapshot(&remote));
        }
        mapping.pending_create = false;
        mapping.pending_archive = false;
        lock.save(&state)?;
        emit_progress(
            &mut on_progress,
            OutlinePublishPhase::UpdatingDocuments,
            index + 1,
            publication.documents.len(),
            None,
        );
    }

    let mut archives = plan
        .actions
        .iter()
        .filter(|action| action.kind == OutlinePublishActionKind::Archive)
        .filter_map(|action| {
            Some((
                action.source_identity.clone()?,
                action.remote_document_id.clone()?,
                action.source_path.clone()?,
            ))
        })
        .collect::<Vec<_>>();
    archives.sort_by(|left, right| {
        right
            .2
            .matches('/')
            .count()
            .cmp(&left.2.matches('/').count())
            .then(right.2.cmp(&left.2))
    });
    let archive_count = archives.len();
    if archive_count > 0 {
        emit_progress(
            &mut on_progress,
            OutlinePublishPhase::ArchivingDocuments,
            0,
            archive_count,
            None,
        );
    }
    for (index, (source_identity, remote_id, source_path)) in archives.into_iter().enumerate() {
        emit_progress(
            &mut on_progress,
            OutlinePublishPhase::ArchivingDocuments,
            index,
            archive_count,
            Some(&source_path),
        );
        state
            .documents
            .get_mut(&source_identity)
            .expect("archive mapping should exist")
            .pending_archive = true;
        lock.save(&state)?;
        if let Err(archive_error) = api.archive_document(&remote_id) {
            let already_archived = api
                .document_info(&remote_id)
                .is_ok_and(|document| document.archived_at.is_some());
            if !already_archived {
                return Err(archive_error);
            }
        }
        state.documents.remove(&source_identity);
        lock.save(&state)?;
        emit_progress(
            &mut on_progress,
            OutlinePublishPhase::ArchivingDocuments,
            index + 1,
            archive_count,
            None,
        );
    }

    let selected_paths = publication
        .documents
        .iter()
        .map(|document| document.source_path.as_str())
        .collect::<BTreeSet<_>>();
    let adopted_removals = plan
        .actions
        .iter()
        .filter(|action| action.kind == OutlinePublishActionKind::AdoptRemoteResult)
        .filter(|action| {
            action
                .source_path
                .as_deref()
                .is_some_and(|path| !selected_paths.contains(path))
        })
        .filter_map(|action| action.source_identity.as_deref())
        .collect::<Vec<_>>();
    for source_identity in adopted_removals {
        state.documents.remove(source_identity);
        lock.save(&state)?;
    }

    emit_progress(
        &mut on_progress,
        OutlinePublishPhase::Completed,
        plan.actions.len(),
        plan.actions.len(),
        None,
    );
    Ok(report(
        plan,
        publication.diagnostics.clone(),
        publication.link_transform.clone(),
        false,
        true,
        adopted_pull_bindings,
    ))
}

fn apply_pull_adoptions(
    api: &dyn OutlineApi,
    collection_id: &str,
    publication: &OutlinePublicationPlan,
    state: &mut super::OutlinePublishState,
    candidates: &[OutlinePulledBinding],
) -> Result<usize, AppError> {
    if candidates.is_empty() {
        return Ok(0);
    }
    let selected = publication
        .documents
        .iter()
        .map(|document| (document.source_path.as_str(), document))
        .collect::<BTreeMap<_, _>>();
    let remote = api
        .list_collection_documents(collection_id)?
        .into_iter()
        .map(|document| (document.id.clone(), document))
        .collect::<BTreeMap<_, _>>();
    let mut adopted = 0;
    for candidate in candidates {
        let Some(document) = selected.get(candidate.local_path.as_str()) else {
            continue;
        };
        if let Some((identity, mapping)) = state
            .documents
            .iter()
            .find(|(_, mapping)| mapping.source_path == candidate.local_path)
        {
            if mapping.remote_document_id != candidate.remote_document_id {
                return Err(AppError::operation(format!(
                    "cannot adopt `{}`: existing publication mapping `{identity}` owns a different remote document",
                    candidate.local_path
                )));
            }
            continue;
        }
        if let Some((identity, mapping)) = state
            .documents
            .iter()
            .find(|(_, mapping)| mapping.remote_document_id == candidate.remote_document_id)
        {
            return Err(AppError::operation(format!(
                "cannot adopt `{}`: remote document `{}` is already owned by publication mapping `{identity}` at `{}`",
                candidate.local_path, candidate.remote_document_id, mapping.source_path
            )));
        }
        let current = remote.get(&candidate.remote_document_id).ok_or_else(|| {
            AppError::operation(format!(
                "cannot adopt `{}`: pulled remote document `{}` is missing",
                candidate.local_path, candidate.remote_document_id
            ))
        })?;
        if current.archived_at.is_some()
            || current.collection_id != collection_id
            || content_hash(&current.text) != candidate.last_remote_source_hash
            || current.title != candidate.last_remote_title
            || current.parent_document_id != candidate.last_remote_parent_id
        {
            return Err(AppError::operation(format!(
                "cannot adopt `{}`: remote content, title, parent, or collection changed since the last successful pull",
                candidate.local_path
            )));
        }
        let source_identity = format!("outline-pull:{}", candidate.remote_document_id);
        let attachments = candidate
            .attachments
            .iter()
            .map(|attachment| {
                (
                    attachment.local_path.clone(),
                    OutlineAttachmentMapping {
                        remote_attachment_id: attachment.remote_url.clone(),
                        remote_url: attachment.remote_url.clone(),
                        content_hash: attachment.content_hash.clone(),
                        owner_remote_document_id: candidate.remote_document_id.clone(),
                    },
                )
            })
            .collect();
        state.documents.insert(
            source_identity,
            OutlineDocumentMapping {
                source_path: candidate.local_path.clone(),
                source_document_id: document.source_document_id.clone(),
                remote_document_id: candidate.remote_document_id.clone(),
                last_published_content_hash: content_hash(&current.text),
                last_published_title: current.title.clone(),
                remote_parent_id: current.parent_document_id.clone(),
                last_observed_remote: Some(remote_snapshot(current)),
                pending_create: false,
                pending_archive: false,
                attachments,
            },
        );
        adopted += 1;
    }
    Ok(adopted)
}

fn emit_progress(
    on_progress: &mut impl FnMut(&OutlinePublishProgress),
    phase: OutlinePublishPhase,
    processed: usize,
    total: usize,
    current_path: Option<&str>,
) {
    on_progress(&OutlinePublishProgress {
        phase,
        processed,
        total,
        current_path: current_path.map(str::to_string),
    });
}

fn report(
    plan: OutlinePublishPlan,
    diagnostics: Vec<OutlineDiagnostic>,
    link_transform: Option<OutlineLinkTransform>,
    dry_run: bool,
    applied: bool,
    adopted_pull_bindings: usize,
) -> OutlinePublishReport {
    let conflicts = plan
        .actions
        .iter()
        .filter(|action| action.kind == OutlinePublishActionKind::Conflict)
        .count();
    OutlinePublishReport {
        dry_run,
        applied,
        conflicts,
        adopted_pull_bindings,
        diagnostics,
        link_transform,
        plan,
    }
}

fn content_hash(content: &str) -> String {
    blake3::hash(content.as_bytes()).to_hex().to_string()
}

fn remote_snapshot(document: &OutlineRemoteDocument) -> OutlineRemoteSnapshot {
    OutlineRemoteSnapshot {
        content_hash: content_hash(&document.text),
        title: document.title.clone(),
        parent_document_id: document.parent_document_id.clone(),
    }
}

fn attachment_content_type(path: &str) -> &'static str {
    match path
        .rsplit('.')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "svg" => "image/svg+xml",
        "webp" => "image/webp",
        "pdf" => "application/pdf",
        "txt" | "md" => "text/plain",
        "json" => "application/json",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::export::outline::{
        OutlineDiagnosticKind, OutlinePlannedAttachment, OutlinePlannedDocument,
        SUPPORTED_OUTLINE_VERSION,
    };
    use crate::publish::outline::{OutlineRemoteAttachment, OutlineRemoteDocument};
    use std::cell::RefCell;
    use tempfile::tempdir;

    #[derive(Default)]
    struct MockApi {
        documents: RefCell<BTreeMap<String, OutlineRemoteDocument>>,
        mutations: RefCell<Vec<String>>,
        info_calls: RefCell<Vec<String>>,
        fail_next_create: RefCell<bool>,
        normalize_markdown: bool,
    }

    impl MockApi {
        fn stored_text(&self, text: &str) -> String {
            if self.normalize_markdown {
                format!("{}\n", text.trim_end())
            } else {
                text.to_string()
            }
        }
    }

    impl OutlineApi for MockApi {
        fn list_collection_documents(
            &self,
            _collection_id: &str,
        ) -> Result<Vec<OutlineRemoteDocument>, AppError> {
            Ok(self.documents.borrow().values().cloned().collect())
        }

        fn document_info(&self, id: &str) -> Result<OutlineRemoteDocument, AppError> {
            self.info_calls.borrow_mut().push(id.to_string());
            self.documents
                .borrow()
                .get(id)
                .cloned()
                .ok_or_else(|| AppError::operation("missing mock document"))
        }

        fn create_document(
            &self,
            id: &str,
            collection_id: &str,
            parent_document_id: Option<&str>,
            title: &str,
            text: &str,
        ) -> Result<OutlineRemoteDocument, AppError> {
            self.mutations.borrow_mut().push(format!("create:{title}"));
            if self.fail_next_create.replace(false) {
                return Err(AppError::operation("interrupted create"));
            }
            let document = OutlineRemoteDocument {
                id: id.to_string(),
                title: title.to_string(),
                text: self.stored_text(text),
                collection_id: collection_id.to_string(),
                parent_document_id: parent_document_id.map(str::to_string),
                archived_at: None,
            };
            self.documents
                .borrow_mut()
                .insert(id.to_string(), document.clone());
            Ok(document)
        }

        fn update_document(
            &self,
            id: &str,
            title: &str,
            text: &str,
        ) -> Result<OutlineRemoteDocument, AppError> {
            self.mutations.borrow_mut().push(format!("update:{title}"));
            let mut documents = self.documents.borrow_mut();
            let document = documents
                .get_mut(id)
                .ok_or_else(|| AppError::operation("missing mock document"))?;
            document.title = title.to_string();
            document.text = self.stored_text(text);
            Ok(document.clone())
        }

        fn move_document(
            &self,
            id: &str,
            _collection_id: &str,
            parent_document_id: Option<&str>,
        ) -> Result<OutlineRemoteDocument, AppError> {
            self.mutations.borrow_mut().push(format!("move:{id}"));
            let mut documents = self.documents.borrow_mut();
            let document = documents
                .get_mut(id)
                .ok_or_else(|| AppError::operation("missing mock document"))?;
            document.parent_document_id = parent_document_id.map(str::to_string);
            Ok(document.clone())
        }

        fn archive_document(&self, id: &str) -> Result<OutlineRemoteDocument, AppError> {
            self.mutations.borrow_mut().push(format!("archive:{id}"));
            let mut documents = self.documents.borrow_mut();
            let document = documents
                .get_mut(id)
                .ok_or_else(|| AppError::operation("missing mock document"))?;
            document.archived_at = Some("now".to_string());
            Ok(document.clone())
        }

        fn upload_attachment(
            &self,
            document_id: &str,
            name: &str,
            _content_type: &str,
            _bytes: &[u8],
        ) -> Result<OutlineRemoteAttachment, AppError> {
            self.mutations.borrow_mut().push(format!("upload:{name}"));
            Ok(OutlineRemoteAttachment {
                id: format!("attachment-{name}"),
                url: format!(
                    "https://outline.test/api/attachments.redirect?id={document_id}-{name}"
                ),
            })
        }
    }

    fn document(
        path: &str,
        title: &str,
        content: &str,
        parent: Option<&str>,
    ) -> OutlinePlannedDocument {
        OutlinePlannedDocument {
            source_path: path.to_string(),
            source_document_id: format!("cache-{path}"),
            title: title.to_string(),
            archive_path: format!("Wiki/{title}.md"),
            parent_source_path: parent.map(str::to_string),
            content_hash: content_hash(content),
            content: content.to_string(),
        }
    }

    fn plan(documents: Vec<OutlinePlannedDocument>) -> OutlinePublicationPlan {
        OutlinePublicationPlan {
            collection_title: "Wiki".to_string(),
            collection_directory: "Wiki".to_string(),
            outline_version: SUPPORTED_OUTLINE_VERSION.to_string(),
            documents,
            attachments: Vec::new(),
            diagnostics: Vec::new(),
            link_transform: None,
        }
    }

    #[test]
    fn publication_is_idempotent_updates_moves_and_archives() {
        let temp = tempdir().expect("temp dir");
        let paths = VaultPaths::new(temp.path());
        let api = MockApi::default();
        let initial = plan(vec![
            document("Projects.md", "Projects", "parent", None),
            document(
                "Projects/Child.md",
                "Projects/Child",
                "child",
                Some("Projects.md"),
            ),
        ]);
        let first = publish_outline(&paths, &api, "wiki", "collection", &initial, false, false)
            .expect("initial publication");
        assert!(first.applied);
        assert_eq!(
            api.mutations.borrow().as_slice(),
            ["create:Projects", "create:Projects/Child"]
        );

        api.mutations.borrow_mut().clear();
        publish_outline(&paths, &api, "wiki", "collection", &initial, false, false)
            .expect("idempotent publication");
        assert!(api.mutations.borrow().is_empty());
        assert!(api.info_calls.borrow().is_empty());

        let mut moved = document("Moved.md", "Moved", "child changed", None);
        moved.source_document_id = "cache-Projects/Child.md".to_string();
        let changed = plan(vec![moved]);
        api.mutations.borrow_mut().clear();
        publish_outline(&paths, &api, "wiki", "collection", &changed, false, false)
            .expect("changed publication");
        let mutations = api.mutations.borrow();
        assert!(mutations.iter().any(|entry| entry.starts_with("move:")));
        assert!(mutations.iter().any(|entry| entry == "update:Moved"));
        assert!(mutations.iter().any(|entry| entry.starts_with("archive:")));
    }

    #[test]
    fn explicit_pull_adoption_updates_the_existing_remote_document_without_duplication() {
        let temp = tempdir().expect("temp dir");
        let paths = VaultPaths::new(temp.path());
        let remote = OutlineRemoteDocument {
            id: "remote-home".to_string(),
            title: "Home".to_string(),
            text: "pulled baseline".to_string(),
            collection_id: "collection".to_string(),
            parent_document_id: None,
            archived_at: None,
        };
        let api = MockApi::default();
        api.documents
            .borrow_mut()
            .insert(remote.id.clone(), remote.clone());
        let publication = plan(vec![document(
            "Imported/Home.md",
            "Home",
            "local edit",
            None,
        )]);
        let options = OutlinePublishOptions {
            adopt_pull_bindings: vec![OutlinePulledBinding {
                local_path: "Imported/Home.md".to_string(),
                remote_document_id: remote.id.clone(),
                last_remote_source_hash: content_hash(&remote.text),
                last_remote_title: remote.title.clone(),
                last_remote_parent_id: None,
                attachments: Vec::new(),
            }],
        };

        let dry_run = publish_outline_with_options_and_progress(
            &paths,
            &api,
            "wiki",
            "collection",
            &publication,
            true,
            &OutlineConflictPolicy::abort(),
            &options,
            |_| {},
        )
        .expect("adoption dry run");
        assert_eq!(dry_run.adopted_pull_bindings, 1);
        assert!(!paths.vulcan_dir().join("publish").exists());

        let applied = publish_outline_with_options_and_progress(
            &paths,
            &api,
            "wiki",
            "collection",
            &publication,
            false,
            &OutlineConflictPolicy::abort(),
            &options,
            |_| {},
        )
        .expect("adopt and publish local edit");
        assert!(applied.applied);
        assert_eq!(applied.adopted_pull_bindings, 1);
        assert_eq!(api.mutations.borrow().as_slice(), ["update:Home"]);
        assert_eq!(api.documents.borrow().len(), 1);
        assert_eq!(api.documents.borrow()["remote-home"].text, "local edit");
        let state = load_outline_state(&paths, "wiki", "collection").expect("publish state");
        assert_eq!(state.documents.len(), 1);
        assert_eq!(
            state.documents.values().next().unwrap().remote_document_id,
            "remote-home"
        );
    }

    #[test]
    fn explicit_pull_adoption_rejects_remote_drift() {
        let temp = tempdir().expect("temp dir");
        let paths = VaultPaths::new(temp.path());
        let api = MockApi::default();
        api.documents.borrow_mut().insert(
            "remote-home".to_string(),
            OutlineRemoteDocument {
                id: "remote-home".to_string(),
                title: "Home".to_string(),
                text: "remote edit".to_string(),
                collection_id: "collection".to_string(),
                parent_document_id: None,
                archived_at: None,
            },
        );
        let options = OutlinePublishOptions {
            adopt_pull_bindings: vec![OutlinePulledBinding {
                local_path: "Imported/Home.md".to_string(),
                remote_document_id: "remote-home".to_string(),
                last_remote_source_hash: content_hash("old baseline"),
                last_remote_title: "Home".to_string(),
                last_remote_parent_id: None,
                attachments: Vec::new(),
            }],
        };
        let error = publish_outline_with_options_and_progress(
            &paths,
            &api,
            "wiki",
            "collection",
            &plan(vec![document("Imported/Home.md", "Home", "local", None)]),
            true,
            &OutlineConflictPolicy::abort(),
            &options,
            |_| {},
        )
        .expect_err("remote drift must prevent adoption");
        assert!(error
            .message()
            .contains("changed since the last successful pull"));
    }

    #[test]
    fn outline_markdown_normalization_does_not_cause_conflicts_or_repeated_updates() {
        let temp = tempdir().expect("temp dir");
        let paths = VaultPaths::new(temp.path());
        let api = MockApi {
            normalize_markdown: true,
            ..MockApi::default()
        };
        let publication = plan(vec![document("Home.md", "Home", "home", None)]);

        publish_outline(
            &paths,
            &api,
            "wiki",
            "collection",
            &publication,
            false,
            false,
        )
        .expect("initial normalized publication");
        let state = load_outline_state(&paths, "wiki", "collection").expect("publish state");
        let mapping = state.documents.values().next().expect("mapping");
        let observed = mapping
            .last_observed_remote
            .as_ref()
            .expect("observed remote snapshot");
        assert_ne!(
            mapping.last_published_content_hash, observed.content_hash,
            "the regression requires Outline to normalize the submitted Markdown"
        );

        api.mutations.borrow_mut().clear();
        let second = publish_outline(
            &paths,
            &api,
            "wiki",
            "collection",
            &publication,
            false,
            false,
        )
        .expect("idempotent normalized publication");

        assert!(second.applied);
        assert_eq!(second.conflicts, 0);
        assert_eq!(
            second.plan.actions[0].kind,
            OutlinePublishActionKind::Unchanged
        );
        assert!(api.mutations.borrow().is_empty());

        let changed_publication = plan(vec![document("Home.md", "Home", "changed", None)]);
        publish_outline(
            &paths,
            &api,
            "wiki",
            "collection",
            &changed_publication,
            false,
            false,
        )
        .expect("normalized update");
        api.mutations.borrow_mut().clear();
        let after_update = publish_outline(
            &paths,
            &api,
            "wiki",
            "collection",
            &changed_publication,
            false,
            false,
        )
        .expect("idempotent publication after normalized update");
        assert_eq!(after_update.conflicts, 0);
        assert_eq!(
            after_update.plan.actions[0].kind,
            OutlinePublishActionKind::Unchanged
        );
        assert!(api.mutations.borrow().is_empty());

        api.documents
            .borrow_mut()
            .values_mut()
            .next()
            .expect("remote document")
            .text = "genuine remote edit\n".to_string();
        let drift = publish_outline(
            &paths,
            &api,
            "wiki",
            "collection",
            &changed_publication,
            false,
            false,
        )
        .expect("remote drift plan");
        assert!(!drift.applied);
        assert_eq!(drift.conflicts, 1);
        assert_eq!(
            drift.plan.actions[0].kind,
            OutlinePublishActionKind::Conflict
        );
    }

    #[test]
    fn publish_report_carries_publication_diagnostics() {
        let temp = tempdir().expect("temp dir");
        let paths = VaultPaths::new(temp.path());
        let api = MockApi::default();
        let mut publication = plan(vec![document(
            "Pantheons/Pantheons.md",
            "Pantheons",
            "# Pantheons\n",
            None,
        )]);
        publication.diagnostics.push(OutlineDiagnostic {
            kind: OutlineDiagnosticKind::MissingFolderNote,
            source_path: Some("Pantheons/Pantheons.md".to_string()),
            target: Some("Pantheons".to_string()),
            line: None,
            column: None,
            byte_offset: None,
            policy: None,
            excluded_target_policy: None,
            action: None,
            message: "generated an export-only placeholder".to_string(),
        });

        let report = publish_outline(
            &paths,
            &api,
            "wiki",
            "collection",
            &publication,
            true,
            false,
        )
        .expect("dry-run publication");

        assert_eq!(report.diagnostics, publication.diagnostics);
        assert!(report.diagnostics[0].is_warning());
    }

    #[test]
    fn direct_publication_rewrites_document_links_to_remote_ids() {
        let temp = tempdir().expect("temp dir");
        let paths = VaultPaths::new(temp.path());
        let api = MockApi::default();
        let publication = plan(vec![
            document("Home.md", "Home", "See [Child](Child.md#details)", None),
            document("Child.md", "Child", "# Details", None),
        ]);

        publish_outline(
            &paths,
            &api,
            "wiki",
            "collection",
            &publication,
            false,
            false,
        )
        .expect("linked publication");

        let documents = api.documents.borrow();
        let home = documents
            .values()
            .find(|document| document.title == "Home")
            .expect("remote home");
        let child = documents
            .values()
            .find(|document| document.title == "Child")
            .expect("remote child");
        assert_eq!(home.text, format!("See [Child](/doc/{}#details)", child.id));
    }

    #[test]
    fn attachments_are_uploaded_rewritten_and_not_reuploaded_unchanged() {
        let temp = tempdir().expect("temp dir");
        let paths = VaultPaths::new(temp.path());
        fs::create_dir_all(temp.path().join("assets")).expect("asset dir");
        fs::write(temp.path().join("assets/logo.png"), b"png").expect("asset");
        let api = MockApi::default();
        let mut publication = plan(vec![document(
            "Home.md",
            "Home",
            "![logo](uploads/hash/logo.png)",
            None,
        )]);
        publication.attachments.push(OutlinePlannedAttachment {
            source_path: "assets/logo.png".to_string(),
            archive_path: "Wiki/uploads/hash/logo.png".to_string(),
            content_hash: blake3::hash(b"png").to_hex().to_string(),
            size: 3,
        });
        publish_outline(
            &paths,
            &api,
            "wiki",
            "collection",
            &publication,
            false,
            false,
        )
        .expect("attachment publication");
        assert!(api
            .mutations
            .borrow()
            .iter()
            .any(|entry| entry == "upload:logo.png"));
        let remote = api
            .documents
            .borrow()
            .values()
            .next()
            .cloned()
            .expect("remote");
        assert!(remote
            .text
            .contains("https://outline.test/api/attachments.redirect"));

        api.mutations.borrow_mut().clear();
        publish_outline(
            &paths,
            &api,
            "wiki",
            "collection",
            &publication,
            false,
            false,
        )
        .expect("idempotent attachment publication");
        assert!(api.mutations.borrow().is_empty());
    }

    #[test]
    fn dry_run_and_conflicts_never_mutate_remote_or_mapping_state() {
        let temp = tempdir().expect("temp dir");
        let paths = VaultPaths::new(temp.path());
        let api = MockApi::default();
        let publication = plan(vec![document("Home.md", "Home", "home", None)]);
        let dry_run = publish_outline(
            &paths,
            &api,
            "wiki",
            "collection",
            &publication,
            true,
            false,
        )
        .expect("dry run");
        assert!(dry_run.dry_run);
        assert!(api.mutations.borrow().is_empty());
        assert!(!paths.vulcan_dir().join("publish").exists());

        publish_outline(
            &paths,
            &api,
            "wiki",
            "collection",
            &publication,
            false,
            false,
        )
        .expect("initial publication");
        let remote_id = api
            .documents
            .borrow()
            .keys()
            .next()
            .cloned()
            .expect("remote id");
        api.documents
            .borrow_mut()
            .get_mut(&remote_id)
            .expect("remote")
            .text = "remote edit".to_string();
        api.mutations.borrow_mut().clear();
        let conflict = publish_outline(
            &paths,
            &api,
            "wiki",
            "collection",
            &publication,
            false,
            false,
        )
        .expect("conflict report");
        assert_eq!(conflict.conflicts, 1);
        assert!(!conflict.applied);
        assert!(api.mutations.borrow().is_empty());
    }

    #[test]
    fn overwrite_conflicts_replaces_remote_drift_and_refreshes_state() {
        let temp = tempdir().expect("temp dir");
        let paths = VaultPaths::new(temp.path());
        let api = MockApi::default();
        let initial = plan(vec![document("Home.md", "Home", "local", None)]);
        publish_outline(&paths, &api, "wiki", "collection", &initial, false, false)
            .expect("initial publication");
        let remote_id = api
            .documents
            .borrow()
            .keys()
            .next()
            .cloned()
            .expect("remote id");
        api.documents
            .borrow_mut()
            .get_mut(&remote_id)
            .expect("remote")
            .text = "remote drift".to_string();
        api.mutations.borrow_mut().clear();

        let report = publish_outline(&paths, &api, "wiki", "collection", &initial, false, true)
            .expect("overwrite publication");

        assert!(report.applied);
        assert_eq!(report.conflicts, 0);
        assert_eq!(report.plan.overwritten_conflicts, 1);
        assert_eq!(api.mutations.borrow().as_slice(), ["update:Home"]);
        assert_eq!(
            api.documents.borrow().get(&remote_id).expect("remote").text,
            "local"
        );
    }

    #[test]
    fn publication_reports_phase_and_item_progress() {
        let temp = tempdir().expect("temp dir");
        let paths = VaultPaths::new(temp.path());
        let api = MockApi::default();
        let publication = plan(vec![document("Home.md", "Home", "home", None)]);
        let mut progress = Vec::new();

        let report = publish_outline_with_progress(
            &paths,
            &api,
            "wiki",
            "collection",
            &publication,
            false,
            &OutlineConflictPolicy::abort(),
            |event| progress.push(event.clone()),
        )
        .expect("publication with progress");

        assert!(report.applied);
        assert_eq!(
            progress.first().expect("first event").phase,
            OutlinePublishPhase::Planning
        );
        assert_eq!(
            progress.last().expect("last event").phase,
            OutlinePublishPhase::Completed
        );
        assert!(progress.iter().any(|event| {
            event.phase == OutlinePublishPhase::ReconcilingDocuments
                && event.processed == 0
                && event.total == 1
                && event.current_path.as_deref() == Some("Home.md")
        }));
        assert!(progress.iter().any(|event| {
            event.phase == OutlinePublishPhase::UpdatingDocuments
                && event.processed == 1
                && event.total == 1
        }));
    }

    #[test]
    fn interrupted_create_reuses_the_provisionally_persisted_remote_id() {
        let temp = tempdir().expect("temp dir");
        let paths = VaultPaths::new(temp.path());
        let api = MockApi::default();
        api.fail_next_create.replace(true);
        let publication = plan(vec![document("Home.md", "Home", "home", None)]);
        assert!(publish_outline(
            &paths,
            &api,
            "wiki",
            "collection",
            &publication,
            false,
            false,
        )
        .is_err());
        let pending = load_outline_state(&paths, "wiki", "collection").expect("pending state");
        let requested_id = pending
            .documents
            .values()
            .next()
            .expect("provisional mapping")
            .remote_document_id
            .clone();
        assert!(pending
            .documents
            .values()
            .all(|mapping| mapping.pending_create));

        publish_outline(
            &paths,
            &api,
            "wiki",
            "collection",
            &publication,
            false,
            false,
        )
        .expect("resumed create");
        assert!(api.documents.borrow().contains_key(&requested_id));
        let completed = load_outline_state(&paths, "wiki", "collection").expect("completed state");
        assert!(completed
            .documents
            .values()
            .all(|mapping| !mapping.pending_create));
    }
}
