use crate::outline_markdown::{outline_document_links_to_obsidian, outline_to_obsidian_markdown};
use crate::publish::outline::{OutlineApi, OutlineRemoteDocument};
use crate::AppError;
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use vulcan_core::paths::{secure_read_to_string, secure_write};
use vulcan_core::VaultPaths;

const STATE_VERSION: u32 = 1;

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
    Unchanged,
    Conflict,
    WriteConflictMarkers,
    RemoteMissing,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OutlinePullAction {
    pub kind: OutlinePullActionKind,
    pub remote_document_id: String,
    pub local_path: String,
    pub reason: String,
    pub local_changed: bool,
    pub remote_changed: bool,
    #[serde(skip)]
    desired_content: Option<String>,
    #[serde(skip)]
    local_content: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OutlinePullReport {
    pub profile: String,
    pub collection_id: String,
    pub destination: String,
    pub dry_run: bool,
    pub applied: bool,
    pub conflicts: usize,
    pub created: usize,
    pub updated: usize,
    pub unchanged: usize,
    pub conflict_markers_written: usize,
    pub remote_missing: usize,
    pub actions: Vec<OutlinePullAction>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct OutlinePullState {
    version: u32,
    profile: String,
    collection_id: String,
    destination: String,
    #[serde(default)]
    documents: BTreeMap<String, OutlinePullMapping>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct OutlinePullMapping {
    local_path: String,
    last_remote_content_hash: String,
    last_remote_title: String,
    last_remote_parent_id: Option<String>,
    last_materialized_local_hash: String,
    base_content: String,
}

impl OutlinePullState {
    fn empty(profile: &str, collection_id: &str, destination: &str) -> Self {
        Self {
            version: STATE_VERSION,
            profile: profile.to_string(),
            collection_id: collection_id.to_string(),
            destination: destination.to_string(),
            documents: BTreeMap::new(),
        }
    }

    fn validate(
        &self,
        profile: &str,
        collection_id: &str,
        destination: &str,
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
            validate_managed_path(destination, &mapping.local_path)?;
            if !local_paths.insert(mapping.local_path.to_lowercase()) {
                return Err(AppError::operation(
                    "Outline pull state maps multiple documents to the same local path",
                ));
            }
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
    pull_outline_with_write_authorizer(
        paths,
        api,
        profile,
        collection_id,
        destination,
        dry_run,
        conflict_policy,
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
    let destination = validate_destination(destination)?;
    if dry_run {
        let state = load_state(paths, profile, collection_id, &destination)?;
        let remote = api.list_collection_documents(collection_id)?;
        let actions = plan_pull(paths, &remote, &state, conflict_policy)?;
        return Ok(report(
            profile,
            collection_id,
            &destination,
            true,
            false,
            actions,
        ));
    }

    let lock = StateLock::acquire(paths, profile)?;
    let mut state = load_state(paths, profile, collection_id, &destination)?;
    let remote = api.list_collection_documents(collection_id)?;
    let mut actions = plan_pull(paths, &remote, &state, conflict_policy)?;
    if actions
        .iter()
        .any(|action| action.kind == OutlinePullActionKind::Conflict)
    {
        return Ok(report(
            profile,
            collection_id,
            &destination,
            false,
            false,
            actions,
        ));
    }
    for action in &actions {
        if matches!(
            action.kind,
            OutlinePullActionKind::Create
                | OutlinePullActionKind::Update
                | OutlinePullActionKind::WriteConflictMarkers
        ) {
            authorize_write(&action.local_path)?;
        }
    }
    let remote_by_id = remote
        .iter()
        .map(|document| (document.id.as_str(), document))
        .collect::<BTreeMap<_, _>>();
    for action in &mut actions {
        if !matches!(
            action.kind,
            OutlinePullActionKind::Create
                | OutlinePullActionKind::Update
                | OutlinePullActionKind::WriteConflictMarkers
        ) {
            continue;
        }
        let remote = remote_by_id
            .get(action.remote_document_id.as_str())
            .ok_or_else(|| AppError::operation("planned Outline pull document disappeared"))?;
        let desired = action
            .desired_content
            .as_deref()
            .ok_or_else(|| AppError::operation("Outline pull action omitted desired content"))?;
        let written = if action.kind == OutlinePullActionKind::WriteConflictMarkers {
            conflict_markers(
                original_local_content(action.local_content.as_deref().unwrap_or_default()),
                state
                    .documents
                    .get(&action.remote_document_id)
                    .map_or("", |mapping| mapping.base_content.as_str()),
                desired,
                &action.remote_document_id,
            )
        } else {
            desired.to_string()
        };
        secure_write(
            paths.vault_root(),
            Path::new(&action.local_path),
            written.as_bytes(),
        )
        .map_err(AppError::operation)?;
        if action.kind != OutlinePullActionKind::WriteConflictMarkers {
            state.documents.insert(
                action.remote_document_id.clone(),
                OutlinePullMapping {
                    local_path: action.local_path.clone(),
                    last_remote_content_hash: content_hash(desired),
                    last_remote_title: remote.title.clone(),
                    last_remote_parent_id: remote.parent_document_id.clone(),
                    last_materialized_local_hash: content_hash(desired),
                    base_content: desired.to_string(),
                },
            );
            lock.save(&state)?;
        }
    }
    crate::scan::refresh_cache_incrementally(paths)?;
    Ok(report(
        profile,
        collection_id,
        &destination,
        false,
        true,
        actions,
    ))
}

#[allow(clippy::too_many_lines)]
fn plan_pull(
    paths: &VaultPaths,
    remote: &[OutlineRemoteDocument],
    state: &OutlinePullState,
    conflict_policy: &OutlinePullConflictPolicy,
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
    let generated_paths = generate_paths(&active, &state.destination)?;
    let local_paths = active
        .iter()
        .map(|document| {
            let path = state.documents.get(&document.id).map_or_else(
                || generated_paths[&document.id].clone(),
                |mapping| mapping.local_path.clone(),
            );
            (document.id.clone(), path)
        })
        .collect::<BTreeMap<_, _>>();
    let mut seen_local_paths = BTreeMap::<String, String>::new();
    for (remote_id, local_path) in &local_paths {
        validate_managed_path(&state.destination, local_path)?;
        if let Some(existing) =
            seen_local_paths.insert(local_path.to_lowercase(), remote_id.clone())
        {
            return Err(AppError::operation(format!(
                "Outline documents `{existing}` and `{remote_id}` map to the same case-insensitive local path `{local_path}`"
            )));
        }
    }
    let mut actions = Vec::with_capacity(active.len() + state.documents.len());
    for document in &active {
        let local_path = local_paths[&document.id].clone();
        let desired = outline_document_links_to_obsidian(
            &outline_to_obsidian_markdown(&document.text),
            |remote_id| local_paths.get(remote_id).cloned(),
        );
        let local_content = match secure_read_to_string(paths.vault_root(), Path::new(&local_path))
        {
            Ok(content) => Some(content),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(AppError::operation(error)),
        };
        let mapping = state.documents.get(&document.id);
        let desired_hash = content_hash(&desired);
        let local_hash = local_content.as_deref().map(content_hash);
        let local_changed = match (mapping, local_hash.as_deref()) {
            (Some(mapping), Some(hash)) => hash != mapping.last_materialized_local_hash,
            (Some(_), None) | (None, Some(_)) => true,
            (None, None) => false,
        };
        let remote_changed = mapping.is_none_or(|mapping| {
            desired_hash != mapping.last_remote_content_hash
                || document.title != mapping.last_remote_title
                || document.parent_document_id != mapping.last_remote_parent_id
        });
        let desired_matches_local = local_hash.as_deref() == Some(desired_hash.as_str());
        let collision = mapping.is_none() && local_content.is_some() && !desired_matches_local;
        let conflicted = collision || (local_changed && remote_changed && !desired_matches_local);
        let (kind, reason) = if conflicted {
            match conflict_policy.resolution(&local_path) {
                Some(OutlinePullConflictResolution::OverwriteLocal) => (
                    if mapping.is_some() {
                        OutlinePullActionKind::Update
                    } else {
                        OutlinePullActionKind::Create
                    },
                    "overwrite the reviewed local conflict with Outline",
                ),
                Some(OutlinePullConflictResolution::ConflictMarkers) => (
                    OutlinePullActionKind::WriteConflictMarkers,
                    "write a reviewed diff3-style local/base/Outline conflict",
                ),
                _ => (
                    OutlinePullActionKind::Conflict,
                    if collision {
                        "an unmanaged local file already occupies the Outline destination"
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
        } else if remote_changed && (!local_changed || desired_matches_local) {
            (
                OutlinePullActionKind::Update,
                if desired_matches_local {
                    "adopt a resolved local file that matches the current Outline document"
                } else {
                    "Outline document changed"
                },
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
        actions.push(OutlinePullAction {
            kind,
            remote_document_id: document.id.clone(),
            local_path,
            reason: reason.to_string(),
            local_changed,
            remote_changed,
            desired_content: Some(desired),
            local_content,
        });
    }
    for (remote_id, mapping) in &state.documents {
        if !active.iter().any(|document| document.id == *remote_id) {
            actions.push(OutlinePullAction {
                kind: OutlinePullActionKind::RemoteMissing,
                remote_document_id: remote_id.clone(),
                local_path: mapping.local_path.clone(),
                reason:
                    "managed Outline document is no longer in the collection; local file retained"
                        .to_string(),
                local_changed: false,
                remote_changed: true,
                desired_content: None,
                local_content: None,
            });
        }
    }
    actions.sort_by(|left, right| left.local_path.cmp(&right.local_path));
    Ok(actions)
}

fn generate_paths(
    remote: &[OutlineRemoteDocument],
    destination: &str,
) -> Result<BTreeMap<String, String>, AppError> {
    let by_id = remote
        .iter()
        .map(|document| (document.id.as_str(), document))
        .collect::<BTreeMap<_, _>>();
    let mut paths = BTreeMap::new();
    for document in remote
        .iter()
        .filter(|document| document.archived_at.is_none())
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
                break;
            };
            titles.push(safe_title(&parent_document.title, &parent_document.id));
            parent = parent_document.parent_document_id.as_deref();
        }
        titles.reverse();
        let file = titles.pop().expect("document title exists");
        let mut path = PathBuf::from(destination);
        path.extend(titles);
        path.push(format!("{file}.md"));
        paths.insert(
            document.id.clone(),
            path.to_string_lossy().replace('\\', "/"),
        );
    }
    let mut seen = BTreeMap::<String, String>::new();
    for (remote_id, path) in &paths {
        if let Some(existing) = seen.insert(path.to_lowercase(), remote_id.clone()) {
            return Err(AppError::operation(format!(
                "Outline hierarchy maps remote documents `{existing}` and `{remote_id}` to the same case-insensitive local path `{path}`"
            )));
        }
    }
    Ok(paths)
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
    let title = title.trim().trim_matches('.');
    if title.is_empty() {
        format!("untitled-{}", &remote_id[..remote_id.len().min(8)])
    } else {
        title.to_string()
    }
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

fn conflict_markers(local: &str, base: &str, remote: &str, remote_id: &str) -> String {
    format!(
        "<<<<<<< LOCAL\n{}\n||||||| BASE\n{}\n=======\n{}\n>>>>>>> OUTLINE {remote_id}\n",
        local.trim_end(),
        base.trim_end(),
        remote.trim_end()
    )
}

fn original_local_content(content: &str) -> &str {
    content
        .strip_prefix("<<<<<<< LOCAL\n")
        .and_then(|content| content.split_once("\n||||||| BASE\n"))
        .map_or(content, |(local, _)| local)
}

fn report(
    profile: &str,
    collection_id: &str,
    destination: &str,
    dry_run: bool,
    applied: bool,
    actions: Vec<OutlinePullAction>,
) -> OutlinePullReport {
    let count = |kind| actions.iter().filter(|action| action.kind == kind).count();
    OutlinePullReport {
        profile: profile.to_string(),
        collection_id: collection_id.to_string(),
        destination: destination.to_string(),
        dry_run,
        applied,
        conflicts: count(OutlinePullActionKind::Conflict),
        created: count(OutlinePullActionKind::Create),
        updated: count(OutlinePullActionKind::Update),
        unchanged: count(OutlinePullActionKind::Unchanged),
        conflict_markers_written: count(OutlinePullActionKind::WriteConflictMarkers),
        remote_missing: count(OutlinePullActionKind::RemoteMissing),
        actions,
    }
}

fn content_hash(content: &str) -> String {
    blake3::hash(content.as_bytes()).to_hex().to_string()
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
) -> Result<OutlinePullState, AppError> {
    let path = state_path(paths, profile)?;
    if !path.exists() {
        return Ok(OutlinePullState::empty(profile, collection_id, destination));
    }
    let bytes = fs::read(path).map_err(AppError::operation)?;
    let state: OutlinePullState = serde_json::from_slice(&bytes)
        .map_err(|_| AppError::operation("Outline pull state contains malformed JSON"))?;
    state.validate(profile, collection_id, destination)?;
    Ok(state)
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
    use crate::publish::outline::OutlineRemoteAttachment;
    use tempfile::tempdir;
    use vulcan_core::initialize_vulcan_dir;

    struct MockApi {
        documents: Vec<OutlineRemoteDocument>,
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
    }

    fn document(id: &str, title: &str, text: &str, parent: Option<&str>) -> OutlineRemoteDocument {
        OutlineRemoteDocument {
            id: id.to_string(),
            title: title.to_string(),
            text: text.to_string(),
            collection_id: "collection".to_string(),
            parent_document_id: parent.map(str::to_string),
            archived_at: None,
        }
    }

    #[test]
    fn pull_materializes_hierarchy_reverse_markdown_and_links_idempotently() {
        let temp = tempdir().expect("temp dir");
        let paths = VaultPaths::new(temp.path());
        initialize_vulcan_dir(&paths).expect("initialize vault");
        let api = MockApi {
            documents: vec![
                document(
                    "parent",
                    "THE ÒRÌSHÀ",
                    ":::warning\nCareful\n:::\n\n[Yemoja](/doc/child)",
                    None,
                ),
                document("child", "Yemoja", "# Water\n", Some("parent")),
            ],
        };

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
    fn pull_conflicts_support_overwrite_and_diff3_markers() {
        let temp = tempdir().expect("temp dir");
        let paths = VaultPaths::new(temp.path());
        initialize_vulcan_dir(&paths).expect("initialize vault");
        let initial = MockApi {
            documents: vec![document("home", "Home", "base\n", None)],
        };
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
        let changed = MockApi {
            documents: vec![document("home", "Home", "remote edit\n", None)],
        };

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
    fn pull_rejects_case_collisions_and_internal_destinations() {
        let temp = tempdir().expect("temp dir");
        let paths = VaultPaths::new(temp.path());
        initialize_vulcan_dir(&paths).expect("initialize vault");
        let api = MockApi {
            documents: vec![
                document("one", "Home", "one", None),
                document("two", "home", "two", None),
            ],
        };
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
    fn live_pull_authorizes_the_fresh_plan_before_writing_notes() {
        let temp = tempdir().expect("temp dir");
        let paths = VaultPaths::new(temp.path());
        initialize_vulcan_dir(&paths).expect("initialize vault");
        let api = MockApi {
            documents: vec![document("home", "Home", "remote\n", None)],
        };

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
    fn pull_state_rejects_unsafe_or_duplicate_managed_paths() {
        let mapping = |path: &str| OutlinePullMapping {
            local_path: path.to_string(),
            last_remote_content_hash: "remote".to_string(),
            last_remote_title: "Home".to_string(),
            last_remote_parent_id: None,
            last_materialized_local_hash: "local".to_string(),
            base_content: "base".to_string(),
        };
        let mut unsafe_state = OutlinePullState::empty("wiki", "collection", "Imported");
        unsafe_state
            .documents
            .insert("one".to_string(), mapping(".vulcan/config.md"));
        assert!(unsafe_state
            .validate("wiki", "collection", "Imported")
            .is_err());

        let mut duplicate_state = OutlinePullState::empty("wiki", "collection", "Imported");
        duplicate_state
            .documents
            .insert("one".to_string(), mapping("Imported/Home.md"));
        duplicate_state
            .documents
            .insert("two".to_string(), mapping("Imported/home.md"));
        assert!(duplicate_state
            .validate("wiki", "collection", "Imported")
            .is_err());
    }
}
