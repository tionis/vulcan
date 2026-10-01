//! Device-level, non-secret Vulcan policy (Roadmap 12.22.1).
//!
//! `device.toml` sits beside `daemon.toml` in the user configuration directory.
//! It says how this installation reaches Git remotes (the device key or
//! ambient credentials) and where to send a forge credential for a given host.
//! Vulcan ships no forge, host, or OAuth client: they come from this file. It is
//! never read from a vault or a remote, and holds no secret, only the *name* of
//! a token variable and a public OAuth client ID.

use crate::sync_forge::ForgeKind;
use crate::{durable_file, AppError};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

pub const DEVICE_CONFIG_FILE: &str = "device.toml";
const DEVICE_CONFIG_VERSION: u32 = 1;
const MAX_DEVICE_CONFIG_BYTES: u64 = 64 * 1024;
const MAX_FORGE_ENTRIES: usize = 256;

/// How an installation authenticates Git over SSH.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TransportPolicy {
    /// Enroll and bind the device key; the default.
    #[default]
    DeviceKey,
    /// Leave authentication to the user's own SSH setup.
    Ambient,
}

/// When `vault enroll` may start an interactive OAuth login.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LoginMode {
    /// Only when attached to a terminal (or when `--login` is given).
    #[default]
    Auto,
    /// Never; report `needs_login` instead.
    Never,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransportSection {
    #[serde(default)]
    pub default: TransportPolicy,
}

/// What this device knows about one forge host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ForgeEntry {
    /// Lowercase hostname only: no scheme, port, or path.
    pub host: String,
    /// The adapter for this host. Absent for a host listed only to override
    /// the transport policy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<ForgeKind>,
    /// Public OAuth client ID for `sync forge login`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oauth_client_id: Option<String>,
    /// Environment variable holding an API token (the fallback credential).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_env: Option<String>,
    /// Per-host override of the default transport policy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transport: Option<TransportPolicy>,
    #[serde(default)]
    pub login: LoginMode,
}

impl ForgeEntry {
    fn validate(&self) -> Result<(), AppError> {
        validate_host(&self.host)?;
        if let Some(client_id) = &self.oauth_client_id {
            crate::sync_forge::validate_oauth_client_id(client_id)?;
        }
        if let Some(name) = &self.token_env {
            let ok = !name.is_empty()
                && name.len() <= 128
                && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                && !name.starts_with(|c: char| c.is_ascii_digit());
            if !ok {
                return Err(AppError::operation(
                    "token environment variable must be a plain name such as FORGE_TOKEN",
                ));
            }
        }
        if (self.oauth_client_id.is_some() || self.token_env.is_some()) && self.kind.is_none() {
            return Err(AppError::operation(format!(
                "forge `{}` names a credential but no kind; set `kind` so Vulcan knows which API to use",
                self.host
            )));
        }
        Ok(())
    }
}

