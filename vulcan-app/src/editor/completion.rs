//! Completion of link targets, headings, block ids, tags, and properties.

use super::{is_markdown_path, EditorRange, EditorSession};
use crate::AppError;
use std::collections::{BTreeMap, HashMap};
use vulcan_core::editor::{completion_context, CompletionContext, TextPosition};
use vulcan_core::LinkKind;

/// Most items returned for one request; the result is then incomplete.
const MAX_ITEMS: usize = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditorCompletionKind {
    Note,
    Alias,
    Attachment,
    Heading,
    Block,
    Tag,
    Property,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditorCompletionItem {
    pub label: String,
    pub kind: EditorCompletionKind,
    /// Text replacing the completion range.
    pub insert_text: String,
    /// Text clients filter on, when it differs from `label`.
    pub filter_text: Option<String>,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditorCompletion {
    /// The range every item's `insert_text` replaces.
    pub range: EditorRange,
    pub items: Vec<EditorCompletionItem>,
    /// More items match than were returned; clients should ask again as
    /// the user types.
    pub incomplete: bool,
}

impl EditorSession {
    /// Completion items for what is being typed at `position`.
    pub fn completion(
        &self,
        path: &str,
        position: TextPosition,
    ) -> Result<Option<EditorCompletion>, AppError> {
        let Some(document) = self.document(path) else {
            return Ok(None);
        };
        let Some(offset) = self.offset(path, position) else {
            return Ok(None);
        };
        let Some(context) = completion_context(&document.text, offset) else {
            return Ok(None);
        };
        let range = self.range(&document.text, &document.line_index, context.range());
        let items = match &context {
            CompletionContext::LinkTarget { query, .. } => self.link_target_items(query)?,
            CompletionContext::LinkHeading { note, query, .. } => {
                self.heading_items(path, note, query)?
            }
            CompletionContext::LinkBlock { note, query, .. } => {
                self.block_items(path, note, query)?
            }
            CompletionContext::Tag { query, .. } => self.tag_items(path, query),
            CompletionContext::PropertyKey { query, .. } => self.property_items(query),
            CompletionContext::PropertyValue { key, query, .. } => {
                if key == "tags" {
                    self.tag_items(path, query)
                        .into_iter()
                        .map(|mut item| {
                            item.insert_text = item.label.trim_start_matches('#').to_string();
                            item
                        })
                        .collect()
                } else {
                    Vec::new()
                }
            }
        };
        let incomplete = items.len() > MAX_ITEMS;
        let mut items = items;
        items.truncate(MAX_ITEMS);
        Ok(Some(EditorCompletion {
            range,
            items,
            incomplete,
        }))
    }

    fn link_target_items(&self, query: &str) -> Result<Vec<EditorCompletionItem>, AppError> {
        let resolver = self.resolver()?;
        let documents = resolver.documents();
        // A file name shared by several files needs its path to be unique.
        let mut name_counts = HashMap::<String, usize>::new();
        for document in documents {
            *name_counts
                .entry(link_name(&document.path).to_lowercase())
                .or_default() += 1;
        }
        let mut items = Vec::new();
        for document in documents {
            let name = link_name(&document.path);
            let target = if name_counts[&name.to_lowercase()] > 1 {
                link_path(&document.path)
            } else {
                name.clone()
            };
            let markdown = is_markdown_path(&document.path);
            if matches_query(query, &document.path) {
                items.push(EditorCompletionItem {
                    label: name.clone(),
                    kind: if markdown {
                        EditorCompletionKind::Note
                    } else {
                        EditorCompletionKind::Attachment
                    },
                    insert_text: target.clone(),
                    filter_text: Some(document.path.clone()),
                    detail: Some(document.path.clone()),
                });
            }
            for alias in &document.aliases {
                if matches_query(query, alias) {
                    items.push(EditorCompletionItem {
                        label: alias.clone(),
                        kind: EditorCompletionKind::Alias,
                        insert_text: format!("{target}|{alias}"),
                        filter_text: Some(alias.clone()),
                        detail: Some(format!("alias of {}", document.path)),
                    });
                }
            }
        }
        items.sort_by(|left, right| {
            rank(query, &left.label)
                .cmp(&rank(query, &right.label))
                .then_with(|| left.label.cmp(&right.label))
        });
        Ok(items)
    }

    fn heading_items(
        &self,
        path: &str,
        note: &str,
        query: &str,
    ) -> Result<Vec<EditorCompletionItem>, AppError> {
        let Some(view) = self.subpath_target(path, note)? else {
            return Ok(Vec::new());
        };
        Ok(view
            .parsed
            .headings
            .iter()
            .filter(|heading| matches_query(query, &heading.text))
            .map(|heading| EditorCompletionItem {
                label: heading.text.clone(),
                kind: EditorCompletionKind::Heading,
                insert_text: heading.text.clone(),
                filter_text: None,
                detail: Some("#".repeat(usize::from(heading.level))),
            })
            .collect())
    }

    fn block_items(
        &self,
        path: &str,
        note: &str,
        query: &str,
    ) -> Result<Vec<EditorCompletionItem>, AppError> {
        let Some(view) = self.subpath_target(path, note)? else {
            return Ok(Vec::new());
        };
        Ok(view
            .parsed
            .block_refs
            .iter()
            .filter(|block_ref| matches_query(query, &block_ref.block_id_text))
            .map(|block_ref| {
                let preview = view
                    .text
                    .get(block_ref.target_block_byte_start..block_ref.target_block_byte_end)
                    .and_then(|block| block.lines().next())
                    .map(|line| line.trim().to_string());
                EditorCompletionItem {
                    label: block_ref.block_id_text.clone(),
                    kind: EditorCompletionKind::Block,
                    insert_text: block_ref.block_id_text.clone(),
                    filter_text: None,
                    detail: preview,
                }
            })
            .collect())
    }

    /// The document a `[[note#...` subpath refers to; an empty note part is
    /// the current document.
    fn subpath_target(
        &self,
        path: &str,
        note: &str,
    ) -> Result<Option<super::DocumentView>, AppError> {
        let target = if note.is_empty() {
            Some(path.to_string())
        } else {
            self.resolver()?
                .resolve_target(
                    path,
                    Some(note),
                    LinkKind::Wikilink,
                    self.config().link_resolution,
                )
                .resolved_target_id
        };
        Ok(target.and_then(|target| self.view(&target)))
    }

    fn tag_items(&self, path: &str, query: &str) -> Vec<EditorCompletionItem> {
        let mut counts = BTreeMap::<String, usize>::new();
        if let Ok(tags) = vulcan_core::list_tags(self.paths()) {
            for tag in tags {
                counts.insert(tag.name, tag.count);
            }
        }
        if let Some(document) = self.document(path) {
            // The tag being typed is not a suggestion for itself.
            for tag in document
                .parsed
                .tags
                .iter()
                .filter(|tag| tag.tag_text != query)
            {
                counts.entry(tag.tag_text.clone()).or_insert(0);
            }
        }
        counts
            .into_iter()
            .filter(|(name, _)| matches_query(query, name))
            .map(|(name, count)| EditorCompletionItem {
                label: format!("#{name}"),
                kind: EditorCompletionKind::Tag,
                insert_text: name.clone(),
                filter_text: Some(name),
                detail: Some(format!("{count} note{}", if count == 1 { "" } else { "s" })),
            })
            .collect()
    }

    fn property_items(&self, query: &str) -> Vec<EditorCompletionItem> {
        vulcan_core::list_properties(self.paths())
            .unwrap_or_default()
            .into_iter()
            .filter(|entry| matches_query(query, &entry.key))
            .map(|entry| EditorCompletionItem {
                insert_text: format!("{}: ", entry.key),
                filter_text: Some(entry.key.clone()),
                detail: Some(format!(
                    "{} ({} notes)",
                    entry.types.join(" | "),
                    entry.count
                )),
                label: entry.key,
                kind: EditorCompletionKind::Property,
            })
            .collect()
    }
}

/// The name a wikilink uses for a file: the stem of a note, the full file
/// name of an attachment.
fn link_name(path: &str) -> String {
    let file = path.rsplit('/').next().unwrap_or(path);
    if is_markdown_path(path) {
        file[..file.len() - 3].to_string()
    } else {
        file.to_string()
    }
}

/// The vault path a wikilink uses for a file, without a note's `.md`.
fn link_path(path: &str) -> String {
    if is_markdown_path(path) {
        path[..path.len() - 3].to_string()
    } else {
        path.to_string()
    }
}

/// Whether every character of `query` appears in `candidate` in order,
/// ignoring case.
fn matches_query(query: &str, candidate: &str) -> bool {
    let mut candidate = candidate.chars().flat_map(char::to_lowercase);
    query
        .chars()
        .flat_map(char::to_lowercase)
        .all(|wanted| candidate.any(|character| character == wanted))
}

/// Sort key: prefix matches first, then substring matches, then the rest.
fn rank(query: &str, label: &str) -> u8 {
    let query = query.to_lowercase();
    let label = label.to_lowercase();
    if label.starts_with(&query) {
        0
    } else if label.contains(&query) {
        1
    } else {
        2
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::*;
    use super::*;

    fn labels(completion: &EditorCompletion) -> Vec<&str> {
        completion
            .items
            .iter()
            .map(|item| item.label.as_str())
            .collect()
    }

    /// Complete at the `|` marker in `text`.
    fn complete(session: &mut EditorSession, text: &str) -> EditorCompletion {
        let (before, after) = text.split_once('|').unwrap_or((text, ""));
        let full = format!("{before}{after}");
        session.open("Draft.md", 1, full.clone());
        let cursor = position_of(&full, &full, before.len());
        session.completion("Draft.md", cursor).unwrap().unwrap()
    }

    fn session() -> (tempfile::TempDir, EditorSession) {
        vault(&[
            (
                "Projects/Alpha.md",
                "---\naliases: [First]\nstatus: open\n---\n# Alpha\n## Status\n- x ^task\n#proj\n",
            ),
            ("Archive/Alpha.md", "# Old\n"),
            ("Beta.md", "# Beta\n"),
            ("img.png", "x"),
        ])
    }

    #[test]
    fn completes_link_targets_with_unique_names() {
        let (_dir, mut session) = session();
        let completion = complete(&mut session, "See [[al");
        assert_eq!(labels(&completion), vec!["Alpha", "Alpha"]);
        let inserts = completion
            .items
            .iter()
            .map(|item| item.insert_text.as_str())
            .collect::<Vec<_>>();
        assert!(inserts.contains(&"Projects/Alpha"));
        assert!(inserts.contains(&"Archive/Alpha"));
        assert_eq!(completion.range.start, TextPosition { line: 0, column: 6 });

        let completion = complete(&mut session, "[[fir");
        assert_eq!(completion.items[0].kind, EditorCompletionKind::Alias);
        assert_eq!(completion.items[0].insert_text, "Projects/Alpha|First");

        let completion = complete(&mut session, "[[img");
        assert_eq!(completion.items[0].insert_text, "img.png");
        assert_eq!(completion.items[0].kind, EditorCompletionKind::Attachment);
    }

    #[test]
    fn completes_headings_blocks_and_unsaved_notes() {
        let (_dir, mut session) = session();
        let completion = complete(&mut session, "[[First#sta");
        assert_eq!(labels(&completion), vec!["Status"]);
        let completion = complete(&mut session, "[[Projects/Alpha#^");
        assert_eq!(labels(&completion), vec!["task"]);
        // A note that exists only as an open buffer completes too.
        session.open("Fresh.md", 1, "# Brand new\n".into());
        let completion = complete(&mut session, "[[Fresh#");
        assert_eq!(labels(&completion), vec!["Brand new"]);
    }

    #[test]
    fn completes_tags_and_properties() {
        let (_dir, mut session) = session();
        let completion = complete(&mut session, "x #pr");
        assert_eq!(labels(&completion), vec!["#proj"]);
        assert_eq!(completion.items[0].insert_text, "proj");

        let completion = complete(&mut session, "---\nsta|\n---\n");
        assert_eq!(labels(&completion), vec!["status"]);
        assert_eq!(completion.items[0].insert_text, "status: ");

        let completion = complete(&mut session, "---\ntags: pr|\n---\n");
        assert_eq!(labels(&completion), vec!["#proj"]);
        assert_eq!(completion.items[0].insert_text, "proj");
    }
}
