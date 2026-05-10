#![allow(clippy::wildcard_imports)]

use super::*;
use std::error::Error;
use std::fmt::{Display, Formatter};

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ConfigImportMapping {
    pub source: String,
    pub target: String,
    pub value: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[allow(clippy::struct_excessive_bools)]
pub struct ConfigImportReport {
    pub plugin: String,
    pub source_path: PathBuf,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_paths: Vec<PathBuf>,
    pub config_path: PathBuf,
    pub target_file: PathBuf,
    pub created_config: bool,
    pub updated: bool,
    #[serde(skip)]
    pub config_updated: bool,
    #[serde(skip)]
    pub previous_contents: Option<String>,
    #[serde(skip)]
    pub rendered_contents: Option<String>,
    pub dry_run: bool,
    pub mappings: Vec<ConfigImportMapping>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub migrated_files: Vec<ImportMigratedFile>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skipped: Vec<ImportSkippedSetting>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conflicts: Vec<ImportConflict>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ImportMigratedFileAction {
    Copy,
    ValidateOnly,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ImportMigratedFile {
    pub source: PathBuf,
    pub target: PathBuf,
    pub action: ImportMigratedFileAction,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ImportSkippedSetting {
    pub source: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ImportConflict {
    pub key: String,
    pub sources: Vec<String>,
    pub kept_value: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImportTarget {
    Shared,
    Local,
}

impl ImportTarget {
    fn config_path(self, paths: &VaultPaths) -> PathBuf {
        match self {
            Self::Shared => paths.config_file().to_path_buf(),
            Self::Local => paths.local_config_file().to_path_buf(),
        }
    }
}

pub trait PluginImporter {
    fn name(&self) -> &'static str;

    fn display_name(&self) -> &'static str;

    fn source_paths(&self, paths: &VaultPaths) -> Vec<PathBuf>;

    fn detect(&self, paths: &VaultPaths) -> bool {
        self.source_paths(paths).iter().any(|path| path.exists())
    }

    fn import(
        &self,
        paths: &VaultPaths,
        target: ImportTarget,
    ) -> Result<ConfigImportReport, ConfigImportError> {
        self.import_with_mode(paths, target, false)
    }

    fn dry_run(&self, paths: &VaultPaths) -> Result<ConfigImportReport, ConfigImportError> {
        self.import_with_mode(paths, ImportTarget::Shared, true)
    }

    fn dry_run_to(
        &self,
        paths: &VaultPaths,
        target: ImportTarget,
    ) -> Result<ConfigImportReport, ConfigImportError> {
        self.import_with_mode(paths, target, true)
    }

    fn import_with_mode(
        &self,
        paths: &VaultPaths,
        target: ImportTarget,
        dry_run: bool,
    ) -> Result<ConfigImportReport, ConfigImportError>;
}

#[derive(Debug)]
pub enum ConfigImportError {
    Io(std::io::Error),
    Json(serde_json::Error),
    MissingSource(PathBuf),
    TomlDeserialize(toml::de::Error),
    TomlSerialize(toml::ser::Error),
    InvalidConfig(String),
}

impl Display for ConfigImportError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "{error}"),
            Self::Json(error) => write!(formatter, "{error}"),
            Self::MissingSource(path) => {
                write!(formatter, "missing plugin config at {}", path.display())
            }
            Self::TomlDeserialize(error) => write!(formatter, "{error}"),
            Self::TomlSerialize(error) => write!(formatter, "{error}"),
            Self::InvalidConfig(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for ConfigImportError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Json(error) => Some(error),
            Self::TomlDeserialize(error) => Some(error),
            Self::TomlSerialize(error) => Some(error),
            Self::MissingSource(_) | Self::InvalidConfig(_) => None,
        }
    }
}

impl From<std::io::Error> for ConfigImportError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<serde_json::Error> for ConfigImportError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

impl From<toml::de::Error> for ConfigImportError {
    fn from(error: toml::de::Error) -> Self {
        Self::TomlDeserialize(error)
    }
}

impl From<toml::ser::Error> for ConfigImportError {
    fn from(error: toml::ser::Error) -> Self {
        Self::TomlSerialize(error)
    }
}

#[derive(Debug, Clone)]
pub(super) struct ImportSetting {
    source: String,
    target: String,
    path: Vec<String>,
    value: Value,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct CoreImporter;

#[derive(Debug, Clone, Copy, Default)]
pub struct DataviewImporter;

#[derive(Debug, Clone, Copy, Default)]
pub struct KanbanImporter;

#[derive(Debug, Clone, Copy, Default)]
pub struct PeriodicNotesImporter;

#[derive(Debug, Clone, Copy, Default)]
pub struct QuickAddImporter;

#[derive(Debug, Clone, Copy, Default)]
pub struct TaskNotesImporter;

#[derive(Debug, Clone, Copy, Default)]
pub struct TasksImporter;

#[derive(Debug, Clone, Copy, Default)]
pub struct TemplaterImporter;

#[must_use]
pub fn all_importers() -> Vec<Box<dyn PluginImporter>> {
    vec![
        Box::new(CoreImporter),
        Box::new(DataviewImporter),
        Box::new(KanbanImporter),
        Box::new(PeriodicNotesImporter),
        Box::new(QuickAddImporter),
        Box::new(TaskNotesImporter),
        Box::new(TasksImporter),
        Box::new(TemplaterImporter),
    ]
}

pub fn import_tasks_plugin_config(
    paths: &VaultPaths,
) -> Result<ConfigImportReport, ConfigImportError> {
    TasksImporter.import(paths, ImportTarget::Shared)
}

pub fn import_tasknotes_plugin_config(
    paths: &VaultPaths,
) -> Result<ConfigImportReport, ConfigImportError> {
    TaskNotesImporter.import(paths, ImportTarget::Shared)
}

pub fn import_quickadd_plugin_config(
    paths: &VaultPaths,
) -> Result<ConfigImportReport, ConfigImportError> {
    QuickAddImporter.import(paths, ImportTarget::Shared)
}

pub fn import_templater_plugin_config(
    paths: &VaultPaths,
) -> Result<ConfigImportReport, ConfigImportError> {
    TemplaterImporter.import(paths, ImportTarget::Shared)
}

pub fn import_kanban_plugin_config(
    paths: &VaultPaths,
) -> Result<ConfigImportReport, ConfigImportError> {
    KanbanImporter.import(paths, ImportTarget::Shared)
}

pub fn import_periodic_notes_plugin_config(
    paths: &VaultPaths,
) -> Result<ConfigImportReport, ConfigImportError> {
    PeriodicNotesImporter.import(paths, ImportTarget::Shared)
}

pub fn import_core_plugin_config(
    paths: &VaultPaths,
) -> Result<ConfigImportReport, ConfigImportError> {
    CoreImporter.import(paths, ImportTarget::Shared)
}

pub fn import_dataview_plugin_config(
    paths: &VaultPaths,
) -> Result<ConfigImportReport, ConfigImportError> {
    DataviewImporter.import(paths, ImportTarget::Shared)
}

pub(super) fn importer_source_path(paths: &VaultPaths, relative: &str) -> PathBuf {
    paths.vault_root().join(relative)
}

fn import_settings_from_mappings(mappings: Vec<ConfigImportMapping>) -> Vec<ImportSetting> {
    mappings
        .into_iter()
        .map(|mapping| ImportSetting {
            source: mapping.source,
            target: mapping.target.clone(),
            path: mapping
                .target
                .split('.')
                .map(ToOwned::to_owned)
                .collect::<Vec<_>>(),
            value: mapping.value,
        })
        .collect()
}

fn import_settings_to_mappings(settings: &[ImportSetting]) -> Vec<ConfigImportMapping> {
    settings
        .iter()
        .map(|setting| ConfigImportMapping {
            source: setting.source.clone(),
            target: setting.target.clone(),
            value: setting.value.clone(),
        })
        .collect()
}

pub(super) fn import_setting<T: Serialize>(
    settings: &mut Vec<ImportSetting>,
    source: &str,
    path: &[&str],
    value: &T,
) -> Result<(), ConfigImportError> {
    import_setting_path(
        settings,
        source,
        path.iter().map(|segment| (*segment).to_string()).collect(),
        value,
    )
}

pub(super) fn import_setting_path<T: Serialize>(
    settings: &mut Vec<ImportSetting>,
    source: &str,
    path: Vec<String>,
    value: &T,
) -> Result<(), ConfigImportError> {
    settings.push(ImportSetting {
        source: source.to_string(),
        target: path.join("."),
        path,
        value: serde_json::to_value(value)?,
    });
    Ok(())
}

fn apply_import_settings(
    paths: &VaultPaths,
    plugin: &str,
    source_path: PathBuf,
    source_paths: Vec<PathBuf>,
    settings: &[ImportSetting],
    target: ImportTarget,
    dry_run: bool,
) -> Result<ConfigImportReport, ConfigImportError> {
    if !dry_run {
        ensure_vulcan_dir(paths)?;
    }

    let target_file = target.config_path(paths);
    let created_config = !target_file.exists();
    let existing_contents = fs::read_to_string(&target_file).ok();
    let mut config_value = load_config_value(&target_file)?;
    merge_import_into_toml(&mut config_value, settings)?;
    let rendered = toml::to_string_pretty(&config_value)?;
    let updated = existing_contents.as_deref() != Some(rendered.as_str());
    if updated && !dry_run {
        fs::write(&target_file, &rendered)?;
    }

    Ok(ConfigImportReport {
        plugin: plugin.to_string(),
        source_path,
        source_paths,
        config_path: target_file.clone(),
        target_file,
        created_config,
        updated,
        config_updated: updated,
        previous_contents: existing_contents,
        rendered_contents: Some(rendered),
        dry_run,
        mappings: import_settings_to_mappings(settings),
        migrated_files: Vec::new(),
        skipped: Vec::new(),
        conflicts: Vec::new(),
    })
}

fn merge_import_into_toml(
    config_value: &mut toml::Value,
    settings: &[ImportSetting],
) -> Result<(), ConfigImportError> {
    let Some(root_table) = config_value.as_table_mut() else {
        return Err(ConfigImportError::InvalidConfig(
            "expected .vulcan config to contain a TOML table".to_string(),
        ));
    };

    for setting in settings {
        merge_import_setting(root_table, &setting.path, &setting.value)?;
    }
    Ok(())
}

fn merge_import_setting(
    table: &mut toml::map::Map<String, toml::Value>,
    path: &[String],
    value: &Value,
) -> Result<(), ConfigImportError> {
    let Some((segment, rest)) = path.split_first() else {
        return Err(ConfigImportError::InvalidConfig(
            "import setting path cannot be empty".to_string(),
        ));
    };

    if rest.is_empty() {
        match json_to_toml_value(value)? {
            Some(value) => {
                table.insert(segment.clone(), value);
            }
            None => {
                table.remove(segment);
            }
        }
        return Ok(());
    }

    let entry = table
        .entry(segment.clone())
        .or_insert_with(|| toml::Value::Table(toml::map::Map::new()));
    if !entry.is_table() {
        *entry = toml::Value::Table(toml::map::Map::new());
    }
    let Some(child_table) = entry.as_table_mut() else {
        return Err(ConfigImportError::InvalidConfig(format!(
            "expected [{}] to be a TOML table",
            path[..path.len() - rest.len()].join(".")
        )));
    };

    merge_import_setting(child_table, rest, value)
}

fn json_to_toml_value(value: &Value) -> Result<Option<toml::Value>, ConfigImportError> {
    match value {
        Value::Null => Ok(None),
        Value::Bool(value) => Ok(Some(toml::Value::Boolean(*value))),
        Value::Number(number) => {
            if let Some(value) = number.as_i64() {
                Ok(Some(toml::Value::Integer(value)))
            } else if let Some(value) = number.as_u64() {
                let integer = i64::try_from(value).map_err(|_| {
                    ConfigImportError::InvalidConfig(
                        "numeric config import value does not fit in signed 64-bit TOML integer"
                            .to_string(),
                    )
                })?;
                Ok(Some(toml::Value::Integer(integer)))
            } else if let Some(value) = number.as_f64() {
                Ok(Some(toml::Value::Float(value)))
            } else {
                Err(ConfigImportError::InvalidConfig(
                    "unsupported numeric config import value".to_string(),
                ))
            }
        }
        Value::String(text) => Ok(Some(toml::Value::String(text.clone()))),
        Value::Array(values) => {
            let mut items = Vec::new();
            for value in values {
                if let Some(value) = json_to_toml_value(value)? {
                    items.push(value);
                }
            }
            Ok(Some(toml::Value::Array(items)))
        }
        Value::Object(entries) => {
            let mut table = toml::map::Map::new();
            for (key, value) in entries {
                if let Some(value) = json_to_toml_value(value)? {
                    table.insert(key.clone(), value);
                }
            }
            Ok(Some(toml::Value::Table(table)))
        }
    }
}

pub fn annotate_import_conflicts(reports: &mut [ConfigImportReport]) {
    let mut previous_sources = BTreeMap::<String, Vec<String>>::new();

    for report in reports {
        report.conflicts.clear();
        for mapping in &report.mappings {
            if let Some(sources) = previous_sources.get_mut(&mapping.target) {
                if !sources.iter().any(|source| source == &report.plugin) {
                    sources.push(report.plugin.clone());
                }
                report.conflicts.push(ImportConflict {
                    key: mapping.target.clone(),
                    sources: sources.clone(),
                    kept_value: mapping.value.clone(),
                });
            } else {
                previous_sources.insert(mapping.target.clone(), vec![report.plugin.clone()]);
            }
        }
    }
}

impl PluginImporter for TasksImporter {
    fn name(&self) -> &'static str {
        "tasks"
    }

    fn display_name(&self) -> &'static str {
        "Obsidian Tasks plugin"
    }

    fn source_paths(&self, paths: &VaultPaths) -> Vec<PathBuf> {
        vec![importer_source_path(
            paths,
            ".obsidian/plugins/obsidian-tasks-plugin/data.json",
        )]
    }

    fn import_with_mode(
        &self,
        paths: &VaultPaths,
        target: ImportTarget,
        dry_run: bool,
    ) -> Result<ConfigImportReport, ConfigImportError> {
        let source_path = self
            .source_paths(paths)
            .into_iter()
            .next()
            .expect("source path");
        if !source_path.exists() {
            return Err(ConfigImportError::MissingSource(source_path));
        }

        let obsidian =
            serde_json::from_str::<ObsidianTasksConfig>(&fs::read_to_string(&source_path)?)?;
        let imported_tasks = imported_tasks_config(obsidian);
        let settings =
            import_settings_from_mappings(tasks_config_import_mappings(&imported_tasks)?);
        apply_import_settings(
            paths,
            self.name(),
            source_path.clone(),
            vec![source_path],
            &settings,
            target,
            dry_run,
        )
    }
}

impl PluginImporter for TemplaterImporter {
    fn name(&self) -> &'static str {
        "templater"
    }

    fn display_name(&self) -> &'static str {
        "Obsidian Templater plugin"
    }

    fn source_paths(&self, paths: &VaultPaths) -> Vec<PathBuf> {
        vec![importer_source_path(
            paths,
            ".obsidian/plugins/templater-obsidian/data.json",
        )]
    }

    fn import_with_mode(
        &self,
        paths: &VaultPaths,
        target: ImportTarget,
        dry_run: bool,
    ) -> Result<ConfigImportReport, ConfigImportError> {
        let source_path = self
            .source_paths(paths)
            .into_iter()
            .next()
            .expect("source path");
        if !source_path.exists() {
            return Err(ConfigImportError::MissingSource(source_path));
        }

        let obsidian =
            serde_json::from_str::<ObsidianTemplaterConfig>(&fs::read_to_string(&source_path)?)?;
        let imported_templates = imported_templater_config(obsidian);
        let settings =
            import_settings_from_mappings(templater_config_import_mappings(&imported_templates)?);
        apply_import_settings(
            paths,
            self.name(),
            source_path.clone(),
            vec![source_path],
            &settings,
            target,
            dry_run,
        )
    }
}

