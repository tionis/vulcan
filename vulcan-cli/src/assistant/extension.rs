use crate::assistant::AssistantHostOptions;
use crate::cli::McpToolPackArg;
use crate::CliError;
use serde::Serialize;
use std::fs;
use std::path::{Path, PathBuf};

const EXTENSION_INDEX: &str = include_str!("extension/vulcan-tools/index.ts");
const EXTENSION_PACKAGE: &str = include_str!("extension/vulcan-tools/package.json");

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct AssistantExtensionInstall {
    pub(crate) root: PathBuf,
    pub(crate) entrypoint: PathBuf,
    pub(crate) files: Vec<PathBuf>,
}

pub(crate) fn materialize_extension(
    vault_root: &Path,
) -> Result<AssistantExtensionInstall, CliError> {
    let root = vault_root.join(".vulcan/assistant/extension/vulcan-tools");
    fs::create_dir_all(&root).map_err(CliError::operation)?;
    let files = vec![root.join("index.ts"), root.join("package.json")];
    write_if_changed(&files[0], EXTENSION_INDEX)?;
    write_if_changed(&files[1], EXTENSION_PACKAGE)?;
    Ok(AssistantExtensionInstall {
        root,
        entrypoint: files[0].clone(),
        files,
    })
}

pub(crate) fn extension_environment(
    options: &AssistantHostOptions,
    vault_root: &Path,
    tool_packs: &[McpToolPackArg],
) -> Vec<(String, String)> {
    vec![
        (
            "VULCAN_VAULT_ROOT".to_string(),
            vault_root.display().to_string(),
        ),
        (
            "VULCAN_ASSISTANT_PERMISSIONS".to_string(),
            options
                .permission_profile
                .clone()
                .unwrap_or_else(|| "readonly".to_string()),
        ),
        (
            "VULCAN_ASSISTANT_TOOL_PACKS".to_string(),
            tool_packs
                .iter()
                .map(|pack| pack.as_str())
                .collect::<Vec<_>>()
                .join(","),
        ),
    ]
}

fn write_if_changed(path: &Path, contents: &str) -> Result<(), CliError> {
    if path.exists() && fs::read_to_string(path).is_ok_and(|current| current == contents) {
        return Ok(());
    }
    fs::write(path, contents).map_err(CliError::operation)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assistant::AssistantHostOptions;
    use tempfile::TempDir;

    #[test]
    fn materialize_extension_writes_package_and_entrypoint() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let install = materialize_extension(temp_dir.path()).expect("extension should materialize");

        assert!(install.entrypoint.ends_with("index.ts"));
        assert!(install.entrypoint.exists());
        assert!(install.root.join("package.json").exists());
        assert!(fs::read_to_string(&install.entrypoint)
            .expect("entrypoint should be readable")
            .contains("registerVulcanExtension"));
    }

    #[test]
    fn extension_environment_includes_profile_and_tool_packs() {
        let options = AssistantHostOptions {
            runtime: "pi".to_string(),
            pi_binary: "pi".to_string(),
            provider: None,
            model: None,
            thinking_level: None,
            permission_profile: Some("readonly".to_string()),
            sessions_dir: None,
            no_tools: false,
            extension_entrypoint: None,
            extension_env: Vec::new(),
            resume_session: None,
            session_export: "manual".to_string(),
            session_exports_dir: Some(PathBuf::from("AI/Assistant Sessions")),
        };
        let env = extension_environment(
            &options,
            Path::new("/vault"),
            &[McpToolPackArg::NotesRead, McpToolPackArg::Status],
        );

        assert!(env.contains(&("VULCAN_VAULT_ROOT".to_string(), "/vault".to_string())));
        assert!(env.contains(&(
            "VULCAN_ASSISTANT_PERMISSIONS".to_string(),
            "readonly".to_string()
        )));
        assert!(env.contains(&(
            "VULCAN_ASSISTANT_TOOL_PACKS".to_string(),
            "notes-read,status".to_string()
        )));
    }

    #[test]
    fn bundled_extension_enforces_profile_boundary() {
        assert!(EXTENSION_INDEX.contains("VULCAN_ASSISTANT_PERMISSIONS"));
        assert!(EXTENSION_INDEX.contains("--permissions"));
        assert!(EXTENSION_INDEX.contains("readonly"));
        assert!(EXTENSION_INDEX.contains("bash"));
        assert!(EXTENSION_INDEX.contains("edit"));
        assert!(EXTENSION_INDEX.contains("write"));
    }
}
