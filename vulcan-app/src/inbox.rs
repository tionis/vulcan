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

#[cfg(test)]
mod tests {
    use super::*;

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
