//! Extract a heading's section into a new note.
//!
//! The section (the heading and everything up to the next heading of the
//! same or a higher level) becomes a note named after the heading, in the
//! source note's folder, and a link to it takes the section's place. Links
//! anywhere in the vault to the heading, its subheadings, or its block ids
//! follow the content into the new note. Like renames, this works on the
//! editor's text: open buffers, including unsaved ones, are edited in the
//! editor, and closed files change on disk in one batch.

use super::edits::{text_edits, EditorDocumentEdit, EditorEditPlan};
use super::navigation::same_heading;
use super::EditorSession;
use crate::AppError;
use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;
use std::path::Path;
use vulcan_core::editor::{heading_range, link_range, TextPosition};
use vulcan_core::move_rewrite::{relative_path_from_source, rewrite_link_destination};
use vulcan_core::ordinary_write::OrdinaryWriteChange;
use vulcan_core::{LinkKind, LinkStylePreference, ParsedDocument, RawLink};

/// The section being extracted.
struct Section {
    source: String,
    destination: String,
    /// The section's byte range in the source text.
    range: Range<usize>,
    title: String,
    headings: Vec<String>,
    blocks: Vec<String>,
}

impl Section {
    /// Where a link to the source note's `heading` or `block` points once
    /// the section moved: the new note and the subpath it keeps there.
    fn moved_target(
        &self,
        heading: Option<&str>,
        block: Option<&str>,
    ) -> Option<(Option<String>, Option<String>)> {
        if let Some(heading) = heading {
            if same_heading(heading, &self.title) {
                return Some((None, None));
            }
            return self
                .headings
                .iter()
                .any(|known| same_heading(heading, known))
                .then(|| (Some(heading.to_string()), None));
        }
        let block = block?;
        self.blocks
            .iter()
            .any(|known| known.eq_ignore_ascii_case(block))
            .then(|| (None, Some(block.to_string())))
    }
}

impl EditorSession {
    /// The heading whose line holds `position` in an open document, when a
    /// section could be extracted there.
    pub(super) fn extractable_heading(&self, path: &str, position: TextPosition) -> Option<usize> {
        let document = self.document(path)?;
        let offset = self.offset(path, position)?;
        document.parsed.headings.iter().position(|heading| {
            let line = heading_range(&document.text, heading);
            line.start <= offset && offset <= line.end
        })
    }

    /// Plan extracting the section of the heading at `position` into a new
    /// note next to the open note at `path`.
    pub fn plan_extract_section(
        &self,
        path: &str,
        position: TextPosition,
    ) -> Result<EditorEditPlan, AppError> {
        let invalid = |message: &str| AppError::operation_with_code("invalid_request", message);
        let document = self
            .document(path)
            .ok_or_else(|| invalid("open the note to extract a section from it"))?;
        let index = self
            .extractable_heading(path, position)
            .ok_or_else(|| invalid("place the cursor on a heading to extract its section"))?;
        let section = self.section(path, &document.text, &document.parsed, index)?;
        let resolver = self.resolver()?;
        let mut documents_after = resolver
            .documents()
            .iter()
            .map(|document| document.path.clone())
            .collect::<Vec<_>>();
        documents_after.push(section.destination.clone());
        documents_after.sort();
        let rewrite = |link: &RawLink, written_in: &str, now_in: &str| {
            self.retarget(
                &section,
                &resolver,
                &documents_after,
                link,
                written_in,
                now_in,
            )
        };

        // The new note: the section without its heading line.
        let text = &document.text;
        let body_start = text[section.range.clone()]
            .find('\n')
            .map_or(section.range.end, |newline| {
                section.range.start + newline + 1
            });
        let body = rewritten(
            text,
            body_start..section.range.end,
            &document.parsed,
            |link| rewrite(link, path, &section.destination),
        );
        let body = body.trim_start_matches(['\n', '\r']).trim_end();
        let content = if body.is_empty() {
            String::new()
        } else {
            format!("{body}\n")
        };

        // The source: a link in place of the section, other links retargeted.
        let link = self.link_to(path, &section.destination, &section.title, &documents_after);
        let after_section = &text[section.range.end..];
        let replacement = if after_section.is_empty() {
            format!("{link}\n")
        } else {
            format!("{link}\n\n")
        };
        let before = rewritten(text, 0..section.range.start, &document.parsed, |link| {
            rewrite(link, path, path)
        });
        let after = rewritten(
            text,
            section.range.end..text.len(),
            &document.parsed,
            |link| rewrite(link, path, path),
        );
        let source_after = format!("{before}{replacement}{after}");

        let mut plan = EditorEditPlan {
            action: "extract_section",
            report: serde_json::Value::Null,
            disk: vec![OrdinaryWriteChange {
                path: section.destination.clone(),
                before: None,
                after: Some(content),
            }],
            disk_renames: BTreeMap::new(),
            open: vec![EditorDocumentEdit {
                path: path.to_string(),
                version: document.version,
                edits: text_edits(self, text, &source_after),
            }],
            editor_rename: None,
            moved: None,
        };

        let rewritten_paths = self.retarget_linking_notes(path, &mut plan, &rewrite)?;
        plan.report = serde_json::json!({
            "action": "extract_section",
            "source": path,
            "heading": section.title,
            "destination": section.destination,
            "rewritten_files": rewritten_paths,
        });
        Ok(plan)
    }

