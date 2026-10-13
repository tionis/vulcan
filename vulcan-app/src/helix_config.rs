//! Helix integration: configuration merged into Helix's own files.
//!
//! Helix has no plugin system, so Vulcan's integration is configuration:
//! `languages.toml` (the language server for Markdown) and `config.toml`
//! (keybindings). Besides printing it, this module merges it into the
//! user's files: it edits them in place (keeping comments and layout), adds
//! Vulcan to lists rather than replacing them, and records what it wrote in a
//! marker beside them, so a refresh updates only entries Vulcan owns and an
//! uninstall removes exactly those. Entries the user set or changed are left
//! alone unless forced.

use crate::AppError;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use toml_edit::{Array, DocumentMut, Item, Table, TableLike, Value};

/// The `languages.toml` entries this binary carries.
pub const LANGUAGES_TOML: &str = include_str!("../../integrations/helix/languages.toml");
/// The `config.toml` keybindings this binary carries.
pub const CONFIG_TOML: &str = include_str!("../../integrations/helix/config.toml");
/// The record of what Vulcan wrote, in Helix's config directory.
pub const MARKER_FILE: &str = ".vulcan-helix.json";
/// The copy of a file kept before Vulcan first changes it.
pub const BACKUP_SUFFIX: &str = ".vulcan-backup";

const VERSION: &str = env!("CARGO_PKG_VERSION");
const LANGUAGES: &str = "languages.toml";
const CONFIG: &str = "config.toml";
/// Helix's built-in Markdown servers and roots, kept when Vulcan first sets
/// the lists (an absent list means these defaults).
const DEFAULT_MARKDOWN_SERVERS: &[&str] = &["marksman", "markdown-oxide"];
const DEFAULT_MARKDOWN_ROOTS: &[&str] = &[".marksman.toml"];

type Env<'a> = &'a dyn Fn(&str) -> Option<OsString>;

/// The Helix configuration this binary carries, for printing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HelixConfig {
    /// Where Helix reads it: `$XDG_CONFIG_HOME/helix` or `~/.config/helix`
    /// (`%AppData%\helix` on Windows).
    pub config_dir: Option<PathBuf>,
    pub languages_toml: &'static str,
    pub config_toml: &'static str,
}

/// The Helix configuration this binary carries.
#[must_use]
pub fn helix_config() -> HelixConfig {
    HelixConfig {
        config_dir: default_config_dir(),
        languages_toml: LANGUAGES_TOML,
        config_toml: CONFIG_TOML,
    }
}

/// Helix's configuration directory.
#[must_use]
pub fn default_config_dir() -> Option<PathBuf> {
    let env = |name: &str| std::env::var_os(name).filter(|value| !value.is_empty());
    config_dir_with(&env)
}

fn config_dir_with(env: Env) -> Option<PathBuf> {
    if cfg!(windows) {
        return Some(PathBuf::from(env("APPDATA")?).join("helix"));
    }
    let config = env("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| Some(PathBuf::from(env("HOME")?).join(".config")))?;
    Some(config.join("helix"))
}

/// The state of one entry Vulcan manages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryState {
    /// Not set.
    Missing,
    /// Set as Vulcan wants it.
    Current,
    /// Set as an older Vulcan wrote it.
    Outdated,
    /// Vulcan wrote it, then it was changed.
    Modified,
    /// Vulcan wrote it, then it was removed.
    Removed,
    /// Set to something else, not by Vulcan.
    Foreign,
}