impl PluginImporter for QuickAddImporter {
    fn name(&self) -> &'static str {
        "quickadd"
    }

    fn display_name(&self) -> &'static str {
        "Obsidian QuickAdd plugin"
    }

    fn source_paths(&self, paths: &VaultPaths) -> Vec<PathBuf> {
        vec![importer_source_path(
            paths,
            ".obsidian/plugins/quickadd/data.json",
        )]
    }

    fn import_with_mode(
        &self,
        paths: &VaultPaths,
        target: ImportTarget,
        dry_run: bool,
    ) -> Result<ConfigImportReport, ConfigImportError> {
        let source_path = self
            .source_paths(paths)
            .into_iter()
            .next()
            .expect("source path");
        if !source_path.exists() {
            return Err(ConfigImportError::MissingSource(source_path));
        }

        let source = fs::read_to_string(&source_path)?;
        let raw = serde_json::from_str::<Value>(&source)?;
        let obsidian = serde_json::from_value::<ObsidianQuickAddConfig>(raw.clone())?;
        let imported_quickadd = imported_quickadd_config(obsidian);
        let settings =
            import_settings_from_mappings(quickadd_config_import_mappings(&imported_quickadd)?);
        let mut report = apply_import_settings(
            paths,
            self.name(),
            source_path.clone(),
            vec![source_path],
            &settings,
            target,
            dry_run,
        )?;
        report.skipped = quickadd_skipped_settings(&raw);
        Ok(report)
    }
}

