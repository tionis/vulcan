use crate::config::LinkResolutionMode;
use crate::parser::LinkKind;
use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolverDocument {
    pub id: String,
    pub path: String,
    pub filename: String,
    pub aliases: Vec<String>,
}

/// Pre-computed document with normalized paths for fast lookup.
struct IndexedDoc {
    id: String,
    directory: String,
}

/// Pre-built index for O(1) link resolution instead of O(N) document scans.
pub struct ResolverIndex {
    docs: Vec<IndexedDoc>,
    /// Exact normalized path (with and without .md extension) → doc index.
    by_path: HashMap<String, usize>,
    /// Lowercase filename → list of doc indices.
    by_filename: HashMap<String, Vec<usize>>,
    /// Lowercase alias → list of doc indices.
    by_alias: HashMap<String, Vec<usize>>,
    /// Normalized path suffix (everything after last `/`) → list of doc indices.
    /// Used for suffix matching in shortest mode.
    by_path_suffix: HashMap<String, Vec<usize>>,
}

impl ResolverIndex {
    #[must_use]
    pub fn build(documents: &[ResolverDocument]) -> Self {
        let mut docs = Vec::with_capacity(documents.len());
        let mut by_path = HashMap::with_capacity(documents.len() * 2);
        let mut by_filename: HashMap<String, Vec<usize>> = HashMap::new();
        let mut by_alias: HashMap<String, Vec<usize>> = HashMap::new();
        let mut by_path_suffix: HashMap<String, Vec<usize>> = HashMap::new();

        for (index, document) in documents.iter().enumerate() {
            let normalized_path = normalize_path(&document.path);
            let normalized_path_no_ext = strip_markdown_extension(&normalized_path);
            let directory = source_directory_from_normalized(&normalized_path);
            let filename_lower = document.filename.to_ascii_lowercase();
            let aliases_lower = document
                .aliases
                .iter()
                .map(|a| a.to_ascii_lowercase())
                .collect::<Vec<_>>();

            by_path.insert(normalized_path.clone(), index);
            if normalized_path != normalized_path_no_ext {
                by_path.insert(normalized_path_no_ext.clone(), index);
            }

            by_filename
                .entry(filename_lower.clone())
                .or_default()
                .push(index);

            for alias in &aliases_lower {
                by_alias.entry(alias.clone()).or_default().push(index);
            }

            // Build suffix entries for each path segment boundary.
            // e.g. "a/b/c.md" → suffixes "a/b/c.md", "b/c.md", "c.md"
            // and without extension: "a/b/c", "b/c", "c"
            for path_variant in [&normalized_path, &normalized_path_no_ext] {
                let mut remaining = path_variant.as_str();
                while let Some(slash_pos) = remaining.find('/') {
                    remaining = &remaining[slash_pos + 1..];
                    by_path_suffix
                        .entry(remaining.to_string())
                        .or_default()
                        .push(index);
                }
            }

            docs.push(IndexedDoc {
                id: document.id.clone(),
                directory,
            });
        }

        Self {
            docs,
            by_path,
            by_filename,
            by_alias,
            by_path_suffix,
        }
    }

    #[must_use]
    pub fn resolve(&self, link: &ResolverLink, mode: LinkResolutionMode) -> LinkResolutionResult {
        if matches!(link.link_kind, LinkKind::External) {
            return LinkResolutionResult {
                resolved_target_id: None,
                problem: None,
            };
        }

        let Some(target) = link.target_path_candidate.as_deref() else {
            return LinkResolutionResult {
                resolved_target_id: Some(link.source_document_id.clone()),
                problem: None,
            };
        };

        match mode {
            LinkResolutionMode::Absolute => self.resolve_absolute_indexed(target),
            LinkResolutionMode::Relative => {
                self.resolve_relative_indexed(&link.source_path, target)
            }
            LinkResolutionMode::Shortest => {
                self.resolve_shortest_indexed(&link.source_path, target)
            }
        }
    }