/// What a command does (or would do) with one entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryAction {
    None,
    Write,
    Update,
    Remove,
    /// Left alone; `--force` would change it.
    Skip,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HelixEntry {
    pub file: String,
    /// A dotted key, with list members in brackets
    /// (`language.markdown.language-servers[vulcan]`).
    pub key: String,
    pub state: EntryState,
    pub action: EntryAction,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HelixReport {
    pub dry_run: bool,
    pub config_dir: PathBuf,
    /// Whether Vulcan's marker is present (after the command).
    pub installed: bool,
    pub entries: Vec<HelixEntry>,
    /// A line diff per changed file.
    pub diffs: BTreeMap<String, String>,
    /// Copies made before Vulcan first changed a file.
    pub backups: Vec<PathBuf>,
}

/// What Vulcan wrote, keyed by `file:key`.
#[derive(Debug, Default, Serialize, Deserialize)]
struct Marker {
    managed_by: String,
    version: String,
    entries: BTreeMap<String, serde_json::Value>,
    /// Whether Vulcan created the Markdown `[[language]]` entry, so an
    /// uninstall may remove it when nothing else was added to it.
    #[serde(default)]
    created_markdown: bool,
}

/// One entry Vulcan wants.
enum Wanted {
    /// A value or table at a key path.
    Value {
        file: &'static str,
        path: Vec<String>,
        item: Item,
    },
    /// A member of a list in the Markdown language entry.
    Member { list: &'static str, value: String },
}

impl Wanted {
    fn file(&self) -> &'static str {
        match self {
            Self::Value { file, .. } => file,
            Self::Member { .. } => LANGUAGES,
        }
    }

    fn key(&self) -> String {
        match self {
            Self::Value { path, .. } => path.join("."),
            Self::Member { list, value } => format!("language.markdown.{list}[{value}]"),
        }
    }

    fn id(&self) -> String {
        format!("{}:{}", self.file(), self.key())
    }
}

/// Everything Vulcan wants, from the files the binary carries.
fn wanted() -> Vec<Wanted> {
    let languages = LANGUAGES_TOML
        .parse::<DocumentMut>()
        .expect("bundled languages.toml parses");
    let config = CONFIG_TOML
        .parse::<DocumentMut>()
        .expect("bundled config.toml parses");
    let mut wanted = vec![Wanted::Value {
        file: LANGUAGES,
        path: vec!["language-server".into(), "vulcan".into()],
        item: languages["language-server"]["vulcan"].clone(),
    }];
    let markdown = languages["language"]
        .as_array_of_tables()
        .and_then(|languages| languages.iter().find(|language| is_markdown(language)))
        .expect("bundled languages.toml has a Markdown entry");
    for (list, defaults) in [
        ("language-servers", DEFAULT_MARKDOWN_SERVERS),
        ("roots", DEFAULT_MARKDOWN_ROOTS),
    ] {
        for value in markdown[list].as_array().into_iter().flatten() {
            let value = value.as_str().unwrap_or_default();
            if !defaults.contains(&value) {
                wanted.push(Wanted::Member {
                    list,
                    value: value.to_string(),
                });
            }
        }
    }
    let bindings = config["keys"]["normal"]["space"]["v"]
        .as_table()
        .expect("bundled config.toml binds space v");
    for (key, item) in bindings {
        wanted.push(Wanted::Value {
            file: CONFIG,
            path: ["keys", "normal", "space", "v", key]
                .iter()
                .map(ToString::to_string)
                .collect(),
            item: item.clone(),
        });
    }
    wanted
}

fn is_markdown(table: &Table) -> bool {
    table.get("name").and_then(Item::as_str) == Some("markdown")
}

/// The user's files, parsed.
struct Files {
    dir: PathBuf,
    texts: BTreeMap<&'static str, Option<String>>,
    documents: BTreeMap<&'static str, DocumentMut>,
}

impl Files {
    fn read(dir: &Path) -> Result<Self, AppError> {
        let mut texts = BTreeMap::new();
        let mut documents = BTreeMap::new();
        for file in [LANGUAGES, CONFIG] {
            let path = dir.join(file);
            let text = match fs::read_to_string(&path) {
                Ok(text) => Some(text),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(error) => return Err(AppError::operation(error)),
            };
            let document = text
                .as_deref()
                .unwrap_or_default()
                .parse::<DocumentMut>()
                .map_err(|error| {
                    AppError::operation_with_code(
                        "invalid_request",
                        format!("{} is not valid TOML: {error}", path.display()),
                    )
                })?;
            texts.insert(file, text);
            documents.insert(file, document);
        }
        Ok(Self {
            dir: dir.to_path_buf(),
            texts,
            documents,
        })
    }

    fn document(&mut self, file: &str) -> &mut DocumentMut {
        self.documents.get_mut(file).expect("known file")
    }

    /// The Markdown `[[language]]` entry, if the user has one.
    fn markdown(&mut self) -> Result<Option<&mut Table>, AppError> {
        let document = self.document(LANGUAGES);
        match document.get_mut("language") {
            None => Ok(None),
            Some(Item::ArrayOfTables(languages)) => {
                Ok(languages.iter_mut().find(|language| is_markdown(language)))
            }
            Some(_) => Err(AppError::operation_with_code(
                "conflict",
                "languages.toml defines `language` other than as [[language]] tables; edit it by hand",
            )),
        }
    }
}

fn read_marker(dir: &Path) -> Result<Option<Marker>, AppError> {
    let Ok(text) = fs::read_to_string(dir.join(MARKER_FILE)) else {
        return Ok(None);
    };
    let marker = serde_json::from_str::<Marker>(&text).map_err(|error| {
        AppError::operation(format!(
            "unreadable {}: {error}",
            dir.join(MARKER_FILE).display()
        ))
    })?;
    Ok((marker.managed_by == "vulcan").then_some(marker))
}

/// The semantic value of a TOML item, for comparisons and the marker
/// (comments and layout aside).
fn semantic(item: &Item) -> Option<serde_json::Value> {
    let mut document = DocumentMut::new();
    document.insert("v", item.clone());
    let table = document.to_string().parse::<toml::Table>().ok()?;
    serde_json::to_value(table.get("v")?).ok()
}

fn get_path<'a>(table: &'a dyn TableLike, path: &[String]) -> Option<&'a Item> {
    let (first, rest) = path.split_first()?;
    let item = table.get(first)?;
    if rest.is_empty() {
        return Some(item);
    }
    get_path(item.as_table_like()?, rest)
}

