//! Install the editor plugins bundled with the binary (`vulcan.nvim`,
//! `vulcan.vim`).
//!
//! The plugins' files are built into the binary, so an installed copy always
//! matches the server it talks to. An install writes them (plus generated
//! help tags) into a directory, by default the editor's own package path,
//! where the editor loads the plugin without a plugin manager. A marker file
//! records the plugin, the version, and each file's hash: a refresh replaces
//! only a copy Vulcan wrote and nobody edited since, and removes files a
//! newer bundle dropped.

use crate::AppError;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

/// The marker an installed copy carries, in its root.
pub const MARKER_FILE: &str = ".vulcan-managed.json";

const VERSION: &str = env!("CARGO_PKG_VERSION");

macro_rules! bundled {
    ($plugin:literal: $($path:literal),* $(,)?) => {
        &[$(($path, include_str!(concat!("../../integrations/", $plugin, "/", $path)))),*]
    };
}

/// Environment lookup and existence checks, injectable for tests.
type Env<'a> = &'a dyn Fn(&str) -> Option<OsString>;
type Exists<'a> = &'a dyn Fn(&Path) -> bool;

/// An editor plugin the binary carries.
#[derive(Clone, Copy)]
pub struct EditorPlugin {
    /// The plugin's name, also its install directory's name.
    pub name: &'static str,
    /// Its files, by path relative to its root. Smoke tests under `test/`
    /// are not installed.
    pub files: &'static [(&'static str, &'static str)],
    package_dir: fn(Env, Exists) -> Option<PathBuf>,
}

impl std::fmt::Debug for EditorPlugin {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.name)
    }
}

/// `vulcan.nvim`, for Neovim.
pub const NVIM: EditorPlugin = EditorPlugin {
    name: "vulcan.nvim",
    files: bundled!["vulcan.nvim":
        "README.md",
        "doc/vulcan.txt",
        "lua/vulcan/cli.lua",
        "lua/vulcan/commands.lua",
        "lua/vulcan/health.lua",
        "lua/vulcan/init.lua",
        "lua/vulcan/pickers.lua",
        "lua/vulcan/render.lua",
        "plugin/vulcan.lua",
    ],
    package_dir: neovim_package_dir,
};

/// `vulcan.vim`, for Vim 9 with yegappan/lsp.
pub const VIM: EditorPlugin = EditorPlugin {
    name: "vulcan.vim",
    files: bundled!["vulcan.vim":
        "README.md",
        "autoload/vulcan.vim",
        "autoload/vulcan/cli.vim",
        "autoload/vulcan/health.vim",
        "autoload/vulcan/pickers.vim",
        "autoload/vulcan/render.vim",
        "doc/vulcan.txt",
        "plugin/vulcan.vim",
    ],
    package_dir: vim_package_dir,
};

