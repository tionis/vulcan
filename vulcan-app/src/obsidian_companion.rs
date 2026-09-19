//! Installation of the bundled Obsidian companion into a registered vault.

use crate::{durable_file, AppError};
use serde::Serialize;
use serde_json::json;
use std::fs;
use std::path::{Path, PathBuf};

pub const COMPANION_PLUGIN_ID: &str = "vulcan-companion";
const OBSIDIAN_DIRECTORY: &str = ".obsidian";
const OBSIDIAN_PLUGINS_DIRECTORY: &str = "plugins";

const COMPANION_ASSETS: [(&str, &[u8]); 3] = [
    (
        "manifest.json",
        include_bytes!("../assets/obsidian-vulcan/manifest.json"),
    ),
    (
        "main.js",
        include_bytes!("../assets/obsidian-vulcan/main.js"),
    ),
    (
        "styles.css",
        include_bytes!("../assets/obsidian-vulcan/styles.css"),
    ),
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObsidianCompanionInstallRequest<'a> {
    pub vault_root: &'a Path,
    pub base_url: &'a str,
    pub wiki_id: &'a str,
    pub dry_run: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ObsidianCompanionInstallReport {
    pub version: u32,
    pub dry_run: bool,
    pub plugin_id: String,
    pub plugin_version: String,
    pub vault: PathBuf,
    pub plugin_directory: PathBuf,
    pub base_url: String,
    pub wiki_id: String,
    pub changed: bool,
    pub configuration_created: bool,
    pub pairing: ObsidianCompanionPairingStatus,
    pub changed_files: Vec<String>,
    pub preserved_files: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ObsidianCompanionPairingStatus {
    Required,
}

pub fn install_obsidian_companion(
    request: &ObsidianCompanionInstallRequest<'_>,
) -> Result<ObsidianCompanionInstallReport, AppError> {
    validate_loopback_base_url(request.base_url)?;
    if request.wiki_id.trim().is_empty() {
        return Err(AppError::operation("companion wiki ID must not be empty"));
    }

    let vault = fs::canonicalize(request.vault_root).map_err(|error| {
        AppError::operation(format!(
            "cannot resolve companion vault {}: {error}",
            request.vault_root.display()
        ))
    })?;
    require_real_directory(&vault, "vault")?;
    let obsidian_directory = vault.join(OBSIDIAN_DIRECTORY);
    require_real_directory(&obsidian_directory, "Obsidian configuration directory")?;

    let plugins_root = obsidian_directory.join(OBSIDIAN_PLUGINS_DIRECTORY);
    require_directory_or_missing(&plugins_root, "Obsidian plugins directory")?;
    let companion_directory = plugins_root.join(COMPANION_PLUGIN_ID);
    require_directory_or_missing(&companion_directory, "Vulcan companion directory")?;

    let manifest: serde_json::Value =
        serde_json::from_slice(COMPANION_ASSETS[0].1).map_err(AppError::operation)?;
    let plugin_version = manifest
        .get("version")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| AppError::operation("bundled companion manifest has no version"))?
        .to_string();

    let mut writes = COMPANION_ASSETS
        .iter()
        .map(|(name, bytes)| {
            let path = companion_directory.join(name);
            require_regular_file_or_missing(&path, "managed companion asset")?;
            Ok(file_differs(&path, bytes).then(|| ((*name).to_string(), *bytes)))
        })
        .collect::<Result<Vec<_>, AppError>>()?
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();

    let data_path = companion_directory.join("data.json");
    require_regular_file_or_missing(&data_path, "companion plugin data")?;
    let configuration_created = !data_path.exists();
    let configuration = configuration_created.then(|| {
        serde_json::to_vec_pretty(&json!({
            "baseUrl": request.base_url,
            "wikiId": request.wiki_id,
            "syncOnSave": false,
            "saveDebounceMs": 1500,
            "eventStream": true,
            "notifyOnFailure": true,
        }))
        .map(|mut bytes| {
            bytes.push(b'\n');
            bytes
        })
        .map_err(AppError::operation)
    });
    let configuration = configuration.transpose()?;
    if let Some(bytes) = configuration.as_deref() {
        writes.push(("data.json".to_string(), bytes));
    }

    let changed_files: Vec<String> = writes.iter().map(|(name, _)| name.clone()).collect();
    let preserved_files = if data_path.exists() {
        vec!["data.json".to_string()]
    } else {
        Vec::new()
    };

    if !request.dry_run && !writes.is_empty() {
        fs::create_dir_all(&companion_directory).map_err(AppError::operation)?;
        require_real_directory(&plugins_root, "Obsidian plugins directory")?;
        require_real_directory(&companion_directory, "Vulcan companion directory")?;
        for (name, bytes) in writes {
            durable_file::replace(&companion_directory.join(name), bytes)?;
        }
    }

    Ok(ObsidianCompanionInstallReport {
        version: 1,
        dry_run: request.dry_run,
        plugin_id: COMPANION_PLUGIN_ID.to_string(),
        plugin_version,
        vault,
        plugin_directory: companion_directory,
        base_url: request.base_url.to_string(),
        wiki_id: request.wiki_id.to_string(),
        changed: !changed_files.is_empty(),
        configuration_created,
        pairing: ObsidianCompanionPairingStatus::Required,
        changed_files,
        preserved_files,
    })
}

fn file_differs(path: &Path, expected: &[u8]) -> bool {
    fs::read(path).map_or(true, |existing| existing != expected)
}

fn require_real_directory(path: &Path, label: &str) -> Result<(), AppError> {
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        AppError::operation(format!(
            "{label} {} is unavailable: {error}",
            path.display()
        ))
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(AppError::operation(format!(
            "{label} {} must be a real directory, not a symlink or file",
            path.display()
        )));
    }
    Ok(())
}