impl PluginImporter for KanbanImporter {
    fn name(&self) -> &'static str {
        "kanban"
    }

    fn display_name(&self) -> &'static str {
        "Obsidian Kanban plugin"
    }

    fn source_paths(&self, paths: &VaultPaths) -> Vec<PathBuf> {
        vec![importer_source_path(
            paths,
            ".obsidian/plugins/obsidian-kanban/data.json",
        )]
    }

    fn import_with_mode(
        &self,
        paths: &VaultPaths,
        target: ImportTarget,
        dry_run: bool,
    ) -> Result<ConfigImportReport, ConfigImportError> {
        let source_path = self
            .source_paths(paths)
            .into_iter()
            .next()
            .expect("source path");
        if !source_path.exists() {
            return Err(ConfigImportError::MissingSource(source_path));
        }

        let obsidian =
            serde_json::from_str::<ObsidianKanbanConfig>(&fs::read_to_string(&source_path)?)?;
        let imported_kanban = imported_kanban_config(obsidian);
        let settings =
            import_settings_from_mappings(kanban_config_import_mappings(&imported_kanban)?);
        apply_import_settings(
            paths,
            self.name(),
            source_path.clone(),
            vec![source_path],
            &settings,
            target,
            dry_run,
        )
    }
}

impl PluginImporter for PeriodicNotesImporter {
    fn name(&self) -> &'static str {
        "periodic-notes"
    }

    fn display_name(&self) -> &'static str {
        "Obsidian Daily Notes and Periodic Notes"
    }

    fn source_paths(&self, paths: &VaultPaths) -> Vec<PathBuf> {
        vec![
            importer_source_path(paths, ".obsidian/daily-notes.json"),
            importer_source_path(paths, ".obsidian/plugins/periodic-notes/data.json"),
        ]
    }

    fn import_with_mode(
        &self,
        paths: &VaultPaths,
        target: ImportTarget,
        dry_run: bool,
    ) -> Result<ConfigImportReport, ConfigImportError> {
        let source_paths = self
            .source_paths(paths)
            .into_iter()
            .filter(|path| path.exists())
            .collect::<Vec<_>>();
        if source_paths.is_empty() {
            return Err(ConfigImportError::MissingSource(importer_source_path(
                paths,
                ".obsidian/plugins/periodic-notes/data.json",
            )));
        }

        let mut mappings = Vec::new();
        let daily_path = importer_source_path(paths, ".obsidian/daily-notes.json");
        if daily_path.exists() {
            let daily = serde_json::from_str::<ObsidianDailyNotesConfig>(&fs::read_to_string(
                &daily_path,
            )?)?;
            mappings.extend(periodic_daily_notes_import_mappings(&daily)?);
        }

        let periodic_path =
            importer_source_path(paths, ".obsidian/plugins/periodic-notes/data.json");
        if periodic_path.exists() {
            let periodic = serde_json::from_str::<ObsidianPeriodicNotesConfig>(
                &fs::read_to_string(&periodic_path)?,
            )?;
            mappings.extend(periodic_plugin_import_mappings(&periodic)?);
        }

        let settings = import_settings_from_mappings(mappings);
        apply_import_settings(
            paths,
            self.name(),
            source_paths[0].clone(),
            source_paths,
            &settings,
            target,
            dry_run,
        )
    }
}