    /// Retarget links into the section in every other note that may hold
    /// one: indexed backlinks, and open buffers whose unsaved text may add
    /// more. Returns the notes changed.
    fn retarget_linking_notes(
        &self,
        path: &str,
        plan: &mut EditorEditPlan,
        rewrite: &dyn Fn(&RawLink, &str, &str) -> Option<String>,
    ) -> Result<Vec<String>, AppError> {
        let mut linking = self
            .backlinks_report(path)?
            .map(|report| {
                report
                    .backlinks
                    .into_iter()
                    .map(|backlink| backlink.source_path)
                    .collect::<BTreeSet<_>>()
            })
            .unwrap_or_default();
        linking.extend(self.open_paths().map(ToString::to_string));
        linking.remove(path);
        let mut rewritten_paths = Vec::new();
        for other in linking {
            let Some(view) = self.view(&other) else {
                continue;
            };
            let updated = rewritten(&view.text, 0..view.text.len(), &view.parsed, |link| {
                rewrite(link, &other, &other)
            });
            if updated == view.text {
                continue;
            }
            rewritten_paths.push(other.clone());
            match self.document(&other) {
                Some(open) => plan.open.push(EditorDocumentEdit {
                    edits: text_edits(self, &open.text, &updated),
                    path: other,
                    version: open.version,
                }),
                None => plan.disk.push(OrdinaryWriteChange {
                    path: other,
                    before: Some(view.text),
                    after: Some(updated),
                }),
            }
        }
        Ok(rewritten_paths)
    }

    fn section(
        &self,
        path: &str,
        text: &str,
        parsed: &ParsedDocument,
        index: usize,
    ) -> Result<Section, AppError> {
        let heading = &parsed.headings[index];
        let end = parsed.headings[index + 1..]
            .iter()
            .find(|next| next.level <= heading.level)
            .map_or(text.len(), |next| next.byte_offset);
        let range = heading.byte_offset..end;
        let name = vulcan_core::portable_note_segment(
            &heading.text.replace(['#', '^', '[', ']', '|'], " "),
        );
        let destination = match Path::new(path).parent().and_then(Path::to_str) {
            Some(folder) if !folder.is_empty() => format!("{folder}/{name}.md"),
            _ => format!("{name}.md"),
        };
        let taken = self.paths().vault_root().join(&destination).exists()
            || self.document(&destination).is_some()
            || self
                .resolver()?
                .documents()
                .iter()
                .any(|document| document.path.eq_ignore_ascii_case(&destination));
        if taken {
            return Err(AppError::operation_with_code(
                "conflict",
                format!("{destination} already exists"),
            ));
        }
        Ok(Section {
            source: path.to_string(),
            destination,
            title: heading.text.clone(),
            headings: parsed.headings[index..]
                .iter()
                .take_while(|inner| inner.byte_offset < end)
                .map(|inner| inner.text.clone())
                .collect(),
            blocks: parsed
                .block_refs
                .iter()
                .filter(|block| range.contains(&block.block_id_byte_offset))
                .map(|block| block.block_id_text.clone())
                .collect(),
            range,
        })
    }

    /// The new text of `link`, written in `written_in` and now living in
    /// `now_in`, or `None` when it stays as it is.
    fn retarget(
        &self,
        section: &Section,
        resolver: &vulcan_core::editor::EditorResolver,
        documents_after: &[String],
        link: &RawLink,
        written_in: &str,
        now_in: &str,
    ) -> Option<String> {
        if matches!(link.link_kind, LinkKind::External) {
            return None;
        }
        let mode = self.config().link_resolution;
        let target = resolver
            .resolve(written_in, link, mode)
            .resolved_target_id?;
        if target != section.source {
            return None;
        }
        let heading = link.target_heading.as_deref();
        let block = link.target_block.as_deref();
        let (destination, heading, block) = match section.moved_target(heading, block) {
            Some((heading, block)) => (section.destination.as_str(), heading, block),
            // A same-note link that moved into the new note now needs the
            // source note's name.
            None if now_in != written_in
                && link
                    .target_path_candidate
                    .as_deref()
                    .unwrap_or("")
                    .is_empty() =>
            {
                (
                    section.source.as_str(),
                    heading.map(ToString::to_string),
                    block.map(ToString::to_string),
                )
            }
            None => return None,
        };
        let local = destination == now_in && (heading.is_some() || block.is_some());
        let rewritten = rewrite_link_destination(
            link,
            now_in,
            (!local).then_some(destination),
            heading.as_deref(),
            block.as_deref(),
            documents_after,
            mode,
            self.config().link_style,
        );
        (rewritten != link.raw_text).then_some(rewritten)
    }