/// What is at an install location.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginState {
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
pub struct PluginStatus {
    pub plugin: String,
    pub path: PathBuf,
    pub state: PluginState,
    /// The version this binary bundles.
    pub version: String,
    /// The version the installed copy came from, when Vulcan installed it.
    pub installed_version: Option<String>,
    /// Installed files that differ from what Vulcan wrote.
    pub modified_files: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PluginReport {
    pub plugin: String,
    pub dry_run: bool,
    pub path: PathBuf,
    /// The state before the change.
    pub previous: PluginState,
    /// Files written (created or replaced), relative to `path`.
    pub written: Vec<String>,
    /// Files removed, relative to `path`.
    pub removed: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct Marker {
    managed_by: String,
    /// Which plugin; markers written before Vim support name none, and were
    /// all `vulcan.nvim`.
    #[serde(default = "default_marker_plugin")]
    plugin: String,
    version: String,
    /// BLAKE3 hash of each installed file.
    files: BTreeMap<String, String>,
}

/// Helix has no plugin system; its integration is configuration to merge:
/// `languages.toml` (the language server) and `config.toml` (keybindings).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HelixConfig {
    /// Where Helix reads them: `$XDG_CONFIG_HOME/helix` or `~/.config/helix`
    /// (`%AppData%\\helix` on Windows).
    pub config_dir: Option<PathBuf>,
    pub languages_toml: &'static str,
    pub config_toml: &'static str,
}

/// The Helix configuration this binary carries.
#[must_use]
pub fn helix_config() -> HelixConfig {
    let env = |name: &str| std::env::var_os(name).filter(|value| !value.is_empty());
    HelixConfig {
        config_dir: helix_config_dir(&env),
        languages_toml: include_str!("../../integrations/helix/languages.toml"),
        config_toml: include_str!("../../integrations/helix/config.toml"),
    }
}

fn helix_config_dir(env: Env) -> Option<PathBuf> {
    if cfg!(windows) {
        return Some(PathBuf::from(env("APPDATA")?).join("helix"));
    }
    let config = env("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| Some(PathBuf::from(env("HOME")?).join(".config")))?;
    Some(config.join("helix"))
}

fn default_marker_plugin() -> String {
    NVIM.name.to_string()
}

/// Neovim's package directory: `stdpath('data')` (honoring `NVIM_APPNAME`,
/// `XDG_DATA_HOME`, and `LOCALAPPDATA` on Windows) plus `site/pack`.
fn neovim_package_dir(env: Env, _: Exists) -> Option<PathBuf> {
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
    Some(data.join("site/pack"))
}

/// Vim's package directory: `~/.vim/pack` (`vimfiles` on Windows), or
/// `$XDG_CONFIG_HOME/vim/pack` when Vim 9.1 uses its XDG location (no
/// `~/.vimrc` or `~/.vim`, but a `vimrc` there).
fn vim_package_dir(env: Env, exists: Exists) -> Option<PathBuf> {
    let home = PathBuf::from(env("HOME").or_else(|| env("USERPROFILE"))?);
    if cfg!(windows) {
        return Some(home.join("vimfiles/pack"));
    }
    let classic = home.join(".vim");
    if !exists(&classic) && !exists(&home.join(".vimrc")) {
        let config = env("XDG_CONFIG_HOME").map_or_else(|| home.join(".config"), PathBuf::from);
        let xdg = config.join("vim");
        if exists(&xdg.join("vimrc")) {
            return Some(xdg.join("pack"));
        }
    }
    Some(classic.join("pack"))
}

impl EditorPlugin {
    /// Where the plugin goes by default: the editor's package directory plus
    /// `vulcan/start/<name>`, which the editor loads at startup.
    #[must_use]
    pub fn default_install_dir(&self) -> Option<PathBuf> {
        let env = |name: &str| std::env::var_os(name).filter(|value| !value.is_empty());
        let exists = |path: &Path| path.exists();
        self.default_install_dir_with(&env, &exists)
    }

    fn default_install_dir_with(&self, env: Env, exists: Exists) -> Option<PathBuf> {
        Some(
            (self.package_dir)(env, exists)?
                .join("vulcan/start")
                .join(self.name),
        )
    }

    /// What is installed at `dir`.
    pub fn status(&self, dir: &Path) -> Result<PluginStatus, AppError> {
        plugin_status(*self, dir)
    }

    /// Install or refresh the plugin at `dir`. A copy Vulcan did not
    /// install, or one edited since, is only replaced with `force`.
    pub fn install(
        &self,
        dir: &Path,
        force: bool,
        dry_run: bool,
    ) -> Result<PluginReport, AppError> {
        install_plugin(*self, dir, force, dry_run)
    }

    /// Remove a copy Vulcan installed at `dir`: its files, the marker, and
    /// the directories they leave empty. Others are left alone unless
    /// `force`.
    pub fn uninstall(
        &self,
        dir: &Path,
        force: bool,
        dry_run: bool,
    ) -> Result<PluginReport, AppError> {
        uninstall_plugin(*self, dir, force, dry_run)
    }
}

fn plugin_status(plugin: EditorPlugin, dir: &Path) -> Result<PluginStatus, AppError> {
    let mut status = PluginStatus {
        plugin: plugin.name.to_string(),
        path: dir.to_path_buf(),
        state: PluginState::Missing,
        version: VERSION.to_string(),
        installed_version: None,
        modified_files: Vec::new(),
    };
    if !dir.exists() {
        return Ok(status);
    }
    let Some(marker) = read_marker(dir)?.filter(|marker| marker.plugin == plugin.name) else {
        status.state = PluginState::Unmanaged;
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
        PluginState::Modified
    } else if marker.files == bundle_hashes(plugin) && marker.version == VERSION {
        PluginState::Current
    } else {
        PluginState::Outdated
    };
    Ok(status)
}

fn install_plugin(
    plugin: EditorPlugin,
    dir: &Path,
    force: bool,
    dry_run: bool,
) -> Result<PluginReport, AppError> {
    let status = plugin_status(plugin, dir)?;
    refuse_foreign(&status, force, "replace")?;
    let previous = read_marker(dir)
        .ok()
        .flatten()
        .filter(|marker| marker.plugin == plugin.name);
    let files = bundle(plugin);
    let mut report = PluginReport {
        plugin: plugin.name.to_string(),
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
        plugin: plugin.name.to_string(),
        version: VERSION.to_string(),
        files: bundle_hashes(plugin),
    };
    let marker = serde_json::to_vec_pretty(&marker).map_err(AppError::operation)?;
    write_file(&dir.join(MARKER_FILE), &marker)?;
    Ok(report)
}

fn uninstall_plugin(
    plugin: EditorPlugin,
    dir: &Path,
    force: bool,
    dry_run: bool,
) -> Result<PluginReport, AppError> {
    let status = plugin_status(plugin, dir)?;
    refuse_foreign(&status, force, "remove")?;
    let mut report = PluginReport {
        plugin: plugin.name.to_string(),
        dry_run,
        path: dir.to_path_buf(),
        previous: status.state,
        written: Vec::new(),
        removed: Vec::new(),
    };
    if status.state == PluginState::Missing {
        return Ok(report);
    }
    let marker = read_marker(dir)?.filter(|marker| marker.plugin == plugin.name);
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
    if status.state == PluginState::Unmanaged {
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

fn refuse_foreign(status: &PluginStatus, force: bool, verb: &str) -> Result<(), AppError> {
    let reason = match status.state {
        PluginState::Unmanaged => format!(
            "{} was not installed by Vulcan; pass --force to {verb} it",
            status.path.display()
        ),
        PluginState::Modified => format!(
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
fn bundle(plugin: EditorPlugin) -> BTreeMap<String, String> {
    let mut files = plugin
        .files
        .iter()
        .map(|(path, contents)| ((*path).to_string(), (*contents).to_string()))
        .collect::<BTreeMap<_, _>>();
    let tags = plugin
        .files
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

fn bundle_hashes(plugin: EditorPlugin) -> BTreeMap<String, String> {
    bundle(plugin)
        .into_iter()
        .map(|(path, contents)| (path, hash_bytes(contents.as_bytes())))
        .collect()
}

/// `:helptags` lines for the `*tag*` anchors in a help file, so `:help`
/// finds them without running `:helptags` (plugin managers do; the
/// editors' own package loading does not).
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
        for plugin in [NVIM, VIM] {
            let root = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../integrations")
                .join(plugin.name);
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
            let bundled = plugin
                .files
                .iter()
                .map(|(path, _)| (*path).to_string())
                .collect::<Vec<_>>();
            assert_eq!(bundled, on_disk, "update the {} file list", plugin.name);
        }
    }

    #[test]
    fn installs_refreshes_and_uninstalls_a_managed_copy() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join("pack/vulcan/start/vulcan.nvim");
        assert_eq!(NVIM.status(&dir).unwrap().state, PluginState::Missing);

        let preview = NVIM.install(&dir, false, true).unwrap();
        assert!(preview.written.contains(&"plugin/vulcan.lua".to_string()));
        assert!(!dir.exists());

        let report = NVIM.install(&dir, false, false).unwrap();
        assert_eq!(report.previous, PluginState::Missing);
        assert!(dir.join("lua/vulcan/init.lua").is_file());
        let tags = fs::read_to_string(dir.join("doc/tags")).unwrap();
        assert!(tags.contains(":Vulcan\tvulcan.txt\t/*:Vulcan*"), "{tags}");
        assert!(tags.contains("vulcan-setup\tvulcan.txt\t/*vulcan-setup*"));
        assert_eq!(NVIM.status(&dir).unwrap().state, PluginState::Current);
        // Refreshing an up-to-date copy writes nothing.
        assert!(NVIM.install(&dir, false, false).unwrap().written.is_empty());

        // An older install, with a file the bundle no longer has.
        let mut marker = read_marker(&dir).unwrap().unwrap();
        marker.version = "0.0.1".to_string();
        fs::write(dir.join("lua/vulcan/old.lua"), "-- old").unwrap();
        marker
            .files
            .insert("lua/vulcan/old.lua".to_string(), hash_bytes(b"-- old"));
        fs::write(dir.join(MARKER_FILE), serde_json::to_vec(&marker).unwrap()).unwrap();
        let status = NVIM.status(&dir).unwrap();
        assert_eq!(status.state, PluginState::Outdated);
        assert_eq!(status.installed_version.as_deref(), Some("0.0.1"));
        let report = NVIM.install(&dir, false, false).unwrap();
        assert_eq!(report.removed, vec!["lua/vulcan/old.lua".to_string()]);
        assert!(!dir.join("lua/vulcan/old.lua").exists());

        // Local edits are kept unless forced.
        fs::write(dir.join("lua/vulcan/cli.lua"), "-- mine").unwrap();
        let status = NVIM.status(&dir).unwrap();
        assert_eq!(status.state, PluginState::Modified);
        assert_eq!(
            status.modified_files,
            vec!["lua/vulcan/cli.lua".to_string()]
        );
        let error = NVIM.install(&dir, false, false).unwrap_err();
        assert!(error.to_string().contains("local changes"), "{error}");
        assert!(NVIM.uninstall(&dir, false, false).is_err());
        NVIM.install(&dir, true, false).unwrap();
        assert_eq!(NVIM.status(&dir).unwrap().state, PluginState::Current);

        let report = NVIM.uninstall(&dir, false, false).unwrap();
        assert!(report.removed.contains(&MARKER_FILE.to_string()));
        assert!(!dir.exists());
        assert!(temp.path().join("pack/vulcan/start").exists());
    }

    #[test]
    fn installs_the_vim_plugin_and_keeps_plugins_apart() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join("vulcan.vim");
        VIM.install(&dir, false, false).unwrap();
        assert!(dir.join("autoload/vulcan/render.vim").is_file());
        let tags = fs::read_to_string(dir.join("doc/tags")).unwrap();
        assert!(tags.contains("g:vulcan\tvulcan.txt\t/*g:vulcan*"), "{tags}");
        assert_eq!(VIM.status(&dir).unwrap().state, PluginState::Current);
        // The Neovim plugin does not claim the Vim plugin's copy.
        assert_eq!(NVIM.status(&dir).unwrap().state, PluginState::Unmanaged);
        assert!(NVIM.install(&dir, false, false).is_err());

        // Markers from before Vim support name no plugin: Neovim's.
        let nvim = temp.path().join("vulcan.nvim");
        NVIM.install(&nvim, false, false).unwrap();
        let marker = fs::read_to_string(nvim.join(MARKER_FILE)).unwrap();
        let mut legacy = serde_json::from_str::<serde_json::Value>(&marker).unwrap();
        legacy.as_object_mut().unwrap().remove("plugin");
        fs::write(nvim.join(MARKER_FILE), legacy.to_string()).unwrap();
        assert_eq!(NVIM.status(&nvim).unwrap().state, PluginState::Current);
    }

    #[test]
    fn leaves_copies_vulcan_did_not_install() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("init.lua"), "-- a checkout").unwrap();
        assert_eq!(
            NVIM.status(temp.path()).unwrap().state,
            PluginState::Unmanaged
        );
        assert!(NVIM.install(temp.path(), false, false).is_err());
        assert!(NVIM.uninstall(temp.path(), false, false).is_err());
        assert!(temp.path().join("init.lua").exists());
    }

    fn env(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<OsString> {
        move |name: &str| {
            pairs
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| OsString::from(value))
        }
    }

    #[test]
    fn finds_neovims_package_directory() {
        let none = |_: &Path| false;
        let dir = |pairs| NVIM.default_install_dir_with(&env(pairs), &none);
        if cfg!(windows) {
            assert_eq!(
                dir(&[("LOCALAPPDATA", r"C:\Users\me\AppData\Local")]),
                Some(PathBuf::from(
                    r"C:\Users\me\AppData\Local\nvim-data\site/pack/vulcan/start/vulcan.nvim"
                ))
            );
            return;
        }
        assert_eq!(
            dir(&[("HOME", "/home/me")]),
            Some(PathBuf::from(
                "/home/me/.local/share/nvim/site/pack/vulcan/start/vulcan.nvim"
            ))
        );
        assert_eq!(
            dir(&[
                ("HOME", "/home/me"),
                ("XDG_DATA_HOME", "/data"),
                ("NVIM_APPNAME", "astro"),
            ]),
            Some(PathBuf::from(
                "/data/astro/site/pack/vulcan/start/vulcan.nvim"
            ))
        );
        assert_eq!(dir(&[]), None);
    }

    #[test]
    fn finds_vims_package_directory() {
        if cfg!(windows) {
            let none = |_: &Path| false;
            assert_eq!(
                VIM.default_install_dir_with(&env(&[("USERPROFILE", r"C:\Users\me")]), &none),
                Some(PathBuf::from(
                    r"C:\Users\me\vimfiles/pack/vulcan/start/vulcan.vim"
                ))
            );
            return;
        }
        let home = env(&[("HOME", "/home/me")]);
        let none = |_: &Path| false;
        assert_eq!(
            VIM.default_install_dir_with(&home, &none),
            Some(PathBuf::from("/home/me/.vim/pack/vulcan/start/vulcan.vim"))
        );
        // Vim 9.1 uses ~/.config/vim when that holds the vimrc and the
        // classic locations do not exist.
        let xdg = |path: &Path| path == Path::new("/home/me/.config/vim/vimrc");
        assert_eq!(
            VIM.default_install_dir_with(&home, &xdg),
            Some(PathBuf::from(
                "/home/me/.config/vim/pack/vulcan/start/vulcan.vim"
            ))
        );
        let both = |path: &Path| {
            path == Path::new("/home/me/.config/vim/vimrc") || path == Path::new("/home/me/.vim")
        };
        assert_eq!(
            VIM.default_install_dir_with(&home, &both),
            Some(PathBuf::from("/home/me/.vim/pack/vulcan/start/vulcan.vim"))
        );
    }

    #[test]
    fn carries_valid_helix_configuration() {
        let config = helix_config();
        let languages = config.languages_toml.parse::<toml::Table>().unwrap();
        let server = &languages["language-server"]["vulcan"];
        assert_eq!(server["command"].as_str(), Some("vulcan"));
        assert_eq!(server["config"]["showDocument"].as_bool(), Some(true));
        let markdown = languages["language"]
            .as_array()
            .unwrap()
            .iter()
            .find(|language| language["name"].as_str() == Some("markdown"))
            .unwrap();
        assert!(markdown["language-servers"]
            .as_array()
            .unwrap()
            .iter()
            .any(|server| server.as_str() == Some("vulcan")));
        let keys = config.config_toml.parse::<toml::Table>().unwrap();
        let bindings = keys["keys"]["normal"]["space"]["v"].as_table().unwrap();
        assert!(bindings["x"]
            .as_str()
            .unwrap()
            .contains("vulcan.task.toggle"));
        assert_eq!(bindings["c"].as_str(), Some(":pipe-to vulcan inbox -"));

        let home = env(&[("HOME", "/home/me")]);
        if !cfg!(windows) {
            assert_eq!(
                helix_config_dir(&home),
                Some(PathBuf::from("/home/me/.config/helix"))
            );
            assert_eq!(
                helix_config_dir(&env(&[("HOME", "/home/me"), ("XDG_CONFIG_HOME", "/cfg")])),
                Some(PathBuf::from("/cfg/helix"))
            );
        }
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
