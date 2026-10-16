//! Quick capture into the configured inbox note.

use crate::templates::{template_variables_for_path, TemplateTimestamp, TemplateVariables};
use crate::AppError;
use vulcan_core::paths::{normalize_relative_input_path, RelativePathOptions};
use vulcan_core::{InboxConfig, VaultPaths};

/// The inbox note's vault-relative path (`inbox.path`, `.md` added).
pub fn inbox_path(config: &InboxConfig) -> Result<String, AppError> {
    normalize_relative_input_path(
        &config.path,
        RelativePathOptions {
            expected_extension: Some("md"),
            append_extension_if_missing: true,
        },
    )
    .map_err(AppError::operation)
}

/// Fill `inbox.format` placeholders (`{text}`, `{date}`, `{time}`,
/// `{datetime}`).
#[must_use]
pub fn render_inbox_entry(format: &str, text: &str, variables: &TemplateVariables) -> String {
    format
        .replace("{text}", text.trim_end())
        .replace("{date}", &variables.date)
        .replace("{time}", &variables.time)
        .replace("{datetime}", &variables.datetime)
}

/// The inbox note's path and its text after capturing `text` into
/// `current` (the note's current text, empty when it does not exist yet):
/// the entry is formatted, timestamped when configured, and appended under
/// `inbox.heading` or at the end.
pub fn plan_inbox_capture(
    paths: &VaultPaths,
    current: &str,
    text: &str,
) -> Result<(String, String), AppError> {
    let config = vulcan_core::load_vault_config(paths).config.inbox;
    let path = inbox_path(&config)?;
    let variables = template_variables_for_path(&path, TemplateTimestamp::current());
    let rendered = render_inbox_entry(&config.format, text, &variables);
    let entry = if config.timestamp {
        format!("{} {}", variables.datetime, rendered)
    } else {
        rendered
    };
    let updated = match config.heading.as_deref() {
        Some(heading) => crate::notes::append_entry_under_heading(current, heading, &entry),
        None => crate::notes::append_entry_at_end(current, &entry),
    };
    Ok((path, updated))
}

/// Captures `text` into the configured inbox note, creating it when it does
/// not exist, and returns the note's vault-relative path. The caller checks
/// write access, refreshes the cache, and auto-commits.
pub fn write_inbox_capture(
    paths: &VaultPaths,
    text: &str,
    permission_profile: Option<&str>,
    verbosity: vulcan_core::Verbosity,
) -> Result<String, AppError> {
    let relative_path = inbox_path(&vulcan_core::load_vault_config(paths).config.inbox)?;
    let absolute_path = paths.vault_root().join(&relative_path);
    if let Some(parent) = absolute_path.parent() {
        std::fs::create_dir_all(parent).map_err(AppError::operation)?;
    }
    let existing = crate::notes::read_note_for_update(paths, &relative_path)?;
    let (_, updated) = plan_inbox_capture(paths, existing.as_deref().unwrap_or_default(), text)?;
    crate::notes::write_note_content(
        paths,
        &relative_path,
        existing.as_deref(),
        &updated,
        "inbox",
        permission_profile,
        verbosity,
    )?;
    Ok(relative_path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writing_a_capture_creates_the_inbox_note_and_appends_to_it() {
        let dir = tempfile::tempdir().unwrap();
        let paths = VaultPaths::new(dir.path());
        vulcan_core::paths::initialize_vulcan_dir(&paths).unwrap();
        std::fs::write(
            paths.config_file(),
            "[inbox]\npath = \"Capture/Inbox\"\nformat = \"- {text}\"\ntimestamp = false\n",
        )
        .unwrap();
        let path =
            write_inbox_capture(&paths, "first", None, vulcan_core::Verbosity::Quiet).unwrap();
        assert_eq!(path, "Capture/Inbox.md");
        write_inbox_capture(&paths, "second", None, vulcan_core::Verbosity::Quiet).unwrap();
        let text = std::fs::read_to_string(dir.path().join("Capture/Inbox.md")).unwrap();
        assert!(
            text.contains("- first") && text.contains("- second"),
            "{text}"
        );
        assert!(text.find("- first") < text.find("- second"));
    }

    #[test]
    fn captures_under_the_configured_heading() {
        let dir = tempfile::tempdir().unwrap();
        let paths = VaultPaths::new(dir.path());
        vulcan_core::paths::initialize_vulcan_dir(&paths).unwrap();
        std::fs::write(
            paths.config_file(),
            "[inbox]\npath = \"Capture/Inbox\"\nformat = \"- [ ] {text}\"\ntimestamp = false\nheading = \"## Todo\"\n",
        )
        .unwrap();
        let (path, updated) = plan_inbox_capture(
            &paths,
            "# Inbox\n\n## Todo\n\n- [ ] old\n\n## Later\n",
            "new idea\n",
        )
        .unwrap();
        assert_eq!(path, "Capture/Inbox.md");
        assert_eq!(
            updated,
            "# Inbox\n\n## Todo\n\n- [ ] old\n\n- [ ] new idea\n\n## Later\n"
        );
        let (_, created) = plan_inbox_capture(&paths, "", "first").unwrap();
        assert_eq!(created, "## Todo\n\n- [ ] first\n");
    }
}
