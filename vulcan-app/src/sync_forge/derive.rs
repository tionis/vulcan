//! Deriving the forge host and `owner/name` from a Git remote URL, so a vault
//! needs no forge-specific configuration in the common case.

use crate::AppError;

/// Where a repository's forge API lives, as implied by its Git remote.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ForgeTarget {
    /// Lowercased host without port, as it appears in the remote.
    pub host: String,
    /// Default API origin: `https://<host>` (HTTPS remotes keep their port).
    pub url: String,
    /// `owner/name`.
    pub repo: String,
}

/// Parses `ssh://`, `git+ssh://`, `https://`, and scp-like remotes. SSH ports
/// belong to SSH and are dropped; pass an explicit URL for a forge on another
/// HTTPS port.
pub fn derive_forge_target(remote_url: &str) -> Result<ForgeTarget, AppError> {
    let unsupported = || {
        AppError::operation(
            "cannot derive the forge from this remote; pass the settings explicitly",
        )
    };
    let text = remote_url.trim();
    if text.is_empty() || text.contains(char::is_control) {
        return Err(unsupported());
    }
    let (scheme, host_port, path) = if let Some((scheme, rest)) = text.split_once("://") {
        let (authority, path) = rest.split_once('/').ok_or_else(unsupported)?;
        (
            scheme.to_ascii_lowercase(),
            authority.to_owned(),
            path.to_owned(),
        )
    } else if let Some((authority, path)) = text.split_once(':') {
        // scp-like `[user@]host:path`; a lone letter before `:` is a Windows drive.
        if authority.is_empty()
            || authority.contains('/')
            || (authority.len() == 1 && authority.chars().all(|c| c.is_ascii_alphabetic()))
        {
            return Err(unsupported());
        }
        ("ssh".to_owned(), authority.to_owned(), path.to_owned())
    } else {
        return Err(unsupported());
    };
    if !matches!(scheme.as_str(), "ssh" | "git+ssh" | "https") {
        return Err(unsupported());
    }
    let host_port = host_port
        .rsplit_once('@')
        .map_or(host_port.as_str(), |(_, rest)| rest);
    let (host, port) = split_host_port(host_port).ok_or_else(unsupported)?;
    if host.is_empty() {
        return Err(unsupported());
    }
    let host = host.to_ascii_lowercase();
    let path = path.trim_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let segments = path.split('/').collect::<Vec<_>>();
    if segments.len() != 2 || segments.iter().any(|segment| segment.is_empty()) {
        return Err(AppError::operation(
            "cannot derive `owner/name` from the remote path; pass --repo",
        ));
    }
    let url = match (scheme.as_str(), port) {
        ("https", Some(port)) => format!("https://{host}:{port}"),
        _ => format!("https://{host}"),
    };
    Ok(ForgeTarget {
        host,
        url,
        repo: format!("{}/{}", segments[0], segments[1]),
    })
}

/// Splits `host`, `host:port`, `[v6]`, or `[v6]:port`.
fn split_host_port(text: &str) -> Option<(String, Option<u16>)> {
    if let Some(rest) = text.strip_prefix('[') {
        let (host, after) = rest.split_once(']')?;
        let port = match after.strip_prefix(':') {
            Some(port) => Some(port.parse().ok()?),
            None if after.is_empty() => None,
            None => return None,
        };
        return Some((format!("[{host}]"), port));
    }
    match text.split_once(':') {
        Some((host, port)) => Some((host.to_owned(), Some(port.parse().ok()?))),
        None => Some((text.to_owned(), None)),
    }
}

/// Host of an `http(s)://host[:port]/...` URL, lowercased, without port.
pub(crate) fn url_host(url: &str) -> Option<String> {
    let rest = url.split_once("://")?.1;
    let authority = rest.split('/').next()?;
    let authority = authority
        .rsplit_once('@')
        .map_or(authority, |(_, rest)| rest);
    split_host_port(authority).map(|(host, _)| host.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(remote: &str) -> ForgeTarget {
        derive_forge_target(remote).unwrap_or_else(|error| panic!("{remote}: {error}"))
    }

    #[test]
    fn derives_the_forge_from_every_common_remote_shape() {
        let expected = ForgeTarget {
            host: "forge.example.com".to_owned(),
            url: "https://forge.example.com".to_owned(),
            repo: "eric/mimir".to_owned(),
        };
        for remote in [
            "git@forge.example.com:eric/mimir",
            "git@forge.example.com:eric/mimir.git",
            "forge.example.com:eric/mimir.git",
            "ssh://git@forge.example.com/eric/mimir.git",
            "ssh://git@forge.example.com:2222/eric/mimir",
            "git+ssh://forge.example.com/eric/mimir/",
            "https://forge.example.com/eric/mimir.git",
            "https://user:secret@forge.example.com/eric/mimir",
            "ssh://git@FORGE.Example.com/eric/mimir",
        ] {
            assert_eq!(target(remote), expected, "{remote}");
        }
    }

    #[test]
    fn surrounding_whitespace_is_trimmed_like_git_output() {
        assert_eq!(target("git@forge.example.com:o/r\n").repo, "o/r");
    }

    #[test]
    fn keeps_an_https_port_but_drops_an_ssh_port() {
        assert_eq!(
            target("https://forge.example.com:8443/o/r").url,
            "https://forge.example.com:8443"
        );
        assert_eq!(
            target("ssh://git@forge.example.com:2222/o/r").url,
            "https://forge.example.com"
        );
        assert_eq!(target("ssh://git@[::1]:22/o/r").host, "[::1]");
    }

    #[test]
    fn refuses_what_it_cannot_derive_without_guessing() {
        for remote in [
            "",
            "/srv/git/repo.git",
            "C:\\repos\\repo.git",
            "C:/repos/repo.git",
            "file:///srv/repo.git",
            "http://forge.example.com/o/r",
            "git@forge.example.com:repo",
            "git@forge.example.com:a/b/c",
            "git@forge.example.com:/o/",
            "ssh://git@forge.example.com",
            "ssh://git@forge.example.com:notaport/o/r",
            "git@:o/r",
            "git@forge.example.com:o\n/r",
        ] {
            assert!(derive_forge_target(remote).is_err(), "{remote:?}");
        }
    }

    #[test]
    fn extracts_the_host_for_the_same_host_rule() {
        assert_eq!(
            url_host("https://Forge.Example.com:8443/sub").as_deref(),
            Some("forge.example.com")
        );
        assert_eq!(
            url_host("http://127.0.0.1:3000").as_deref(),
            Some("127.0.0.1")
        );
        assert_eq!(
            url_host("https://user@evil.example/").as_deref(),
            Some("evil.example")
        );
        assert_eq!(url_host("not a url"), None);
    }
}
