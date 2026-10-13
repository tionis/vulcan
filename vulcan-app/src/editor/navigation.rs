//! Definition, references, hover, and symbols.

use super::{is_markdown_path, DocumentView, EditorLocation, EditorRange, EditorSession};
use crate::AppError;
use std::collections::BTreeSet;
use std::ops::Range;
use vulcan_core::editor::{
    block_id_range, element_at, heading_range, link_range, EditorElement, TextPosition,
};
use vulcan_core::resolver::LinkResolutionProblem;
use vulcan_core::{outline_note, LinkKind, ParsedDocument, RawLink};

/// Lines of a target shown in a hover preview.
const HOVER_LINES: usize = 20;
/// Bytes of a target shown in a hover preview.
const HOVER_BYTES: usize = 2_000;

/// Markdown shown for the element under the cursor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditorHover {
    pub markdown: String,
    pub range: EditorRange,
}

/// A section of a document's outline, nested under its parent heading.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditorSymbol {
    pub name: String,
    /// The `note outline` section id.
    pub section_id: String,
    pub level: u8,
    /// The whole section.
    pub range: EditorRange,
    /// The heading line.
    pub selection_range: EditorRange,
    pub children: Vec<EditorSymbol>,
}

/// A note or alias matching a workspace symbol query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditorWorkspaceSymbol {
    pub name: String,
    pub path: String,
    /// Set when the symbol is an alias of the note at `path`.
    pub alias_of: Option<String>,
}

/// Where a resolved link points inside its target document.
struct LinkTarget {
    path: String,
    range: Range<usize>,
    view: Option<DocumentView>,
}

impl EditorSession {
    /// The location a link under the cursor points to.
    pub fn definition(
        &self,
        path: &str,
        position: TextPosition,
    ) -> Result<Option<EditorLocation>, AppError> {
        let Some(document) = self.document(path) else {
            return Ok(None);
        };
        let Some(offset) = self.offset(path, position) else {
            return Ok(None);
        };
        let Some(EditorElement::Link { link, .. }) =
            element_at(&document.text, &document.parsed, offset)
        else {
            return Ok(None);
        };
        Ok(self
            .link_target(path, link)?
            .map(|target| self.target_location(target)))
    }

    /// Every link in the vault to what the cursor is on: the link's target,
    /// a heading of this document, or this document itself.
    pub fn references(
        &self,
        path: &str,
        position: TextPosition,
    ) -> Result<Vec<EditorLocation>, AppError> {
        let Some(document) = self.document(path) else {
            return Ok(Vec::new());
        };
        let Some(offset) = self.offset(path, position) else {
            return Ok(Vec::new());
        };
        let (target, heading) = match element_at(&document.text, &document.parsed, offset) {
            Some(EditorElement::Link { link, .. }) => {
                let resolver = self.resolver()?;
                let resolution = resolver.resolve(path, link, self.config().link_resolution);
                match resolution.resolved_target_id {
                    Some(target) => (target, None),
                    None => return Ok(Vec::new()),
                }
            }
            Some(EditorElement::Heading { heading, .. }) => {
                (path.to_string(), Some(heading.text.clone()))
            }
            _ => (path.to_string(), None),
        };
        self.links_to(&target, heading.as_deref())
    }

    /// Locations of links resolving to `target`, optionally only those to
    /// one of its headings. Sources are the cache's backlinks plus every
    /// open buffer, each re-read so unsaved edits count.
    pub fn links_to(
        &self,
        target: &str,
        heading: Option<&str>,
    ) -> Result<Vec<EditorLocation>, AppError> {
        let resolver = self.resolver()?;
        let mut sources = BTreeSet::new();
        if !self.readable(target) {
            return Ok(Vec::new());
        }
        let filter = self.read_filter();
        if let Ok(report) =
            vulcan_core::query_backlinks_with_filter(self.paths(), target, filter.as_ref())
        {
            sources.extend(report.backlinks.into_iter().map(|link| link.source_path));
        }
        sources.extend(self.open_paths().map(ToString::to_string));
        sources.insert(target.to_string());

        let mut locations = Vec::new();
        for source in sources {
            let Some(view) = self.view(&source) else {
                continue;
            };
            for link in &view.parsed.links {
                if matches!(link.link_kind, LinkKind::External) {
                    continue;
                }
                if let Some(heading) = heading {
                    if !link
                        .target_heading
                        .as_deref()
                        .is_some_and(|candidate| same_heading(candidate, heading))
                    {
                        continue;
                    }
                }
                let resolution = resolver.resolve(&source, link, self.config().link_resolution);
                if resolution.resolved_target_id.as_deref() == Some(target) {
                    locations.push(EditorLocation {
                        path: source.clone(),
                        range: self.range(&view.text, &view.line_index, link_range(link)),
                    });
                }
            }
        }
        Ok(locations)
    }

