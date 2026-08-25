use super::{
    lock_outline_state, outline_state_collection_id, OutlineApi, OutlineCollectionCreate,
    OutlineRemoteCollection,
};
use crate::config::{
    apply_config_set_report, plan_config_set_report_to, ConfigSetReport, ConfigTarget,
};
use crate::AppError;
use serde::Serialize;
use std::collections::BTreeSet;
use toml::Value as TomlValue;
use vulcan_core::{load_vault_config, VaultPaths};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OutlineCollectionProvisionReport {
    pub profile: String,
    pub requested_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub collection: Option<OutlineRemoteCollection>,
    pub status: OutlineCollectionProvisionStatus,
    pub profile_updated: bool,
    pub config_path: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OutlineCollectionProvisionStatus {
    Planned,
    Created,
    RecoveredAfterCreateError,
}

pub fn bind_outline_profile_collection(
    paths: &VaultPaths,
    profile: &str,
    collection_id: &str,
    replace_existing: bool,
    dry_run: bool,
) -> Result<ConfigSetReport, AppError> {
    validate_profile_name(profile)?;
    let collection_id = collection_id.trim();
    if collection_id.is_empty() {
        return Err(AppError::operation("Outline collection ID cannot be empty"));
    }
    let loaded = load_vault_config(paths);
    let configured = loaded
        .config
        .publish
        .outline
        .profiles
        .get(profile)
        .ok_or_else(|| {
            AppError::operation(format!("Outline publish profile `{profile}` was not found"))
        })?;
    if configured
        .collection_id
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .is_some_and(|existing| existing != collection_id && !replace_existing)
    {
        return Err(AppError::operation(format!(
            "Outline profile `{profile}` already references another collection; use --replace-profile-collection to replace it"
        )));
    }
    if let Some(state_collection_id) = outline_state_collection_id(paths, profile)? {
        if state_collection_id != collection_id {
            return Err(AppError::operation(format!(
                "Outline profile `{profile}` has durable publication state for collection `{state_collection_id}`; use a new profile or remove that state after reviewing it"
            )));
        }
    }
    let key = format!("publish.outline.profiles.{profile}.collection_id");
    let report = plan_config_set_report_to(
        paths,
        &key,
        &TomlValue::String(collection_id.to_string()),
        ConfigTarget::Shared,
        dry_run,
    )?;
    if dry_run {
        Ok(report)
    } else {
        apply_config_set_report(paths, report)
    }
}

pub fn provision_outline_profile_collection(
    paths: &VaultPaths,
    api: &dyn OutlineApi,
    profile: &str,
    request: &OutlineCollectionCreate,
    replace_existing: bool,
    dry_run: bool,
) -> Result<OutlineCollectionProvisionReport, AppError> {
    validate_outline_collection_create(request)?;
    validate_profile_name(profile)?;
    let configured_id = load_vault_config(paths)
        .config
        .publish
        .outline
        .profiles
        .get(profile)
        .ok_or_else(|| {
            AppError::operation(format!("Outline publish profile `{profile}` was not found"))
        })?
        .collection_id
        .clone()
        .filter(|value| !value.trim().is_empty());
    if configured_id.is_some() && !replace_existing {
        return Err(AppError::operation(format!(
            "Outline profile `{profile}` already has a collection_id; use --replace-profile-collection to create and bind a replacement"
        )));
    }
    if dry_run {
        return Ok(OutlineCollectionProvisionReport {
            profile: profile.to_string(),
            requested_name: request.name.trim().to_string(),
            collection: None,
            status: OutlineCollectionProvisionStatus::Planned,
            profile_updated: true,
            config_path: ".vulcan/config.toml".to_string(),
        });
    }

    let _lock = lock_outline_state(paths, profile)?;
    if let Some(state_collection_id) = outline_state_collection_id(paths, profile)? {
        return Err(AppError::operation(format!(
            "Outline profile `{profile}` has durable publication state for collection `{state_collection_id}`; use a new profile before creating a replacement collection"
        )));
    }
    let (collection, recovered_after_create_error) =
        create_outline_collection_safely(api, request)?;
    let config = bind_outline_profile_collection(
        paths,
        profile,
        &collection.id,
        replace_existing,
        false,
    )
    .map_err(|error| {
        AppError::operation(format!(
            "Outline collection `{}` was created as `{}`, but its UUID could not be persisted: {error}",
            collection.name, collection.id
        ))
    })?;
    Ok(OutlineCollectionProvisionReport {
        profile: profile.to_string(),
        requested_name: request.name.trim().to_string(),
        collection: Some(collection),
        status: if recovered_after_create_error {
            OutlineCollectionProvisionStatus::RecoveredAfterCreateError
        } else {
            OutlineCollectionProvisionStatus::Created
        },
        profile_updated: config.updated,
        config_path: config.config_path.display().to_string(),
    })
}

fn create_outline_collection_safely(
    api: &dyn OutlineApi,
    request: &OutlineCollectionCreate,
) -> Result<(OutlineRemoteCollection, bool), AppError> {
    let before_collections = api.list_collections(Some(request.name.trim()), false)?;
    let exact_existing = before_collections
        .iter()
        .filter(|collection| collection.name.trim() == request.name.trim())
        .collect::<Vec<_>>();
    if !exact_existing.is_empty() {
        let ids = exact_existing
            .iter()
            .map(|collection| collection.id.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        return Err(AppError::operation(format!(
            "Outline already has a collection named `{}` ({ids}); bind the intended UUID explicitly instead of creating a duplicate",
            request.name.trim()
        )));
    }
    let before = before_collections
        .into_iter()
        .map(|collection| collection.id)
        .collect::<BTreeSet<_>>();
    Ok(match api.create_collection(request) {
        Ok(collection) => (collection, false),
        Err(create_error) => {
            let candidates = api
                .list_collections(Some(request.name.trim()), false)?
                .into_iter()
                .filter(|collection| {
                    collection.name.trim() == request.name.trim()
                        && !before.contains(&collection.id)
                })
                .collect::<Vec<_>>();
            match candidates.as_slice() {
                [collection] => (collection.clone(), true),
                [] => return Err(create_error),
                _ => {
                    return Err(AppError::operation(
                        "Outline collection creation had an ambiguous result; list collections and bind the intended UUID explicitly",
                    ))
                }
            }
        }
    })
}

fn validate_profile_name(profile: &str) -> Result<(), AppError> {
    if profile.is_empty()
        || !profile
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '-'))
    {
        return Err(AppError::operation(
            "Outline profile names may contain only ASCII letters, digits, `_`, and `-`",
        ));
    }
    Ok(())
}

