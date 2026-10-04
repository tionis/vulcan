//! Bases workflows that write notes.

use crate::notes::{json_properties_to_frontmatter, persist_note_create_with_template_effects};
use crate::templates::{
    load_named_template, load_named_template_with_guard, merge_template_frontmatter,
    parse_frontmatter_document, render_loaded_template_with_staged_creates, render_note_from_parts,
    staged_template_creates, LoadedTemplateRenderRequest, TemplateEngineKind, TemplateRunMode,
};
use crate::AppError;
use serde::Serialize;
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use vulcan_core::paths::{normalize_relative_input_path, RelativePathOptions};
use vulcan_core::{
    load_vault_config, plan_base_note_create, PermissionGuard, ProfilePermissionGuard, VaultPaths,
};

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BasesCreateReport {
    pub file: String,
    pub view_name: Option<String>,
    pub view_index: usize,
    pub dry_run: bool,
    pub path: String,
    pub folder: Option<String>,
    pub template: Option<String>,
    pub properties: BTreeMap<String, Value>,
    pub filters: Vec<String>,
}

/// Create a note from a Bases view's folder, equality filters, and template.
///
/// With a guard, the `.base` source must be readable, the template is chosen
/// among readable templates and rendered with the caller's read and write
/// authority, and the final note path (plus any template side-effect notes)
/// must be writable. The write goes through the shared note-create path, so
/// it is journaled, mdbase-routed, and dispatches plugin hooks.
pub fn apply_bases_note_create(
    paths: &VaultPaths,
    file: &str,
    view_index: usize,
    title: Option<&str>,
    dry_run: bool,
    guard: Option<&ProfilePermissionGuard>,
    quiet: bool,
) -> Result<BasesCreateReport, AppError> {
    if let Some(guard) = guard {
        let normalized = normalize_relative_input_path(
            file,
            RelativePathOptions {
                expected_extension: Some("base"),
                append_extension_if_missing: true,
            },
        )
        .map_err(AppError::operation)?;
        guard
            .check_read_path(&normalized)
            .map_err(AppError::operation)?;
    }
    let context = plan_base_note_create(paths, file, view_index).map_err(AppError::operation)?;
    let mut path = allocate_bases_note_path(paths, context.folder.as_deref(), title)?;
    let config = load_vault_config(paths).config;
    let staged_creates = staged_template_creates();

    let rendered_template = if let Some(template_name) = context.template.as_deref() {
        let loaded = match guard {
            Some(guard) => load_named_template_with_guard(paths, &config, template_name, guard)?,
            None => load_named_template(paths, &config, template_name)?,
        };
        let read_filter = guard.map(PermissionGuard::read_filter);
        let rendered = render_loaded_template_with_staged_creates(
            paths,
            &config,
            &loaded,
            &LoadedTemplateRenderRequest {
                target_path: &path,
                target_contents: None,
                engine: TemplateEngineKind::Auto,
                vars: &HashMap::new(),
                allow_mutations: !dry_run,
                run_mode: TemplateRunMode::Create,
            },
            read_filter.as_ref(),
            guard,
            Some(staged_creates.clone()),
        )?;
        path.clone_from(&rendered.target_path);
        rendered.content
    } else {
        String::new()
    };
    if let Some(guard) = guard {
        guard.check_write_path(&path).map_err(AppError::operation)?;
    }
    if paths.vault_root().join(&path).exists() {
        return Err(AppError::operation(format!(
            "destination note already exists: {path}"
        )));
    }

    let (template_frontmatter, template_body) =
        parse_frontmatter_document(&rendered_template, true).map_err(AppError::operation)?;
    let derived_frontmatter = json_properties_to_frontmatter(&context.properties)?;
    let merged_frontmatter = merge_template_frontmatter(derived_frontmatter, template_frontmatter);
    let contents = render_note_from_parts(merged_frontmatter.as_ref(), &template_body)
        .map_err(AppError::operation)?;

    if !dry_run {
        persist_note_create_with_template_effects(
            paths,
            &path,
            &contents,
            &staged_creates,
            guard.map(PermissionGuard::profile_name),
            quiet,
        )?;
    }

    Ok(BasesCreateReport {
        file: context.file,
        view_name: context.view_name,
        view_index: context.view_index,
        dry_run,
        path,
        folder: context.folder,
        template: context.template,
        properties: context.properties,
        filters: context.filters,
    })
}