/// A bare, lowercase DNS hostname (or `localhost`). Rejecting URLs here means
/// a credential destination is never smuggled in through this file.
fn validate_host(host: &str) -> Result<(), AppError> {
    let label_ok = |label: &str| {
        !label.is_empty()
            && label.len() <= 63
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    };
    if host.len() <= 253 && host.split('.').all(label_ok) {
        Ok(())
    } else {
        Err(AppError::operation(
            "forge host must be a lowercase hostname such as forge.example.com (no scheme, port, or path)",
        ))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceConfig {
    pub version: u32,
    #[serde(default)]
    pub transport: TransportSection,
    #[serde(default, rename = "forge", skip_serializing_if = "Vec::is_empty")]
    pub forges: Vec<ForgeEntry>,
}

impl Default for DeviceConfig {
    fn default() -> Self {
        Self {
            version: DEVICE_CONFIG_VERSION,
            transport: TransportSection::default(),
            forges: Vec::new(),
        }
    }
}

impl DeviceConfig {
    fn validate(&self) -> Result<(), AppError> {
        if self.version != DEVICE_CONFIG_VERSION {
            return Err(AppError::operation(format!(
                "unsupported device configuration version {}",
                self.version
            )));
        }
        if self.forges.len() > MAX_FORGE_ENTRIES {
            return Err(AppError::operation("too many forge entries"));
        }
        let mut seen = std::collections::BTreeSet::new();
        for entry in &self.forges {
            entry.validate()?;
            if !seen.insert(entry.host.as_str()) {
                return Err(AppError::operation(format!(
                    "forge `{}` is listed more than once",
                    entry.host
                )));
            }
        }
        Ok(())
    }

    /// The entry for `host`, if this device knows it.
    #[must_use]
    pub fn forge(&self, host: &str) -> Option<&ForgeEntry> {
        let host = host.to_ascii_lowercase();
        self.forges.iter().find(|entry| entry.host == host)
    }

    /// The transport policy for a remote on `host`: a per-host override, else
    /// the device default.
    #[must_use]
    pub fn policy_for(&self, host: Option<&str>) -> TransportPolicy {
        host.and_then(|host| self.forge(host))
            .and_then(|entry| entry.transport)
            .unwrap_or(self.transport.default)
    }
}

/// Where the device configuration lives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceConfigStore {
    path: PathBuf,
}

impl DeviceConfigStore {
    pub fn user_default() -> Result<Self, AppError> {
        let directory = vulcan_core::vulcan_user_config_dir().ok_or_else(|| {
            AppError::operation("cannot determine the Vulcan configuration directory")
        })?;
        Ok(Self::at(directory.join(DEVICE_CONFIG_FILE)))
    }

    #[must_use]
    pub fn at(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Loads the configuration; an absent file is the default configuration.
    pub fn load(&self) -> Result<DeviceConfig, AppError> {
        let metadata = match fs::symlink_metadata(&self.path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(DeviceConfig::default())
            }
            Err(error) => return Err(AppError::operation(error)),
        };
        if !metadata.is_file() || metadata.len() > MAX_DEVICE_CONFIG_BYTES {
            return Err(AppError::operation(
                "device.toml is not a bounded regular file",
            ));
        }
        let text = fs::read_to_string(&self.path).map_err(AppError::operation)?;
        let config: DeviceConfig = toml::from_str(&text)
            .map_err(|error| AppError::operation(format!("invalid device.toml: {error}")))?;
        config.validate()?;
        Ok(config)
    }

    fn save(&self, config: &DeviceConfig) -> Result<(), AppError> {
        config.validate()?;
        let text = toml::to_string_pretty(config).map_err(AppError::operation)?;
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).map_err(AppError::operation)?;
        }
        durable_file::replace(&self.path, text.as_bytes())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DeviceConfigReport {
    pub version: u32,
    pub path: PathBuf,
    /// Whether the file exists; an absent file means the defaults apply.
    pub exists: bool,
    pub dry_run: bool,
    pub changed: bool,
    pub config: DeviceConfig,
}

impl DeviceConfigStore {
    /// The effective configuration, defaults included.
    pub fn show(&self) -> Result<DeviceConfigReport, AppError> {
        Ok(DeviceConfigReport {
            version: 1,
            path: self.path.clone(),
            exists: self.path.is_file(),
            dry_run: false,
            changed: false,
            config: self.load()?,
        })
    }

    fn edit(
        &self,
        dry_run: bool,
        change: impl FnOnce(&mut DeviceConfig) -> Result<(), AppError>,
    ) -> Result<DeviceConfigReport, AppError> {
        let before = self.load()?;
        let mut after = before.clone();
        change(&mut after)?;
        after.validate()?;
        let changed = after != before;
        if changed && !dry_run {
            self.save(&after)?;
        }
        Ok(DeviceConfigReport {
            version: 1,
            path: self.path.clone(),
            exists: self.path.is_file(),
            dry_run,
            changed,
            config: after,
        })
    }

    pub fn set_transport(
        &self,
        policy: TransportPolicy,
        dry_run: bool,
    ) -> Result<DeviceConfigReport, AppError> {
        self.edit(dry_run, |config| {
            config.transport.default = policy;
            Ok(())
        })
    }

    /// Adds or replaces the entry for `entry.host`.
    pub fn set_forge(
        &self,
        entry: ForgeEntry,
        dry_run: bool,
    ) -> Result<DeviceConfigReport, AppError> {
        self.edit(dry_run, |config| {
            entry.validate()?;
            match config
                .forges
                .iter_mut()
                .find(|existing| existing.host == entry.host)
            {
                Some(existing) => *existing = entry,
                None => config.forges.push(entry),
            }
            config.forges.sort_by(|a, b| a.host.cmp(&b.host));
            Ok(())
        })
    }

    pub fn remove_forge(&self, host: &str, dry_run: bool) -> Result<DeviceConfigReport, AppError> {
        let host = host.to_ascii_lowercase();
        self.edit(dry_run, |config| {
            let before = config.forges.len();
            config.forges.retain(|entry| entry.host != host);
            if config.forges.len() == before {
                return Err(AppError::operation(format!("no forge entry for `{host}`")));
            }
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn store() -> (TempDir, DeviceConfigStore) {
        let dir = TempDir::new().unwrap();
        let store = DeviceConfigStore::at(dir.path().join("vulcan").join(DEVICE_CONFIG_FILE));
        (dir, store)
    }

    fn entry(host: &str) -> ForgeEntry {
        ForgeEntry {
            host: host.to_owned(),
            kind: Some(ForgeKind::Forgejo),
            oauth_client_id: Some("client-abc".to_owned()),
            token_env: Some("FORGE_TOKEN".to_owned()),
            transport: None,
            login: LoginMode::Auto,
        }
    }

    #[test]
    fn an_absent_file_means_the_device_key_is_the_default() {
        let (_dir, store) = store();
        let shown = store.show().unwrap();
        assert!(!shown.exists);
        assert_eq!(
            shown.config.policy_for(Some("anything.example")),
            TransportPolicy::DeviceKey
        );
        assert_eq!(shown.config.policy_for(None), TransportPolicy::DeviceKey);
        assert!(!store.path().exists(), "reading never creates the file");
    }

    #[test]
    fn policy_resolution_prefers_a_per_host_override() {
        let (_dir, store) = store();
        store
            .set_transport(TransportPolicy::Ambient, false)
            .unwrap();
        store
            .set_forge(
                ForgeEntry {
                    transport: Some(TransportPolicy::DeviceKey),
                    ..entry("forge.example.com")
                },
                false,
            )
            .unwrap();
        store
            .set_forge(
                ForgeEntry {
                    host: "github.com".to_owned(),
                    kind: None,
                    oauth_client_id: None,
                    token_env: None,
                    transport: Some(TransportPolicy::Ambient),
                    login: LoginMode::Auto,
                },
                false,
            )
            .unwrap();
        let config = store.load().unwrap();
        assert_eq!(
            config.policy_for(Some("forge.example.com")),
            TransportPolicy::DeviceKey
        );
        assert_eq!(
            config.policy_for(Some("FORGE.Example.com")),
            TransportPolicy::DeviceKey,
            "hosts are case-insensitive"
        );
        assert_eq!(
            config.policy_for(Some("github.com")),
            TransportPolicy::Ambient
        );
        assert_eq!(
            config.policy_for(Some("other.example")),
            TransportPolicy::Ambient,
            "the device default"
        );
        assert_eq!(config.policy_for(None), TransportPolicy::Ambient);
    }

    #[test]
    fn edits_are_atomic_idempotent_and_honor_dry_run() {
        let (_dir, store) = store();
        let preview = store.set_forge(entry("forge.example.com"), true).unwrap();
        assert!(preview.dry_run && preview.changed);
        assert!(!store.path().exists(), "a dry run writes nothing");

        assert!(
            store
                .set_forge(entry("forge.example.com"), false)
                .unwrap()
                .changed
        );
        assert!(
            !store
                .set_forge(entry("forge.example.com"), false)
                .unwrap()
                .changed,
            "idempotent"
        );
        let text = fs::read_to_string(store.path()).unwrap();
        assert!(
            text.contains("host = \"forge.example.com\"") && text.contains("[[forge]]"),
            "{text}"
        );

        // Replacing keeps one entry per host, sorted.
        store.set_forge(entry("a.example.com"), false).unwrap();
        store
            .set_forge(
                ForgeEntry {
                    oauth_client_id: Some("other-id".to_owned()),
                    ..entry("forge.example.com")
                },
                false,
            )
            .unwrap();
        let config = store.load().unwrap();
        assert_eq!(
            config
                .forges
                .iter()
                .map(|e| e.host.as_str())
                .collect::<Vec<_>>(),
            ["a.example.com", "forge.example.com"]
        );
        assert_eq!(
            config
                .forge("forge.example.com")
                .unwrap()
                .oauth_client_id
                .as_deref(),
            Some("other-id")
        );

        assert!(store.remove_forge("a.example.com", true).unwrap().dry_run);
        assert!(
            store.load().unwrap().forge("a.example.com").is_some(),
            "a dry-run removal keeps it"
        );
        store.remove_forge("A.example.com", false).unwrap();
        assert!(store.load().unwrap().forge("a.example.com").is_none());
        assert!(store.remove_forge("a.example.com", false).is_err());
    }

    #[test]
    fn the_file_is_validated_strictly() {
        let (_dir, store) = store();
        fs::create_dir_all(store.path().parent().unwrap()).unwrap();
        let write = |text: &str| fs::write(store.path(), text).unwrap();

        write("version = 1\n[transport]\ndefault = \"ambient\"\n");
        assert_eq!(
            store.load().unwrap().transport.default,
            TransportPolicy::Ambient
        );

        for bad in [
            "version = 2\n",
            "version = 1\nunknown = true\n",
            "version = 1\n[transport]\ndefault = \"sometimes\"\n",
            "version = 1\n[[forge]]\nhost = \"https://forge.example.com\"\n",
            "version = 1\n[[forge]]\nhost = \"forge.example.com:8443\"\n",
            "version = 1\n[[forge]]\nhost = \"Forge.Example.com\"\n",
            "version = 1\n[[forge]]\nhost = \"forge.example.com/path\"\n",
            "version = 1\n[[forge]]\nhost = \"-bad.example.com\"\n",
            "version = 1\n[[forge]]\nhost = \"a.example.com\"\nsecret = \"x\"\n",
            "version = 1\n[[forge]]\nhost = \"a.example.com\"\noauth_client_id = \"bad id\"\nkind = \"forgejo\"\n",
            "version = 1\n[[forge]]\nhost = \"a.example.com\"\ntoken_env = \"A B\"\nkind = \"forgejo\"\n",
            "version = 1\n[[forge]]\nhost = \"a.example.com\"\ntoken_env = \"TOKEN\"\n",
            "version = 1\n[[forge]]\nhost = \"a.example.com\"\n[[forge]]\nhost = \"a.example.com\"\n",
            "not toml at all [",
        ] {
            write(bad);
            assert!(store.load().is_err(), "{bad}");
        }
        fs::write(store.path(), vec![b'#'; 70 * 1024]).unwrap();
        assert!(store.load().is_err(), "oversize files are refused");
    }

    #[test]
    fn a_policy_only_entry_needs_no_kind_or_credential() {
        let (_dir, store) = store();
        store
            .set_forge(
                ForgeEntry {
                    host: "github.com".to_owned(),
                    kind: None,
                    oauth_client_id: None,
                    token_env: None,
                    transport: Some(TransportPolicy::Ambient),
                    login: LoginMode::Auto,
                },
                false,
            )
            .unwrap();
        assert_eq!(
            store.load().unwrap().forge("github.com").unwrap().kind,
            None
        );
        // A credential without a kind is refused before anything is written.
        let error = store
            .set_forge(
                ForgeEntry {
                    kind: None,
                    ..entry("x.example.com")
                },
                false,
            )
            .unwrap_err();
        assert!(error.to_string().contains("no kind"), "{error}");
    }

    #[test]
    fn nothing_secret_is_ever_stored() {
        let (_dir, store) = store();
        store.set_forge(entry("forge.example.com"), false).unwrap();
        let text = fs::read_to_string(store.path()).unwrap();
        assert!(text.contains("FORGE_TOKEN"), "only the variable name");
        assert!(!text.to_lowercase().contains("secret"));
    }

    #[test]
    fn a_symlinked_config_is_refused() {
        #[cfg(unix)]
        {
            let (dir, store) = store();
            fs::create_dir_all(store.path().parent().unwrap()).unwrap();
            let target = dir.path().join("elsewhere.toml");
            fs::write(&target, "version = 1\n").unwrap();
            std::os::unix::fs::symlink(&target, store.path()).unwrap();
            assert!(store.load().is_err());
        }
    }
}