    fn resolve_absolute_indexed(&self, target: &str) -> LinkResolutionResult {
        let normalized = normalize_path(target);
        if let Some(&idx) = self.by_path.get(&normalized) {
            return LinkResolutionResult {
                resolved_target_id: Some(self.docs[idx].id.clone()),
                problem: None,
            };
        }
        LinkResolutionResult {
            resolved_target_id: None,
            problem: Some(LinkResolutionProblem::Unresolved),
        }
    }

    fn resolve_relative_indexed(&self, source_path: &str, target: &str) -> LinkResolutionResult {
        let source_dir = source_directory(source_path);
        let normalized = normalize_joined_path(&source_dir, target);
        if let Some(&idx) = self.by_path.get(&normalized) {
            return LinkResolutionResult {
                resolved_target_id: Some(self.docs[idx].id.clone()),
                problem: None,
            };
        }
        LinkResolutionResult {
            resolved_target_id: None,
            problem: Some(LinkResolutionProblem::Unresolved),
        }
    }

    fn resolve_shortest_indexed(&self, source_path: &str, target: &str) -> LinkResolutionResult {
        let target_normalized = normalize_path(target);
        let target_name = file_name_without_extension(&target_normalized);
        let target_name_lower = target_name.to_ascii_lowercase();
        let target_lower = target.to_ascii_lowercase();
        let source_dir = source_directory(source_path);
        let mut scored: Vec<(usize, &str)> = Vec::new();

        // 1. Exact path match → score 0 (instant HashMap lookup).
        if let Some(&idx) = self.by_path.get(&target_normalized) {
            return LinkResolutionResult {
                resolved_target_id: Some(self.docs[idx].id.clone()),
                problem: None,
            };
        }

        // 2. Path suffix match → score 2 + distance.
        if let Some(indices) = self.by_path_suffix.get(&target_normalized) {
            for &idx in indices {
                let doc = &self.docs[idx];
                let distance = folder_distance_precomputed(&source_dir, &doc.directory);
                scored.push((2 + distance, &doc.id));
            }
        }
        // Also check suffix without extension.
        let target_normalized_no_ext = strip_markdown_extension(&target_normalized);
        if target_normalized_no_ext != target_normalized {
            if let Some(indices) = self.by_path_suffix.get(&target_normalized_no_ext) {
                for &idx in indices {
                    let doc = &self.docs[idx];
                    let distance = folder_distance_precomputed(&source_dir, &doc.directory);
                    scored.push((2 + distance, &doc.id));
                }
            }
        }

        // 3. Filename match (case-insensitive) → score 4 or 10 + distance.
        if let Some(indices) = self.by_filename.get(&target_name_lower) {
            for &idx in indices {
                let doc = &self.docs[idx];
                let score = if doc.directory == source_dir {
                    4
                } else {
                    10 + folder_distance_precomputed(&source_dir, &doc.directory)
                };
                scored.push((score, &doc.id));
            }
        }

        // 4. Alias match (case-insensitive) → score 20 + distance.
        if let Some(indices) = self.by_alias.get(&target_lower) {
            for &idx in indices {
                let doc = &self.docs[idx];
                let distance = folder_distance_precomputed(&source_dir, &doc.directory);
                scored.push((20 + distance, &doc.id));
            }
        }

        // Deduplicate by document id (a doc could match via multiple paths).
        scored.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(b.1)));
        scored.dedup_by(|a, b| a.1 == b.1);

        match scored.first() {
            None => LinkResolutionResult {
                resolved_target_id: None,
                problem: Some(LinkResolutionProblem::Unresolved),
            },
            Some(&(best_score, best_id)) => {
                let tied: Vec<String> = scored
                    .iter()
                    .filter(|(score, _)| *score == best_score)
                    .map(|(_, id)| (*id).to_string())
                    .collect();

                if tied.len() == 1 {
                    LinkResolutionResult {
                        resolved_target_id: Some(best_id.to_string()),
                        problem: None,
                    }
                } else {
                    LinkResolutionResult {
                        resolved_target_id: None,
                        problem: Some(LinkResolutionProblem::Ambiguous(tied)),
                    }
                }
            }
        }
    }
}

