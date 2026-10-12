//! Link resolution over the cache plus unsaved editor buffers.
//!
//! An editor sees two kinds of documents: those the cache indexed from
//! disk, and open buffers whose unsaved text may rename aliases or create
//! notes that do not exist on disk yet. Resolution merges both without ever
//! writing buffer state to the cache. Document ids here are vault-relative
//! paths, which identify cached files and unsaved buffers alike.

use crate::cache::CacheError;
use crate::config::LinkResolutionMode;
use crate::parser::RawLink;
use crate::resolver::{LinkResolutionResult, ResolverDocument, ResolverIndex, ResolverLink};
use crate::{CacheDatabase, VaultPaths};
use std::collections::HashMap;
use std::path::Path;

/// Every indexed file's identity at one store clock version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VaultIdentities {
    /// The `note_store_clock` version the identities were read at.
    pub clock_version: i64,
    /// Identities with `id` set to the vault-relative path.
    pub documents: Vec<ResolverDocument>,
}

/// The current store clock version, or `None` without a cache.
pub fn store_clock_version(paths: &VaultPaths) -> Result<Option<i64>, CacheError> {
    if !paths.cache_db().exists() {
        return Ok(None);
    }
    let database = CacheDatabase::open(paths)?;
    let version =
        database
            .connection()
            .query_row("SELECT version FROM note_store_clock", [], |row| row.get(0))?;
    Ok(Some(version))
}

