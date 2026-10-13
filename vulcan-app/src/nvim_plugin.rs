//! Install the bundled `vulcan.nvim` plugin.
//!
//! The plugin's files are built into the binary, so an installed copy always
//! matches the server it talks to. An install writes them (plus generated
//! help tags) into a directory, by default Neovim's own package path, where
//! Neovim loads the plugin without a plugin manager. A marker file records
//! the version and each file's hash: a refresh replaces only a copy Vulcan
//! wrote and nobody edited since, and removes files a newer bundle dropped.

use crate::AppError;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

/// The marker an installed copy carries, in its root.
pub const MARKER_FILE: &str = ".vulcan-managed.json";

const VERSION: &str = env!("CARGO_PKG_VERSION");

macro_rules! bundled {
    ($($path:literal),* $(,)?) => {
        &[$(($path, include_str!(concat!("../../integrations/vulcan.nvim/", $path)))),*]
    };
}

/// The plugin's files, by path relative to its root. The smoke test under
/// `test/` is not installed.
pub const BUNDLED_FILES: &[(&str, &str)] = bundled![
    "README.md",
    "doc/vulcan.txt",
    "lua/vulcan/cli.lua",
    "lua/vulcan/commands.lua",
    "lua/vulcan/health.lua",
    "lua/vulcan/init.lua",
    "lua/vulcan/pickers.lua",
    "lua/vulcan/render.lua",
    "plugin/vulcan.lua",
];

