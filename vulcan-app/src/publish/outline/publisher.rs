use super::{
    deterministic_remote_uuid, load_outline_state, lock_outline_state, plan_outline_reconciliation,
    OutlineApi, OutlineAttachmentMapping, OutlineDocumentMapping, OutlinePublishActionKind,
    OutlinePublishPlan,
};
use crate::export::outline::{
    planned_document_references_attachment, render_remote_document_content_with_links,
    OutlineDiagnostic, OutlinePublicationPlan,
};
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
    pub diagnostics: Vec<OutlineDiagnostic>,
    #[serde(flatten)]
    pub plan: OutlinePublishPlan,
}

#[allow(clippy::too_many_lines)]
pub fn publish_outline(
    paths: &VaultPaths,
    api: &dyn OutlineApi,
    profile: &str,
    collection_id: &str,
    publication: &OutlinePublicationPlan,
    dry_run: bool,
) -> Result<OutlinePublishReport, AppError> {
    if dry_run {
        let state = load_outline_state(paths, profile, collection_id)?;
        let plan = plan_outline_reconciliation(api, profile, collection_id, publication, &state)?;
        return Ok(report(plan, publication.diagnostics.clone(), true, false));
    }

    let lock = lock_outline_state(paths, profile)?;
    let mut state = load_outline_state(paths, profile, collection_id)?;
    let mut plan = plan_outline_reconciliation(api, profile, collection_id, publication, &state)?;
    plan.dry_run = false;
    if plan.has_conflicts() {
        return Ok(report(plan, publication.diagnostics.clone(), false, false));
    }

    for document in &publication.documents {
        let action = plan
            .actions
            .iter()
            .find(|action| {
                action.source_path.as_deref() == Some(document.source_path.as_str())
                    && !matches!(
                        action.kind,
                        OutlinePublishActionKind::UploadAttachment
                            | OutlinePublishActionKind::Archive
                    )
            })
            .ok_or_else(|| AppError::operation("Outline plan omitted a local document"))?;
        let desired_parent = document
            .parent_source_path
            .as_deref()
            .and_then(|path| mapping_by_path(&state.documents, path))
            .map(|(_, mapping)| mapping.remote_document_id.clone());
        let source_identity = if action.kind == OutlinePublishActionKind::Create {
            let source_identity = action
                .source_identity
                .clone()
                .unwrap_or_else(|| Ulid::new().to_string());
            let requested_remote_id = action
                .remote_document_id
                .clone()
                .unwrap_or_else(|| deterministic_remote_uuid(&source_identity));
            state
                .documents
                .entry(source_identity.clone())
                .or_insert_with(|| OutlineDocumentMapping {
                    source_path: document.source_path.clone(),
                    source_document_id: document.source_document_id.clone(),
                    remote_document_id: requested_remote_id.clone(),
                    last_published_content_hash: content_hash(&document.content),
                    last_published_title: document.title.clone(),
                    remote_parent_id: desired_parent.clone(),
                    pending_create: true,
                    pending_archive: false,
                    attachments: BTreeMap::new(),
                });
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
            mapping.remote_document_id = remote.id;
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
        if matches!(
            action.kind,
            OutlinePublishActionKind::Move | OutlinePublishActionKind::UpdateAndMove
        ) {
            api.move_document(
                &mapping.remote_document_id,
                collection_id,
                desired_parent.as_deref(),
            )?;
        }
        mapping.source_path.clone_from(&document.source_path);
        mapping
            .source_document_id
            .clone_from(&document.source_document_id);
        mapping.remote_parent_id = desired_parent;
        lock.save(&state)?;
    }

    for attachment in &publication.attachments {
        let existing = state.documents.values().find_map(|mapping| {
            mapping
                .attachments
                .get(&attachment.source_path)
                .filter(|mapped| mapped.content_hash == attachment.content_hash)
                .cloned()
        });
        if existing.is_some() {
            continue;
        }
        let owner = publication
            .documents
            .iter()
            .find(|document| planned_document_references_attachment(document, attachment))
            .ok_or_else(|| AppError::operation("planned attachment has no owning document"))?;
        let (owner_identity, owner_mapping) = mapping_by_path(&state.documents, &owner.source_path)
            .ok_or_else(|| AppError::operation("attachment owner has no remote mapping"))?;
        let owner_identity = owner_identity.to_string();
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

    for document in &publication.documents {
        let (source_identity, mapping) =
            mapping_by_path(&state.documents, &document.source_path)
                .ok_or_else(|| AppError::operation("local document has no remote mapping"))?;
        let source_identity = source_identity.to_string();
        let remote_id = mapping.remote_document_id.clone();
        let desired = render_remote_document_content_with_links(
            document,
            &publication.documents,
            &remote_document_ids,
            &publication.attachments,
            &remote_urls,
        );
        let remote = api.document_info(&remote_id)?;
        if remote.text != desired || remote.title != document.title {
            api.update_document(&remote_id, &document.title, &desired)?;
        }
        let mapping = state
            .documents
            .get_mut(&source_identity)
            .expect("document mapping should exist");
        mapping.last_published_content_hash = content_hash(&desired);
        mapping.last_published_title.clone_from(&document.title);
        mapping.pending_create = false;
        mapping.pending_archive = false;
        lock.save(&state)?;
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
    for (source_identity, remote_id, _) in archives {
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
    }

    Ok(report(plan, publication.diagnostics.clone(), false, true))
}

fn report(
    plan: OutlinePublishPlan,
    diagnostics: Vec<OutlineDiagnostic>,
    dry_run: bool,
    applied: bool,
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
        diagnostics,
        plan,
    }
}

fn mapping_by_path<'a>(
    mappings: &'a BTreeMap<String, OutlineDocumentMapping>,
    path: &str,
) -> Option<(&'a str, &'a OutlineDocumentMapping)> {
    mappings
        .iter()
        .find(|(_, mapping)| mapping.source_path == path)
        .map(|(identity, mapping)| (identity.as_str(), mapping))
}

fn content_hash(content: &str) -> String {
    blake3::hash(content.as_bytes()).to_hex().to_string()
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
        fail_next_create: RefCell<bool>,
    }

    impl OutlineApi for MockApi {
        fn list_collection_documents(
            &self,
            _collection_id: &str,
        ) -> Result<Vec<OutlineRemoteDocument>, AppError> {
            Ok(self.documents.borrow().values().cloned().collect())
        }

        fn document_info(&self, id: &str) -> Result<OutlineRemoteDocument, AppError> {
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
                text: text.to_string(),
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
            document.text = text.to_string();
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
        let first = publish_outline(&paths, &api, "wiki", "collection", &initial, false)
            .expect("initial publication");
        assert!(first.applied);
        assert_eq!(
            api.mutations.borrow().as_slice(),
            ["create:Projects", "create:Projects/Child"]
        );

        api.mutations.borrow_mut().clear();
        publish_outline(&paths, &api, "wiki", "collection", &initial, false)
            .expect("idempotent publication");
        assert!(api.mutations.borrow().is_empty());

        let mut moved = document("Moved.md", "Moved", "child changed", None);
        moved.source_document_id = "cache-Projects/Child.md".to_string();
        let changed = plan(vec![moved]);
        api.mutations.borrow_mut().clear();
        publish_outline(&paths, &api, "wiki", "collection", &changed, false)
            .expect("changed publication");
        let mutations = api.mutations.borrow();
        assert!(mutations.iter().any(|entry| entry.starts_with("move:")));
        assert!(mutations.iter().any(|entry| entry == "update:Moved"));
        assert!(mutations.iter().any(|entry| entry.starts_with("archive:")));
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
            message: "generated an export-only placeholder".to_string(),
        });

        let report = publish_outline(&paths, &api, "wiki", "collection", &publication, true)
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

        publish_outline(&paths, &api, "wiki", "collection", &publication, false)
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
        publish_outline(&paths, &api, "wiki", "collection", &publication, false)
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
        publish_outline(&paths, &api, "wiki", "collection", &publication, false)
            .expect("idempotent attachment publication");
        assert!(api.mutations.borrow().is_empty());
    }

    #[test]
    fn dry_run_and_conflicts_never_mutate_remote_or_mapping_state() {
        let temp = tempdir().expect("temp dir");
        let paths = VaultPaths::new(temp.path());
        let api = MockApi::default();
        let publication = plan(vec![document("Home.md", "Home", "home", None)]);
        let dry_run = publish_outline(&paths, &api, "wiki", "collection", &publication, true)
            .expect("dry run");
        assert!(dry_run.dry_run);
        assert!(api.mutations.borrow().is_empty());
        assert!(!paths.vulcan_dir().join("publish").exists());

        publish_outline(&paths, &api, "wiki", "collection", &publication, false)
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
        let conflict = publish_outline(&paths, &api, "wiki", "collection", &publication, false)
            .expect("conflict report");
        assert_eq!(conflict.conflicts, 1);
        assert!(!conflict.applied);
        assert!(api.mutations.borrow().is_empty());
    }

    #[test]
    fn interrupted_create_reuses_the_provisionally_persisted_remote_id() {
        let temp = tempdir().expect("temp dir");
        let paths = VaultPaths::new(temp.path());
        let api = MockApi::default();
        api.fail_next_create.replace(true);
        let publication = plan(vec![document("Home.md", "Home", "home", None)]);
        assert!(publish_outline(&paths, &api, "wiki", "collection", &publication, false).is_err());
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

        publish_outline(&paths, &api, "wiki", "collection", &publication, false)
            .expect("resumed create");
        assert!(api.documents.borrow().contains_key(&requested_id));
        let completed = load_outline_state(&paths, "wiki", "collection").expect("completed state");
        assert!(completed
            .documents
            .values()
            .all(|mapping| !mapping.pending_create));
    }
}