    /// A link from `from` to the new note, in the vault's link style.
    fn link_to(
        &self,
        from: &str,
        destination: &str,
        title: &str,
        documents_after: &[String],
    ) -> String {
        match self.config().link_style {
            LinkStylePreference::Markdown => format!(
                "[{}](<{}>)",
                title.replace('\\', "\\\\").replace(']', "\\]"),
                relative_path_from_source(from, destination)
            ),
            LinkStylePreference::Wikilink => {
                let placeholder = vulcan_core::parse_document("[[placeholder]]", self.config())
                    .links
                    .remove(0);
                rewrite_link_destination(
                    &placeholder,
                    from,
                    Some(destination),
                    None,
                    None,
                    documents_after,
                    self.config().link_resolution,
                    LinkStylePreference::Wikilink,
                )
            }
        }
    }
}

/// `text[range]` with each link fully inside it replaced by `rewrite`'s
/// answer.
fn rewritten(
    text: &str,
    range: Range<usize>,
    parsed: &ParsedDocument,
    mut rewrite: impl FnMut(&RawLink) -> Option<String>,
) -> String {
    let mut links = parsed
        .links
        .iter()
        .filter(|link| {
            let span = link_range(link);
            range.start <= span.start
                && span.end <= range.end
                && text.get(span.clone()) == Some(link.raw_text.as_str())
        })
        .collect::<Vec<_>>();
    links.sort_by_key(|link| link.byte_offset);
    let mut output = String::with_capacity(range.len());
    let mut at = range.start;
    for link in links {
        let span = link_range(link);
        if span.start < at {
            continue;
        }
        if let Some(replacement) = rewrite(link) {
            output.push_str(&text[at..span.start]);
            output.push_str(&replacement);
            at = span.end;
        }
    }
    output.push_str(&text[at..range.end]);
    output
}

#[cfg(test)]
mod tests {
    use super::super::test_support::*;

    const SOURCE: &str = "# Source\n\nIntro [[#Details]] and [[#Other]].\n\n## Details\n\nBody ^b1\n\n### Deep\n\nSee [[#Other]] and [[#Deep]].\n\n## Other\n\nRest\n";
    const REF: &str =
        "[[Source#Details]] [[Source#Deep]] [[Source#^b1]] [[Source#Other]] [[Source]]\n";

    #[test]
    fn extracts_a_section_and_retargets_links_into_it() {
        let (_dir, mut session) = vault(&[
            ("Notes/Source.md", SOURCE),
            ("Ref.md", REF),
            ("Open.md", "x\n"),
        ]);
        session.open("Notes/Source.md", 1, SOURCE.into());
        // Unsaved text links into the section too.
        session.open("Open.md", 3, "[[Source#Deep]]\n".into());
        let position = position_of(SOURCE, "## Details", 3);
        let plan = session
            .plan_extract_section("Notes/Source.md", position)
            .unwrap();

        assert_eq!(plan.report["destination"], "Notes/Details.md");
        let created = &plan.disk[0];
        assert_eq!(created.path, "Notes/Details.md");
        assert_eq!(created.before, None);
        assert_eq!(
            created.after.as_deref(),
            Some("Body ^b1\n\n### Deep\n\nSee [[Source#Other]] and [[#Deep]].\n")
        );
        let source = &plan.open[0];
        assert_eq!(
            (source.path.as_str(), source.version),
            ("Notes/Source.md", 1)
        );
        assert_eq!(
            apply(SOURCE, &source.edits),
            "# Source\n\nIntro [[Details]] and [[#Other]].\n\n[[Details]]\n\n## Other\n\nRest\n"
        );
        let open = plan
            .open
            .iter()
            .find(|edit| edit.path == "Open.md")
            .unwrap();
        assert_eq!(
            apply("[[Source#Deep]]\n", &open.edits),
            "[[Details#Deep]]\n"
        );
        let closed = plan
            .disk
            .iter()
            .find(|change| change.path == "Ref.md")
            .unwrap();
        assert_eq!(
            closed.after.as_deref(),
            Some("[[Details]] [[Details#Deep]] [[Details#^b1]] [[Source#Other]] [[Source]]\n")
        );
    }

    #[test]
    fn the_last_section_and_collisions() {
        let text = "# Top\n\n## Last\n\nTail\n";
        let (_dir, mut session) = vault(&[("Top.md", text), ("Taken.md", "x\n")]);
        session.open("Top.md", 1, text.into());
        let plan = session
            .plan_extract_section("Top.md", position_of(text, "## Last", 0))
            .unwrap();
        assert_eq!(plan.disk[0].after.as_deref(), Some("Tail\n"));
        assert_eq!(apply(text, &plan.open[0].edits), "# Top\n\n[[Last]]\n");

        let text = "# Taken\n\nBody\n";
        session.open("Top.md", 2, text.into());
        let error = session
            .plan_extract_section("Top.md", position_of(text, "Taken", 0))
            .unwrap_err();
        assert!(error.to_string().contains("already exists"), "{error}");
        assert!(session
            .plan_extract_section("Top.md", position_of(text, "Body", 0))
            .is_err());
    }
}
