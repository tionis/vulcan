//! Bounded MCP access to non-note vault files: reading any file in slices,
//! and writing attachments and other non-Markdown files.

use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use base64::Engine;
use serde::Serialize;
use vulcan_core::paths::{normalize_relative_input_path, RelativePathOptions};
use vulcan_core::{PermissionGuard, ProfilePermissionGuard, VaultPaths, Verbosity};

use crate::commit::AutoCommitPolicy;
use crate::mcp_access::{check_destructive_access, check_write_path_access};
use crate::mcp_protocol::{McpFileGetArgs, McpFilePutArgs, McpMethodError};

pub const FILE_GET_DEFAULT_BYTES: usize = 256 * 1024;
pub const FILE_GET_MAX_BYTES: usize = 1024 * 1024;
pub const FILE_PUT_MAX_BYTES: usize = 10 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FileGetReport {
    pub path: String,
    /// Size of the whole file in bytes.
    pub size: u64,
    pub offset: u64,
    /// Bytes in this slice.
    pub length: usize,
    /// `utf8` when the slice is valid UTF-8, otherwise `base64`.
    pub encoding: &'static str,
    pub content: String,
    /// BLAKE3 of the whole file; `file_put.expected_hash` compares it.
    pub content_hash: String,
    /// Where the next slice starts, when the file continues.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_offset: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FilePutReport {
    pub path: String,
    pub dry_run: bool,
    pub created: bool,
    pub size: usize,
    pub content_hash: String,
}

fn tool_error(error: impl std::fmt::Display) -> McpMethodError {
    McpMethodError::tool(error.to_string())
}

/// A vault-relative path that is neither Vulcan's state nor Git's.
fn vault_file_path(path: &str) -> Result<String, McpMethodError> {
    let normalized = normalize_relative_input_path(
        path,
        RelativePathOptions {
            expected_extension: None,
            append_extension_if_missing: false,
        },
    )
    .map_err(|error| McpMethodError::invalid_params(error.to_string()))?;
    let first = normalized.split('/').next().unwrap_or_default();
    if matches!(first, ".vulcan" | ".git") {
        return Err(McpMethodError::tool(format!(
            "`{normalized}` is internal state, not a vault file"
        )));
    }
    Ok(normalized)
}

fn hash_file(file: &mut std::fs::File) -> Result<String, McpMethodError> {
    file.seek(SeekFrom::Start(0)).map_err(tool_error)?;
    let mut hasher = blake3::Hasher::new();
    std::io::copy(file, &mut hasher).map_err(tool_error)?;
    Ok(hasher.finalize().to_hex().to_string())
}

pub fn file_get(
    paths: &VaultPaths,
    guard: &ProfilePermissionGuard,
    args: &McpFileGetArgs,
) -> Result<FileGetReport, McpMethodError> {
    let path = vault_file_path(&args.path)?;
    guard.check_read_path(&path).map_err(tool_error)?;
    let mut file = vulcan_core::paths::secure_open_read(paths.vault_root(), Path::new(&path))
        .map_err(tool_error)?;
    let metadata = file.metadata().map_err(tool_error)?;
    if !metadata.is_file() {
        return Err(McpMethodError::tool(format!(
            "`{path}` is not a regular file"
        )));
    }
    let size = metadata.len();
    let content_hash = hash_file(&mut file)?;
    let offset = args.offset.unwrap_or(0).min(size);
    let limit = args
        .limit_bytes
        .unwrap_or(FILE_GET_DEFAULT_BYTES)
        .clamp(1, FILE_GET_MAX_BYTES);
    file.seek(SeekFrom::Start(offset)).map_err(tool_error)?;
    let mut slice = Vec::with_capacity(limit.min(usize::try_from(size).unwrap_or(limit)));
    file.by_ref()
        .take(limit as u64)
        .read_to_end(&mut slice)
        .map_err(tool_error)?;
    let end = offset + slice.len() as u64;
    let (encoding, content) = match String::from_utf8(slice) {
        Ok(text) => ("utf8", text),
        Err(error) => (
            "base64",
            base64::engine::general_purpose::STANDARD.encode(error.as_bytes()),
        ),
    };
    let length = if encoding == "utf8" {
        content.len()
    } else {
        usize::try_from(end - offset).unwrap_or_default()
    };
    Ok(FileGetReport {
        path,
        size,
        offset,
        length,
        encoding,
        content,
        content_hash,
        next_offset: (end < size).then_some(end),
    })
}