/// What is at an install location.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NvimPluginState {
    /// Nothing is installed there.
    Missing,
    /// Vulcan installed this bundle there.
    Current,
    /// Vulcan installed another version (or build) there.
    Outdated,
    /// Vulcan installed it, but files were edited or deleted since.
    Modified,
    /// The directory exists but Vulcan did not install it (a checkout or a
    /// plugin manager's copy).
    Unmanaged,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NvimPluginStatus {
    pub path: PathBuf,
    pub state: NvimPluginState,
    /// The version this binary bundles.
    pub version: String,
    /// The version the installed copy came from, when Vulcan installed it.
    pub installed_version: Option<String>,
    /// Installed files that differ from what Vulcan wrote.
    pub modified_files: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NvimPluginReport {
    pub dry_run: bool,
    pub path: PathBuf,
    /// The state before the change.
    pub previous: NvimPluginState,
    /// Files written (created or replaced), relative to `path`.
    pub written: Vec<String>,
    /// Files removed, relative to `path`.
    pub removed: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct Marker {
    managed_by: String,
    version: String,
    /// BLAKE3 hash of each installed file.
    files: BTreeMap<String, String>,
}

/// Neovim's package directory for the plugin: `stdpath('data')` (honoring
/// `NVIM_APPNAME` and `XDG_DATA_HOME`) plus `site/pack/vulcan/start/vulcan.nvim`.
#[must_use]
pub fn default_install_dir() -> Option<PathBuf> {
    default_install_dir_with(|name| std::env::var_os(name).filter(|value| !value.is_empty()))
}

fn default_install_dir_with(env: impl Fn(&str) -> Option<std::ffi::OsString>) -> Option<PathBuf> {
    let app = env("NVIM_APPNAME").map_or_else(
        || "nvim".to_string(),
        |name| name.to_string_lossy().into_owned(),
    );
    let data = if cfg!(windows) {
        PathBuf::from(env("LOCALAPPDATA")?).join(format!("{app}-data"))
    } else if let Some(data) = env("XDG_DATA_HOME") {
        PathBuf::from(data).join(app)
    } else {
        PathBuf::from(env("HOME")?).join(".local/share").join(app)
    };
    Some(data.join("site/pack/vulcan/start/vulcan.nvim"))
}

/// What is installed at `dir`.
pub fn plugin_status(dir: &Path) -> Result<NvimPluginStatus, AppError> {
    let mut status = NvimPluginStatus {
        path: dir.to_path_buf(),
        state: NvimPluginState::Missing,
        version: VERSION.to_string(),
        installed_version: None,
        modified_files: Vec::new(),
    };
    if !dir.exists() {
        return Ok(status);
    }
    let Some(marker) = read_marker(dir)? else {
        status.state = NvimPluginState::Unmanaged;
        return Ok(status);
    };
    status.installed_version = Some(marker.version.clone());
    for (path, hash) in &marker.files {
        let current = fs::read(dir.join(path))
            .ok()
            .map(|bytes| hash_bytes(&bytes));
        if current.as_deref() != Some(hash.as_str()) {
            status.modified_files.push(path.clone());
        }
    }
    status.state = if !status.modified_files.is_empty() {
        NvimPluginState::Modified
    } else if marker.files == bundle_hashes() && marker.version == VERSION {
        NvimPluginState::Current
    } else {
        NvimPluginState::Outdated
    };
    Ok(status)
}

/// Install or refresh the bundle at `dir`. A copy Vulcan did not install,
/// or one edited since, is only replaced with `force`.
pub fn install_plugin(
    dir: &Path,
    force: bool,
    dry_run: bool,
) -> Result<NvimPluginReport, AppError> {
    let status = plugin_status(dir)?;
    refuse_foreign(&status, force, "replace")?;
    let previous = read_marker(dir).ok().flatten();
    let files = bundle();
    let mut report = NvimPluginReport {
        dry_run,
        path: dir.to_path_buf(),
        previous: status.state,
        written: Vec::new(),
        removed: Vec::new(),
    };
    for (path, contents) in &files {
        let current = fs::read(dir.join(path)).ok();
        if current.as_deref() != Some(contents.as_bytes()) {
            report.written.push(path.clone());
        }
    }
    if let Some(previous) = &previous {
        report.removed = previous
            .files
            .keys()
            .filter(|path| !files.contains_key(*path) && dir.join(path).exists())
            .cloned()
            .collect();
    }
    if dry_run {
        return Ok(report);
    }
    for path in &report.written {
        write_file(&dir.join(path), files[path].as_bytes())?;
    }
    for path in &report.removed {
        fs::remove_file(dir.join(path)).map_err(AppError::operation)?;
    }
    let marker = Marker {
        managed_by: "vulcan".to_string(),
        version: VERSION.to_string(),
        files: bundle_hashes(),
    };
    let marker = serde_json::to_vec_pretty(&marker).map_err(AppError::operation)?;
    write_file(&dir.join(MARKER_FILE), &marker)?;
    Ok(report)
}

/// Remove a copy Vulcan installed at `dir`: its files, the marker, and the
/// directories they leave empty. Others are left alone unless `force`.
pub fn uninstall_plugin(
    dir: &Path,
    force: bool,
    dry_run: bool,
) -> Result<NvimPluginReport, AppError> {
    let status = plugin_status(dir)?;
    refuse_foreign(&status, force, "remove")?;
    let mut report = NvimPluginReport {
        dry_run,
        path: dir.to_path_buf(),
        previous: status.state,
        written: Vec::new(),
        removed: Vec::new(),
    };
    if status.state == NvimPluginState::Missing {
        return Ok(report);
    }
    let marker = read_marker(dir)?;
    let mut paths = marker
        .map(|marker| marker.files.into_keys().collect::<Vec<_>>())
        .unwrap_or_default();
    paths.push(MARKER_FILE.to_string());
    report.removed = paths
        .into_iter()
        .filter(|path| dir.join(path).exists())
        .collect();
    if dry_run {
        return Ok(report);
    }
    if status.state == NvimPluginState::Unmanaged {
        // Forced: the whole directory goes.
        fs::remove_dir_all(dir).map_err(AppError::operation)?;
        return Ok(report);
    }
    for path in &report.removed {
        fs::remove_file(dir.join(path)).map_err(AppError::operation)?;
    }
    remove_empty_dirs(dir);
    Ok(report)
}

fn refuse_foreign(status: &NvimPluginStatus, force: bool, verb: &str) -> Result<(), AppError> {
    let reason = match status.state {
        NvimPluginState::Unmanaged => format!(
            "{} was not installed by Vulcan; pass --force to {verb} it",
            status.path.display()
        ),
        NvimPluginState::Modified => format!(
            "{} has local changes ({}); pass --force to {verb} it",
            status.path.display(),
            status.modified_files.join(", ")
        ),
        _ => return Ok(()),
    };
    if force {
        Ok(())
    } else {
        Err(AppError::operation_with_code("conflict", reason))
    }
}

/// Every installed file: the bundle plus generated help tags.
fn bundle() -> BTreeMap<String, String> {
    let mut files = BUNDLED_FILES
        .iter()
        .map(|(path, contents)| ((*path).to_string(), (*contents).to_string()))
        .collect::<BTreeMap<_, _>>();
    let tags = BUNDLED_FILES
        .iter()
        .filter_map(|(path, contents)| {
            let name = path.strip_prefix("doc/")?;
            Path::new(name)
                .extension()
                .is_some_and(|extension| extension == "txt")
                .then(|| help_tags(name, contents))
        })
        .flatten()
        .collect::<Vec<_>>();
    if !tags.is_empty() {
        let mut tags = tags;
        tags.sort();
        tags.dedup();
        files.insert("doc/tags".to_string(), format!("{}\n", tags.join("\n")));
    }
    files
}

fn bundle_hashes() -> BTreeMap<String, String> {
    bundle()
        .into_iter()
        .map(|(path, contents)| (path, hash_bytes(contents.as_bytes())))
        .collect()
}

/// `:helptags` lines for the `*tag*` anchors in a help file, so `:help`
/// finds them without running `:helptags` (plugin managers do; Neovim's
/// own package loading does not).
fn help_tags(file: &str, contents: &str) -> Vec<String> {
    let mut tags = Vec::new();
    for line in contents.lines() {
        let mut rest = line;
        while let Some(start) = rest.find('*') {
            let after = &rest[start + 1..];
            let Some(end) = after.find('*') else {
                break;
            };
            let tag = &after[..end];
            let delimited = rest[..start].chars().last().is_none_or(char::is_whitespace)
                && after[end + 1..]
                    .chars()
                    .next()
                    .is_none_or(char::is_whitespace);
            if delimited && !tag.is_empty() && !tag.contains(char::is_whitespace) {
                tags.push(format!("{tag}\t{file}\t/*{}*", tag.replace('/', "\\/")));
                rest = &after[end + 1..];
            } else {
                rest = after;
            }
        }
    }
    tags
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

fn hash_bytes(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

/// Write `contents` to `path` through a temporary sibling, so a reader never
/// sees a half-written file.
fn write_file(path: &Path, contents: &[u8]) -> Result<(), AppError> {
    let parent = path.parent().unwrap_or(Path::new("."));
    fs::create_dir_all(parent).map_err(AppError::operation)?;
    let temporary = parent.join(format!(
        ".{}.vulcan-tmp",
        path.file_name().unwrap_or_default().to_string_lossy()
    ));
    fs::write(&temporary, contents).map_err(AppError::operation)?;
    fs::rename(&temporary, path).map_err(AppError::operation)
}

/// Remove directories under (and including) `dir` that are now empty.
fn remove_empty_dirs(dir: &Path) {
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                remove_empty_dirs(&entry.path());
            }
        }
    }
    let _ = fs::remove_dir(dir);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundles_every_plugin_file_except_tests() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../integrations/vulcan.nvim");
        let mut on_disk = Vec::new();
        let mut pending = vec![root.clone()];
        while let Some(dir) = pending.pop() {
            for entry in fs::read_dir(dir).unwrap().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    pending.push(path);
                } else {
                    let relative = path.strip_prefix(&root).unwrap();
                    let relative = relative.to_string_lossy().replace('\\', "/");
                    if !relative.starts_with("test/") {
                        on_disk.push(relative);
                    }
                }
            }
        }
        on_disk.sort();
        let bundled = BUNDLED_FILES
            .iter()
            .map(|(path, _)| (*path).to_string())
            .collect::<Vec<_>>();
        assert_eq!(bundled, on_disk, "update BUNDLED_FILES");
    }

    #[test]
    fn installs_refreshes_and_uninstalls_a_managed_copy() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join("pack/vulcan/start/vulcan.nvim");
        assert_eq!(plugin_status(&dir).unwrap().state, NvimPluginState::Missing);

        let preview = install_plugin(&dir, false, true).unwrap();
        assert!(preview.written.contains(&"plugin/vulcan.lua".to_string()));
        assert!(!dir.exists());

        let report = install_plugin(&dir, false, false).unwrap();
        assert_eq!(report.previous, NvimPluginState::Missing);
        assert!(dir.join("lua/vulcan/init.lua").is_file());
        let tags = fs::read_to_string(dir.join("doc/tags")).unwrap();
        assert!(tags.contains(":Vulcan\tvulcan.txt\t/*:Vulcan*"), "{tags}");
        assert!(tags.contains("vulcan-setup\tvulcan.txt\t/*vulcan-setup*"));
        assert_eq!(plugin_status(&dir).unwrap().state, NvimPluginState::Current);
        // Refreshing an up-to-date copy writes nothing.
        assert!(install_plugin(&dir, false, false)
            .unwrap()
            .written
            .is_empty());

        // An older install, with a file the bundle no longer has.
        let mut marker = read_marker(&dir).unwrap().unwrap();
        marker.version = "0.0.1".to_string();
        fs::write(dir.join("lua/vulcan/old.lua"), "-- old").unwrap();
        marker
            .files
            .insert("lua/vulcan/old.lua".to_string(), hash_bytes(b"-- old"));
        fs::write(dir.join(MARKER_FILE), serde_json::to_vec(&marker).unwrap()).unwrap();
        let status = plugin_status(&dir).unwrap();
        assert_eq!(status.state, NvimPluginState::Outdated);
        assert_eq!(status.installed_version.as_deref(), Some("0.0.1"));
        let report = install_plugin(&dir, false, false).unwrap();
        assert_eq!(report.removed, vec!["lua/vulcan/old.lua".to_string()]);
        assert!(!dir.join("lua/vulcan/old.lua").exists());

        // Local edits are kept unless forced.
        fs::write(dir.join("lua/vulcan/cli.lua"), "-- mine").unwrap();
        let status = plugin_status(&dir).unwrap();
        assert_eq!(status.state, NvimPluginState::Modified);
        assert_eq!(
            status.modified_files,
            vec!["lua/vulcan/cli.lua".to_string()]
        );
        let error = install_plugin(&dir, false, false).unwrap_err();
        assert!(error.to_string().contains("local changes"), "{error}");
        assert!(uninstall_plugin(&dir, false, false).is_err());
        install_plugin(&dir, true, false).unwrap();
        assert_eq!(plugin_status(&dir).unwrap().state, NvimPluginState::Current);

        let report = uninstall_plugin(&dir, false, false).unwrap();
        assert!(report.removed.contains(&MARKER_FILE.to_string()));
        assert!(!dir.exists());
        assert!(temp.path().join("pack/vulcan/start").exists());
    }

    #[test]
    fn leaves_copies_vulcan_did_not_install() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("init.lua"), "-- a checkout").unwrap();
        assert_eq!(
            plugin_status(temp.path()).unwrap().state,
            NvimPluginState::Unmanaged
        );
        assert!(install_plugin(temp.path(), false, false).is_err());
        assert!(uninstall_plugin(temp.path(), false, false).is_err());
        assert!(temp.path().join("init.lua").exists());
    }

    #[test]
    fn finds_neovims_package_directory() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |name: &str| {
                pairs
                    .iter()
                    .find(|(key, _)| *key == name)
                    .map(|(_, value)| std::ffi::OsString::from(value))
            }
        };
        if cfg!(windows) {
            assert_eq!(
                default_install_dir_with(env(&[("LOCALAPPDATA", r"C:\Users\me\AppData\Local")])),
                Some(PathBuf::from(
                    r"C:\Users\me\AppData\Local\nvim-data\site/pack/vulcan/start/vulcan.nvim"
                ))
            );
            return;
        }
        assert_eq!(
            default_install_dir_with(env(&[("HOME", "/home/me")])),
            Some(PathBuf::from(
                "/home/me/.local/share/nvim/site/pack/vulcan/start/vulcan.nvim"
            ))
        );
        assert_eq!(
            default_install_dir_with(env(&[
                ("HOME", "/home/me"),
                ("XDG_DATA_HOME", "/data"),
                ("NVIM_APPNAME", "astro"),
            ])),
            Some(PathBuf::from(
                "/data/astro/site/pack/vulcan/start/vulcan.nvim"
            ))
        );
        assert_eq!(default_install_dir_with(env(&[])), None);
    }

    #[test]
    fn generates_help_tags_only_for_delimited_anchors() {
        let tags = help_tags(
            "x.txt",
            "*x.txt*  Title\n  *:X* *x-api*\nsee |x| and a*b*c and *two words*\n",
        );
        assert_eq!(
            tags,
            vec![
                "x.txt\tx.txt\t/*x.txt*".to_string(),
                ":X\tx.txt\t/*:X*".to_string(),
                "x-api\tx.txt\t/*x-api*".to_string(),
            ]
        );
    }
}