impl PluginImporter for TaskNotesImporter {
    fn name(&self) -> &'static str {
        "tasknotes"
    }

    fn display_name(&self) -> &'static str {
        "Obsidian TaskNotes plugin"
    }

    fn source_paths(&self, paths: &VaultPaths) -> Vec<PathBuf> {
        vec![importer_source_path(
            paths,
            ".obsidian/plugins/tasknotes/data.json",
        )]
    }

    fn import_with_mode(
        &self,
        paths: &VaultPaths,
        target: ImportTarget,
        dry_run: bool,
    ) -> Result<ConfigImportReport, ConfigImportError> {
        let source_path = self
            .source_paths(paths)
            .into_iter()
            .next()
            .expect("source path");
        if !source_path.exists() {
            return Err(ConfigImportError::MissingSource(source_path));
        }

        let source = fs::read_to_string(&source_path)?;
        let raw = serde_json::from_str::<Value>(&source)?;
        let obsidian = serde_json::from_value::<ObsidianTaskNotesConfig>(raw.clone())?;
        let imported_tasknotes = imported_tasknotes_config(obsidian);
        let settings =
            import_settings_from_mappings(tasknotes_config_import_mappings(&imported_tasknotes)?);
        let mut report = apply_import_settings(
            paths,
            self.name(),
            source_path.clone(),
            vec![source_path],
            &settings,
            target,
            dry_run,
        )?;
        let migration = tasknotes_migrate_view_files(paths, &raw, dry_run)?;
        report.source_paths.extend(migration.source_paths);
        report.source_paths.sort();
        report.source_paths.dedup();
        report.migrated_files = migration.migrated_files;
        report.skipped = tasknotes_skipped_settings(&raw);
        report.skipped.extend(migration.skipped);
        if report
            .migrated_files
            .iter()
            .any(|file| matches!(file.action, ImportMigratedFileAction::Copy))
        {
            report.updated = true;
        }
        Ok(report)
    }
}