/// The largest file a `vulcan://file/{path}` read returns.
const FILE_RESOURCE_MAX_BYTES: u64 = 16 * 1024 * 1024;

/// Encode a vault path for a `vulcan://note/` or `vulcan://file/` URI:
/// everything but unreserved characters and `/` is percent-encoded.
#[must_use]
pub fn vault_path_uri(scheme_prefix: &str, path: &str) -> String {
    use std::fmt::Write as _;

    let mut uri = String::from(scheme_prefix);
    for byte in path.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~' | b'/') {
            uri.push(char::from(byte));
        } else {
            let _ = write!(uri, "%{byte:02X}");
        }
    }
    uri
}

fn decode_uri_path(encoded: &str) -> Option<String> {
    let bytes = encoded.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let hex = encoded.get(index + 1..index + 3)?;
            decoded.push(u8::from_str_radix(hex, 16).ok()?);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded).ok()
}

/// Read a `vulcan://note/{path}` or `vulcan://file/{path}` resource under the
/// caller's read permission. `None` when the URI is neither.
#[must_use]
pub fn read_vault_resource(
    paths: &VaultPaths,
    guard: &ProfilePermissionGuard,
    uri: &str,
) -> Option<Result<serde_json::Value, McpMethodError>> {
    let (note, encoded) = if let Some(rest) = uri.strip_prefix("vulcan://note/") {
        (true, rest)
    } else {
        (false, uri.strip_prefix("vulcan://file/")?)
    };
    Some(read_vault_resource_at(paths, guard, uri, note, encoded))
}

fn read_vault_resource_at(
    paths: &VaultPaths,
    guard: &ProfilePermissionGuard,
    uri: &str,
    note: bool,
    encoded: &str,
) -> Result<serde_json::Value, McpMethodError> {
    let not_found = || McpMethodError::JsonRpc {
        code: crate::mcp_protocol::MCP_RESOURCE_NOT_FOUND,
        message: format!("Resource not found: {uri}"),
        data: Some(serde_json::json!({ "uri": uri })),
    };
    let path = decode_uri_path(encoded).ok_or_else(not_found)?;
    let path = vault_file_path(&path).map_err(|_| not_found())?;
    if note
        && Path::new(&path)
            .extension()
            .is_none_or(|extension| extension != "md")
    {
        return Err(not_found());
    }
    // A path the caller may not read looks like one that does not exist.
    guard.check_read_path(&path).map_err(|_| not_found())?;
    let mut file = vulcan_core::paths::secure_open_read(paths.vault_root(), Path::new(&path))
        .map_err(|_| not_found())?;
    let metadata = file.metadata().map_err(tool_error)?;
    if !metadata.is_file() {
        return Err(not_found());
    }
    if metadata.len() > FILE_RESOURCE_MAX_BYTES {
        return Err(McpMethodError::tool(format!(
            "`{path}` is larger than {FILE_RESOURCE_MAX_BYTES} bytes; read it in parts with file_get"
        )));
    }
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).map_err(tool_error)?;
    let content = match String::from_utf8(bytes) {
        Ok(text) => serde_json::json!({
            "uri": uri,
            "mimeType": if note { "text/markdown" } else { text_mime_type(&path) },
            "text": text,
        }),
        Err(error) => serde_json::json!({
            "uri": uri,
            "mimeType": "application/octet-stream",
            "blob": base64::engine::general_purpose::STANDARD.encode(error.as_bytes()),
        }),
    };
    Ok(serde_json::json!({ "contents": [content] }))
}

fn text_mime_type(path: &str) -> &'static str {
    match Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("md" | "markdown") => "text/markdown",
        Some("json" | "base" | "canvas") => "application/json",
        Some("yaml" | "yml") => "application/yaml",
        Some("csv") => "text/csv",
        Some("html" | "htm") => "text/html",
        _ => "text/plain",
    }
}

