use serde::{Deserialize, Serialize};
use std::path::{Component, Path};

pub const FOLDER_NAME_TOKEN: &str = "{{folder_name}}";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FolderNotePlacement {
    #[default]
    Inside,
    Outside,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FolderNotesConfig {
    pub placement: FolderNotePlacement,
    pub name: String,
}

impl Default for FolderNotesConfig {
    fn default() -> Self {
        Self {
            placement: FolderNotePlacement::Inside,
            name: FOLDER_NAME_TOKEN.to_string(),
        }
    }
}

impl FolderNotesConfig {
    pub fn validate(&self) -> Result<(), String> {
        let name = self.name.trim();
        if name.is_empty() {
            return Err("folder_notes.name must not be empty".to_string());
        }
        if Path::new(name)
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("md"))
        {
            return Err("folder_notes.name is a note stem and must not include `.md`".to_string());
        }
        if name.contains('/') || name.contains('\\') {
            return Err("folder_notes.name must not contain path separators".to_string());
        }
        if name.contains("{{") && !name.contains(FOLDER_NAME_TOKEN) {
            return Err(format!(
                "folder_notes.name only supports the {FOLDER_NAME_TOKEN} placeholder"
            ));
        }
        if name.matches(FOLDER_NAME_TOKEN).count() > 1 {
            return Err(format!(
                "folder_notes.name may contain {FOLDER_NAME_TOKEN} at most once"
            ));
        }
        if self.placement == FolderNotePlacement::Outside && !name.contains(FOLDER_NAME_TOKEN) {
            return Err(format!(
                "outside folder notes require {FOLDER_NAME_TOKEN} in folder_notes.name"
            ));
        }
        let rendered = name.replace(FOLDER_NAME_TOKEN, "folder");
        if rendered == "." || rendered == ".." || rendered.chars().any(char::is_control) {
            return Err("folder_notes.name does not produce a safe filename".to_string());
        }
        Ok(())
    }

    #[must_use]
    pub fn note_path_for_folder(&self, folder: &str) -> Option<String> {
        if self.validate().is_err() {
            return None;
        }
        let folder = normalize_relative_path(folder)?;
        if folder.is_empty() {
            return None;
        }
        let folder_name = folder.rsplit('/').next()?;
        let note_name = self.name.replace(FOLDER_NAME_TOKEN, folder_name);
        let parent = folder.rsplit_once('/').map_or("", |(parent, _)| parent);
        let note_folder = match self.placement {
            FolderNotePlacement::Inside => folder.as_str(),
            FolderNotePlacement::Outside => parent,
        };
        Some(if note_folder.is_empty() {
            format!("{note_name}.md")
        } else {
            format!("{note_folder}/{note_name}.md")
        })
    }

    #[must_use]
    pub fn folder_for_note_path(&self, note_path: &str) -> Option<String> {
        let note_path = normalize_relative_path(note_path)?;
        let note_parent = note_path.rsplit_once('/').map_or("", |(parent, _)| parent);
        let stem = note_path.rsplit('/').next()?.strip_suffix(".md")?;
        let folder = match self.placement {
            FolderNotePlacement::Inside => note_parent.to_string(),
            FolderNotePlacement::Outside => {
                let folder_name = extract_folder_name(&self.name, stem)?;
                if note_parent.is_empty() {
                    folder_name
                } else {
                    format!("{note_parent}/{folder_name}")
                }
            }
        };
        (self.note_path_for_folder(&folder).as_deref() == Some(note_path.as_str()))
            .then_some(folder)
    }
}

fn extract_folder_name(template: &str, rendered: &str) -> Option<String> {
    if let Some((prefix, suffix)) = template.split_once(FOLDER_NAME_TOKEN) {
        let value = rendered.strip_prefix(prefix)?.strip_suffix(suffix)?;
        (!value.is_empty()).then(|| value.to_string())
    } else {
        (template == rendered).then(|| rendered.to_string())
    }
}

fn normalize_relative_path(path: &str) -> Option<String> {
    let path = Path::new(path);
    if path.is_absolute() {
        return None;
    }
    let mut segments = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(segment) => segments.push(segment.to_string_lossy().to_string()),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    Some(segments.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configured_conventions_map_folders_and_notes_both_ways() {
        for (placement, name, expected) in [
            (
                FolderNotePlacement::Inside,
                "index",
                "Area/Projects/index.md",
            ),
            (
                FolderNotePlacement::Inside,
                "README",
                "Area/Projects/README.md",
            ),
            (
                FolderNotePlacement::Inside,
                FOLDER_NAME_TOKEN,
                "Area/Projects/Projects.md",
            ),
            (
                FolderNotePlacement::Outside,
                FOLDER_NAME_TOKEN,
                "Area/Projects.md",
            ),
        ] {
            let config = FolderNotesConfig {
                placement,
                name: name.to_string(),
            };
            assert_eq!(
                config.note_path_for_folder("Area/Projects").as_deref(),
                Some(expected)
            );
            assert_eq!(
                config.folder_for_note_path(expected).as_deref(),
                Some("Area/Projects")
            );
        }
    }

    #[test]
    fn matching_is_exact_and_does_not_auto_detect_other_conventions() {
        let config = FolderNotesConfig {
            placement: FolderNotePlacement::Inside,
            name: "README".to_string(),
        };
        assert_eq!(
            config.folder_for_note_path("Projects/README.md").as_deref(),
            Some("Projects")
        );
        assert_eq!(config.folder_for_note_path("Projects/readme.md"), None);
        assert_eq!(config.folder_for_note_path("Projects/Projects.md"), None);
        assert_eq!(config.folder_for_note_path("Projects/index.md"), None);
    }

    #[test]
    fn unsafe_names_are_rejected() {
        for name in [
            "",
            "index.md",
            "INDEX.MD",
            "../index",
            "{{unknown}}",
            "{{folder_name}}-{{folder_name}}",
        ] {
            assert!(FolderNotesConfig {
                placement: FolderNotePlacement::Inside,
                name: name.to_string(),
            }
            .validate()
            .is_err());
        }
    }
}