fn allocate_bases_note_path(
    paths: &VaultPaths,
    folder: Option<&str>,
    title: Option<&str>,
) -> Result<String, AppError> {
    let stem = sanitize_new_note_title(title.unwrap_or("Untitled"));
    let folder_prefix = folder
        .filter(|folder| !folder.is_empty())
        .map_or_else(String::new, |folder| format!("{folder}/"));

    for index in 0.. {
        let suffix = if index == 0 {
            String::new()
        } else {
            format!(" {}", index + 1)
        };
        let candidate = format!("{folder_prefix}{stem}{suffix}.md");
        let normalized = normalize_relative_input_path(
            &candidate,
            RelativePathOptions {
                expected_extension: Some("md"),
                append_extension_if_missing: false,
            },
        )
        .map_err(AppError::operation)?;
        if !paths.vault_root().join(&normalized).exists() {
            return Ok(normalized);
        }
    }

    Err(AppError::operation("failed to allocate a note path"))
}

fn sanitize_new_note_title(title: &str) -> String {
    let trimmed = title.trim();
    let trimmed = trimmed.strip_suffix(".md").unwrap_or(trimmed);
    let sanitized = trimmed
        .chars()
        .map(|character| match character {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '-',
            _ if character.is_control() => '-',
            _ => character,
        })
        .collect::<String>();
    let sanitized = sanitized.trim().trim_matches('.').to_string();
    if sanitized.is_empty() {
        "Untitled".to_string()
    } else {
        sanitized
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;
    use vulcan_core::{resolve_permission_profile, scan_vault, ScanMode};

    fn base(folder: &str, template: Option<&str>) -> String {
        let template =
            template.map_or_else(String::new, |name| format!("create_template: {name}\n"));
        format!(
            "{template}filters:\n  - 'file.folder = \"{folder}\"'\nviews:\n  - name: Inbox\n    type: table\n    filters:\n      - 'status = todo'\n"
        )
    }

    #[test]
    fn guarded_bases_create_authorizes_base_template_and_target() {
        let temp = tempdir().unwrap();
        let root = temp.path();
        fs::create_dir_all(root.join(".vulcan/templates")).unwrap();
        fs::create_dir_all(root.join("Public/Templates")).unwrap();
        fs::create_dir_all(root.join("Private")).unwrap();
        fs::write(
            root.join(".vulcan/templates/Hidden.md"),
            "---\nsecret: hidden\n---\nHidden template.\n",
        )
        .unwrap();
        fs::write(
            root.join("Public/Templates/Visible.md"),
            "---\nowner: Visible\n---\nVisible template.\n",
        )
        .unwrap();
        fs::write(
            root.join(".vulcan/config.toml"),
            concat!(
                "[templates]\nobsidian_folder = \"Public/Templates\"\n",
                "[permissions.profiles.scoped]\n",
                "read = { allow = [\"folder:Public/**\", \"note:mdbase.yaml\"] }\n",
                "write = { allow = [\"folder:Public/Projects/**\"] }\n",
            ),
        )
        .unwrap();
        for (path, folder, template) in [
            ("Public/ok.base", "Public/Projects", Some("Visible")),
            (
                "Public/hidden-template.base",
                "Public/Projects",
                Some("Hidden"),
            ),
            ("Public/unwritable.base", "Public/Other", None),
            ("Private/private.base", "Public/Projects", None),
        ] {
            fs::write(root.join(path), base(folder, template)).unwrap();
        }
        let paths = VaultPaths::new(root);
        scan_vault(&paths, ScanMode::Full).unwrap();
        let guard = ProfilePermissionGuard::new(
            &paths,
            resolve_permission_profile(&paths, Some("scoped")).unwrap(),
        );
        let create = |file: &str, dry_run: bool| {
            apply_bases_note_create(&paths, file, 0, Some("Plan"), dry_run, Some(&guard), true)
        };

        let preview = create("Public/ok.base", true).unwrap();
        assert_eq!(preview.path, "Public/Projects/Plan.md");
        assert!(!root.join("Public/Projects/Plan.md").exists());

        let created = create("Public/ok.base", false).unwrap();
        assert_eq!(created.path, "Public/Projects/Plan.md");
        let source = fs::read_to_string(root.join(&created.path)).unwrap();
        assert!(source.contains("owner: Visible"), "{source}");
        assert!(source.contains("status: todo"), "{source}");

        for file in [
            "Public/hidden-template.base",
            "Public/unwritable.base",
            "Private/private.base",
        ] {
            let error = create(file, false).unwrap_err();
            assert!(!error.to_string().contains("hidden"), "{file}: {error}");
        }
        assert!(!root.join("Public/Other").exists());
        assert_eq!(
            fs::read_dir(root.join("Public/Projects")).unwrap().count(),
            1,
            "only the authorized create may write"
        );

        // The unguarded workflow keeps its historical behavior.
        let unguarded = apply_bases_note_create(
            &paths,
            "Public/hidden-template.base",
            0,
            None,
            false,
            None,
            true,
        )
        .unwrap();
        let source = fs::read_to_string(root.join(&unguarded.path)).unwrap();
        assert!(source.contains("Hidden template."));
    }
}