fn require_directory_or_missing(path: &Path, label: &str) -> Result<(), AppError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            Err(AppError::operation(format!(
                "{label} {} must be a real directory, not a symlink or file",
                path.display()
            )))
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(AppError::operation(format!(
            "cannot inspect {label} {}: {error}",
            path.display()
        ))),
    }
}

fn require_regular_file_or_missing(path: &Path, label: &str) -> Result<(), AppError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            Err(AppError::operation(format!(
                "{label} {} must be a regular file, not a symlink or directory",
                path.display()
            )))
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(AppError::operation(format!(
            "cannot inspect {label} {}: {error}",
            path.display()
        ))),
    }
}

fn validate_loopback_base_url(base_url: &str) -> Result<(), AppError> {
    let authority = base_url
        .strip_prefix("http://")
        .ok_or_else(|| AppError::operation("companion endpoint must use loopback HTTP"))?;
    let host = authority
        .rsplit_once(':')
        .map_or(authority, |(host, _)| host)
        .trim_matches(['[', ']']);
    if !matches!(host, "127.0.0.1" | "localhost" | "::1") {
        return Err(AppError::operation(
            "companion endpoint must use localhost, 127.0.0.1, or ::1",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn request(vault_root: &Path, dry_run: bool) -> ObsidianCompanionInstallRequest<'_> {
        ObsidianCompanionInstallRequest {
            vault_root,
            base_url: "http://127.0.0.1:3210",
            wiki_id: "personal",
            dry_run,
        }
    }

    #[test]
    fn installs_embedded_assets_and_non_secret_configuration() {
        let temporary = tempdir().expect("temporary directory");
        fs::create_dir(temporary.path().join(OBSIDIAN_DIRECTORY)).expect("Obsidian directory");

        let report = install_obsidian_companion(&request(temporary.path(), false))
            .expect("companion install");

        assert!(report.changed);
        assert!(report.configuration_created);
        assert_eq!(report.pairing, ObsidianCompanionPairingStatus::Required);
        assert_eq!(report.changed_files.len(), 4);
        for (name, expected) in COMPANION_ASSETS {
            assert_eq!(
                fs::read(report.plugin_directory.join(name)).expect("installed asset"),
                expected
            );
        }
        let data: serde_json::Value = serde_json::from_slice(
            &fs::read(report.plugin_directory.join("data.json")).expect("plugin data"),
        )
        .expect("valid plugin data");
        assert_eq!(data["baseUrl"], "http://127.0.0.1:3210");
        assert_eq!(data["wikiId"], "personal");
        assert!(data.get("token").is_none());
    }

    #[test]
    fn dry_run_reports_changes_without_creating_plugin_directory() {
        let temporary = tempdir().expect("temporary directory");
        fs::create_dir(temporary.path().join(OBSIDIAN_DIRECTORY)).expect("Obsidian directory");

        let report = install_obsidian_companion(&request(temporary.path(), true))
            .expect("companion dry run");

        assert!(report.changed);
        assert!(!report.plugin_directory.exists());
    }

    #[test]
    fn upgrades_assets_without_overwriting_plugin_configuration() {
        let temporary = tempdir().expect("temporary directory");
        let plugin_directory = temporary
            .path()
            .join(OBSIDIAN_DIRECTORY)
            .join(OBSIDIAN_PLUGINS_DIRECTORY)
            .join(COMPANION_PLUGIN_ID);
        fs::create_dir_all(&plugin_directory).expect("plugin directory");
        fs::write(plugin_directory.join("main.js"), "stale").expect("stale asset");
        fs::write(
            plugin_directory.join("data.json"),
            "{\"wikiId\":\"chosen\"}\n",
        )
        .expect("existing data");
        fs::write(plugin_directory.join("custom.txt"), "keep").expect("foreign file");

        let report = install_obsidian_companion(&request(temporary.path(), false))
            .expect("companion upgrade");

        assert!(!report.configuration_created);
        assert_eq!(report.preserved_files, vec!["data.json"]);
        assert_eq!(
            fs::read_to_string(plugin_directory.join("data.json")).expect("preserved data"),
            "{\"wikiId\":\"chosen\"}\n"
        );
        assert_eq!(
            fs::read_to_string(plugin_directory.join("custom.txt")).expect("preserved file"),
            "keep"
        );
    }

    #[test]
    fn requires_an_existing_obsidian_vault() {
        let temporary = tempdir().expect("temporary directory");
        let error = install_obsidian_companion(&request(temporary.path(), false))
            .expect_err("plain Markdown directory must be rejected");
        assert!(error
            .to_string()
            .contains("Obsidian configuration directory"));
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlinked_plugin_directory() {
        use std::os::unix::fs::symlink;

        let temporary = tempdir().expect("temporary directory");
        let outside = tempdir().expect("outside directory");
        let plugins = temporary
            .path()
            .join(OBSIDIAN_DIRECTORY)
            .join(OBSIDIAN_PLUGINS_DIRECTORY);
        fs::create_dir_all(&plugins).expect("plugins directory");
        symlink(outside.path(), plugins.join(COMPANION_PLUGIN_ID)).expect("plugin symlink");

        let error = install_obsidian_companion(&request(temporary.path(), false))
            .expect_err("symlink must be rejected");
        assert!(error.to_string().contains("must be a real directory"));
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlinked_managed_asset() {
        use std::os::unix::fs::symlink;

        let temporary = tempdir().expect("temporary directory");
        let outside = temporary.path().join("outside.js");
        fs::write(&outside, COMPANION_ASSETS[1].1).expect("outside asset");
        let plugin = temporary
            .path()
            .join(OBSIDIAN_DIRECTORY)
            .join(OBSIDIAN_PLUGINS_DIRECTORY)
            .join(COMPANION_PLUGIN_ID);
        fs::create_dir_all(&plugin).expect("plugin directory");
        symlink(&outside, plugin.join("main.js")).expect("asset symlink");

        let error = install_obsidian_companion(&request(temporary.path(), false))
            .expect_err("asset symlink must be rejected");
        assert!(error.to_string().contains("must be a regular file"));
    }
}