/// Read every indexed file's identity in one read transaction.
pub fn load_vault_identities(paths: &VaultPaths) -> Result<VaultIdentities, CacheError> {
    if !paths.cache_db().exists() {
        return Ok(VaultIdentities {
            clock_version: 0,
            documents: Vec::new(),
        });
    }
    let database = CacheDatabase::open(paths)?;
    let connection = database.connection();
    connection.execute_batch("BEGIN")?;
    let result = (|| {
        let clock_version =
            connection.query_row("SELECT version FROM note_store_clock", [], |row| row.get(0))?;
        let mut statement =
            connection.prepare("SELECT path, filename, aliases FROM note_query ORDER BY path")?;
        let documents = statement
            .query_map([], |row| {
                let path: String = row.get(0)?;
                let aliases: String = row.get(2)?;
                Ok(ResolverDocument {
                    id: path.clone(),
                    path,
                    filename: row.get(1)?,
                    aliases: serde_json::from_str(&aliases).unwrap_or_default(),
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(VaultIdentities {
            clock_version,
            documents,
        })
    })();
    let _ = connection.execute_batch("ROLLBACK");
    result
}

/// An open buffer's identity, taken from its unsaved text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OverlayIdentity {
    /// Vault-relative path of the buffer, which may not exist on disk.
    pub path: String,
    /// Aliases parsed from the buffer's frontmatter.
    pub aliases: Vec<String>,
}

/// A resolver over cached identities with open buffers substituted.
pub struct EditorResolver {
    documents: Vec<ResolverDocument>,
    index: ResolverIndex,
}

impl EditorResolver {
    /// Build from cached identities (ids are paths) and open buffers. A
    /// buffer replaces the cached identity at its path or adds a new one.
    #[must_use]
    pub fn new(cached: &[ResolverDocument], overlays: &[OverlayIdentity]) -> Self {
        let mut by_path = cached
            .iter()
            .map(|document| (document.path.clone(), document.clone()))
            .collect::<HashMap<_, _>>();
        for overlay in overlays {
            by_path.insert(
                overlay.path.clone(),
                ResolverDocument {
                    id: overlay.path.clone(),
                    path: overlay.path.clone(),
                    filename: file_stem(&overlay.path),
                    aliases: overlay.aliases.clone(),
                },
            );
        }
        let mut documents = by_path.into_values().collect::<Vec<_>>();
        documents.sort_by(|left, right| left.path.cmp(&right.path));
        let index = ResolverIndex::build(&documents);
        Self { documents, index }
    }

    #[must_use]
    pub fn documents(&self) -> &[ResolverDocument] {
        &self.documents
    }

    /// Resolve `link` written in `source_path`. The resolved id and any
    /// ambiguity candidates are vault-relative paths.
    #[must_use]
    pub fn resolve(
        &self,
        source_path: &str,
        link: &RawLink,
        mode: LinkResolutionMode,
    ) -> LinkResolutionResult {
        self.resolve_target(
            source_path,
            link.target_path_candidate.as_deref(),
            link.link_kind,
            mode,
        )
    }

    /// Resolve a bare target such as the note part of a typed `[[note#`.
    /// `None` means the source document itself.
    #[must_use]
    pub fn resolve_target(
        &self,
        source_path: &str,
        target: Option<&str>,
        link_kind: crate::parser::LinkKind,
        mode: LinkResolutionMode,
    ) -> LinkResolutionResult {
        self.index.resolve(
            &ResolverLink {
                source_document_id: source_path.to_string(),
                source_path: source_path.to_string(),
                target_path_candidate: target.map(ToString::to_string),
                link_kind,
            },
            mode,
        )
    }
}

fn file_stem(path: &str) -> String {
    Path::new(path)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or(path)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::LinkKind;
    use crate::resolver::LinkResolutionProblem;
    use crate::{parse_document, scan_vault, ScanMode, VaultConfig};

    fn document(path: &str, aliases: &[&str]) -> ResolverDocument {
        ResolverDocument {
            id: path.to_string(),
            path: path.to_string(),
            filename: file_stem(path),
            aliases: aliases.iter().map(ToString::to_string).collect(),
        }
    }

    fn link(source: &str) -> RawLink {
        parse_document(source, &VaultConfig::default())
            .links
            .remove(0)
    }

    #[test]
    fn overlays_replace_cached_identities_and_add_unsaved_notes() {
        let cached = [document("a/Alpha.md", &["Old"]), document("Beta.md", &[])];
        let resolver = EditorResolver::new(
            &cached,
            &[
                OverlayIdentity {
                    path: "a/Alpha.md".into(),
                    aliases: vec!["New".into()],
                },
                OverlayIdentity {
                    path: "Draft.md".into(),
                    aliases: Vec::new(),
                },
            ],
        );
        let mode = LinkResolutionMode::Shortest;
        let resolve_text = |text: &str| resolver.resolve("Beta.md", &link(text), mode);

        assert_eq!(
            resolve_text("[[New]]").resolved_target_id.as_deref(),
            Some("a/Alpha.md")
        );
        assert_eq!(
            resolve_text("[[Old]]").problem,
            Some(LinkResolutionProblem::Unresolved)
        );
        assert_eq!(
            resolve_text("[[Draft]]").resolved_target_id.as_deref(),
            Some("Draft.md")
        );
        assert_eq!(
            resolver
                .resolve_target("Beta.md", None, LinkKind::Wikilink, mode)
                .resolved_target_id
                .as_deref(),
            Some("Beta.md")
        );
    }

    #[test]
    fn loads_identities_from_the_cache_with_the_store_clock() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("Alpha.md"),
            "---\naliases: [First]\n---\n# Alpha\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("image.png"), [0_u8]).unwrap();
        let paths = VaultPaths::new(dir.path());
        assert_eq!(store_clock_version(&paths).unwrap(), None);
        assert!(load_vault_identities(&paths).unwrap().documents.is_empty());

        crate::paths::initialize_vulcan_dir(&paths).unwrap();
        scan_vault(&paths, ScanMode::Full).unwrap();
        let identities = load_vault_identities(&paths).unwrap();
        assert_eq!(
            Some(identities.clock_version),
            store_clock_version(&paths).unwrap()
        );
        let alpha = identities
            .documents
            .iter()
            .find(|document| document.path == "Alpha.md")
            .unwrap();
        assert_eq!(alpha.filename, "Alpha");
        assert_eq!(alpha.aliases, vec!["First".to_string()]);
        assert!(identities
            .documents
            .iter()
            .any(|document| document.path == "image.png"));
    }
}