fn member_present(markdown: Option<&Table>, list: &str, value: &str) -> bool {
    markdown
        .and_then(|markdown| markdown.get(list))
        .and_then(Item::as_array)
        .is_some_and(|array| array.iter().any(|member| member.as_str() == Some(value)))
}

/// The state of one wanted entry in the user's files.
fn state(
    files: &mut Files,
    wanted: &Wanted,
    marker: Option<&Marker>,
) -> Result<EntryState, AppError> {
    let recorded = marker.and_then(|marker| marker.entries.get(&wanted.id()));
    Ok(match wanted {
        Wanted::Value { file, path, item } => {
            let current = get_path(files.document(file).as_table(), path).and_then(semantic);
            let desired = semantic(item);
            match (current, recorded) {
                (None, None) => EntryState::Missing,
                (None, Some(_)) => EntryState::Removed,
                (Some(current), _) if Some(&current) == desired.as_ref() => EntryState::Current,
                (Some(current), Some(recorded)) if &current == recorded => EntryState::Outdated,
                (Some(_), Some(_)) => EntryState::Modified,
                (Some(_), None) => EntryState::Foreign,
            }
        }
        Wanted::Member { list, value } => {
            let present = member_present(files.markdown()?.as_deref(), list, value);
            match (present, recorded) {
                (true, _) => EntryState::Current,
                (false, None) => EntryState::Missing,
                (false, Some(_)) => EntryState::Removed,
            }
        }
    })
}

/// The comment heading what Vulcan adds to a file.
const MANAGED: &str = "managed by `vulcan editor helix install`";

/// The position after every table in the document, so new tables go last.
fn next_position(document: &DocumentMut) -> usize {
    fn highest(table: &Table) -> usize {
        let mut highest_seen = table.position().unwrap_or(0);
        for (_, item) in table {
            match item {
                Item::Table(child) => highest_seen = highest_seen.max(highest(child)),
                Item::ArrayOfTables(tables) => {
                    for child in tables {
                        highest_seen = highest_seen.max(highest(child));
                    }
                }
                _ => {}
            }
        }
        highest_seen
    }
    highest(document.as_table()) + 1
}

/// Number a new table and its subtables from `next`, parents first.
fn place(table: &mut Table, next: &mut usize) {
    table.set_position(*next);
    *next += 1;
    for (_, item) in table.iter_mut() {
        if let Item::Table(child) = item {
            place(child, next);
        }
    }
}

fn set_value(document: &mut DocumentMut, path: &[String], item: &Item) -> Result<(), AppError> {
    let mut next = next_position(document);
    let (last, parents) = path.split_last().expect("non-empty path");
    let mut table: &mut dyn TableLike = document.as_table_mut();
    for (depth, parent) in parents.iter().enumerate() {
        let created = table.get(parent).is_none();
        if created {
            let mut new = Table::new();
            new.set_implicit(true);
            table.insert(parent, Item::Table(new));
        }
        let child = table.get_mut(parent).expect("present");
        // A new table that will hold Vulcan's values (the keybindings)
        // gets a heading and goes last.
        if created && depth + 1 == parents.len() && !matches!(item, Item::Table(_)) {
            if let Some(new) = child.as_table_mut() {
                new.set_implicit(false);
                new.decor_mut()
                    .set_prefix(format!("\n# Vulcan keybindings ({MANAGED}).\n"));
                place(new, &mut next);
            }
        }
        table = child.as_table_like_mut().ok_or_else(|| {
            AppError::operation_with_code(
                "conflict",
                format!("`{parent}` is set to a value, not a table; edit it by hand"),
            )
        })?;
    }
    let mut item = item.clone();
    if let Item::Table(new) = &mut item {
        new.decor_mut().set_prefix(format!(
            "\n# Vulcan: `vulcan lsp` for Markdown notes in a vault ({MANAGED}).\n"
        ));
        place(new, &mut next);
    }
    table.insert(last, item);
    Ok(())
}

