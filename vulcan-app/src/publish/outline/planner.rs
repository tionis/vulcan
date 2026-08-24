use super::{deterministic_remote_uuid, OutlineApi, OutlineDocumentMapping, OutlinePublishState};
use crate::export::outline::{
    planned_document_references_attachment, render_remote_document_content_with_links,
    OutlinePublicationPlan,
};
use crate::AppError;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct OutlineConflictPolicy {
    overwrite_all: bool,
    overwrite_paths: BTreeSet<String>,
}

impl OutlineConflictPolicy {
    #[must_use]
    pub fn abort() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn overwrite_all() -> Self {
        Self {
            overwrite_all: true,
            overwrite_paths: BTreeSet::new(),
        }
    }

    #[must_use]
    pub fn overwrite_paths(paths: impl IntoIterator<Item = String>) -> Self {
        Self {
            overwrite_all: false,
            overwrite_paths: paths.into_iter().collect(),
        }
    }

    fn overwrites(&self, source_path: &str) -> bool {
        self.overwrite_all || self.overwrite_paths.contains(source_path)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OutlineConflictKind {
    MissingRemoteDocument,
    RemoteDocumentDrift,
    RemovedRemoteDocumentMissing,
    RemovedRemoteDocumentDrift,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OutlineConflictDetail {
    pub kind: OutlineConflictKind,
    pub local: OutlineConflictSide,
    pub remote: OutlineConflictSide,
    pub base_content_hash: String,
    pub local_content_hash: Option<String>,
    pub remote_content_hash: Option<String>,
    pub base_title: String,
    pub local_title: Option<String>,
    pub remote_title: Option<String>,
    pub base_parent_remote_id: Option<String>,
    pub local_parent_remote_id: Option<String>,
    pub remote_parent_remote_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OutlineConflictSideState {
    Unchanged,
    Changed,
    Missing,
    Removed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OutlineConflictField {
    Content,
    Title,
    Parent,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OutlineConflictSide {
    pub state: OutlineConflictSideState,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub changed_fields: Vec<OutlineConflictField>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OutlinePublishActionKind {
    Create,
    Update,
    Move,
    UpdateAndMove,
    UploadAttachment,
    Archive,
    AdoptRemoteResult,
    Conflict,
    Unchanged,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OutlinePublishAction {
    pub kind: OutlinePublishActionKind,
    pub source_identity: Option<String>,
    pub source_path: Option<String>,
    pub remote_document_id: Option<String>,
    pub parent_source_path: Option<String>,
    pub desired_parent_remote_id: Option<String>,
    pub reason: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conflict: Option<OutlineConflictDetail>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OutlinePublishPlan {
    pub profile: String,
    pub collection_id: String,
    pub dry_run: bool,
    pub unmanaged_remote_documents: usize,
    pub overwritten_conflicts: usize,
    pub actions: Vec<OutlinePublishAction>,
}

impl OutlinePublishPlan {
    #[must_use]
    pub fn has_conflicts(&self) -> bool {
        self.actions
            .iter()
            .any(|action| action.kind == OutlinePublishActionKind::Conflict)
    }
}

#[allow(clippy::too_many_lines)]
pub fn plan_outline_reconciliation(
    api: &dyn OutlineApi,
    profile: &str,
    collection_id: &str,
    publication: &OutlinePublicationPlan,
    state: &OutlinePublishState,
    overwrite_conflicts: bool,
) -> Result<OutlinePublishPlan, AppError> {
    let conflict_policy = if overwrite_conflicts {
        OutlineConflictPolicy::overwrite_all()
    } else {
        OutlineConflictPolicy::abort()
    };
    plan_outline_reconciliation_with_policy(
        api,
        profile,
        collection_id,
        publication,
        state,
        &conflict_policy,
    )
}

#[allow(clippy::too_many_lines)]
pub fn plan_outline_reconciliation_with_policy(
    api: &dyn OutlineApi,
    profile: &str,
    collection_id: &str,
    publication: &OutlinePublicationPlan,
    state: &OutlinePublishState,
    conflict_policy: &OutlineConflictPolicy,
) -> Result<OutlinePublishPlan, AppError> {
    state.validate(profile, collection_id)?;
    if !publication.is_valid() {
        return Err(AppError::operation(
            "cannot publish an Outline hierarchy with export diagnostics",
        ));
    }
    let listed = api.list_collection_documents(collection_id)?;
    let listed_by_id = listed
        .iter()
        .map(|document| (document.id.as_str(), document))
        .collect::<BTreeMap<_, _>>();
    let managed_ids = state
        .documents
        .values()
        .map(|mapping| mapping.remote_document_id.as_str())
        .collect::<BTreeSet<_>>();
    let unmanaged_remote_documents = listed
        .iter()
        .filter(|document| !managed_ids.contains(document.id.as_str()))
        .count();

    let document_matches = match_local_documents(publication, state);
    let remote_urls = state
        .documents
        .values()
        .flat_map(|mapping| &mapping.attachments)
        .map(|(path, attachment)| (path.clone(), attachment.remote_url.clone()))
        .collect::<BTreeMap<_, _>>();
    let remote_document_ids = publication
        .documents
        .iter()
        .map(|document| {
            let remote_id = document_matches
                .get(&document.source_path)
                .and_then(|identity| state.documents.get(*identity))
                .map_or_else(
                    || deterministic_remote_uuid(&document.source_document_id),
                    |mapping| mapping.remote_document_id.clone(),
                );
            (document.source_path.clone(), remote_id)
        })
        .collect::<BTreeMap<_, _>>();
    let mut matched_identities = BTreeSet::new();
    let mut actions = Vec::new();
    let mut overwritten_conflicts = 0;
    for document in &publication.documents {
        let mapped_identity = document_matches.get(&document.source_path).copied();
        let desired_parent_remote_id = document
            .parent_source_path
            .as_deref()
            .and_then(|parent| remote_document_ids.get(parent))
            .cloned();
        let Some(source_identity) = mapped_identity else {
            actions.push(OutlinePublishAction {
                kind: OutlinePublishActionKind::Create,
                source_identity: None,
                source_path: Some(document.source_path.clone()),
                remote_document_id: remote_document_ids.get(&document.source_path).cloned(),
                parent_source_path: document.parent_source_path.clone(),
                desired_parent_remote_id,
                reason: "local document has no durable Outline mapping".to_string(),
                conflict: None,
            });
            continue;
        };
        matched_identities.insert(source_identity.to_string());
        let mapping = &state.documents[source_identity];
        let desired_content = render_remote_document_content_with_links(
            document,
            &publication.documents,
            &remote_document_ids,
            &publication.attachments,
            &remote_urls,
        );
        let desired_hash = content_hash(&desired_content);
        if !listed_by_id.contains_key(mapping.remote_document_id.as_str()) {
            if mapping.pending_create || conflict_policy.overwrites(&document.source_path) {
                overwritten_conflicts += usize::from(!mapping.pending_create);
                actions.push(OutlinePublishAction {
                    kind: OutlinePublishActionKind::Create,
                    source_identity: Some(source_identity.to_string()),
                    source_path: Some(document.source_path.clone()),
                    remote_document_id: Some(mapping.remote_document_id.clone()),
                    parent_source_path: document.parent_source_path.clone(),
                    desired_parent_remote_id,
                    reason: if mapping.pending_create {
                        "resume a provisionally mapped document create"
                    } else {
                        "recreate a managed remote document missing from the collection"
                    }
                    .to_string(),
                    conflict: None,
                });
            } else {
                actions.push(conflict_action(
                    source_identity,
                    mapping,
                    &document.source_path,
                    document.parent_source_path.clone(),
                    desired_parent_remote_id.clone(),
                    "managed remote document is missing from the collection",
                    OutlineConflictDetail {
                        kind: OutlineConflictKind::MissingRemoteDocument,
                        local: changed_conflict_side([
                            (
                                desired_hash != mapping.last_published_content_hash,
                                OutlineConflictField::Content,
                            ),
                            (
                                document.title != mapping.last_published_title,
                                OutlineConflictField::Title,
                            ),
                            (
                                desired_parent_remote_id != mapping.remote_parent_id,
                                OutlineConflictField::Parent,
                            ),
                        ]),
                        remote: conflict_side(OutlineConflictSideState::Missing),
                        base_content_hash: mapping.last_published_content_hash.clone(),
                        local_content_hash: Some(desired_hash),
                        remote_content_hash: None,
                        base_title: mapping.last_published_title.clone(),
                        local_title: Some(document.title.clone()),
                        remote_title: None,
                        base_parent_remote_id: mapping.remote_parent_id.clone(),
                        local_parent_remote_id: desired_parent_remote_id.clone(),
                        remote_parent_remote_id: None,
                    },
                ));
            }
            continue;
        }
        let remote = listed_by_id[mapping.remote_document_id.as_str()];
        let remote_hash = content_hash(&remote.text);
        let remote_baseline = remote_baseline(mapping);
        let remote_drift = remote_hash != remote_baseline.content_hash
            || remote.title != remote_baseline.title
            || remote.parent_document_id.as_deref() != remote_baseline.parent_document_id;
        let desired_matches_remote = remote_hash == desired_hash
            && remote.title == document.title
            && remote.parent_document_id == desired_parent_remote_id;
        if remote_drift && !desired_matches_remote {
            if conflict_policy.overwrites(&document.source_path) {
                overwritten_conflicts += 1;
                let content_differs = remote_hash != desired_hash || remote.title != document.title;
                let parent_differs = remote.parent_document_id != desired_parent_remote_id;
                let kind = match (content_differs, parent_differs) {
                    (true, true) => OutlinePublishActionKind::UpdateAndMove,
                    (true, false) => OutlinePublishActionKind::Update,
                    (false, true) => OutlinePublishActionKind::Move,
                    (false, false) => OutlinePublishActionKind::AdoptRemoteResult,
                };
                actions.push(OutlinePublishAction {
                    kind,
                    source_identity: Some(source_identity.to_string()),
                    source_path: Some(document.source_path.clone()),
                    remote_document_id: Some(mapping.remote_document_id.clone()),
                    parent_source_path: document.parent_source_path.clone(),
                    desired_parent_remote_id,
                    reason: "overwrite remote drift with the canonical local document".to_string(),
                    conflict: None,
                });
            } else {
                actions.push(conflict_action(
                    source_identity,
                    mapping,
                    &document.source_path,
                    document.parent_source_path.clone(),
                    desired_parent_remote_id.clone(),
                    "remote content, title, or parent changed since the last successful publication",
                    OutlineConflictDetail {
                        kind: OutlineConflictKind::RemoteDocumentDrift,
                        local: changed_conflict_side([
                            (
                                desired_hash != mapping.last_published_content_hash,
                                OutlineConflictField::Content,
                            ),
                            (
                                document.title != mapping.last_published_title,
                                OutlineConflictField::Title,
                            ),
                            (
                                desired_parent_remote_id != mapping.remote_parent_id,
                                OutlineConflictField::Parent,
                            ),
                        ]),
                        remote: changed_conflict_side([
                            (
                                remote_hash != remote_baseline.content_hash,
                                OutlineConflictField::Content,
                            ),
                            (
                                remote.title != remote_baseline.title,
                                OutlineConflictField::Title,
                            ),
                            (
                                remote.parent_document_id.as_deref()
                                    != remote_baseline.parent_document_id,
                                OutlineConflictField::Parent,
                            ),
                        ]),
                        base_content_hash: mapping.last_published_content_hash.clone(),
                        local_content_hash: Some(desired_hash.clone()),
                        remote_content_hash: Some(remote_hash.clone()),
                        base_title: mapping.last_published_title.clone(),
                        local_title: Some(document.title.clone()),
                        remote_title: Some(remote.title.clone()),
                        base_parent_remote_id: mapping.remote_parent_id.clone(),
                        local_parent_remote_id: desired_parent_remote_id.clone(),
                        remote_parent_remote_id: remote.parent_document_id.clone(),
                    },
                ));
            }
            continue;
        }
        let local_content_changed = desired_hash != mapping.last_published_content_hash
            || document.title != mapping.last_published_title;
        let local_parent_changed = desired_parent_remote_id != mapping.remote_parent_id;
        let (kind, reason) = if remote_drift && desired_matches_remote {
            (
                OutlinePublishActionKind::AdoptRemoteResult,
                "remote already contains the desired result from an interrupted publication",
            )
        } else if local_content_changed && local_parent_changed {
            (
                OutlinePublishActionKind::UpdateAndMove,
                "local title or content and parent changed",
            )
        } else if local_content_changed {
            (
                OutlinePublishActionKind::Update,
                "local title or content changed",
            )
        } else if local_parent_changed {
            (OutlinePublishActionKind::Move, "local parent changed")
        } else {
            (
                OutlinePublishActionKind::Unchanged,
                "remote document matches the last publication",
            )
        };
        actions.push(OutlinePublishAction {
            kind,
            source_identity: Some(source_identity.to_string()),
            source_path: Some(document.source_path.clone()),
            remote_document_id: Some(mapping.remote_document_id.clone()),
            parent_source_path: document.parent_source_path.clone(),
            desired_parent_remote_id,
            reason: reason.to_string(),
            conflict: None,
        });
    }

    let mapped_attachments = state
        .documents
        .values()
        .flat_map(|mapping| &mapping.attachments)
        .collect::<BTreeMap<_, _>>();
    for attachment in &publication.attachments {
        let unchanged = mapped_attachments
            .get(&attachment.source_path)
            .is_some_and(|mapping| mapping.content_hash == attachment.content_hash);
        if unchanged {
            continue;
        }
        let owner = publication
            .documents
            .iter()
            .find(|document| planned_document_references_attachment(document, attachment));
        actions.push(OutlinePublishAction {
            kind: OutlinePublishActionKind::UploadAttachment,
            source_identity: None,
            source_path: Some(attachment.source_path.clone()),
            remote_document_id: owner
                .and_then(|document| document_matches.get(&document.source_path))
                .and_then(|identity| state.documents.get(*identity))
                .map(|mapping| mapping.remote_document_id.clone()),
            parent_source_path: owner.map(|document| document.source_path.clone()),
            desired_parent_remote_id: None,
            reason: if mapped_attachments.contains_key(&attachment.source_path) {
                "local attachment content changed"
            } else {
                "referenced attachment has not been uploaded"
            }
            .to_string(),
            conflict: None,
        });
    }

    for (source_identity, mapping) in &state.documents {
        if matched_identities.contains(source_identity) {
            continue;
        }
        if mapping.pending_archive {
            actions.push(OutlinePublishAction {
                kind: OutlinePublishActionKind::Archive,
                source_identity: Some(source_identity.clone()),
                source_path: Some(mapping.source_path.clone()),
                remote_document_id: Some(mapping.remote_document_id.clone()),
                parent_source_path: None,
                desired_parent_remote_id: None,
                reason: "resume an interrupted managed-document archive".to_string(),
                conflict: None,
            });
            continue;
        }
        if !listed_by_id.contains_key(mapping.remote_document_id.as_str()) {
            if conflict_policy.overwrites(&mapping.source_path) {
                overwritten_conflicts += 1;
                actions.push(OutlinePublishAction {
                    kind: OutlinePublishActionKind::AdoptRemoteResult,
                    source_identity: Some(source_identity.clone()),
                    source_path: Some(mapping.source_path.clone()),
                    remote_document_id: Some(mapping.remote_document_id.clone()),
                    parent_source_path: None,
                    desired_parent_remote_id: None,
                    reason:
                        "adopt the disappearance of a remotely missing, locally removed document"
                            .to_string(),
                    conflict: None,
                });
            } else {
                actions.push(conflict_action(
                    source_identity,
                    mapping,
                    &mapping.source_path,
                    None,
                    None,
                    "locally removed mapping points to a missing remote document",
                    OutlineConflictDetail {
                        kind: OutlineConflictKind::RemovedRemoteDocumentMissing,
                        local: conflict_side(OutlineConflictSideState::Removed),
                        remote: conflict_side(OutlineConflictSideState::Missing),
                        base_content_hash: mapping.last_published_content_hash.clone(),
                        local_content_hash: None,
                        remote_content_hash: None,
                        base_title: mapping.last_published_title.clone(),
                        local_title: None,
                        remote_title: None,
                        base_parent_remote_id: mapping.remote_parent_id.clone(),
                        local_parent_remote_id: None,
                        remote_parent_remote_id: None,
                    },
                ));
            }
            continue;
        }
        let remote = listed_by_id[mapping.remote_document_id.as_str()];
        let remote_baseline = remote_baseline(mapping);
        if remote.archived_at.is_some() {
            actions.push(OutlinePublishAction {
                kind: OutlinePublishActionKind::AdoptRemoteResult,
                source_identity: Some(source_identity.clone()),
                source_path: Some(mapping.source_path.clone()),
                remote_document_id: Some(mapping.remote_document_id.clone()),
                parent_source_path: None,
                desired_parent_remote_id: None,
                reason: "remote document was already archived".to_string(),
                conflict: None,
            });
        } else if content_hash(&remote.text) != remote_baseline.content_hash
            || remote.title != remote_baseline.title
            || remote.parent_document_id.as_deref() != remote_baseline.parent_document_id
        {
            if conflict_policy.overwrites(&mapping.source_path) {
                overwritten_conflicts += 1;
                actions.push(OutlinePublishAction {
                    kind: OutlinePublishActionKind::Archive,
                    source_identity: Some(source_identity.clone()),
                    source_path: Some(mapping.source_path.clone()),
                    remote_document_id: Some(mapping.remote_document_id.clone()),
                    parent_source_path: None,
                    desired_parent_remote_id: None,
                    reason:
                        "archive the remotely changed document removed from the local selection"
                            .to_string(),
                    conflict: None,
                });
            } else {
                let remote_hash = content_hash(&remote.text);
                actions.push(conflict_action(
                    source_identity,
                    mapping,
                    &mapping.source_path,
                    None,
                    None,
                    "remote document changed after its local source was removed",
                    OutlineConflictDetail {
                        kind: OutlineConflictKind::RemovedRemoteDocumentDrift,
                        local: conflict_side(OutlineConflictSideState::Removed),
                        remote: changed_conflict_side([
                            (
                                remote_hash != remote_baseline.content_hash,
                                OutlineConflictField::Content,
                            ),
                            (
                                remote.title != remote_baseline.title,
                                OutlineConflictField::Title,
                            ),
                            (
                                remote.parent_document_id.as_deref()
                                    != remote_baseline.parent_document_id,
                                OutlineConflictField::Parent,
                            ),
                        ]),
                        base_content_hash: mapping.last_published_content_hash.clone(),
                        local_content_hash: None,
                        remote_content_hash: Some(remote_hash),
                        base_title: mapping.last_published_title.clone(),
                        local_title: None,
                        remote_title: Some(remote.title.clone()),
                        base_parent_remote_id: mapping.remote_parent_id.clone(),
                        local_parent_remote_id: None,
                        remote_parent_remote_id: remote.parent_document_id.clone(),
                    },
                ));
            }
        } else {
            actions.push(OutlinePublishAction {
                kind: OutlinePublishActionKind::Archive,
                source_identity: Some(source_identity.clone()),
                source_path: Some(mapping.source_path.clone()),
                remote_document_id: Some(mapping.remote_document_id.clone()),
                parent_source_path: None,
                desired_parent_remote_id: None,
                reason: "previously managed local source is no longer selected".to_string(),
                conflict: None,
            });
        }
    }

    Ok(OutlinePublishPlan {
        profile: profile.to_string(),
        collection_id: collection_id.to_string(),
        dry_run: true,
        unmanaged_remote_documents,
        overwritten_conflicts,
        actions,
    })
}

fn match_local_documents<'a>(
    publication: &OutlinePublicationPlan,
    state: &'a OutlinePublishState,
) -> BTreeMap<String, &'a str> {
    let mut document_matches = BTreeMap::new();
    let mut available = state
        .documents
        .keys()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    for document in &publication.documents {
        let mapped_identity = unique_mapping(&available, state, |mapping| {
            mapping.source_document_id == document.source_document_id
        })
        .or_else(|| {
            unique_mapping(&available, state, |mapping| {
                mapping.source_path == document.source_path
            })
        })
        .or_else(|| {
            unique_mapping(&available, state, |mapping| {
                mapping.last_published_content_hash == document.content_hash
            })
        });
        if let Some(source_identity) = mapped_identity {
            available.remove(source_identity);
            document_matches.insert(document.source_path.clone(), source_identity);
        }
    }
    document_matches
}

fn unique_mapping<'a>(
    available: &BTreeSet<&'a str>,
    state: &'a OutlinePublishState,
    predicate: impl Fn(&OutlineDocumentMapping) -> bool,
) -> Option<&'a str> {
    let mut matches = available
        .iter()
        .copied()
        .filter(|identity| predicate(&state.documents[*identity]));
    let first = matches.next()?;
    matches.next().is_none().then_some(first)
}

fn conflict_action(
    source_identity: &str,
    mapping: &OutlineDocumentMapping,
    source_path: &str,
    parent_source_path: Option<String>,
    desired_parent_remote_id: Option<String>,
    reason: &str,
    conflict: OutlineConflictDetail,
) -> OutlinePublishAction {
    OutlinePublishAction {
        kind: OutlinePublishActionKind::Conflict,
        source_identity: Some(source_identity.to_string()),
        source_path: Some(source_path.to_string()),
        remote_document_id: Some(mapping.remote_document_id.clone()),
        parent_source_path,
        desired_parent_remote_id,
        reason: reason.to_string(),
        conflict: Some(conflict),
    }
}

struct RemoteBaseline<'a> {
    content_hash: &'a str,
    title: &'a str,
    parent_document_id: Option<&'a str>,
}

fn remote_baseline(mapping: &OutlineDocumentMapping) -> RemoteBaseline<'_> {
    mapping.last_observed_remote.as_ref().map_or_else(
        || RemoteBaseline {
            content_hash: &mapping.last_published_content_hash,
            title: &mapping.last_published_title,
            parent_document_id: mapping.remote_parent_id.as_deref(),
        },
        |snapshot| RemoteBaseline {
            content_hash: &snapshot.content_hash,
            title: &snapshot.title,
            parent_document_id: snapshot.parent_document_id.as_deref(),
        },
    )
}

fn conflict_side(state: OutlineConflictSideState) -> OutlineConflictSide {
    OutlineConflictSide {
        state,
        changed_fields: Vec::new(),
    }
}

fn changed_conflict_side(fields: [(bool, OutlineConflictField); 3]) -> OutlineConflictSide {
    let changed_fields = fields
        .into_iter()
        .filter_map(|(changed, field)| changed.then_some(field))
        .collect::<Vec<_>>();
    OutlineConflictSide {
        state: if changed_fields.is_empty() {
            OutlineConflictSideState::Unchanged
        } else {
            OutlineConflictSideState::Changed
        },
        changed_fields,
    }
}

fn content_hash(content: &str) -> String {
    blake3::hash(content.as_bytes()).to_hex().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::export::outline::{OutlinePlannedDocument, SUPPORTED_OUTLINE_VERSION};
    use crate::publish::outline::OutlineRemoteDocument;
    use std::cell::RefCell;

    #[derive(Default)]
    struct MockApi {
        documents: BTreeMap<String, OutlineRemoteDocument>,
        info_calls: RefCell<Vec<String>>,
    }

    impl OutlineApi for MockApi {
        fn list_collection_documents(
            &self,
            _collection_id: &str,
        ) -> Result<Vec<OutlineRemoteDocument>, AppError> {
            Ok(self.documents.values().cloned().collect())
        }

        fn document_info(&self, id: &str) -> Result<OutlineRemoteDocument, AppError> {
            self.info_calls.borrow_mut().push(id.to_string());
            self.documents
                .get(id)
                .cloned()
                .ok_or_else(|| AppError::operation("missing mock document"))
        }

        fn create_document(
            &self,
            _id: &str,
            _collection_id: &str,
            _parent_document_id: Option<&str>,
            _title: &str,
            _text: &str,
        ) -> Result<OutlineRemoteDocument, AppError> {
            unreachable!("dry-run planner must not mutate")
        }

        fn update_document(
            &self,
            _id: &str,
            _title: &str,
            _text: &str,
        ) -> Result<OutlineRemoteDocument, AppError> {
            unreachable!("dry-run planner must not mutate")
        }

        fn move_document(
            &self,
            _id: &str,
            _collection_id: &str,
            _parent_document_id: Option<&str>,
        ) -> Result<OutlineRemoteDocument, AppError> {
            unreachable!("dry-run planner must not mutate")
        }

        fn archive_document(&self, _id: &str) -> Result<OutlineRemoteDocument, AppError> {
            unreachable!("dry-run planner must not mutate")
        }

        fn upload_attachment(
            &self,
            _document_id: &str,
            _name: &str,
            _content_type: &str,
            _bytes: &[u8],
        ) -> Result<super::super::OutlineRemoteAttachment, AppError> {
            unreachable!("dry-run planner must not mutate")
        }
    }

    fn publication(documents: &[(&str, &str, Option<&str>)]) -> OutlinePublicationPlan {
        OutlinePublicationPlan {
            collection_title: "Wiki".to_string(),
            collection_directory: "Wiki".to_string(),
            outline_version: SUPPORTED_OUTLINE_VERSION.to_string(),
            documents: documents
                .iter()
                .map(|(path, content, parent)| OutlinePlannedDocument {
                    source_path: (*path).to_string(),
                    source_document_id: format!("cache-{path}"),
                    title: path.trim_end_matches(".md").to_string(),
                    archive_path: format!("Wiki/{path}"),
                    parent_source_path: parent.map(str::to_string),
                    content_hash: content_hash(content),
                    content: (*content).to_string(),
                })
                .collect(),
            attachments: Vec::new(),
            diagnostics: Vec::new(),
            link_transform: None,
        }
    }

    fn remote(id: &str, title: &str, text: &str, parent: Option<&str>) -> OutlineRemoteDocument {
        OutlineRemoteDocument {
            id: id.to_string(),
            title: title.to_string(),
            text: text.to_string(),
            collection_id: "collection".to_string(),
            parent_document_id: parent.map(str::to_string),
            archived_at: None,
        }
    }

    fn state_entry(path: &str, remote: &OutlineRemoteDocument) -> OutlineDocumentMapping {
        OutlineDocumentMapping {
            source_path: path.to_string(),
            source_document_id: format!("cache-{path}"),
            remote_document_id: remote.id.clone(),
            last_published_content_hash: content_hash(&remote.text),
            last_published_title: remote.title.clone(),
            remote_parent_id: remote.parent_document_id.clone(),
            last_observed_remote: Some(super::super::OutlineRemoteSnapshot {
                content_hash: content_hash(&remote.text),
                title: remote.title.clone(),
                parent_document_id: remote.parent_document_id.clone(),
            }),
            pending_create: false,
            pending_archive: false,
            attachments: BTreeMap::new(),
        }
    }

    #[test]
    fn initial_plan_creates_in_hierarchy_order_without_mutations() {
        let api = MockApi::default();
        let state = OutlinePublishState::empty("wiki", "collection");
        let publication = publication(&[
            ("Projects.md", "parent", None),
            ("Projects/Child.md", "child", Some("Projects.md")),
        ]);
        let plan =
            plan_outline_reconciliation(&api, "wiki", "collection", &publication, &state, false)
                .expect("initial plan");
        assert_eq!(
            plan.actions
                .iter()
                .map(|action| action.kind)
                .collect::<Vec<_>>(),
            vec![
                OutlinePublishActionKind::Create,
                OutlinePublishActionKind::Create
            ]
        );
        assert!(api.info_calls.borrow().is_empty());
    }

    #[test]
    fn plans_idempotent_update_move_archive_and_leaves_unmanaged_documents_untouched() {
        let parent = remote("parent", "Projects", "parent", None);
        let child = remote("child", "Projects/Child", "child", Some("parent"));
        let stale_remote = remote("stale", "Old", "old", None);
        let unmanaged = remote("unmanaged", "Personal", "remote", None);
        let api = MockApi {
            documents: [
                parent.clone(),
                child.clone(),
                stale_remote.clone(),
                unmanaged,
            ]
            .into_iter()
            .map(|document| (document.id.clone(), document))
            .collect(),
            info_calls: RefCell::new(Vec::new()),
        };
        let mut state = OutlinePublishState::empty("wiki", "collection");
        state.documents.insert(
            "source-parent".to_string(),
            state_entry("Projects.md", &parent),
        );
        state.documents.insert(
            "source-child".to_string(),
            state_entry("Projects/Child.md", &child),
        );
        state.documents.insert(
            "source-stale".to_string(),
            state_entry("Old.md", &stale_remote),
        );
        let publication = publication(&[
            ("Projects.md", "parent changed", None),
            ("Moved.md", "child", None),
        ]);
        let plan =
            plan_outline_reconciliation(&api, "wiki", "collection", &publication, &state, false)
                .expect("reconciliation plan");
        assert_eq!(plan.unmanaged_remote_documents, 1);
        assert_eq!(plan.actions[0].kind, OutlinePublishActionKind::Update);
        assert_eq!(
            plan.actions[1].kind,
            OutlinePublishActionKind::UpdateAndMove
        );
        assert_eq!(plan.actions[1].source_path.as_deref(), Some("Moved.md"));
        assert_eq!(plan.actions[2].kind, OutlinePublishActionKind::Archive);
        assert!(api.info_calls.borrow().is_empty());
    }

    #[test]
    fn remote_drift_is_a_conflict_but_an_interrupted_desired_result_is_adopted() {
        let original = remote("remote", "Home", "old", None);
        let mut state = OutlinePublishState::empty("wiki", "collection");
        state
            .documents
            .insert("source".to_string(), state_entry("Home.md", &original));

        let drifted = remote("remote", "Home", "someone else's edit", None);
        let api = MockApi {
            documents: BTreeMap::from([("remote".to_string(), drifted)]),
            info_calls: RefCell::new(Vec::new()),
        };
        let plan = plan_outline_reconciliation(
            &api,
            "wiki",
            "collection",
            &publication(&[("Home.md", "local edit", None)]),
            &state,
            false,
        )
        .expect("conflict plan");
        assert!(plan.has_conflicts());
        let conflict = plan.actions[0].conflict.as_ref().expect("conflict detail");
        assert_eq!(conflict.kind, OutlineConflictKind::RemoteDocumentDrift);
        assert_eq!(conflict.local.state, OutlineConflictSideState::Changed);
        assert_eq!(
            conflict.local.changed_fields,
            vec![OutlineConflictField::Content]
        );
        assert_eq!(conflict.remote.state, OutlineConflictSideState::Changed);
        assert_eq!(
            conflict.remote.changed_fields,
            vec![OutlineConflictField::Content]
        );
        assert_eq!(conflict.local_title.as_deref(), Some("Home"));
        assert_eq!(conflict.remote_title.as_deref(), Some("Home"));
        assert_ne!(conflict.local_content_hash, conflict.remote_content_hash);

        let desired = remote("remote", "Home", "local edit", None);
        let api = MockApi {
            documents: BTreeMap::from([("remote".to_string(), desired)]),
            info_calls: RefCell::new(Vec::new()),
        };
        let plan = plan_outline_reconciliation(
            &api,
            "wiki",
            "collection",
            &publication(&[("Home.md", "local edit", None)]),
            &state,
            false,
        )
        .expect("interrupted plan");
        assert_eq!(
            plan.actions[0].kind,
            OutlinePublishActionKind::AdoptRemoteResult
        );
    }

    #[test]
    fn overwrite_conflicts_plans_the_canonical_local_result() {
        let original = remote("remote", "Home", "old", None);
        let mut state = OutlinePublishState::empty("wiki", "collection");
        state
            .documents
            .insert("source".to_string(), state_entry("Home.md", &original));
        let drifted = remote(
            "remote",
            "Remote title",
            "remote edit",
            Some("remote-parent"),
        );
        let api = MockApi {
            documents: BTreeMap::from([("remote".to_string(), drifted)]),
            info_calls: RefCell::new(Vec::new()),
        };

        let plan = plan_outline_reconciliation(
            &api,
            "wiki",
            "collection",
            &publication(&[("Home.md", "local edit", None)]),
            &state,
            true,
        )
        .expect("overwrite plan");

        assert_eq!(plan.overwritten_conflicts, 1);
        assert_eq!(
            plan.actions[0].kind,
            OutlinePublishActionKind::UpdateAndMove
        );
        assert!(!plan.has_conflicts());
        assert!(api.info_calls.borrow().is_empty());
    }

    #[test]
    fn selective_conflict_policy_overwrites_only_named_source_paths() {
        let home = remote("home", "Home", "old home", None);
        let notes = remote("notes", "Notes", "old notes", None);
        let mut state = OutlinePublishState::empty("wiki", "collection");
        state
            .documents
            .insert("source-home".to_string(), state_entry("Home.md", &home));
        state
            .documents
            .insert("source-notes".to_string(), state_entry("Notes.md", &notes));
        let api = MockApi {
            documents: BTreeMap::from([
                (
                    "home".to_string(),
                    remote("home", "Home", "remote home", None),
                ),
                (
                    "notes".to_string(),
                    remote("notes", "Notes", "remote notes", None),
                ),
            ]),
            info_calls: RefCell::new(Vec::new()),
        };
        let policy = OutlineConflictPolicy::overwrite_paths(["Home.md".to_string()]);

        let plan = plan_outline_reconciliation_with_policy(
            &api,
            "wiki",
            "collection",
            &publication(&[
                ("Home.md", "local home", None),
                ("Notes.md", "local notes", None),
            ]),
            &state,
            &policy,
        )
        .expect("selective conflict plan");

        assert_eq!(plan.overwritten_conflicts, 1);
        assert_eq!(plan.actions[0].kind, OutlinePublishActionKind::Update);
        assert_eq!(plan.actions[1].kind, OutlinePublishActionKind::Conflict);
        assert_eq!(plan.actions[1].source_path.as_deref(), Some("Notes.md"));
    }

    #[test]
    fn moved_source_conflict_reports_current_local_path_and_three_way_parent_state() {
        let original = remote("remote", "Home", "old", Some("old-parent"));
        let mut state = OutlinePublishState::empty("wiki", "collection");
        state
            .documents
            .insert("source".to_string(), state_entry("Home.md", &original));
        let api = MockApi {
            documents: BTreeMap::from([(
                "remote".to_string(),
                remote("remote", "Home", "remote edit", Some("old-parent")),
            )]),
            info_calls: RefCell::new(Vec::new()),
        };
        let mut desired = publication(&[("Moved.md", "local edit", None)]);
        desired.documents[0].source_document_id = "cache-Home.md".to_string();

        let plan = plan_outline_reconciliation(&api, "wiki", "collection", &desired, &state, false)
            .expect("moved conflict plan");

        let action = &plan.actions[0];
        assert_eq!(action.source_path.as_deref(), Some("Moved.md"));
        assert_eq!(action.desired_parent_remote_id, None);
        let conflict = action.conflict.as_ref().expect("conflict detail");
        assert!(conflict
            .local
            .changed_fields
            .contains(&OutlineConflictField::Parent));
        assert!(!conflict
            .remote
            .changed_fields
            .contains(&OutlineConflictField::Parent));
        assert_eq!(
            conflict.base_parent_remote_id.as_deref(),
            Some("old-parent")
        );
        assert_eq!(conflict.local_parent_remote_id, None);
        assert_eq!(
            conflict.remote_parent_remote_id.as_deref(),
            Some("old-parent")
        );
    }

    #[test]
    fn interrupted_archive_is_resumed_without_requiring_collection_visibility() {
        let prior = remote("remote", "Home", "home", None);
        let mut mapping = state_entry("Home.md", &prior);
        mapping.pending_archive = true;
        let mut state = OutlinePublishState::empty("wiki", "collection");
        state.documents.insert("source".to_string(), mapping);
        let api = MockApi::default();
        let plan = plan_outline_reconciliation(
            &api,
            "wiki",
            "collection",
            &publication(&[]),
            &state,
            false,
        )
        .expect("resume archive plan");
        assert_eq!(plan.actions[0].kind, OutlinePublishActionKind::Archive);
        assert!(api.info_calls.borrow().is_empty());
    }
}
