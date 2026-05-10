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
struct ImportSetting {
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

fn importer_source_path(paths: &VaultPaths, relative: &str) -> PathBuf {
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

fn import_setting<T: Serialize>(
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

fn import_setting_path<T: Serialize>(
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

fn core_import_settings(paths: &VaultPaths) -> Result<Vec<ImportSetting>, ConfigImportError> {
    let app_path = importer_source_path(paths, ".obsidian/app.json");
    let templates_path = importer_source_path(paths, ".obsidian/templates.json");
    let types_path = importer_source_path(paths, ".obsidian/types.json");
    let mut settings = Vec::new();

    if app_path.exists() {
        let app = serde_json::from_str::<ObsidianAppConfig>(&fs::read_to_string(&app_path)?)?;
        if let Some(use_markdown_links) = app.use_markdown_links {
            let link_style = if use_markdown_links {
                LinkStylePreference::Markdown
            } else {
                LinkStylePreference::Wikilink
            };
            import_setting(
                &mut settings,
                "app.json.useMarkdownLinks",
                &["links", "style"],
                &link_style,
            )?;
        }
        if let Some(new_link_format) = app.new_link_format {
            import_setting(
                &mut settings,
                "app.json.newLinkFormat",
                &["links", "resolution"],
                &new_link_format,
            )?;
        }
        if let Some(attachment_folder_path) = app.attachment_folder_path {
            let normalized = normalize_attachment_folder(&attachment_folder_path);
            import_setting(
                &mut settings,
                "app.json.attachmentFolderPath",
                &["links", "attachment_folder"],
                &normalized,
            )?;
        }
        if let Some(strict_line_breaks) = app.strict_line_breaks {
            import_setting(
                &mut settings,
                "app.json.strictLineBreaks",
                &["strict_line_breaks"],
                &strict_line_breaks,
            )?;
        }
    }

    if templates_path.exists() {
        let templates =
            serde_json::from_str::<ObsidianTemplatesConfig>(&fs::read_to_string(&templates_path)?)?;
        if let Some(date_format) = templates.date_format {
            import_setting(
                &mut settings,
                "templates.json.dateFormat",
                &["templates", "date_format"],
                &date_format,
            )?;
        }
        if let Some(time_format) = templates.time_format {
            import_setting(
                &mut settings,
                "templates.json.timeFormat",
                &["templates", "time_format"],
                &time_format,
            )?;
        }
        if let Some(folder) = templates.folder {
            let normalized = normalize_template_path(Some(folder));
            import_setting(
                &mut settings,
                "templates.json.folder",
                &["templates", "obsidian_folder"],
                &normalized,
            )?;
        }
    }

    if types_path.exists() {
        for (property, value_type) in load_explicit_obsidian_property_types(&types_path)? {
            import_setting_path(
                &mut settings,
                "types.json",
                vec!["property_types".to_string(), property],
                &value_type,
            )?;
        }
    }

    Ok(settings)
}

fn imported_tasks_config(obsidian: ObsidianTasksConfig) -> TasksConfig {
    let mut config = VaultConfig::default();
    apply_obsidian_tasks_defaults(&mut config, obsidian);
    config.tasks
}

fn imported_templater_config(obsidian: ObsidianTemplaterConfig) -> TemplatesConfig {
    let mut config = VaultConfig::default();
    apply_obsidian_templater_defaults(&mut config, obsidian);
    config.templates
}

fn imported_quickadd_config(obsidian: ObsidianQuickAddConfig) -> QuickAddConfig {
    let mut config = VaultConfig::default();
    apply_obsidian_quickadd_defaults(&mut config, obsidian);
    config.quickadd
}

fn imported_dataview_config(obsidian: ObsidianDataviewConfig) -> DataviewConfig {
    let mut config = VaultConfig::default();
    apply_obsidian_dataview_defaults(&mut config, obsidian);
    config.dataview
}

fn imported_tasknotes_config(obsidian: ObsidianTaskNotesConfig) -> TaskNotesConfig {
    let mut config = VaultConfig::default();
    apply_obsidian_tasknotes_defaults(&mut config, obsidian);
    config.tasknotes
}

fn imported_kanban_config(obsidian: ObsidianKanbanConfig) -> KanbanConfig {
    let mut config = VaultConfig::default();
    apply_obsidian_kanban_defaults(&mut config, obsidian);
    config.kanban
}

fn tasks_config_import_mappings(
    config: &TasksConfig,
) -> Result<Vec<ConfigImportMapping>, ConfigImportError> {
    let status_source = "statusSettings.coreStatuses + statusSettings.customStatuses";
    Ok(vec![
        ConfigImportMapping {
            source: status_source.to_string(),
            target: "tasks.statuses.todo".to_string(),
            value: serde_json::to_value(&config.statuses.todo)?,
        },
        ConfigImportMapping {
            source: status_source.to_string(),
            target: "tasks.statuses.completed".to_string(),
            value: serde_json::to_value(&config.statuses.completed)?,
        },
        ConfigImportMapping {
            source: status_source.to_string(),
            target: "tasks.statuses.in_progress".to_string(),
            value: serde_json::to_value(&config.statuses.in_progress)?,
        },
        ConfigImportMapping {
            source: status_source.to_string(),
            target: "tasks.statuses.cancelled".to_string(),
            value: serde_json::to_value(&config.statuses.cancelled)?,
        },
        ConfigImportMapping {
            source: status_source.to_string(),
            target: "tasks.statuses.non_task".to_string(),
            value: serde_json::to_value(&config.statuses.non_task)?,
        },
        ConfigImportMapping {
            source: status_source.to_string(),
            target: "tasks.statuses.definitions".to_string(),
            value: serde_json::to_value(&config.statuses.definitions)?,
        },
        ConfigImportMapping {
            source: "globalFilter".to_string(),
            target: "tasks.global_filter".to_string(),
            value: serde_json::to_value(&config.global_filter)?,
        },
        ConfigImportMapping {
            source: "globalQuery".to_string(),
            target: "tasks.global_query".to_string(),
            value: serde_json::to_value(&config.global_query)?,
        },
        ConfigImportMapping {
            source: "removeGlobalFilter".to_string(),
            target: "tasks.remove_global_filter".to_string(),
            value: Value::Bool(config.remove_global_filter),
        },
        ConfigImportMapping {
            source: "setCreatedDate".to_string(),
            target: "tasks.set_created_date".to_string(),
            value: Value::Bool(config.set_created_date),
        },
        ConfigImportMapping {
            source: "recurrenceOnCompletion".to_string(),
            target: "tasks.recurrence_on_completion".to_string(),
            value: serde_json::to_value(&config.recurrence_on_completion)?,
        },
    ])
}

fn push_config_import_mapping<T: Serialize>(
    mappings: &mut Vec<ConfigImportMapping>,
    source: &str,
    target: &str,
    value: &T,
) -> Result<(), ConfigImportError> {
    mappings.push(ConfigImportMapping {
        source: source.to_string(),
        target: target.to_string(),
        value: serde_json::to_value(value)?,
    });
    Ok(())
}

#[allow(clippy::too_many_lines)]
fn templater_config_import_mappings(
    config: &TemplatesConfig,
) -> Result<Vec<ConfigImportMapping>, ConfigImportError> {
    let mut mappings = Vec::new();
    push_config_import_mapping(
        &mut mappings,
        "templates_folder",
        "templates.templater_folder",
        &config.templater_folder,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "command_timeout",
        "templates.command_timeout",
        &config.command_timeout,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "templates_pairs",
        "templates.templates_pairs",
        &config.templates_pairs,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "trigger_on_file_creation",
        "templates.trigger_on_file_creation",
        &config.trigger_on_file_creation,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "auto_jump_to_cursor",
        "templates.auto_jump_to_cursor",
        &config.auto_jump_to_cursor,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "enable_system_commands",
        "templates.enable_system_commands",
        &config.enable_system_commands,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "shell_path",
        "templates.shell_path",
        &config.shell_path,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "user_scripts_folder",
        "templates.user_scripts_folder",
        &config.user_scripts_folder,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "enable_folder_templates",
        "templates.enable_folder_templates",
        &config.enable_folder_templates,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "folder_templates",
        "templates.folder_templates",
        &config.folder_templates,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "enable_file_templates",
        "templates.enable_file_templates",
        &config.enable_file_templates,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "file_templates",
        "templates.file_templates",
        &config.file_templates,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "syntax_highlighting",
        "templates.syntax_highlighting",
        &config.syntax_highlighting,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "syntax_highlighting_mobile",
        "templates.syntax_highlighting_mobile",
        &config.syntax_highlighting_mobile,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "enabled_templates_hotkeys",
        "templates.enabled_templates_hotkeys",
        &config.enabled_templates_hotkeys,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "startup_templates",
        "templates.startup_templates",
        &config.startup_templates,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "intellisense_render",
        "templates.intellisense_render",
        &config.intellisense_render,
    )?;
    Ok(mappings)
}

fn quickadd_config_import_mappings(
    config: &QuickAddConfig,
) -> Result<Vec<ConfigImportMapping>, ConfigImportError> {
    let mut mappings = Vec::new();
    push_config_import_mapping(
        &mut mappings,
        "templateFolderPath",
        "quickadd.template_folder",
        &config.template_folder,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "globalVariables",
        "quickadd.global_variables",
        &config.global_variables,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "choices[type=Capture]",
        "quickadd.capture_choices",
        &config.capture_choices,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "choices[type=Template]",
        "quickadd.template_choices",
        &config.template_choices,
    )?;
    push_config_import_mapping(&mut mappings, "ai", "quickadd.ai", &config.ai)?;
    Ok(mappings)
}

fn dataview_config_import_mappings(
    config: &DataviewConfig,
) -> Result<Vec<ConfigImportMapping>, ConfigImportError> {
    let mut mappings = Vec::new();
    push_config_import_mapping(
        &mut mappings,
        "inlineQueryPrefix",
        "dataview.inline_query_prefix",
        &config.inline_query_prefix,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "inlineJsQueryPrefix",
        "dataview.inline_js_query_prefix",
        &config.inline_js_query_prefix,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "enableDataviewJs",
        "dataview.enable_dataview_js",
        &config.enable_dataview_js,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "enableInlineDataviewJs",
        "dataview.enable_inline_dataview_js",
        &config.enable_inline_dataview_js,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "taskCompletionTracking",
        "dataview.task_completion_tracking",
        &config.task_completion_tracking,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "taskCompletionUseEmojiShorthand",
        "dataview.task_completion_use_emoji_shorthand",
        &config.task_completion_use_emoji_shorthand,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "taskCompletionText",
        "dataview.task_completion_text",
        &config.task_completion_text,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "recursiveSubTaskCompletion",
        "dataview.recursive_subtask_completion",
        &config.recursive_subtask_completion,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "displayResultCount",
        "dataview.display_result_count",
        &config.display_result_count,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "defaultDateFormat",
        "dataview.default_date_format",
        &config.default_date_format,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "defaultDateTimeFormat",
        "dataview.default_datetime_format",
        &config.default_datetime_format,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "timezone",
        "dataview.timezone",
        &config.timezone,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "maxRecursiveRenderDepth",
        "dataview.max_recursive_render_depth",
        &config.max_recursive_render_depth,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "primaryColumnName",
        "dataview.primary_column_name",
        &config.primary_column_name,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "groupColumnName",
        "dataview.group_column_name",
        &config.group_column_name,
    )?;
    Ok(mappings)
}

fn quickadd_skipped_settings(raw: &Value) -> Vec<ImportSkippedSetting> {
    let Some(settings) = raw.as_object() else {
        return Vec::new();
    };

    let mut skipped = Vec::new();
    if let Some(choices) = settings.get("choices").and_then(Value::as_array) {
        for (index, choice) in choices.iter().enumerate() {
            let choice_type = choice.get("type").and_then(Value::as_str).unwrap_or("");
            let choice_name = choice
                .get("name")
                .and_then(Value::as_str)
                .filter(|name| !name.trim().is_empty())
                .or_else(|| choice.get("id").and_then(Value::as_str))
                .unwrap_or("unnamed-choice");
            let source = format!("choices[{index}] ({choice_name})");
            if choice_type.eq_ignore_ascii_case("Macro") {
                skipped.push(ImportSkippedSetting {
                    source,
                    reason: "QuickAdd Macro choices are not imported; migrate them to `vulcan run --script` or shell automation".to_string(),
                });
            } else if choice_type.eq_ignore_ascii_case("Multi") {
                skipped.push(ImportSkippedSetting {
                    source,
                    reason: "QuickAdd Multi choices are not imported; migrate them to a `vulcan run --script` orchestration flow".to_string(),
                });
            } else if !choice_type.is_empty()
                && !choice_type.eq_ignore_ascii_case("Capture")
                && !choice_type.eq_ignore_ascii_case("Template")
            {
                skipped.push(ImportSkippedSetting {
                    source,
                    reason: format!("QuickAdd choice type `{choice_type}` is not supported"),
                });
            }
        }
    }

    if let Some(providers) = settings
        .get("ai")
        .and_then(Value::as_object)
        .and_then(|ai| ai.get("providers"))
        .and_then(Value::as_array)
    {
        for (index, provider) in providers.iter().enumerate() {
            let api_key = provider.get("apiKey").and_then(Value::as_str).unwrap_or("");
            if api_key.trim().is_empty() {
                continue;
            }
            let provider_name = provider
                .get("name")
                .and_then(Value::as_str)
                .filter(|name| !name.trim().is_empty())
                .unwrap_or("provider");
            let env_name = quickadd_provider_api_key_env(
                provider_name,
                provider.get("apiKeyRef").and_then(Value::as_str),
                Some(api_key),
            )
            .unwrap_or_else(|| "PROVIDER_API_KEY".to_string());
            skipped.push(ImportSkippedSetting {
                source: format!("ai.providers[{index}].apiKey"),
                reason: format!(
                    "stored API keys are not imported; set `{env_name}` in the environment instead"
                ),
            });
        }
    }

    skipped
}

#[allow(clippy::too_many_lines)]
fn tasknotes_config_import_mappings(
    config: &TaskNotesConfig,
) -> Result<Vec<ConfigImportMapping>, ConfigImportError> {
    let mut mappings = Vec::new();
    push_config_import_mapping(
        &mut mappings,
        "tasksFolder",
        "tasknotes.tasks_folder",
        &config.tasks_folder,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "archiveFolder",
        "tasknotes.archive_folder",
        &config.archive_folder,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "taskTag",
        "tasknotes.task_tag",
        &config.task_tag,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "taskIdentificationMethod",
        "tasknotes.identification_method",
        &config.identification_method,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "taskPropertyName",
        "tasknotes.task_property_name",
        &config.task_property_name,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "taskPropertyValue",
        "tasknotes.task_property_value",
        &config.task_property_value,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "excludedFolders",
        "tasknotes.excluded_folders",
        &config.excluded_folders,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "defaultTaskStatus",
        "tasknotes.default_status",
        &config.default_status,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "defaultTaskPriority",
        "tasknotes.default_priority",
        &config.default_priority,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "fieldMapping.title",
        "tasknotes.field_mapping.title",
        &config.field_mapping.title,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "fieldMapping.status",
        "tasknotes.field_mapping.status",
        &config.field_mapping.status,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "fieldMapping.priority",
        "tasknotes.field_mapping.priority",
        &config.field_mapping.priority,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "fieldMapping.due",
        "tasknotes.field_mapping.due",
        &config.field_mapping.due,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "fieldMapping.scheduled",
        "tasknotes.field_mapping.scheduled",
        &config.field_mapping.scheduled,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "fieldMapping.contexts",
        "tasknotes.field_mapping.contexts",
        &config.field_mapping.contexts,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "fieldMapping.projects",
        "tasknotes.field_mapping.projects",
        &config.field_mapping.projects,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "fieldMapping.timeEstimate",
        "tasknotes.field_mapping.time_estimate",
        &config.field_mapping.time_estimate,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "fieldMapping.completedDate",
        "tasknotes.field_mapping.completed_date",
        &config.field_mapping.completed_date,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "fieldMapping.dateCreated",
        "tasknotes.field_mapping.date_created",
        &config.field_mapping.date_created,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "fieldMapping.dateModified",
        "tasknotes.field_mapping.date_modified",
        &config.field_mapping.date_modified,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "fieldMapping.recurrence",
        "tasknotes.field_mapping.recurrence",
        &config.field_mapping.recurrence,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "fieldMapping.recurrenceAnchor",
        "tasknotes.field_mapping.recurrence_anchor",
        &config.field_mapping.recurrence_anchor,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "fieldMapping.archiveTag",
        "tasknotes.field_mapping.archive_tag",
        &config.field_mapping.archive_tag,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "fieldMapping.timeEntries",
        "tasknotes.field_mapping.time_entries",
        &config.field_mapping.time_entries,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "fieldMapping.completeInstances",
        "tasknotes.field_mapping.complete_instances",
        &config.field_mapping.complete_instances,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "fieldMapping.skippedInstances",
        "tasknotes.field_mapping.skipped_instances",
        &config.field_mapping.skipped_instances,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "fieldMapping.blockedBy",
        "tasknotes.field_mapping.blocked_by",
        &config.field_mapping.blocked_by,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "fieldMapping.pomodoros",
        "tasknotes.field_mapping.pomodoros",
        &config.field_mapping.pomodoros,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "fieldMapping.reminders",
        "tasknotes.field_mapping.reminders",
        &config.field_mapping.reminders,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "customStatuses",
        "tasknotes.statuses",
        &config.statuses,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "customPriorities",
        "tasknotes.priorities",
        &config.priorities,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "userFields",
        "tasknotes.user_fields",
        &config.user_fields,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "enableNaturalLanguageInput",
        "tasknotes.enable_natural_language_input",
        &config.enable_natural_language_input,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "nlpDefaultToScheduled",
        "tasknotes.nlp_default_to_scheduled",
        &config.nlp_default_to_scheduled,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "nlpLanguage",
        "tasknotes.nlp_language",
        &config.nlp_language,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "nlpTriggers.triggers",
        "tasknotes.nlp_triggers",
        &config.nlp_triggers,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "pomodoroWorkDuration",
        "tasknotes.pomodoro.work_duration",
        &config.pomodoro.work_duration,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "pomodoroShortBreakDuration",
        "tasknotes.pomodoro.short_break",
        &config.pomodoro.short_break,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "pomodoroLongBreakDuration",
        "tasknotes.pomodoro.long_break",
        &config.pomodoro.long_break,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "pomodoroLongBreakInterval",
        "tasknotes.pomodoro.long_break_interval",
        &config.pomodoro.long_break_interval,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "pomodoroStorageLocation",
        "tasknotes.pomodoro.storage_location",
        &config.pomodoro.storage_location,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "taskCreationDefaults.defaultContexts",
        "tasknotes.task_creation_defaults.default_contexts",
        &config.task_creation_defaults.default_contexts,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "taskCreationDefaults.defaultTags",
        "tasknotes.task_creation_defaults.default_tags",
        &config.task_creation_defaults.default_tags,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "taskCreationDefaults.defaultProjects",
        "tasknotes.task_creation_defaults.default_projects",
        &config.task_creation_defaults.default_projects,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "taskCreationDefaults.defaultTimeEstimate",
        "tasknotes.task_creation_defaults.default_time_estimate",
        &config.task_creation_defaults.default_time_estimate,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "taskCreationDefaults.defaultDueDate",
        "tasknotes.task_creation_defaults.default_due_date",
        &config.task_creation_defaults.default_due_date,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "taskCreationDefaults.defaultScheduledDate",
        "tasknotes.task_creation_defaults.default_scheduled_date",
        &config.task_creation_defaults.default_scheduled_date,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "taskCreationDefaults.defaultRecurrence",
        "tasknotes.task_creation_defaults.default_recurrence",
        &config.task_creation_defaults.default_recurrence,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "taskCreationDefaults.defaultReminders",
        "tasknotes.task_creation_defaults.default_reminders",
        &config.task_creation_defaults.default_reminders,
    )?;
    Ok(mappings)
}

#[derive(Debug, Default)]
pub(super) struct TaskNotesViewMigrationResult {
    pub(super) source_paths: Vec<PathBuf>,
    pub(super) migrated_files: Vec<ImportMigratedFile>,
    pub(super) skipped: Vec<ImportSkippedSetting>,
}

fn tasknotes_view_target_path(command: &str) -> Option<&'static str> {
    match command {
        "open-calendar-view" => Some("TaskNotes/Views/mini-calendar-default.base"),
        "open-kanban-view" => Some("TaskNotes/Views/kanban-default.base"),
        "open-tasks-view" => Some("TaskNotes/Views/tasks-default.base"),
        "open-advanced-calendar-view" => Some("TaskNotes/Views/calendar-default.base"),
        "open-agenda-view" => Some("TaskNotes/Views/agenda-default.base"),
        "relationships" | "project-subtasks" => Some("TaskNotes/Views/relationships.base"),
        _ => None,
    }
}

fn tasknotes_command_file_mappings(raw: &Value) -> Vec<(String, String)> {
    let mut mappings = BTreeMap::from([
        (
            "open-calendar-view".to_string(),
            "TaskNotes/Views/mini-calendar-default.base".to_string(),
        ),
        (
            "open-kanban-view".to_string(),
            "TaskNotes/Views/kanban-default.base".to_string(),
        ),
        (
            "open-tasks-view".to_string(),
            "TaskNotes/Views/tasks-default.base".to_string(),
        ),
        (
            "open-advanced-calendar-view".to_string(),
            "TaskNotes/Views/calendar-default.base".to_string(),
        ),
        (
            "open-agenda-view".to_string(),
            "TaskNotes/Views/agenda-default.base".to_string(),
        ),
        (
            "relationships".to_string(),
            "TaskNotes/Views/relationships.base".to_string(),
        ),
    ]);

    if let Some(command_mapping) = raw.get("commandFileMapping").and_then(Value::as_object) {
        for (command, path) in command_mapping {
            if let Some(path) = path.as_str() {
                mappings.insert(command.clone(), path.to_string());
            }
        }
        if !command_mapping.contains_key("relationships") {
            if let Some(path) = command_mapping
                .get("project-subtasks")
                .and_then(Value::as_str)
            {
                mappings.insert("project-subtasks".to_string(), path.to_string());
            }
        }
    }

    mappings.into_iter().collect()
}

fn normalize_tasknotes_import_path(path: &str) -> Result<String, ConfigImportError> {
    normalize_relative_input_path(
        path.trim(),
        RelativePathOptions {
            expected_extension: Some("base"),
            append_extension_if_missing: true,
        },
    )
    .map_err(|error| ConfigImportError::InvalidConfig(error.to_string()))
}

fn normalize_tasknotes_import_source_type(source_type: &str) -> String {
    source_type
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|character| character.to_ascii_lowercase())
        .collect()
}

fn supports_tasknotes_import_view_type(view_type: &str) -> bool {
    matches!(
        normalize_tasknotes_import_source_type(view_type).as_str(),
        "table" | "tasknotestasklist" | "tasknoteskanban"
    )
}

fn tasknotes_base_contents_for_vulcan(source: &str, source_type: &str) -> String {
    if normalize_tasknotes_import_source_type(source_type) == "tasknotes" {
        source.to_string()
    } else {
        format!("source: tasknotes\n\n{source}")
    }
}

#[allow(clippy::too_many_lines)]
pub(super) fn tasknotes_migrate_view_files(
    paths: &VaultPaths,
    raw: &Value,
    dry_run: bool,
) -> Result<TaskNotesViewMigrationResult, ConfigImportError> {
    let mut result = TaskNotesViewMigrationResult::default();
    let mut source_paths = BTreeSet::new();
    let mut target_sources = BTreeMap::<String, String>::new();
    let explicit_commands = raw
        .get("commandFileMapping")
        .and_then(Value::as_object)
        .map(|mapping| mapping.keys().cloned().collect::<BTreeSet<_>>())
        .unwrap_or_default();

    for (command, source_path) in tasknotes_command_file_mappings(raw) {
        let Some(target_path) = tasknotes_view_target_path(&command) else {
            continue;
        };
        let source_path = match normalize_tasknotes_import_path(&source_path) {
            Ok(path) => path,
            Err(error) => {
                result.skipped.push(ImportSkippedSetting {
                    source: format!("commandFileMapping.{command}"),
                    reason: error.to_string(),
                });
                continue;
            }
        };

        if let Some(previous_source) =
            target_sources.insert(target_path.to_string(), source_path.clone())
        {
            if previous_source != source_path {
                result.skipped.push(ImportSkippedSetting {
                    source: format!("commandFileMapping.{command}"),
                    reason: format!(
                        "target `{target_path}` already maps to `{previous_source}` during import"
                    ),
                });
                continue;
            }
        }

        let source_absolute = paths.vault_root().join(&source_path);
        if !source_absolute.exists() {
            if explicit_commands.contains(&command) {
                result.skipped.push(ImportSkippedSetting {
                    source: format!("commandFileMapping.{command}"),
                    reason: format!("view file `{source_path}` was not found"),
                });
            }
            continue;
        }
        source_paths.insert(source_absolute.clone());

        let info = match inspect_base_file(paths, &source_path) {
            Ok(info) => info,
            Err(error) => {
                result.skipped.push(ImportSkippedSetting {
                    source: format!("commandFileMapping.{command}"),
                    reason: format!("view file `{source_path}` could not be parsed: {error}"),
                });
                continue;
            }
        };

        if let Some(diagnostic) = info.diagnostics.first() {
            result.skipped.push(ImportSkippedSetting {
                source: format!("commandFileMapping.{command}"),
                reason: format!(
                    "view file `{source_path}` has unsupported syntax: {}",
                    diagnostic.message
                ),
            });
            continue;
        }

        let normalized_source_type = normalize_tasknotes_import_source_type(&info.source_type);
        if !matches!(normalized_source_type.as_str(), "file" | "tasknotes") {
            result.skipped.push(ImportSkippedSetting {
                source: format!("commandFileMapping.{command}"),
                reason: format!(
                    "view file `{source_path}` uses unsupported source type `{}`",
                    info.source_type
                ),
            });
            continue;
        }
        if info.views.is_empty() {
            result.skipped.push(ImportSkippedSetting {
                source: format!("commandFileMapping.{command}"),
                reason: format!("view file `{source_path}` does not define any views"),
            });
            continue;
        }

        let unsupported_view_types = info
            .views
            .iter()
            .filter(|view| !supports_tasknotes_import_view_type(&view.view_type))
            .map(|view| view.view_type.clone())
            .collect::<BTreeSet<_>>();
        if !unsupported_view_types.is_empty() {
            result.skipped.push(ImportSkippedSetting {
                source: format!("commandFileMapping.{command}"),
                reason: format!(
                    "view file `{source_path}` uses unsupported view types: {}",
                    unsupported_view_types
                        .into_iter()
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            });
            continue;
        }

        let source_contents = fs::read_to_string(&source_absolute)?;
        let migrated_contents =
            tasknotes_base_contents_for_vulcan(&source_contents, &info.source_type);
        let target_absolute = paths.vault_root().join(target_path);
        let action = if target_absolute.exists() {
            let existing_contents = fs::read_to_string(&target_absolute)?;
            if existing_contents == migrated_contents {
                ImportMigratedFileAction::ValidateOnly
            } else if source_absolute == target_absolute {
                ImportMigratedFileAction::Copy
            } else {
                result.skipped.push(ImportSkippedSetting {
                    source: format!("commandFileMapping.{command}"),
                    reason: format!(
                        "target `{target_path}` already exists with different contents"
                    ),
                });
                continue;
            }
        } else {
            ImportMigratedFileAction::Copy
        };

        if matches!(action, ImportMigratedFileAction::Copy) && !dry_run {
            if let Some(parent) = target_absolute.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(&target_absolute, migrated_contents)?;
        }

        result.migrated_files.push(ImportMigratedFile {
            source: source_absolute,
            target: target_absolute,
            action,
        });
    }

    result.source_paths = source_paths.into_iter().collect();
    Ok(result)
}

#[allow(clippy::too_many_lines)]
pub(super) fn tasknotes_skipped_settings(raw: &Value) -> Vec<ImportSkippedSetting> {
    let Some(settings) = raw.as_object() else {
        return Vec::new();
    };

    let mut skipped = Vec::new();
    push_tasknotes_skipped_group(
        &mut skipped,
        settings,
        &["calendarViewSettings"],
        "calendar view settings are not yet supported",
    );
    push_tasknotes_skipped_group(
        &mut skipped,
        settings,
        &[
            "pomodoroAutoStartBreaks",
            "pomodoroAutoStartWork",
            "pomodoroNotifications",
            "pomodoroSoundEnabled",
            "pomodoroSoundVolume",
            "pomodoroMobileSidebar",
        ],
        "advanced pomodoro automation settings are not yet supported",
    );
    push_tasknotes_skipped_group(
        &mut skipped,
        settings,
        &[
            "moveArchivedTasks",
            "hideIdentifyingTagsInCards",
            "taskOrgFiltersCollapsed",
            "taskFilenameFormat",
            "storeTitleInFilename",
            "customFilenameTemplate",
            "enableTaskLinkOverlay",
            "disableOverlayOnAlias",
            "enableInstantTaskConvert",
            "useDefaultsOnInstantConvert",
            "uiLanguage",
            "statusSuggestionTrigger",
            "projectAutosuggest",
            "singleClickAction",
            "doubleClickAction",
            "inlineTaskConvertFolder",
            "disableNoteIndexing",
            "suggestionDebounceMs",
            "recurrenceMigrated",
            "lastSeenVersion",
            "showReleaseNotesOnUpdate",
            "showTrackedTasksInStatusBar",
            "autoStopTimeTrackingOnComplete",
            "autoStopTimeTrackingNotification",
            "showRelationships",
            "relationshipsPosition",
            "showTaskCardInNote",
            "showExpandableSubtasks",
            "subtaskChevronPosition",
            "viewsButtonAlignment",
            "hideCompletedFromOverdue",
            "enableNotifications",
            "notificationType",
            "modalFieldsConfig",
            "enableModalSplitLayout",
            "defaultVisibleProperties",
            "inlineVisibleProperties",
        ],
        "UI and editor settings are not yet supported",
    );
    push_tasknotes_skipped_group(
        &mut skipped,
        settings,
        &["icsIntegration"],
        "ICS integration settings are not yet supported",
    );
    push_tasknotes_skipped_group(
        &mut skipped,
        settings,
        &[
            "enableBases",
            "enableMdbaseSpec",
            "autoCreateDefaultBasesFiles",
        ],
        "TaskNotes Bases integration settings are not yet supported",
    );
    push_tasknotes_skipped_group(
        &mut skipped,
        settings,
        &[
            "enableAPI",
            "apiPort",
            "apiAuthToken",
            "enableMCP",
            "webhooks",
        ],
        "API and webhook settings are not yet supported",
    );
    push_tasknotes_skipped_group(
        &mut skipped,
        settings,
        &[
            "googleOAuthClientId",
            "googleOAuthClientSecret",
            "enableGoogleCalendar",
            "enabledGoogleCalendars",
            "googleCalendarSyncTokens",
            "googleCalendarExport",
        ],
        "Google Calendar integration settings are not yet supported",
    );
    push_tasknotes_skipped_group(
        &mut skipped,
        settings,
        &[
            "microsoftOAuthClientId",
            "microsoftOAuthClientSecret",
            "enableMicrosoftCalendar",
            "enabledMicrosoftCalendars",
            "microsoftCalendarSyncTokens",
        ],
        "Microsoft Calendar integration settings are not yet supported",
    );
    skipped
}

fn push_tasknotes_skipped_group(
    skipped: &mut Vec<ImportSkippedSetting>,
    settings: &serde_json::Map<String, Value>,
    keys: &[&str],
    reason: &str,
) {
    let present = keys
        .iter()
        .filter(|key| settings.contains_key(**key))
        .copied()
        .collect::<Vec<_>>();
    if present.is_empty() {
        return;
    }

    skipped.push(ImportSkippedSetting {
        source: present.join(", "),
        reason: reason.to_string(),
    });
}

#[allow(clippy::too_many_lines)]
fn kanban_config_import_mappings(
    config: &KanbanConfig,
) -> Result<Vec<ConfigImportMapping>, ConfigImportError> {
    let mut mappings = Vec::new();
    push_config_import_mapping(
        &mut mappings,
        "date-trigger",
        "kanban.date_trigger",
        &config.date_trigger,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "time-trigger",
        "kanban.time_trigger",
        &config.time_trigger,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "date-format",
        "kanban.date_format",
        &config.date_format,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "time-format",
        "kanban.time_format",
        &config.time_format,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "date-display-format",
        "kanban.date_display_format",
        &config.date_display_format,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "date-time-display-format",
        "kanban.date_time_display_format",
        &config.date_time_display_format,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "link-date-to-daily-note",
        "kanban.link_date_to_daily_note",
        &config.link_date_to_daily_note,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "metadata-keys",
        "kanban.metadata_keys",
        &config.metadata_keys,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "archive-with-date",
        "kanban.archive_with_date",
        &config.archive_with_date,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "append-archive-date",
        "kanban.append_archive_date",
        &config.append_archive_date,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "archive-date-format",
        "kanban.archive_date_format",
        &config.archive_date_format,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "archive-date-separator",
        "kanban.archive_date_separator",
        &config.archive_date_separator,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "new-card-insertion-method",
        "kanban.new_card_insertion_method",
        &config.new_card_insertion_method,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "new-line-trigger",
        "kanban.new_line_trigger",
        &config.new_line_trigger,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "new-note-folder",
        "kanban.new_note_folder",
        &config.new_note_folder,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "new-note-template",
        "kanban.new_note_template",
        &config.new_note_template,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "hide-card-count",
        "kanban.hide_card_count",
        &config.hide_card_count,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "hide-tags-in-title",
        "kanban.hide_tags_in_title",
        &config.hide_tags_in_title,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "hide-tags-display",
        "kanban.hide_tags_display",
        &config.hide_tags_display,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "inline-metadata-position",
        "kanban.inline_metadata_position",
        &config.inline_metadata_position,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "lane-width",
        "kanban.lane_width",
        &config.lane_width,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "full-list-lane-width",
        "kanban.full_list_lane_width",
        &config.full_list_lane_width,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "list-collapse",
        "kanban.list_collapse",
        &config.list_collapse,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "max-archive-size",
        "kanban.max_archive_size",
        &config.max_archive_size,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "show-checkboxes",
        "kanban.show_checkboxes",
        &config.show_checkboxes,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "move-dates",
        "kanban.move_dates",
        &config.move_dates,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "move-tags",
        "kanban.move_tags",
        &config.move_tags,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "move-task-metadata",
        "kanban.move_task_metadata",
        &config.move_task_metadata,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "show-add-list",
        "kanban.show_add_list",
        &config.show_add_list,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "show-archive-all",
        "kanban.show_archive_all",
        &config.show_archive_all,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "show-board-settings",
        "kanban.show_board_settings",
        &config.show_board_settings,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "show-relative-date",
        "kanban.show_relative_date",
        &config.show_relative_date,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "show-search",
        "kanban.show_search",
        &config.show_search,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "show-set-view",
        "kanban.show_set_view",
        &config.show_set_view,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "show-view-as-markdown",
        "kanban.show_view_as_markdown",
        &config.show_view_as_markdown,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "date-picker-week-start",
        "kanban.date_picker_week_start",
        &config.date_picker_week_start,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "table-sizing",
        "kanban.table_sizing",
        &config.table_sizing,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "tag-action",
        "kanban.tag_action",
        &config.tag_action,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "tag-colors",
        "kanban.tag_colors",
        &config.tag_colors,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "tag-sort",
        "kanban.tag_sort",
        &config.tag_sort,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "date-colors",
        "kanban.date_colors",
        &config.date_colors,
    )?;
    Ok(mappings)
}

fn periodic_daily_notes_import_mappings(
    config: &ObsidianDailyNotesConfig,
) -> Result<Vec<ConfigImportMapping>, ConfigImportError> {
    let mut mappings = Vec::new();
    push_config_import_mapping(
        &mut mappings,
        "daily-notes.folder",
        "periodic.daily.folder",
        &normalize_optional_text(config.folder.clone()).map(normalize_periodic_folder),
    )?;
    push_config_import_mapping(
        &mut mappings,
        "daily-notes.format",
        "periodic.daily.format",
        &normalize_optional_text(config.format.clone()),
    )?;
    push_config_import_mapping(
        &mut mappings,
        "daily-notes.template",
        "periodic.daily.template",
        &normalize_optional_text(config.template.clone()),
    )?;
    Ok(mappings)
}

fn periodic_plugin_import_mappings(
    config: &ObsidianPeriodicNotesConfig,
) -> Result<Vec<ConfigImportMapping>, ConfigImportError> {
    let mut mappings = Vec::new();
    push_periodic_plugin_mappings(&mut mappings, "daily", config.daily.as_ref())?;
    push_periodic_plugin_mappings(&mut mappings, "weekly", config.weekly.as_ref())?;
    push_periodic_plugin_mappings(&mut mappings, "monthly", config.monthly.as_ref())?;
    push_periodic_plugin_mappings(&mut mappings, "quarterly", config.quarterly.as_ref())?;
    push_periodic_plugin_mappings(&mut mappings, "yearly", config.yearly.as_ref())?;
    Ok(mappings)
}

fn push_periodic_plugin_mappings(
    mappings: &mut Vec<ConfigImportMapping>,
    period_type: &str,
    config: Option<&ObsidianPeriodicNoteSettings>,
) -> Result<(), ConfigImportError> {
    let Some(config) = config else {
        return Ok(());
    };

    push_config_import_mapping(
        mappings,
        &format!("{period_type}.enabled"),
        &format!("periodic.{period_type}.enabled"),
        &config.enabled,
    )?;
    push_config_import_mapping(
        mappings,
        &format!("{period_type}.folder"),
        &format!("periodic.{period_type}.folder"),
        &normalize_optional_text(config.folder.clone()).map(normalize_periodic_folder),
    )?;
    push_config_import_mapping(
        mappings,
        &format!("{period_type}.format"),
        &format!("periodic.{period_type}.format"),
        &normalize_optional_text(config.format.clone()),
    )?;
    push_config_import_mapping(
        mappings,
        &format!("{period_type}.templatePath"),
        &format!("periodic.{period_type}.template"),
        &normalize_optional_text(config.template_path.clone()),
    )?;
    push_config_import_mapping(
        mappings,
        &format!("{period_type}.startOfWeek"),
        &format!("periodic.{period_type}.start_of_week"),
        &config.start_of_week,
    )?;

    Ok(())
}

fn load_config_value(path: &Path) -> Result<toml::Value, ConfigImportError> {
    if !path.exists() {
        return Ok(toml::Value::Table(toml::map::Map::new()));
    }

    let contents = fs::read_to_string(path)?;
    if contents.trim().is_empty() {
        return Ok(toml::Value::Table(toml::map::Map::new()));
    }

    let value = toml::from_str::<toml::Value>(&contents)?;
    if value.is_table() {
        Ok(value)
    } else {
        Err(ConfigImportError::InvalidConfig(
            "expected .vulcan config file to contain a TOML table".to_string(),
        ))
    }
}