fn remove_value(document: &mut DocumentMut, path: &[String]) {
    fn remove(table: &mut dyn TableLike, path: &[String]) {
        let Some((first, rest)) = path.split_first() else {
            return;
        };
        if rest.is_empty() {
            table.remove(first);
            return;
        }
        if let Some(child) = table.get_mut(first).and_then(Item::as_table_like_mut) {
            remove(child, rest);
            if child.is_empty() {
                table.remove(first);
            }
        }
    }
    remove(document.as_table_mut(), path);
}

/// Add `value` to a Markdown list, creating the entry or the list (with
/// Helix's defaults, which an absent list means) when needed.
fn add_member(
    files: &mut Files,
    marker: &mut Marker,
    list: &str,
    value: &str,
) -> Result<(), AppError> {
    if files.markdown()?.is_none() {
        let mut entry = Table::new();
        entry.insert("name", toml_edit::value("markdown"));
        entry.decor_mut().set_prefix(format!(
            "\n# Markdown notes in a vault go to Vulcan first ({MANAGED}).\n"
        ));
        let document = files.document(LANGUAGES);
        let mut next = next_position(document);
        place(&mut entry, &mut next);
        if document.get("language").is_none() {
            document.insert(
                "language",
                Item::ArrayOfTables(toml_edit::ArrayOfTables::new()),
            );
        }
        document["language"]
            .as_array_of_tables_mut()
            .expect("checked above")
            .push(entry);
        marker.created_markdown = true;
    }
    let markdown = files.markdown()?.expect("created above");
    if markdown.get(list).is_none() {
        let defaults = if list == "roots" {
            DEFAULT_MARKDOWN_ROOTS
        } else {
            DEFAULT_MARKDOWN_SERVERS
        };
        markdown.insert(
            list,
            toml_edit::value(defaults.iter().copied().collect::<Array>()),
        );
    }
    let array = markdown
        .get_mut(list)
        .and_then(Item::as_array_mut)
        .ok_or_else(|| {
            AppError::operation_with_code(
                "conflict",
                format!("the Markdown entry's `{list}` is not a list; edit it by hand"),
            )
        })?;
    if list == "language-servers" {
        // First, so Vulcan answers for links before other Markdown servers.
        array.insert(0, value);
    } else {
        array.push(value);
    }
    array.fmt();
    Ok(())
}

fn remove_member(files: &mut Files, list: &str, value: &str) -> Result<(), AppError> {
    if let Some(array) = files
        .markdown()?
        .and_then(|markdown| markdown.get_mut(list))
        .and_then(Item::as_array_mut)
    {
        array.retain(|member| member.as_str() != Some(value));
        array.fmt();
    }
    Ok(())
}

/// What the user's Helix configuration holds of Vulcan's entries.
pub fn status(dir: &Path) -> Result<HelixReport, AppError> {
    let mut files = Files::read(dir)?;
    let marker = read_marker(dir)?;
    let mut entries = Vec::new();
    for wanted in wanted() {
        let state = state(&mut files, &wanted, marker.as_ref())?;
        entries.push(HelixEntry {
            file: wanted.file().to_string(),
            key: wanted.key(),
            state,
            action: EntryAction::None,
        });
    }
    Ok(HelixReport {
        dry_run: true,
        config_dir: dir.to_path_buf(),
        installed: marker.is_some(),
        entries,
        diffs: BTreeMap::new(),
        backups: Vec::new(),
    })
}