impl PluginImporter for CoreImporter {
    fn name(&self) -> &'static str {
        "core"
    }

    fn display_name(&self) -> &'static str {
        "Obsidian core settings"
    }

    fn source_paths(&self, paths: &VaultPaths) -> Vec<PathBuf> {
        vec![
            importer_source_path(paths, ".obsidian/app.json"),
            importer_source_path(paths, ".obsidian/templates.json"),
            importer_source_path(paths, ".obsidian/types.json"),
        ]
    }

    fn import_with_mode(
        &self,
        paths: &VaultPaths,
        target: ImportTarget,
        dry_run: bool,
    ) -> Result<ConfigImportReport, ConfigImportError> {
        let source_root = paths.vault_root().join(".obsidian");
        let source_paths = self
            .source_paths(paths)
            .into_iter()
            .filter(|path| path.exists())
            .collect::<Vec<_>>();
        if source_paths.is_empty() {
            return Err(ConfigImportError::MissingSource(source_root));
        }

        let settings = core_import_settings(paths)?;
        apply_import_settings(
            paths,
            self.name(),
            paths.vault_root().join(".obsidian"),
            source_paths,
            &settings,
            target,
            dry_run,
        )
    }
}

impl PluginImporter for DataviewImporter {
    fn name(&self) -> &'static str {
        "dataview"
    }

    fn display_name(&self) -> &'static str {
        "Obsidian Dataview plugin"
    }

    fn source_paths(&self, paths: &VaultPaths) -> Vec<PathBuf> {
        vec![importer_source_path(
            paths,
            ".obsidian/plugins/dataview/data.json",
        )]
    }

    fn import_with_mode(
        &self,
        paths: &VaultPaths,
        target: ImportTarget,
        dry_run: bool,
    ) -> Result<ConfigImportReport, ConfigImportError> {
        let source_path = self
            .source_paths(paths)
            .into_iter()
            .next()
            .expect("source path");
        if !source_path.exists() {
            return Err(ConfigImportError::MissingSource(source_path));
        }

        let obsidian =
            serde_json::from_str::<ObsidianDataviewConfig>(&fs::read_to_string(&source_path)?)?;
        let imported_dataview = imported_dataview_config(obsidian);
        let settings =
            import_settings_from_mappings(dataview_config_import_mappings(&imported_dataview)?);
        apply_import_settings(
            paths,
            self.name(),
            source_path.clone(),
            vec![source_path],
            &settings,
            target,
            dry_run,
        )
    }
}