/// Compute folder distance from pre-normalized directory strings.
fn folder_distance_precomputed(left: &str, right: &str) -> usize {
    if left == right {
        return 0;
    }
    let left_parts: Vec<&str> = left.split('/').filter(|s| !s.is_empty()).collect();
    let right_parts: Vec<&str> = right.split('/').filter(|s| !s.is_empty()).collect();
    let shared = left_parts
        .iter()
        .zip(right_parts.iter())
        .take_while(|(l, r)| l == r)
        .count();
    (left_parts.len() - shared) + (right_parts.len() - shared)
}

/// Extract directory from an already-normalized path.
fn source_directory_from_normalized(normalized_path: &str) -> String {
    normalized_path
        .rsplit_once('/')
        .map_or_else(String::new, |(dir, _)| dir.to_string())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolverLink {
    pub source_document_id: String,
    pub source_path: String,
    pub target_path_candidate: Option<String>,
    pub link_kind: LinkKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkResolutionProblem {
    Unresolved,
    Ambiguous(Vec<String>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkResolutionResult {
    pub resolved_target_id: Option<String>,
    pub problem: Option<LinkResolutionProblem>,
}

#[must_use]
pub fn resolve_link(
    documents: &[ResolverDocument],
    link: &ResolverLink,
    mode: LinkResolutionMode,
) -> LinkResolutionResult {
    if matches!(link.link_kind, LinkKind::External) {
        return LinkResolutionResult {
            resolved_target_id: None,
            problem: None,
        };
    }

    let Some(target) = link.target_path_candidate.as_deref() else {
        return LinkResolutionResult {
            resolved_target_id: Some(link.source_document_id.clone()),
            problem: None,
        };
    };

    match mode {
        LinkResolutionMode::Absolute => resolve_absolute(documents, target),
        LinkResolutionMode::Relative => resolve_relative(documents, &link.source_path, target),
        LinkResolutionMode::Shortest => resolve_shortest(documents, &link.source_path, target),
    }
}

fn resolve_absolute(documents: &[ResolverDocument], target: &str) -> LinkResolutionResult {
    let normalized = normalize_path(target);
    let matches = documents
        .iter()
        .filter(|document| matches_exact_path(document, &normalized))
        .map(|document| document.id.clone())
        .collect::<Vec<_>>();

    finalize_matches(matches)
}

fn resolve_relative(
    documents: &[ResolverDocument],
    source_path: &str,
    target: &str,
) -> LinkResolutionResult {
    let source_dir = source_directory(source_path);
    let normalized = normalize_joined_path(&source_dir, target);
    let matches = documents
        .iter()
        .filter(|document| matches_exact_path(document, &normalized))
        .map(|document| document.id.clone())
        .collect::<Vec<_>>();

    finalize_matches(matches)
}

fn resolve_shortest(
    documents: &[ResolverDocument],
    source_path: &str,
    target: &str,
) -> LinkResolutionResult {
    let target_normalized = normalize_path(target);
    let target_name = file_name_without_extension(&target_normalized);
    let source_dir = source_directory(source_path);
    let mut scored = Vec::new();

    for document in documents {
        let document_path = normalize_path(&document.path);
        let document_path_no_ext = strip_markdown_extension(&document_path);
        let document_dir = source_directory(&document.path);
        let distance = folder_distance(&source_dir, &document_dir);

        let score =
            if document_path == target_normalized || document_path_no_ext == target_normalized {
                0
            } else if document_path.ends_with(&format!("/{target_normalized}"))
                || document_path_no_ext.ends_with(&format!("/{target_normalized}"))
            {
                2 + distance
            } else if document.filename.eq_ignore_ascii_case(&target_name) {
                if document_dir == source_dir {
                    4
                } else {
                    10 + distance
                }
            } else if document
                .aliases
                .iter()
                .any(|alias| alias.eq_ignore_ascii_case(target))
            {
                20 + distance
            } else {
                continue;
            };

        scored.push((score, document.id.clone()));
    }

    scored.sort_by(|left, right| left.0.cmp(&right.0).then(left.1.cmp(&right.1)));

    match scored.first() {
        None => LinkResolutionResult {
            resolved_target_id: None,
            problem: Some(LinkResolutionProblem::Unresolved),
        },
        Some((best_score, best_id)) => {
            let tied = scored
                .iter()
                .filter(|(score, _)| score == best_score)
                .map(|(_, id)| id.clone())
                .collect::<Vec<_>>();

            if tied.len() == 1 {
                LinkResolutionResult {
                    resolved_target_id: Some(best_id.clone()),
                    problem: None,
                }
            } else {
                LinkResolutionResult {
                    resolved_target_id: None,
                    problem: Some(LinkResolutionProblem::Ambiguous(tied)),
                }
            }
        }
    }
}

fn matches_exact_path(document: &ResolverDocument, target: &str) -> bool {
    let document_path = normalize_path(&document.path);
    let document_path_no_ext = strip_markdown_extension(&document_path);

    document_path == target || document_path_no_ext == target
}

fn finalize_matches(matches: Vec<String>) -> LinkResolutionResult {
    match matches.as_slice() {
        [] => LinkResolutionResult {
            resolved_target_id: None,
            problem: Some(LinkResolutionProblem::Unresolved),
        },
        [single] => LinkResolutionResult {
            resolved_target_id: Some(single.clone()),
            problem: None,
        },
        _ => LinkResolutionResult {
            resolved_target_id: None,
            problem: Some(LinkResolutionProblem::Ambiguous(matches)),
        },
    }
}

fn source_directory(path: &str) -> String {
    normalize_path(path)
        .rsplit_once('/')
        .map_or_else(String::new, |(dir, _)| dir.to_string())
}

fn normalize_joined_path(base_dir: &str, target: &str) -> String {
    let mut joined = PathBuf::from(base_dir);
    joined.push(target);
    normalize_path(&joined.to_string_lossy())
}

fn normalize_path(path: &str) -> String {
    let mut parts = Vec::new();
    for component in Path::new(path).components() {
        match component {
            Component::CurDir | Component::Prefix(_) | Component::RootDir => {}
            Component::ParentDir => {
                parts.pop();
            }
            Component::Normal(part) => parts.push(percent_decode_lossy(&part.to_string_lossy())),
        }
    }

    parts.join("/")
}

fn percent_decode_lossy(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;

    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            if let (Some(high), Some(low)) =
                (hex_value(bytes[index + 1]), hex_value(bytes[index + 2]))
            {
                decoded.push((high << 4) | low);
                index += 3;
                continue;
            }
        }

        decoded.push(bytes[index]);
        index += 1;
    }

    String::from_utf8(decoded)
        .unwrap_or_else(|error| String::from_utf8_lossy(error.as_bytes()).into_owned())
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn strip_markdown_extension(path: &str) -> String {
    if let Some(stripped) = path.strip_suffix(".md") {
        stripped.to_string()
    } else {
        path.to_string()
    }
}

fn file_name_without_extension(path: &str) -> String {
    Path::new(path)
        .file_stem()
        .or_else(|| Path::new(path).file_name())
        .map_or_else(String::new, |value| value.to_string_lossy().into_owned())
}

fn folder_distance(left: &str, right: &str) -> usize {
    let left_normalized = normalize_path(left);
    let right_normalized = normalize_path(right);
    let left_parts = left_normalized
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>();
    let right_parts = right_normalized
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>();
    let shared_prefix = left_parts
        .iter()
        .zip(right_parts.iter())
        .take_while(|(left, right)| left == right)
        .count();

    (left_parts.len() - shared_prefix) + (right_parts.len() - shared_prefix)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::LinkResolutionMode;
    use crate::parser::LinkKind;

    #[test]
    fn shortest_prefers_same_folder_match() {
        let documents = fixture_documents();
        let link = ResolverLink {
            source_document_id: "source".to_string(),
            source_path: "projects/source.md".to_string(),
            target_path_candidate: Some("Topic".to_string()),
            link_kind: LinkKind::Wikilink,
        };

        let result = resolve_link(&documents, &link, LinkResolutionMode::Shortest);

        assert_eq!(result.resolved_target_id.as_deref(), Some("projects-topic"));
    }

    #[test]
    fn absolute_requires_full_vault_relative_path() {
        let documents = fixture_documents();
        let link = ResolverLink {
            source_document_id: "source".to_string(),
            source_path: "projects/source.md".to_string(),
            target_path_candidate: Some("archive/Topic".to_string()),
            link_kind: LinkKind::Wikilink,
        };

        let result = resolve_link(&documents, &link, LinkResolutionMode::Absolute);

        assert_eq!(result.resolved_target_id.as_deref(), Some("archive-topic"));
    }

    #[test]
    fn relative_resolves_against_source_directory() {
        let documents = fixture_documents();
        let link = ResolverLink {
            source_document_id: "source".to_string(),
            source_path: "projects/source.md".to_string(),
            target_path_candidate: Some("../archive/Topic".to_string()),
            link_kind: LinkKind::Markdown,
        };

        let result = resolve_link(&documents, &link, LinkResolutionMode::Relative);

        assert_eq!(result.resolved_target_id.as_deref(), Some("archive-topic"));
    }

    #[test]
    fn percent_encoded_paths_are_decoded_before_resolution() {
        let documents = vec![ResolverDocument {
            id: "ssh-ca".to_string(),
            path: "notes/SSH CA.md".to_string(),
            filename: "SSH CA".to_string(),
            aliases: Vec::new(),
        }];
        let link = ResolverLink {
            source_document_id: "source".to_string(),
            source_path: "notes/index.md".to_string(),
            target_path_candidate: Some("notes/SSH%20CA.md".to_string()),
            link_kind: LinkKind::Markdown,
        };

        let result = resolve_link(&documents, &link, LinkResolutionMode::Absolute);

        assert_eq!(result.resolved_target_id.as_deref(), Some("ssh-ca"));
    }

    #[test]
    fn alias_resolution_works() {
        let documents = fixture_documents();
        let link = ResolverLink {
            source_document_id: "source".to_string(),
            source_path: "source.md".to_string(),
            target_path_candidate: Some("Second Name".to_string()),
            link_kind: LinkKind::Wikilink,
        };

        let result = resolve_link(&documents, &link, LinkResolutionMode::Shortest);

        assert_eq!(result.resolved_target_id.as_deref(), Some("alias-target"));
    }

    #[test]
    fn ambiguous_results_are_reported() {
        let documents = vec![
            ResolverDocument {
                id: "one".to_string(),
                path: "one/Note.md".to_string(),
                filename: "Note".to_string(),
                aliases: Vec::new(),
            },
            ResolverDocument {
                id: "two".to_string(),
                path: "two/Note.md".to_string(),
                filename: "Note".to_string(),
                aliases: Vec::new(),
            },
        ];
        let link = ResolverLink {
            source_document_id: "source".to_string(),
            source_path: "source.md".to_string(),
            target_path_candidate: Some("Note".to_string()),
            link_kind: LinkKind::Wikilink,
        };

        let result = resolve_link(&documents, &link, LinkResolutionMode::Shortest);

        assert!(matches!(
            result.problem,
            Some(LinkResolutionProblem::Ambiguous(_))
        ));
    }

    #[test]
    fn missing_targets_remain_unresolved() {
        let documents = fixture_documents();
        let link = ResolverLink {
            source_document_id: "source".to_string(),
            source_path: "source.md".to_string(),
            target_path_candidate: Some("Missing".to_string()),
            link_kind: LinkKind::Wikilink,
        };

        let result = resolve_link(&documents, &link, LinkResolutionMode::Shortest);

        assert_eq!(result.resolved_target_id, None);
        assert_eq!(result.problem, Some(LinkResolutionProblem::Unresolved));
    }

    fn fixture_documents() -> Vec<ResolverDocument> {
        vec![
            ResolverDocument {
                id: "projects-topic".to_string(),
                path: "projects/Topic.md".to_string(),
                filename: "Topic".to_string(),
                aliases: Vec::new(),
            },
            ResolverDocument {
                id: "archive-topic".to_string(),
                path: "archive/Topic.md".to_string(),
                filename: "Topic".to_string(),
                aliases: Vec::new(),
            },
            ResolverDocument {
                id: "alias-target".to_string(),
                path: "aliases/Target.md".to_string(),
                filename: "Target".to_string(),
                aliases: vec!["Second Name".to_string()],
            },
        ]
    }
}