/// Merge Vulcan's entries into the user's Helix configuration. Entries the
/// user set, changed, or removed are left alone unless `force`.
pub fn install(dir: &Path, force: bool, dry_run: bool) -> Result<HelixReport, AppError> {
    let mut files = Files::read(dir)?;
    let previous = read_marker(dir)?;
    let mut marker = Marker {
        managed_by: "vulcan".to_string(),
        version: VERSION.to_string(),
        entries: previous
            .as_ref()
            .map(|marker| marker.entries.clone())
            .unwrap_or_default(),
        created_markdown: previous
            .as_ref()
            .is_some_and(|marker| marker.created_markdown),
    };
    let mut entries = Vec::new();
    for wanted in wanted() {
        let state = state(&mut files, &wanted, previous.as_ref())?;
        let action = match state {
            EntryState::Current => EntryAction::None,
            EntryState::Missing => EntryAction::Write,
            EntryState::Outdated => EntryAction::Update,
            EntryState::Modified | EntryState::Removed | EntryState::Foreign if force => {
                EntryAction::Update
            }
            EntryState::Modified | EntryState::Removed | EntryState::Foreign => EntryAction::Skip,
        };
        if matches!(action, EntryAction::Write | EntryAction::Update) {
            match &wanted {
                Wanted::Value { file, path, item } => {
                    set_value(files.document(file), path, item)?;
                    if let Some(value) = semantic(item) {
                        marker.entries.insert(wanted.id(), value);
                    }
                }
                Wanted::Member { list, value } => {
                    add_member(&mut files, &mut marker, list, value)?;
                    marker
                        .entries
                        .insert(wanted.id(), serde_json::Value::Bool(true));
                }
            }
        }
        entries.push(HelixEntry {
            file: wanted.file().to_string(),
            key: wanted.key(),
            state,
            action,
        });
    }
    let marker = serde_json::to_vec_pretty(&marker).map_err(AppError::operation)?;
    finish(files, entries, dry_run, Some(marker))
}

/// Remove the entries Vulcan wrote and still match what it wrote, and the
/// marker. Changed entries are left alone unless `force`.
pub fn uninstall(dir: &Path, force: bool, dry_run: bool) -> Result<HelixReport, AppError> {
    let mut files = Files::read(dir)?;
    let Some(marker) = read_marker(dir)? else {
        let mut report = status(dir)?;
        report.dry_run = dry_run;
        return Ok(report);
    };
    let mut entries = Vec::new();
    for wanted in wanted() {
        let state = state(&mut files, &wanted, Some(&marker))?;
        let recorded = marker.entries.get(&wanted.id());
        let ours = match &wanted {
            Wanted::Value { file, path, .. } => {
                let current = get_path(files.document(file).as_table(), path).and_then(semantic);
                recorded.is_some() && (current.as_ref() == recorded || force)
            }
            Wanted::Member { .. } => recorded.is_some(),
        };
        let action = match state {
            EntryState::Missing | EntryState::Removed => EntryAction::None,
            _ if ours => EntryAction::Remove,
            EntryState::Foreign => EntryAction::None,
            _ => EntryAction::Skip,
        };
        if action == EntryAction::Remove {
            match &wanted {
                Wanted::Value { file, path, .. } => remove_value(files.document(file), path),
                Wanted::Member { list, value } => remove_member(&mut files, list, value)?,
            }
        }
        entries.push(HelixEntry {
            file: wanted.file().to_string(),
            key: wanted.key(),
            state,
            action,
        });
    }
    if marker.created_markdown {
        remove_created_markdown(&mut files)?;
    }
    finish(files, entries, dry_run, None)
}

/// Drop a Markdown entry Vulcan created once it holds only Helix's defaults.
fn remove_created_markdown(files: &mut Files) -> Result<(), AppError> {
    let defaults_only = files.markdown()?.is_some_and(|markdown| {
        let is_defaults = |list: &str, defaults: &[&str]| {
            markdown.get(list).is_none_or(|item| {
                item.as_array().is_some_and(|array| {
                    array
                        .iter()
                        .filter_map(Value::as_str)
                        .eq(defaults.iter().copied())
                })
            })
        };
        markdown
            .iter()
            .all(|(key, _)| matches!(key, "name" | "roots" | "language-servers"))
            && is_defaults("roots", DEFAULT_MARKDOWN_ROOTS)
            && is_defaults("language-servers", DEFAULT_MARKDOWN_SERVERS)
    });
    if defaults_only {
        let document = files.document(LANGUAGES);
        if let Some(languages) = document
            .get_mut("language")
            .and_then(Item::as_array_of_tables_mut)
        {
            languages.retain(|language| !is_markdown(language));
            if languages.is_empty() {
                document.remove("language");
            }
        }
    }
    Ok(())
}