    /// A preview of the link target, or facts about the tag, under the
    /// cursor.
    pub fn hover(
        &self,
        path: &str,
        position: TextPosition,
    ) -> Result<Option<EditorHover>, AppError> {
        let Some(document) = self.document(path) else {
            return Ok(None);
        };
        let Some(offset) = self.offset(path, position) else {
            return Ok(None);
        };
        let Some(element) = element_at(&document.text, &document.parsed, offset) else {
            return Ok(None);
        };
        let range = self.range(&document.text, &document.line_index, element.range());
        let markdown = match element {
            EditorElement::Link { link, .. } => self.link_hover(path, link)?,
            EditorElement::Tag { tag, .. } => {
                let filter = self.read_filter();
                let count = vulcan_core::list_tags_with_filter(self.paths(), filter.as_ref())
                    .ok()
                    .and_then(|tags| {
                        tags.into_iter()
                            .find(|candidate| candidate.name.eq_ignore_ascii_case(&tag.tag_text))
                    })
                    .map_or(0, |entry| entry.count);
                Some(format!(
                    "**#{}**: {count} note{}",
                    tag.tag_text,
                    if count == 1 { "" } else { "s" }
                ))
            }
            _ => None,
        };
        Ok(markdown.map(|markdown| EditorHover { markdown, range }))
    }

    fn link_hover(&self, path: &str, link: &RawLink) -> Result<Option<String>, AppError> {
        if matches!(link.link_kind, LinkKind::External) {
            return Ok(None);
        }
        let resolver = self.resolver()?;
        let resolution = resolver.resolve(path, link, self.config().link_resolution);
        match resolution.problem {
            Some(LinkResolutionProblem::Unresolved) => {
                return Ok(Some(format!(
                    "Unresolved link to `{}`",
                    link.target_path_candidate.as_deref().unwrap_or_default()
                )))
            }
            Some(LinkResolutionProblem::Ambiguous(candidates)) => {
                let list = candidates
                    .iter()
                    .map(|candidate| format!("- `{candidate}`"))
                    .collect::<Vec<_>>()
                    .join("\n");
                return Ok(Some(format!("Ambiguous link; candidates:\n\n{list}")));
            }
            Some(LinkResolutionProblem::OutsideVault) => {
                return Ok(Some("Link points outside the vault".to_string()))
            }
            None => {}
        }
        let Some(target) = self.link_target(path, link)? else {
            return Ok(None);
        };
        let Some(view) = target.view else {
            return Ok(Some(format!("`{}`", target.path)));
        };
        let excerpt_start = if target.range.start == 0 {
            body_start(&view.text)
        } else {
            target.range.start
        };
        let excerpt_end = if link.target_block.is_some() {
            target.range.end
        } else if let Some(heading) = link.target_heading.as_deref() {
            section_end(&view.text, &view.parsed, heading).unwrap_or(view.text.len())
        } else {
            view.text.len()
        };
        let excerpt = truncate_excerpt(&view.text[excerpt_start..excerpt_end.max(excerpt_start)]);
        Ok(Some(format!("`{}`\n\n---\n\n{excerpt}", target.path)))
    }

