//! Device-local daemon environment loading.

use std::collections::HashSet;
use std::error::Error;
use std::fmt::{Display, Formatter};
use std::fs;
use std::path::{Path, PathBuf};

const DAEMON_ENV_FILE: &str = "daemon.env";
const MAX_DAEMON_ENV_BYTES: u64 = 64 * 1024;

#[derive(Debug)]
pub enum DaemonEnvironmentError {
    Invalid(PathBuf, String),
    Io(std::io::Error),
}

impl Display for DaemonEnvironmentError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(path, detail) => {
                write!(
                    formatter,
                    "invalid daemon environment at {}: {detail}",
                    path.display()
                )
            }
            Self::Io(error) => Display::fmt(error, formatter),
        }
    }
}

impl Error for DaemonEnvironmentError {}

impl From<std::io::Error> for DaemonEnvironmentError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

/// Load `daemon.env` without overriding variables inherited by the process.
///
/// The file intentionally supports only literal `NAME=value` records. It does
/// not expand variables, execute shell syntax, or accept export directives.
pub fn load_daemon_environment(config_directory: &Path) -> Result<usize, DaemonEnvironmentError> {
    let path = config_directory.join(DAEMON_ENV_FILE);
    let Some(contents) = read_environment_file(&path)? else {
        return Ok(0);
    };
    let entries = parse_environment(&path, &contents)?;
    let mut loaded = 0;
    for (name, value) in entries {
        if std::env::var_os(&name).is_none() {
            std::env::set_var(name, value);
            loaded += 1;
        }
    }
    Ok(loaded)
}

fn read_environment_file(path: &Path) -> Result<Option<String>, DaemonEnvironmentError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(DaemonEnvironmentError::Invalid(
            path.to_path_buf(),
            "expected a regular file, not a symlink or special file".to_string(),
        ));
    }
    if metadata.len() > MAX_DAEMON_ENV_BYTES {
        return Err(DaemonEnvironmentError::Invalid(
            path.to_path_buf(),
            format!("file exceeds the {MAX_DAEMON_ENV_BYTES} byte limit"),
        ));
    }
    validate_owner_only(&metadata, path)?;
    fs::read_to_string(path).map(Some).map_err(Into::into)
}

fn parse_environment(
    path: &Path,
    contents: &str,
) -> Result<Vec<(String, String)>, DaemonEnvironmentError> {
    let mut names = HashSet::new();
    let mut entries = Vec::new();
    for (index, raw_line) in contents.lines().enumerate() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((name, value)) = line.split_once('=') else {
            return Err(invalid_line(path, index, "expected NAME=value"));
        };
        let name = name.trim();
        if !valid_environment_name(name) {
            return Err(invalid_line(
                path,
                index,
                "invalid environment variable name",
            ));
        }
        if !names.insert(name.to_string()) {
            return Err(invalid_line(path, index, "duplicate environment variable"));
        }
        let value = parse_literal_value(value.trim()).ok_or_else(|| {
            invalid_line(path, index, "mismatched quotes around environment value")
        })?;
        entries.push((name.to_string(), value.to_string()));
    }
    Ok(entries)
}

fn parse_literal_value(value: &str) -> Option<&str> {
    if value.len() >= 2 {
        let bytes = value.as_bytes();
        if matches!(
            (bytes[0], bytes[value.len() - 1]),
            (b'\'', b'\'') | (b'"', b'"')
        ) {
            return value.get(1..value.len() - 1);
        }
        if matches!(bytes[0], b'\'' | b'"') || matches!(bytes[value.len() - 1], b'\'' | b'"') {
            return None;
        }
    } else if matches!(value.as_bytes().first(), Some(b'\'' | b'"')) {
        return None;
    }
    Some(value)
}

fn valid_environment_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    bytes
        .next()
        .is_some_and(|byte| byte.is_ascii_alphabetic() || byte == b'_')
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

fn invalid_line(path: &Path, index: usize, detail: &str) -> DaemonEnvironmentError {
    DaemonEnvironmentError::Invalid(path.to_path_buf(), format!("line {}: {detail}", index + 1))
}

#[cfg(unix)]
fn validate_owner_only(metadata: &fs::Metadata, path: &Path) -> Result<(), DaemonEnvironmentError> {
    use std::os::unix::fs::PermissionsExt;
    if metadata.permissions().mode() & 0o077 != 0 {
        return Err(DaemonEnvironmentError::Invalid(
            path.to_path_buf(),
            "file must not be accessible by group or other users (use mode 0600)".to_string(),
        ));
    }
    Ok(())
}

#[cfg(not(unix))]
#[allow(clippy::unnecessary_wraps)]
fn validate_owner_only(
    _metadata: &fs::Metadata,
    _path: &Path,
) -> Result<(), DaemonEnvironmentError> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use tempfile::tempdir;

    #[test]
    fn parser_accepts_literal_values_without_shell_expansion() {
        let path = Path::new("daemon.env");
        let entries = parse_environment(
            path,
            "# Device-local secrets\nTOKEN='literal $HOME'\nEMPTY=\nURL=https://example.test/v1\n",
        )
        .expect("environment");
        assert_eq!(
            entries,
            [
                ("TOKEN".to_string(), "literal $HOME".to_string()),
                ("EMPTY".to_string(), String::new()),
                ("URL".to_string(), "https://example.test/v1".to_string()),
            ]
        );
    }

    #[test]
    fn parser_rejects_shell_syntax_invalid_names_and_duplicates() {
        let path = Path::new("daemon.env");
        for contents in [
            "export TOKEN=value",
            "1TOKEN=value",
            "TOKEN=value\nTOKEN=other",
            "TOKEN='unterminated",
        ] {
            assert!(parse_environment(path, contents).is_err(), "{contents}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn loader_rejects_group_readable_environment_files() {
        use std::os::unix::fs::PermissionsExt;

        let temporary = tempdir().expect("temporary directory");
        let path = temporary.path().join(DAEMON_ENV_FILE);
        fs::write(&path, "TOKEN=secret\n").expect("environment fixture");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).expect("permissions");

        assert!(matches!(
            load_daemon_environment(temporary.path()),
            Err(DaemonEnvironmentError::Invalid(error_path, _)) if error_path == path
        ));
    }

    #[cfg(unix)]
    #[test]
    fn loader_rejects_symlinked_environment_files() {
        use std::os::unix::fs::symlink;

        let temporary = tempdir().expect("temporary directory");
        let target = temporary.path().join("target.env");
        let path = temporary.path().join(DAEMON_ENV_FILE);
        fs::write(&target, "TOKEN=secret\n").expect("environment fixture");
        symlink(target, &path).expect("environment symlink");

        assert!(load_daemon_environment(temporary.path()).is_err());
    }
}