/// Write the changed files (with a one-time backup) and the marker, or with
/// `dry_run` only report.
fn finish(
    mut files: Files,
    entries: Vec<HelixEntry>,
    dry_run: bool,
    marker: Option<Vec<u8>>,
) -> Result<HelixReport, AppError> {
    let mut diffs = BTreeMap::new();
    let mut writes = Vec::new();
    for file in [LANGUAGES, CONFIG] {
        let before = files.texts[file].clone().unwrap_or_default();
        let mut after = files.document(file).to_string();
        if before.trim().is_empty() {
            after = after.trim_start_matches('\n').to_string();
        }
        if after != before {
            diffs.insert(file.to_string(), line_diff(file, &before, &after));
            writes.push((file, files.texts[file].is_some(), after));
        }
    }
    let mut report = HelixReport {
        dry_run,
        config_dir: files.dir.clone(),
        installed: marker.is_some(),
        entries,
        diffs,
        backups: Vec::new(),
    };
    if dry_run {
        report.installed = read_marker(&files.dir)?.is_some();
        return Ok(report);
    }
    fs::create_dir_all(&files.dir).map_err(AppError::operation)?;
    for (file, existed, contents) in writes {
        let path = files.dir.join(file);
        let backup = files.dir.join(format!("{file}{BACKUP_SUFFIX}"));
        if existed && !backup.exists() {
            fs::copy(&path, &backup).map_err(AppError::operation)?;
            report.backups.push(backup);
        }
        write_atomically(&path, contents.as_bytes())?;
    }
    let marker_path = files.dir.join(MARKER_FILE);
    match marker {
        Some(marker) => write_atomically(&marker_path, &marker)?,
        None => {
            if marker_path.exists() {
                fs::remove_file(&marker_path).map_err(AppError::operation)?;
            }
        }
    }
    Ok(report)
}

fn write_atomically(path: &Path, contents: &[u8]) -> Result<(), AppError> {
    let parent = path.parent().unwrap_or(Path::new("."));
    let temporary = parent.join(format!(
        ".{}.vulcan-tmp",
        path.file_name().unwrap_or_default().to_string_lossy()
    ));
    fs::write(&temporary, contents).map_err(AppError::operation)?;
    fs::rename(&temporary, path).map_err(AppError::operation)
}