    /// The document's outline in the shape of `vulcan note outline --output
    /// json`, computed from the open buffer (or the file) so unsaved
    /// headings appear.
    #[must_use]
    pub fn outline_report(&self, path: &str) -> Option<crate::notes::NoteOutlineReport> {
        let view = self.view(path)?;
        let outline = outline_note(&view.text, &view.parsed);
        let selection =
            vulcan_core::select_note_outline(&outline, &vulcan_core::NoteOutlineOptions::default())
                .ok()?;
        Some(crate::notes::NoteOutlineReport {
            path: path.to_string(),
            total_lines: selection.total_lines,
            frontmatter_span: selection.frontmatter_span,
            scope_section: selection.scope_section,
            depth_limit: None,
            sections: selection.sections,
            block_refs: selection.block_refs,
        })
    }

    /// The note's backlinks in the shape of `vulcan note backlinks --output
    /// json`, from the index and limited to readable sources. `None` when
    /// the note is not indexed (for example, never saved) or not readable.
    pub fn backlinks_report(
        &self,
        path: &str,
    ) -> Result<Option<vulcan_core::BacklinksReport>, AppError> {
        if !self.readable(path) || !self.has_cache() {
            return Ok(None);
        }
        let filter = self.read_filter();
        match vulcan_core::query_backlinks_with_filter(self.paths(), path, filter.as_ref()) {
            Ok(report) => Ok(Some(report)),
            Err(vulcan_core::GraphQueryError::NoteNotFound { .. }) => Ok(None),
            Err(error) => Err(AppError::operation(error)),
        }
    }

    /// The outline of an open document as nested symbols.
    #[must_use]
    pub fn document_symbols(&self, path: &str) -> Option<Vec<EditorSymbol>> {
        let document = self.document(path)?;
        let outline = outline_note(&document.text, &document.parsed);
        let mut flat = Vec::new();
        for section in outline.sections {
            let Some(name) = section.heading.clone() else {
                continue;
            };
            let heading = document
                .parsed
                .headings
                .iter()
                .find(|heading| {
                    line_of(&document.text, heading.byte_offset) + 1 == section.start_line
                })
                .map(|heading| heading_range(&document.text, heading));
            let range = line_range(&document.text, section.start_line, section.end_line);
            flat.push(EditorSymbol {
                name,
                section_id: section.id,
                level: section.level,
                range: self.range(&document.text, &document.line_index, range.clone()),
                selection_range: self.range(
                    &document.text,
                    &document.line_index,
                    heading.unwrap_or(range),
                ),
                children: Vec::new(),
            });
        }
        Some(nest_symbols(flat))
    }

