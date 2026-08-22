use super::{OutlineApi, OutlineDocumentMapping, OutlinePublishState, OutlineRemoteDocument};
use crate::export::outline::OutlinePublicationPlan;
use crate::AppError;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OutlinePublishActionKind {
    Create,
    Update,
    Move,
    UpdateAndMove,
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
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OutlinePublishPlan {
    pub profile: String,
    pub collection_id: String,
    pub dry_run: bool,
    pub unmanaged_remote_documents: usize,
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

pub fn plan_outline_reconciliation(
    api: &dyn OutlineApi,
    profile: &str,
    collection_id: &str,
    publication: &OutlinePublicationPlan,
    state: &OutlinePublishState,
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

    let matches = match_local_documents(publication, state);
    let mut matched_identities = BTreeSet::new();
    let mut actions = Vec::new();
    for document in &publication.documents {
        let matched = matches.get(&document.source_path).copied();
        let desired_parent_remote_id = document
            .parent_source_path
            .as_deref()
            .and_then(|parent| matches.get(parent))
            .and_then(|identity| state.documents.get(*identity))
            .map(|mapping| mapping.remote_document_id.clone());
        let Some(source_identity) = matched else {
            actions.push(OutlinePublishAction {
                kind: OutlinePublishActionKind::Create,
                source_identity: None,
                source_path: Some(document.source_path.clone()),
                remote_document_id: None,
                parent_source_path: document.parent_source_path.clone(),
                desired_parent_remote_id,
                reason: "local document has no durable Outline mapping".to_string(),
            });
            continue;
        };
        matched_identities.insert(source_identity.to_string());
        let mapping = &state.documents[source_identity];
        if !listed_by_id.contains_key(mapping.remote_document_id.as_str()) {
            actions.push(conflict_action(
                source_identity,
                mapping,
                "managed remote document is missing from the collection",
            ));
            continue;
        }
        let remote = api.document_info(&mapping.remote_document_id)?;
        let remote_hash = content_hash(&remote.text);
        let remote_drift = remote_hash != mapping.last_published_content_hash
            || remote.title != mapping.last_published_title
            || remote.parent_document_id != mapping.remote_parent_id;
        let desired_matches_remote = remote_hash == document.content_hash
            && remote.title == document.title
            && remote.parent_document_id == desired_parent_remote_id;
        if remote_drift && !desired_matches_remote {
            actions.push(conflict_action(
                source_identity,
                mapping,
                "remote content, title, or parent changed since the last successful publication",
            ));
            continue;
        }
        let local_content_changed = document.content_hash != mapping.last_published_content_hash
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
        });
    }

    for (source_identity, mapping) in &state.documents {
        if matched_identities.contains(source_identity) {
            continue;
        }
        if !listed_by_id.contains_key(mapping.remote_document_id.as_str()) {
            actions.push(conflict_action(
                source_identity,
                mapping,
                "locally removed mapping points to a missing remote document",
            ));
            continue;
        }
        let remote = api.document_info(&mapping.remote_document_id)?;
        if remote.archived_at.is_some() {
            actions.push(OutlinePublishAction {
                kind: OutlinePublishActionKind::AdoptRemoteResult,
                source_identity: Some(source_identity.clone()),
                source_path: Some(mapping.source_path.clone()),
                remote_document_id: Some(mapping.remote_document_id.clone()),
                parent_source_path: None,
                desired_parent_remote_id: None,
                reason: "remote document was already archived".to_string(),
            });
        } else if content_hash(&remote.text) != mapping.last_published_content_hash
            || remote.title != mapping.last_published_title
            || remote.parent_document_id != mapping.remote_parent_id
        {
            actions.push(conflict_action(
                source_identity,
                mapping,
                "remote document changed after its local source was removed",
            ));
        } else {
            actions.push(OutlinePublishAction {
                kind: OutlinePublishActionKind::Archive,
                source_identity: Some(source_identity.clone()),
                source_path: Some(mapping.source_path.clone()),
                remote_document_id: Some(mapping.remote_document_id.clone()),
                parent_source_path: None,
                desired_parent_remote_id: None,
                reason: "previously managed local source is no longer selected".to_string(),
            });
        }
    }

    Ok(OutlinePublishPlan {
        profile: profile.to_string(),
        collection_id: collection_id.to_string(),
        dry_run: true,
        unmanaged_remote_documents,
        actions,
    })
}

fn match_local_documents<'a>(
    publication: &OutlinePublicationPlan,
    state: &'a OutlinePublishState,
) -> BTreeMap<String, &'a str> {
    let mut matches = BTreeMap::new();
    let mut available = state
        .documents
        .keys()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    for document in &publication.documents {
        let matched = unique_mapping(&available, state, |mapping| {
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
        if let Some(source_identity) = matched {
            available.remove(source_identity);
            matches.insert(document.source_path.clone(), source_identity);
        }
    }
    matches
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
    reason: &str,
) -> OutlinePublishAction {
    OutlinePublishAction {
        kind: OutlinePublishActionKind::Conflict,
        source_identity: Some(source_identity.to_string()),
        source_path: Some(mapping.source_path.clone()),
        remote_document_id: Some(mapping.remote_document_id.clone()),
        parent_source_path: None,
        desired_parent_remote_id: mapping.remote_parent_id.clone(),
        reason: reason.to_string(),
    }
}

fn content_hash(content: &str) -> String {
    blake3::hash(content.as_bytes()).to_hex().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::export::outline::{OutlinePlannedDocument, SUPPORTED_OUTLINE_VERSION};
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
        let plan = plan_outline_reconciliation(&api, "wiki", "collection", &publication, &state)
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
        let stale = remote("stale", "Old", "old", None);
        let unmanaged = remote("unmanaged", "Personal", "remote", None);
        let api = MockApi {
            documents: [parent.clone(), child.clone(), stale.clone(), unmanaged]
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
        state
            .documents
            .insert("source-stale".to_string(), state_entry("Old.md", &stale));
        let publication = publication(&[
            ("Projects.md", "parent changed", None),
            ("Moved.md", "child", None),
        ]);
        let plan = plan_outline_reconciliation(&api, "wiki", "collection", &publication, &state)
            .expect("reconciliation plan");
        assert_eq!(plan.unmanaged_remote_documents, 1);
        assert_eq!(plan.actions[0].kind, OutlinePublishActionKind::Update);
        assert_eq!(
            plan.actions[1].kind,
            OutlinePublishActionKind::UpdateAndMove
        );
        assert_eq!(plan.actions[1].source_path.as_deref(), Some("Moved.md"));
        assert_eq!(plan.actions[2].kind, OutlinePublishActionKind::Archive);
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
        )
        .expect("conflict plan");
        assert!(plan.has_conflicts());

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
        )
        .expect("interrupted plan");
        assert_eq!(
            plan.actions[0].kind,
            OutlinePublishActionKind::AdoptRemoteResult
        );
    }
}