pub fn validate_outline_collection_create(
    request: &OutlineCollectionCreate,
) -> Result<(), AppError> {
    if request.name.trim().is_empty() {
        return Err(AppError::operation(
            "Outline collection name cannot be empty",
        ));
    }
    Ok(())
}

pub fn validate_outline_collection_update(
    request: &super::OutlineCollectionUpdate,
) -> Result<(), AppError> {
    if request == &super::OutlineCollectionUpdate::default() {
        return Err(AppError::operation(
            "Outline collection update requires at least one changed field",
        ));
    }
    if request
        .name
        .as_deref()
        .is_some_and(|name| name.trim().is_empty())
    {
        return Err(AppError::operation(
            "Outline collection name cannot be empty",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::fs;
    use tempfile::TempDir;

    #[derive(Default)]
    struct MockApi {
        collections: RefCell<Vec<OutlineRemoteCollection>>,
        creates: RefCell<usize>,
        fail_after_create: bool,
    }

    impl OutlineApi for MockApi {
        fn list_collections(
            &self,
            query: Option<&str>,
            _archived: bool,
        ) -> Result<Vec<OutlineRemoteCollection>, AppError> {
            Ok(self
                .collections
                .borrow()
                .iter()
                .filter(|collection| query.is_none_or(|query| collection.name.contains(query)))
                .cloned()
                .collect())
        }

        fn create_collection(
            &self,
            request: &OutlineCollectionCreate,
        ) -> Result<OutlineRemoteCollection, AppError> {
            *self.creates.borrow_mut() += 1;
            let collection = OutlineRemoteCollection {
                id: "11111111-1111-4111-8111-111111111111".to_string(),
                name: request.name.clone(),
                description: request.description.clone(),
                url: Some("/collection/wiki".to_string()),
                url_id: Some("wiki".to_string()),
                icon: None,
                color: None,
                permission: Some("read_write".to_string()),
                sharing: Some(true),
                commenting: Some(true),
                created_at: None,
                updated_at: None,
                archived_at: None,
            };
            self.collections.borrow_mut().push(collection.clone());
            if self.fail_after_create {
                Err(AppError::operation("simulated lost create response"))
            } else {
                Ok(collection)
            }
        }

        fn list_collection_documents(
            &self,
            _collection_id: &str,
        ) -> Result<Vec<super::super::OutlineRemoteDocument>, AppError> {
            unreachable!("document API is not used by collection provisioning tests")
        }

        fn document_info(
            &self,
            _id: &str,
        ) -> Result<super::super::OutlineRemoteDocument, AppError> {
            unreachable!("document API is not used by collection provisioning tests")
        }

        fn create_document(
            &self,
            _id: &str,
            _collection_id: &str,
            _parent_document_id: Option<&str>,
            _title: &str,
            _text: &str,
        ) -> Result<super::super::OutlineRemoteDocument, AppError> {
            unreachable!("document API is not used by collection provisioning tests")
        }

        fn update_document(
            &self,
            _id: &str,
            _title: &str,
            _text: &str,
        ) -> Result<super::super::OutlineRemoteDocument, AppError> {
            unreachable!("document API is not used by collection provisioning tests")
        }

        fn move_document(
            &self,
            _id: &str,
            _collection_id: &str,
            _parent_document_id: Option<&str>,
        ) -> Result<super::super::OutlineRemoteDocument, AppError> {
            unreachable!("document API is not used by collection provisioning tests")
        }

        fn archive_document(
            &self,
            _id: &str,
        ) -> Result<super::super::OutlineRemoteDocument, AppError> {
            unreachable!("document API is not used by collection provisioning tests")
        }

        fn upload_attachment(
            &self,
            _document_id: &str,
            _name: &str,
            _content_type: &str,
            _bytes: &[u8],
        ) -> Result<super::super::OutlineRemoteAttachment, AppError> {
            unreachable!("document API is not used by collection provisioning tests")
        }
    }

    fn configured_vault() -> (TempDir, VaultPaths) {
        let temp = TempDir::new().expect("temp vault");
        fs::create_dir_all(temp.path().join(".vulcan")).expect("config directory");
        fs::write(
            temp.path().join(".vulcan/config.toml"),
            "[publish.outline.profiles.wiki]\nbase_url = \"https://outline.test\"\ntoken_env = \"OUTLINE_TOKEN\"\ncollection_title = \"Players\"\nquery = \"from notes\"\n",
        )
        .expect("profile config");
        let paths = VaultPaths::new(temp.path());
        (temp, paths)
    }

    fn collection(id: &str, name: &str) -> OutlineRemoteCollection {
        OutlineRemoteCollection {
            id: id.to_string(),
            name: name.to_string(),
            description: None,
            url: None,
            url_id: None,
            icon: None,
            color: None,
            permission: None,
            sharing: None,
            commenting: None,
            created_at: None,
            updated_at: None,
            archived_at: None,
        }
    }

    #[test]
    fn provision_creates_collection_and_persists_uuid_in_profile() {
        let (_temp, paths) = configured_vault();
        let api = MockApi::default();
        let report = provision_outline_profile_collection(
            &paths,
            &api,
            "wiki",
            &OutlineCollectionCreate {
                name: "Players".to_string(),
                ..OutlineCollectionCreate::default()
            },
            false,
            false,
        )
        .expect("provision collection");

        assert_eq!(report.status, OutlineCollectionProvisionStatus::Created);
        assert!(report.profile_updated);
        assert_eq!(*api.creates.borrow(), 1);
        let loaded = load_vault_config(&paths);
        assert_eq!(
            loaded.config.publish.outline.profiles["wiki"]
                .collection_id
                .as_deref(),
            Some("11111111-1111-4111-8111-111111111111")
        );
    }

    #[test]
    fn provision_dry_run_does_not_call_outline_or_write_config() {
        let (_temp, paths) = configured_vault();
        let before = fs::read_to_string(paths.config_file()).expect("config before");
        let api = MockApi::default();
        let report = provision_outline_profile_collection(
            &paths,
            &api,
            "wiki",
            &OutlineCollectionCreate {
                name: "Players".to_string(),
                ..OutlineCollectionCreate::default()
            },
            false,
            true,
        )
        .expect("dry run");

        assert_eq!(report.status, OutlineCollectionProvisionStatus::Planned);
        assert_eq!(*api.creates.borrow(), 0);
        assert_eq!(
            fs::read_to_string(paths.config_file()).expect("config after"),
            before
        );
    }

    #[test]
    fn provision_recovers_unique_collection_after_lost_create_response() {
        let (_temp, paths) = configured_vault();
        let api = MockApi {
            fail_after_create: true,
            ..MockApi::default()
        };
        let report = provision_outline_profile_collection(
            &paths,
            &api,
            "wiki",
            &OutlineCollectionCreate {
                name: "Players".to_string(),
                ..OutlineCollectionCreate::default()
            },
            false,
            false,
        )
        .expect("recover completed create");

        assert_eq!(
            report.status,
            OutlineCollectionProvisionStatus::RecoveredAfterCreateError
        );
        assert_eq!(
            load_vault_config(&paths).config.publish.outline.profiles["wiki"]
                .collection_id
                .as_deref(),
            Some("11111111-1111-4111-8111-111111111111")
        );
    }

    #[test]
    fn provision_rejects_title_based_adoption_and_duplicate_creation() {
        let (_temp, paths) = configured_vault();
        let api = MockApi {
            collections: RefCell::new(vec![collection("existing-id", "Players")]),
            ..MockApi::default()
        };
        let error = provision_outline_profile_collection(
            &paths,
            &api,
            "wiki",
            &OutlineCollectionCreate {
                name: "Players".to_string(),
                ..OutlineCollectionCreate::default()
            },
            false,
            false,
        )
        .expect_err("existing title requires explicit UUID binding");

        assert!(error.to_string().contains("existing-id"));
        assert!(error.to_string().contains("bind"));
        assert_eq!(*api.creates.borrow(), 0);
    }
}