    /// Notes whose path, file name, or alias contains `query`
    /// (case-insensitive), at most `limit` of them.
    pub fn workspace_symbols(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<EditorWorkspaceSymbol>, AppError> {
        let resolver = self.resolver()?;
        let query = query.to_lowercase();
        let mut symbols = Vec::new();
        for document in resolver.documents() {
            if !is_markdown_path(&document.path) {
                continue;
            }
            if document.path.to_lowercase().contains(&query) {
                symbols.push(EditorWorkspaceSymbol {
                    name: document.filename.clone(),
                    path: document.path.clone(),
                    alias_of: None,
                });
            }
            for alias in &document.aliases {
                if alias.to_lowercase().contains(&query) {
                    symbols.push(EditorWorkspaceSymbol {
                        name: alias.clone(),
                        path: document.path.clone(),
                        alias_of: Some(document.filename.clone()),
                    });
                }
            }
            if symbols.len() >= limit {
                symbols.truncate(limit);
                break;
            }
        }
        Ok(symbols)
    }

    fn link_target(&self, path: &str, link: &RawLink) -> Result<Option<LinkTarget>, AppError> {
        if matches!(link.link_kind, LinkKind::External) {
            return Ok(None);
        }
        let resolver = self.resolver()?;
        let resolution = resolver.resolve(path, link, self.config().link_resolution);
        let Some(target) = resolution.resolved_target_id else {
            return Ok(None);
        };
        let view = self.view(&target);
        let range = view
            .as_ref()
            .and_then(|view| {
                if let Some(block) = link.target_block.as_deref() {
                    view.parsed
                        .block_refs
                        .iter()
                        .find(|block_ref| block_ref.block_id_text.eq_ignore_ascii_case(block))
                        .map(|block_ref| {
                            let label = block_id_range(&view.text, block_ref);
                            block_ref.target_block_byte_start.min(label.start)
                                ..block_ref.target_block_byte_end.max(label.end)
                        })
                } else {
                    link.target_heading.as_deref().and_then(|heading| {
                        view.parsed
                            .headings
                            .iter()
                            .find(|candidate| same_heading(&candidate.text, heading))
                            .map(|candidate| heading_range(&view.text, candidate))
                    })
                }
            })
            .unwrap_or(0..0);
        Ok(Some(LinkTarget {
            path: target,
            range,
            view,
        }))
    }

    fn target_location(&self, target: LinkTarget) -> EditorLocation {
        let range = match &target.view {
            Some(view) => {
                let start = target.range.start;
                // Jump to the start of the target; a selection spanning a
                // whole block is noisy in most editors.
                self.range(&view.text, &view.line_index, start..start)
            }
            None => EditorRange {
                start: TextPosition { line: 0, column: 0 },
                end: TextPosition { line: 0, column: 0 },
            },
        };
        EditorLocation {
            path: target.path,
            range,
        }
    }
}

/// Whether a link's heading subpath names `heading`. Nested subpaths
/// (`[[note#Parent#Child]]`) match on their last segment.
pub(super) fn same_heading(subpath: &str, heading: &str) -> bool {
    let last = subpath.rsplit('#').next().unwrap_or(subpath);
    last.trim().eq_ignore_ascii_case(heading.trim())
}

fn section_end(text: &str, parsed: &ParsedDocument, heading: &str) -> Option<usize> {
    let index = parsed
        .headings
        .iter()
        .position(|candidate| same_heading(&candidate.text, heading))?;
    let level = parsed.headings[index].level;
    Some(
        parsed.headings[index + 1..]
            .iter()
            .find(|candidate| candidate.level <= level)
            .map_or(text.len(), |candidate| candidate.byte_offset),
    )
}

fn body_start(text: &str) -> usize {
    let Some(rest) = text
        .strip_prefix("---\n")
        .or_else(|| text.strip_prefix("---\r\n"))
    else {
        return 0;
    };
    let mut position = text.len() - rest.len();
    for line in rest.split_inclusive('\n') {
        position += line.len();
        if line.trim_end_matches(['\n', '\r']) == "---" {
            return position;
        }
    }
    0
}

fn truncate_excerpt(text: &str) -> String {
    let mut excerpt = text
        .trim_start_matches(['\n', '\r'])
        .lines()
        .take(HOVER_LINES)
        .collect::<Vec<_>>()
        .join("\n");
    if excerpt.len() > HOVER_BYTES {
        let mut end = HOVER_BYTES;
        while !excerpt.is_char_boundary(end) {
            end -= 1;
        }
        excerpt.truncate(end);
        excerpt.push('…');
    }
    excerpt.trim_end().to_string()
}

fn line_of(text: &str, offset: usize) -> usize {
    text[..offset.min(text.len())].matches('\n').count()
}

/// The byte range of 1-based inclusive lines `start..=end`.
fn line_range(text: &str, start_line: usize, end_line: usize) -> Range<usize> {
    let mut starts = vec![0];
    starts.extend(text.match_indices('\n').map(|(index, _)| index + 1));
    let start = starts
        .get(start_line.saturating_sub(1))
        .copied()
        .unwrap_or(text.len());
    let end = starts
        .get(end_line)
        .map_or(text.len(), |next| next.saturating_sub(1))
        .max(start);
    start..end
}

fn nest_symbols(flat: Vec<EditorSymbol>) -> Vec<EditorSymbol> {
    let mut roots: Vec<EditorSymbol> = Vec::new();
    let mut stack: Vec<EditorSymbol> = Vec::new();
    for symbol in flat {
        while stack.last().is_some_and(|open| open.level >= symbol.level) {
            let done = stack.pop().expect("checked");
            attach(&mut stack, &mut roots, done);
        }
        stack.push(symbol);
    }
    while let Some(done) = stack.pop() {
        attach(&mut stack, &mut roots, done);
    }
    roots
}

fn attach(stack: &mut [EditorSymbol], roots: &mut Vec<EditorSymbol>, symbol: EditorSymbol) {
    match stack.last_mut() {
        Some(parent) => parent.children.push(symbol),
        None => roots.push(symbol),
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::*;
    use super::*;

    const HOME: &str =
        "# Home\n\nSee [[Alpha#Status]] and [[Alpha#^task]] and #proj.\n\n## Sub\n\nx\n";
    const ALPHA: &str =
        "---\naliases: [First]\n---\n# Alpha\n\nIntro.\n\n## Status\n\nGreen.\n\n- item ^task\n";

    fn session() -> (tempfile::TempDir, EditorSession) {
        let (dir, mut session) = vault(&[
            ("Home.md", HOME),
            ("Alpha.md", ALPHA),
            ("Other.md", "Back to [[Home]] and [[First]].\n"),
        ]);
        session.open("Home.md", 1, HOME.to_string());
        (dir, session)
    }

    #[test]
    fn definition_jumps_to_headings_and_blocks() {
        let (_dir, session) = session();
        let location = session
            .definition("Home.md", position_of(HOME, "Alpha#Status", 1))
            .unwrap()
            .unwrap();
        assert_eq!(location.path, "Alpha.md");
        assert_eq!(location.range.start, position_of(ALPHA, "## Status", 0));

        let location = session
            .definition("Home.md", position_of(HOME, "Alpha#^task", 1))
            .unwrap()
            .unwrap();
        assert_eq!(location.range.start, position_of(ALPHA, "- item", 0));

        assert_eq!(
            session
                .definition("Home.md", position_of(HOME, "and #proj", 0))
                .unwrap(),
            None
        );
    }

    #[test]
    fn references_include_unsaved_buffers() {
        let (_dir, mut session) = session();
        let in_body = position_of(HOME, "\nx\n", 1);
        let paths = |session: &EditorSession| {
            session
                .references("Home.md", in_body)
                .unwrap()
                .into_iter()
                .map(|location| location.path)
                .collect::<Vec<_>>()
        };
        assert_eq!(paths(&session), vec!["Other.md"]);
        session.open("Draft.md", 1, "[[Home]]\n".into());
        assert_eq!(paths(&session), vec!["Draft.md", "Other.md"]);

        // On a link, references are the links to its target.
        let locations = session
            .references("Home.md", position_of(HOME, "Alpha#Status", 1))
            .unwrap();
        let mut sources = locations
            .iter()
            .map(|location| location.path.as_str())
            .collect::<Vec<_>>();
        sources.dedup();
        assert_eq!(sources, vec!["Home.md", "Other.md"]);

        // On a heading, references are the links to that section.
        session.open("Alpha.md", 1, ALPHA.to_string());
        let locations = session
            .references("Alpha.md", position_of(ALPHA, "## Status", 4))
            .unwrap();
        assert_eq!(locations.len(), 1);
        assert_eq!(locations[0].path, "Home.md");
        assert_eq!(
            locations[0].range.start,
            position_of(HOME, "[[Alpha#Status", 0)
        );
    }

    #[test]
    fn hover_previews_sections_and_reports_problems() {
        let (_dir, mut session) = session();
        let hover = session
            .hover("Home.md", position_of(HOME, "Alpha#Status", 1))
            .unwrap()
            .unwrap();
        assert!(
            hover.markdown.starts_with("`Alpha.md`"),
            "{}",
            hover.markdown
        );
        assert!(hover.markdown.contains("## Status\n\nGreen."));
        assert!(!hover.markdown.contains("Intro"));

        session.open("Draft.md", 1, "[[Nowhere]]\n".into());
        let hover = session
            .hover("Draft.md", TextPosition { line: 0, column: 3 })
            .unwrap()
            .unwrap();
        assert_eq!(hover.markdown, "Unresolved link to `Nowhere`");
    }

    #[test]
    fn symbols_nest_sections_and_search_aliases() {
        let (_dir, session) = session();
        let symbols = session.document_symbols("Home.md").unwrap();
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "Home");
        assert_eq!(symbols[0].children[0].name, "Sub");
        assert_eq!(symbols[0].children[0].section_id, "home/sub@5");

        let found = session.workspace_symbols("first", 10).unwrap();
        assert_eq!(
            found,
            vec![EditorWorkspaceSymbol {
                name: "First".into(),
                path: "Alpha.md".into(),
                alias_of: Some("Alpha".into()),
            }]
        );
    }
}