pub fn file_put(
    paths: &VaultPaths,
    guard: &ProfilePermissionGuard,
    profile_name: &str,
    args: &McpFilePutArgs,
) -> Result<FilePutReport, McpMethodError> {
    let path = vault_file_path(&args.path)?;
    if Path::new(&path)
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("md"))
    {
        return Err(McpMethodError::tool(
            "use note_create or note_set for Markdown notes, which keep links, validation, and the index consistent",
        ));
    }
    check_write_path_access(guard, &path)?;
    let bytes = match args.encoding.as_deref().unwrap_or("utf8") {
        "utf8" => args.content.as_bytes().to_vec(),
        "base64" => base64::engine::general_purpose::STANDARD
            .decode(args.content.trim())
            .map_err(|error| McpMethodError::invalid_params(format!("invalid base64: {error}")))?,
        other => {
            return Err(McpMethodError::invalid_params(format!(
                "unsupported `file_put.encoding`: {other}"
            )))
        }
    };
    if bytes.len() > FILE_PUT_MAX_BYTES {
        return Err(McpMethodError::tool(format!(
            "the file is {} bytes; file_put accepts at most {FILE_PUT_MAX_BYTES}",
            bytes.len()
        )));
    }
    let root = paths.vault_root();
    let lock = (!args.dry_run)
        .then(|| vulcan_core::write_lock::acquire_write_lock(paths))
        .transpose()
        .map_err(tool_error)?;
    let existing = match vulcan_core::paths::secure_open_read(root, Path::new(&path)) {
        Ok(mut file) => Some(hash_file(&mut file)?),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(tool_error(error)),
    };
    match (&existing, args.expected_hash.as_deref()) {
        (Some(current), Some(expected)) if !current.eq_ignore_ascii_case(expected.trim()) => {
            return Err(McpMethodError::tool(format!(
                "stale_content: `{path}` changed since it was read (expected content hash {expected}, now {current})"
            )));
        }
        (None, Some(_)) => {
            return Err(McpMethodError::tool(format!(
                "stale_content: `{path}` no longer exists"
            )));
        }
        (Some(_), None) if !args.overwrite => {
            return Err(McpMethodError::tool(format!(
                "`{path}` exists; pass overwrite (ideally with expected_hash) to replace it"
            )));
        }
        (Some(_), _) => check_destructive_access(guard)?,
        _ => {}
    }
    let report = FilePutReport {
        path: path.clone(),
        dry_run: args.dry_run,
        created: existing.is_none(),
        size: bytes.len(),
        content_hash: blake3::hash(&bytes).to_hex().to_string(),
    };
    if args.dry_run {
        return Ok(report);
    }
    vulcan_core::paths::secure_write_atomic(root, Path::new(&path), &bytes).map_err(tool_error)?;
    drop(lock);
    crate::scan::refresh_cache_for_paths(paths, std::slice::from_ref(&path)).map_err(tool_error)?;
    AutoCommitPolicy::for_mutation(paths, args.no_commit)
        .commit(
            paths,
            "file-put",
            std::slice::from_ref(&path),
            Some(profile_name),
            Verbosity::Quiet,
        )
        .map_err(|error| McpMethodError::tool(error.clone()))?;
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vault_resources_read_notes_and_files_under_the_read_permission() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path();
        let paths = VaultPaths::new(root);
        vulcan_core::paths::initialize_vulcan_dir(&paths).unwrap();
        std::fs::write(
            paths.config_file(),
            "[permissions.profiles.scoped]\nread = { allow = [\"folder:Public/**\"] }\n",
        )
        .unwrap();
        std::fs::create_dir_all(root.join("Public")).unwrap();
        std::fs::write(root.join("Public/My Note.md"), "# Mine\n").unwrap();
        std::fs::write(root.join("Public/data.bin"), [0_u8, 159, 146, 150]).unwrap();
        std::fs::write(root.join("Secret.md"), "# Secret\n").unwrap();
        let guard = |profile: &str| {
            ProfilePermissionGuard::new(
                &paths,
                vulcan_core::resolve_permission_profile(&paths, Some(profile)).unwrap(),
            )
        };
        let scoped = guard("scoped");
        let uri = vault_path_uri("vulcan://note/", "Public/My Note.md");
        assert_eq!(uri, "vulcan://note/Public/My%20Note.md");
        let note = read_vault_resource(&paths, &scoped, &uri).unwrap().unwrap();
        assert_eq!(note["contents"][0]["mimeType"], "text/markdown");
        assert_eq!(note["contents"][0]["text"], "# Mine\n");
        let binary = read_vault_resource(&paths, &scoped, "vulcan://file/Public/data.bin")
            .unwrap()
            .unwrap();
        assert!(binary["contents"][0]["blob"].is_string());
        for hidden in [
            "vulcan://note/Secret.md",
            "vulcan://note/Public/data.bin",
            "vulcan://file/.vulcan/config.toml",
            "vulcan://note/../outside.md",
        ] {
            assert!(
                matches!(
                    read_vault_resource(&paths, &scoped, hidden),
                    Some(Err(McpMethodError::JsonRpc { .. }))
                ),
                "{hidden}"
            );
        }
        assert!(read_vault_resource(&paths, &scoped, "vulcan://help/overview").is_none());
    }
    use vulcan_core::paths::initialize_vulcan_dir;
    use vulcan_core::resolve_permission_profile;

    fn put(path: &str, content: &str) -> McpFilePutArgs {
        McpFilePutArgs {
            path: path.to_string(),
            content: content.to_string(),
            encoding: None,
            overwrite: false,
            expected_hash: None,
            dry_run: false,
            no_commit: true,
        }
    }

    #[test]
    fn files_are_read_in_slices_and_written_with_guards() {
        let temporary = tempfile::tempdir().unwrap();
        let paths = VaultPaths::new(temporary.path());
        initialize_vulcan_dir(&paths).unwrap();
        let guard = ProfilePermissionGuard::new(
            &paths,
            resolve_permission_profile(&paths, Some("unrestricted")).unwrap(),
        );

        let created = file_put(
            &paths,
            &guard,
            "unrestricted",
            &put("Attachments/a.txt", "hello world"),
        )
        .unwrap();
        assert!(created.created);
        let first = file_get(
            &paths,
            &guard,
            &McpFileGetArgs {
                path: "Attachments/a.txt".to_string(),
                offset: None,
                limit_bytes: Some(5),
            },
        )
        .unwrap();
        assert_eq!((first.encoding, first.content.as_str()), ("utf8", "hello"));
        assert_eq!(first.next_offset, Some(5));
        assert_eq!(first.content_hash, created.content_hash);

        // Existing files need overwrite, and a stale hash is refused.
        assert!(file_put(
            &paths,
            &guard,
            "unrestricted",
            &put("Attachments/a.txt", "x")
        )
        .is_err());
        let mut replace = put("Attachments/a.txt", "//4=");
        replace.encoding = Some("base64".to_string());
        replace.overwrite = true;
        replace.expected_hash = Some(blake3::hash(b"other").to_hex().to_string());
        assert!(file_put(&paths, &guard, "unrestricted", &replace).is_err());
        replace.expected_hash = Some(created.content_hash.clone());
        file_put(&paths, &guard, "unrestricted", &replace).unwrap();
        let binary = file_get(
            &paths,
            &guard,
            &McpFileGetArgs {
                path: "Attachments/a.txt".to_string(),
                offset: None,
                limit_bytes: None,
            },
        )
        .unwrap();
        assert_eq!(binary.size, 2);
        assert_eq!(binary.next_offset, None);
        assert_eq!(
            (binary.encoding, binary.content.as_str()),
            ("base64", "//4=")
        );

        // Notes and internal state are out of bounds.
        assert!(file_put(&paths, &guard, "unrestricted", &put("Note.md", "# no")).is_err());
        assert!(file_put(
            &paths,
            &guard,
            "unrestricted",
            &put(".vulcan/config.toml", "")
        )
        .is_err());
        assert!(file_get(
            &paths,
            &guard,
            &McpFileGetArgs {
                path: ".git/config".to_string(),
                offset: None,
                limit_bytes: None,
            },
        )
        .is_err());
    }

    #[test]
    fn a_readonly_profile_cannot_write_files() {
        let temporary = tempfile::tempdir().unwrap();
        let paths = VaultPaths::new(temporary.path());
        initialize_vulcan_dir(&paths).unwrap();
        let guard = ProfilePermissionGuard::new(
            &paths,
            resolve_permission_profile(&paths, Some("readonly")).unwrap(),
        );
        assert!(file_put(&paths, &guard, "readonly", &put("a.txt", "x")).is_err());
        assert!(!temporary.path().join("a.txt").exists());
    }
}