/// A unified-style line diff with two lines of context.
fn line_diff(file: &str, before: &str, after: &str) -> String {
    let old = before.lines().collect::<Vec<_>>();
    let new = after.lines().collect::<Vec<_>>();
    // Longest common subsequence table, from the end.
    let mut lengths = vec![vec![0usize; new.len() + 1]; old.len() + 1];
    for i in (0..old.len()).rev() {
        for j in (0..new.len()).rev() {
            lengths[i][j] = if old[i] == new[j] {
                lengths[i + 1][j + 1] + 1
            } else {
                lengths[i + 1][j].max(lengths[i][j + 1])
            };
        }
    }
    let mut lines = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < old.len() || j < new.len() {
        if i < old.len() && j < new.len() && old[i] == new[j] {
            lines.push((' ', old[i]));
            i += 1;
            j += 1;
        } else if j < new.len() && (i == old.len() || lengths[i][j + 1] > lengths[i + 1][j]) {
            lines.push(('+', new[j]));
            j += 1;
        } else {
            lines.push(('-', old[i]));
            i += 1;
        }
    }
    let changed = lines
        .iter()
        .enumerate()
        .filter(|(_, (kind, _))| *kind != ' ')
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    let near = |index: usize| {
        changed
            .iter()
            .any(|&changed| index + 2 >= changed && index <= changed + 2)
    };
    let mut output = format!("--- {file}\n+++ {file}\n");
    let mut in_hunk = false;
    for (index, (kind, line)) in lines.iter().enumerate() {
        if near(index) {
            if !in_hunk {
                output.push_str("@@\n");
                in_hunk = true;
            }
            output.push(*kind);
            output.push_str(line);
            output.push('\n');
        } else {
            in_hunk = false;
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<OsString> {
        move |name: &str| {
            pairs
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| OsString::from(value))
        }
    }

    fn states(report: &HelixReport) -> Vec<(String, EntryState, EntryAction)> {
        report
            .entries
            .iter()
            .map(|entry| (entry.key.clone(), entry.state, entry.action))
            .collect()
    }

    fn effective(dir: &Path) -> (toml::Table, toml::Table) {
        let read = |file: &str| {
            fs::read_to_string(dir.join(file))
                .unwrap_or_default()
                .parse::<toml::Table>()
                .unwrap()
        };
        (read(LANGUAGES), read(CONFIG))
    }

    #[test]
    fn carries_valid_helix_configuration() {
        let languages = LANGUAGES_TOML.parse::<toml::Table>().unwrap();
        let server = &languages["language-server"]["vulcan"];
        assert_eq!(server["command"].as_str(), Some("vulcan"));
        assert_eq!(server["config"]["showDocument"].as_bool(), Some(true));
        let keys = CONFIG_TOML.parse::<toml::Table>().unwrap();
        let bindings = keys["keys"]["normal"]["space"]["v"].as_table().unwrap();
        assert!(bindings["x"]
            .as_str()
            .unwrap()
            .contains("vulcan.task.toggle"));
        assert_eq!(bindings["c"].as_str(), Some(":pipe-to vulcan inbox -"));
        if !cfg!(windows) {
            assert_eq!(
                config_dir_with(&env(&[("HOME", "/home/me")])),
                Some(PathBuf::from("/home/me/.config/helix"))
            );
            assert_eq!(
                config_dir_with(&env(&[("HOME", "/home/me"), ("XDG_CONFIG_HOME", "/cfg")])),
                Some(PathBuf::from("/cfg/helix"))
            );
        }
    }

    #[test]
    fn installs_into_an_empty_configuration_and_uninstalls_cleanly() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join("helix");

        let preview = install(&dir, false, true).unwrap();
        assert!(preview.diffs.contains_key(LANGUAGES));
        assert!(!dir.exists());

        let report = install(&dir, false, false).unwrap();
        assert!(report.backups.is_empty());
        assert!(report
            .entries
            .iter()
            .all(|entry| entry.action == EntryAction::Write));
        let (languages, config) = effective(&dir);
        assert_eq!(
            languages["language-server"]["vulcan"]["command"].as_str(),
            Some("vulcan")
        );
        let markdown = &languages["language"].as_array().unwrap()[0];
        assert_eq!(
            markdown["language-servers"].as_array().unwrap(),
            &["vulcan", "marksman", "markdown-oxide"]
                .map(|server| toml::Value::String(server.to_string()))
                .to_vec()
        );
        assert!(config["keys"]["normal"]["space"]["v"]["d"]
            .as_str()
            .is_some());
        assert!(status(&dir)
            .unwrap()
            .entries
            .iter()
            .all(|entry| entry.state == EntryState::Current));
        // Installing again changes nothing.
        assert!(install(&dir, false, false).unwrap().diffs.is_empty());

        uninstall(&dir, false, false).unwrap();
        assert_eq!(fs::read_to_string(dir.join(LANGUAGES)).unwrap().trim(), "");
        assert_eq!(fs::read_to_string(dir.join(CONFIG)).unwrap().trim(), "");
        assert!(!dir.join(MARKER_FILE).exists());
    }

    const USER_LANGUAGES: &str = r#"# My languages
[[language]]
name = "rust"
auto-format = true

[[language]]
name = "markdown"
soft-wrap.enable = true # wrap notes
language-servers = ["marksman"]
"#;

    const USER_CONFIG: &str = r#"theme = "onedark"

[keys.normal.space.v]
d = ":my-own-daily"
"#;

    #[test]
    fn merges_into_existing_configuration_without_clobbering() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path();
        fs::write(dir.join(LANGUAGES), USER_LANGUAGES).unwrap();
        fs::write(dir.join(CONFIG), USER_CONFIG).unwrap();

        let report = install(dir, false, false).unwrap();
        assert_eq!(report.backups.len(), 2);
        assert_eq!(
            fs::read_to_string(dir.join(format!("{LANGUAGES}{BACKUP_SUFFIX}"))).unwrap(),
            USER_LANGUAGES
        );
        let text = fs::read_to_string(dir.join(LANGUAGES)).unwrap();
        // Comments, other languages, and other keys survive.
        assert!(
            text.contains("# My languages") && text.contains("# wrap notes"),
            "{text}"
        );
        let (languages, config) = effective(dir);
        let markdown = languages["language"]
            .as_array()
            .unwrap()
            .iter()
            .find(|language| language["name"].as_str() == Some("markdown"))
            .unwrap();
        assert_eq!(languages["language"].as_array().unwrap().len(), 2);
        assert_eq!(
            markdown["language-servers"].as_array().unwrap(),
            &["vulcan", "marksman"]
                .map(|server| toml::Value::String(server.to_string()))
                .to_vec()
        );
        assert_eq!(
            markdown["roots"].as_array().unwrap().len(),
            1 + 3,
            "Helix's default root plus the vault markers"
        );
        assert_eq!(markdown["soft-wrap"]["enable"].as_bool(), Some(true));
        // The user's own `space v d` stays; the other bindings are added.
        let bindings = &config["keys"]["normal"]["space"]["v"];
        assert_eq!(bindings["d"].as_str(), Some(":my-own-daily"));
        assert!(bindings["x"]
            .as_str()
            .unwrap()
            .contains("vulcan.task.toggle"));
        assert_eq!(config["theme"].as_str(), Some("onedark"));
        assert!(states(&report).contains(&(
            "keys.normal.space.v.d".to_string(),
            EntryState::Foreign,
            EntryAction::Skip
        )));

        // Edits the user makes to Vulcan's entries are kept on refresh.
        let edited = fs::read_to_string(dir.join(CONFIG))
            .unwrap()
            .replace("vulcan.scan\"", "vulcan.scan {\\\"full\\\": true}\"");
        fs::write(dir.join(CONFIG), &edited).unwrap();
        let refresh = install(dir, false, false).unwrap();
        assert!(states(&refresh).contains(&(
            "keys.normal.space.v.s".to_string(),
            EntryState::Modified,
            EntryAction::Skip
        )));
        assert_eq!(fs::read_to_string(dir.join(CONFIG)).unwrap(), edited);
        // A second install keeps the first backup.
        assert!(refresh.backups.is_empty());

        // Uninstalling removes what Vulcan added and nothing else.
        let removed = uninstall(dir, false, false).unwrap();
        assert!(states(&removed).contains(&(
            "keys.normal.space.v.s".to_string(),
            EntryState::Modified,
            EntryAction::Skip
        )));
        let (languages, config) = effective(dir);
        assert!(languages.get("language-server").is_none());
        let markdown = languages["language"]
            .as_array()
            .unwrap()
            .iter()
            .find(|language| language["name"].as_str() == Some("markdown"))
            .unwrap();
        assert_eq!(
            markdown["language-servers"].as_array().unwrap(),
            &[toml::Value::String("marksman".to_string())].to_vec()
        );
        let bindings = config["keys"]["normal"]["space"]["v"].as_table().unwrap();
        assert_eq!(
            bindings.keys().collect::<Vec<_>>(),
            vec!["d", "s"],
            "the user's binding and the one they edited"
        );
    }

    #[test]
    fn force_takes_over_and_outdated_entries_update() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path();
        fs::write(dir.join(CONFIG), USER_CONFIG).unwrap();
        let report = install(dir, true, false).unwrap();
        assert!(states(&report).contains(&(
            "keys.normal.space.v.d".to_string(),
            EntryState::Foreign,
            EntryAction::Update
        )));
        let (_, config) = effective(dir);
        assert!(config["keys"]["normal"]["space"]["v"]["d"]
            .as_str()
            .unwrap()
            .contains("vulcan.periodic.open"));

        // An entry as an older Vulcan wrote it is updated without force.
        let mut marker = read_marker(dir).unwrap().unwrap();
        let old = serde_json::Value::String(":lsp-workspace-command vulcan.scan-old".to_string());
        marker
            .entries
            .insert("config.toml:keys.normal.space.v.s".to_string(), old);
        fs::write(dir.join(MARKER_FILE), serde_json::to_vec(&marker).unwrap()).unwrap();
        let text = fs::read_to_string(dir.join(CONFIG)).unwrap().replace(
            "\":lsp-workspace-command vulcan.scan\"",
            "\":lsp-workspace-command vulcan.scan-old\"",
        );
        fs::write(dir.join(CONFIG), text).unwrap();
        let refresh = install(dir, false, false).unwrap();
        assert!(states(&refresh).contains(&(
            "keys.normal.space.v.s".to_string(),
            EntryState::Outdated,
            EntryAction::Update
        )));
        assert!(status(dir)
            .unwrap()
            .entries
            .iter()
            .all(|entry| entry.state == EntryState::Current));
    }

    #[test]
    fn refuses_files_it_cannot_edit_safely() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join(LANGUAGES), "not = [valid").unwrap();
        let error = install(temp.path(), false, false).unwrap_err();
        assert!(error.to_string().contains("not valid TOML"), "{error}");
        fs::write(temp.path().join(LANGUAGES), "").unwrap();
        fs::write(
            temp.path().join(CONFIG),
            "[keys.normal.space]\nv = \":my-command\"\n",
        )
        .unwrap();
        let error = install(temp.path(), false, false).unwrap_err();
        assert!(error.to_string().contains("not a table"), "{error}");
    }

    #[test]
    fn diffs_show_changed_lines_with_context() {
        let diff = line_diff("x.toml", "a\nb\nc\nd\ne\nf\n", "a\nb\nc\nD\ne\nf\n");
        assert_eq!(diff, "--- x.toml\n+++ x.toml\n@@\n b\n c\n-d\n+D\n e\n f\n");
    }
}
